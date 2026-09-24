//! The virtio 1.x PCI transport: where a virtio device's registers are.
//!
//! A modern virtio device on PCI does not put its registers at fixed offsets.
//! It publishes one vendor-specific capability per register block — common
//! configuration, notification, interrupt status, device-specific
//! configuration — each naming a BAR, an offset into it and a length (virtio
//! 1.2 §4.1.4). This module finds and checks them.
//!
//! The checks matter because the kernel is going to map exactly these regions
//! into a driver process and nothing else, and a capability is the device's
//! claim about where they are. A region that does not fit in the BAR it names
//! would hand the driver a mapping of whatever is next to that BAR.

use crate::bar::{Bar, Region};
use crate::capability::{Capabilities, Capability, ID_VENDOR};
use crate::header::Identity;
use crate::{Address, ConfigSpace, LEGACY_CONFIG_SPACE_SIZE, PciError};

/// The virtio vendor ID, which Red Hat donated.
pub const VENDOR: u16 = 0x1AF4;

/// A modern device's PCI device ID is this plus its virtio device type.
pub const MODERN_DEVICE_BASE: u16 = 0x1040;

/// The last modern device ID.
pub const MODERN_DEVICE_LAST: u16 = 0x107F;

/// The first transitional device ID. A transitional device carries its type
/// in the subsystem ID instead.
pub const TRANSITIONAL_DEVICE_FIRST: u16 = 0x1000;

/// The last transitional device ID.
pub const TRANSITIONAL_DEVICE_LAST: u16 = 0x103F;

/// Virtio device type: network card.
pub const TYPE_NET: u16 = 1;
/// Virtio device type: block device.
pub const TYPE_BLOCK: u16 = 2;
/// Virtio device type: entropy source.
pub const TYPE_ENTROPY: u16 = 4;
/// Virtio device type: GPU.
pub const TYPE_GPU: u16 = 16;

/// Configuration type: common configuration.
pub const CFG_COMMON: u8 = 1;
/// Configuration type: notifications.
pub const CFG_NOTIFY: u8 = 2;
/// Configuration type: interrupt status.
pub const CFG_ISR: u8 = 3;
/// Configuration type: device-specific configuration.
pub const CFG_DEVICE: u8 = 4;
/// Configuration type: access to the others through configuration space.
pub const CFG_PCI: u8 = 5;
/// Configuration type: a shared memory region, which is not registers but
/// memory the device and the driver both see (virtio 1.2 §4.1.4.7).
/// virtio-gpu's host-visible window, where the host maps blob resources, is
/// one.
pub const CFG_SHARED_MEMORY: u8 = 8;

/// Bytes of a virtio capability.
pub const CAPABILITY_LEN: u16 = 16;

/// Bytes of the notification capability, which adds a multiplier.
pub const NOTIFY_CAPABILITY_LEN: u16 = 20;

/// Bytes of `struct virtio_pci_cap64`, the shared memory capability, whose
/// offset and length have high halves: a region may be larger than 4 GiB.
pub const SHARED_MEMORY_CAPABILITY_LEN: u16 = 24;

/// The virtio device type of a function, or `None` if it is not a virtio
/// device.
///
/// `subsystem` is the endpoint's subsystem ID, which is where a transitional
/// device keeps its type.
#[must_use]
pub const fn device_type(identity: &Identity, subsystem: u16) -> Option<u16> {
    if identity.vendor != VENDOR {
        return None;
    }
    match identity.device {
        MODERN_DEVICE_BASE..=MODERN_DEVICE_LAST => Some(identity.device - MODERN_DEVICE_BASE),
        TRANSITIONAL_DEVICE_FIRST..=TRANSITIONAL_DEVICE_LAST => Some(subsystem),
        _ => None,
    }
}

/// A register block inside a BAR.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Location {
    /// Where the capability describing it is, for errors.
    pub capability: u16,
    /// The BAR.
    pub bar: u8,
    /// Bytes into the BAR.
    pub offset: u32,
    /// Bytes long.
    pub length: u32,
}

impl Location {
    /// Whether the block lies inside `region`, which must be the sized BAR
    /// this location names, and that BAR is memory. An I/O BAR's address is a
    /// port number, so a block in one has no physical address to map.
    #[must_use]
    pub const fn fits(self, region: &Region) -> bool {
        matches!(region.bar, Bar::Memory { .. })
            && region.index == self.bar
            && region.contains(self.offset as u64, self.length as u64)
    }
}

/// A shared memory region inside a BAR: memory rather than registers, named
/// by an id the device type defines.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SharedMemory {
    /// Where the capability describing it is, for errors.
    pub capability: u16,
    /// The BAR.
    pub bar: u8,
    /// The device type's name for the region.
    pub id: u8,
    /// Bytes into the BAR.
    pub offset: u64,
    /// Bytes long.
    pub length: u64,
}

impl SharedMemory {
    /// The shared memory region `function` names `id`, if it has one: the
    /// first, as the specification tells a driver to take.
    ///
    /// Returns `None` for a region in a BAR index the specification reserves,
    /// which a driver ignores, as [`read`] does.
    ///
    /// # Errors
    ///
    /// Any error walking the capability list, and
    /// [`PciError::CapabilityTooShort`] for a shared memory capability
    /// shorter than `struct virtio_pci_cap64`.
    pub fn find<C: ConfigSpace + ?Sized>(
        space: &C,
        function: Address,
        id: u8,
    ) -> Result<Option<Self>, PciError> {
        for capability in Capabilities::new(space, function) {
            let capability = capability?;
            let at = capability.offset;
            if capability.id != ID_VENDOR || space.read8(function, at + 3) != CFG_SHARED_MEMORY {
                continue;
            }
            let declared = u16::from(space.read8(function, at + 2));
            let room = LEGACY_CONFIG_SPACE_SIZE.saturating_sub(at);
            if declared < SHARED_MEMORY_CAPABILITY_LEN || room < SHARED_MEMORY_CAPABILITY_LEN {
                return Err(PciError::CapabilityTooShort {
                    function,
                    at,
                    len: declared.min(room),
                });
            }
            let bar = space.read8(function, at + 4);
            if bar > 5 || space.read8(function, at + 5) != id {
                continue;
            }
            let wide = |low: u16, high: u16| {
                u64::from(space.read32(function, at + low))
                    | u64::from(space.read32(function, at + high)) << 32
            };
            return Ok(Some(Self {
                capability: at,
                bar,
                id,
                offset: wide(8, 16),
                length: wide(12, 20),
            }));
        }
        Ok(None)
    }

    /// Whether the region lies inside `region`, which must be the sized BAR
    /// it names, and that BAR is memory: [`Location::fits`] for a region
    /// whose offset and length need not fit in 32 bits.
    #[must_use]
    pub const fn fits(self, region: &Region) -> bool {
        matches!(region.bar, Bar::Memory { .. })
            && region.index == self.bar
            && self.length != 0
            && region.contains(self.offset, self.length)
    }
}

/// One virtio capability, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VirtioCapability {
    /// Which register block it describes.
    pub cfg_type: u8,
    /// Where the block is.
    pub location: Location,
    /// For the notification capability, the multiplier a queue's notify
    /// offset is scaled by.
    pub notify_multiplier: Option<u32>,
}

/// Decode `capability` as a virtio one.
///
/// Returns `None` for a capability that is not vendor-specific, and for one
/// naming a BAR index the specification reserves, which a driver is required
/// to ignore rather than reject.
///
/// # Errors
///
/// [`PciError::CapabilityTooShort`] if the capability declares a length
/// shorter than its format, or runs past the legacy space.
pub fn read<C: ConfigSpace + ?Sized>(
    space: &C,
    capability: Capability,
) -> Result<Option<VirtioCapability>, PciError> {
    let Capability {
        function,
        id,
        offset: at,
    } = capability;
    if id != ID_VENDOR {
        return Ok(None);
    }
    let declared = u16::from(space.read8(function, at + 2));
    let cfg_type = space.read8(function, at + 3);
    let needed = if cfg_type == CFG_NOTIFY {
        NOTIFY_CAPABILITY_LEN
    } else {
        CAPABILITY_LEN
    };
    let room = LEGACY_CONFIG_SPACE_SIZE.saturating_sub(at);
    if declared < needed || room < needed {
        return Err(PciError::CapabilityTooShort {
            function,
            at,
            len: declared.min(room),
        });
    }
    let bar = space.read8(function, at + 4);
    if bar > 5 {
        return Ok(None);
    }
    Ok(Some(VirtioCapability {
        cfg_type,
        location: Location {
            capability: at,
            bar,
            offset: space.read32(function, at + 8),
            length: space.read32(function, at + 12),
        },
        notify_multiplier: (cfg_type == CFG_NOTIFY).then(|| space.read32(function, at + 16)),
    }))
}

/// The register blocks a modern virtio driver needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Transport {
    /// The function.
    pub function: Address,
    /// Common configuration: features, status, queue setup.
    pub common: Location,
    /// Where queue notifications are written.
    pub notify: Location,
    /// What a queue's notify offset is multiplied by.
    pub notify_multiplier: u32,
    /// Interrupt status, for a driver using the legacy pin.
    pub isr: Location,
    /// Device-specific configuration, which some device types do not have.
    pub device: Option<Location>,
}

impl Transport {
    /// Find the transport's register blocks in `function`'s capabilities,
    /// taking the first of each type, as the specification tells a driver to.
    ///
    /// Returns `None` if common configuration, notification or interrupt
    /// status is missing, which is what a legacy-only device looks like.
    ///
    /// # Errors
    ///
    /// Any error walking the capability list, or decoding a vendor capability.
    pub fn find<C: ConfigSpace + ?Sized>(
        space: &C,
        function: Address,
    ) -> Result<Option<Self>, PciError> {
        let mut common = None;
        let mut notify = None;
        let mut isr = None;
        let mut device = None;
        for capability in Capabilities::new(space, function) {
            let Some(found) = read(space, capability?)? else {
                continue;
            };
            let slot = match found.cfg_type {
                CFG_COMMON => &mut common,
                CFG_NOTIFY => &mut notify,
                CFG_ISR => &mut isr,
                CFG_DEVICE => &mut device,
                _ => continue,
            };
            if slot.is_none() {
                *slot = Some(found);
            }
        }
        let (Some(common), Some(notify), Some(isr)) = (common, notify, isr) else {
            return Ok(None);
        };
        Ok(Some(Transport {
            function,
            common: common.location,
            notify: notify.location,
            notify_multiplier: notify.notify_multiplier.unwrap_or(0),
            isr: isr.location,
            device: device.map(|found| found.location),
        }))
    }

    /// Check every block lies inside the BAR it names, given the function's
    /// sized BARs.
    ///
    /// # Errors
    ///
    /// [`PciError::VirtioRegion`] naming the first capability whose block
    /// names a BAR not in `regions` or does not fit in it.
    pub fn verify(&self, regions: &[Region]) -> Result<(), PciError> {
        let blocks = [
            Some(self.common),
            Some(self.notify),
            Some(self.isr),
            self.device,
        ];
        for location in blocks.into_iter().flatten() {
            let inside = regions
                .iter()
                .find(|region| region.index == location.bar)
                .is_some_and(|region| location.fits(region));
            if !inside {
                return Err(PciError::VirtioRegion {
                    function: self.function,
                    at: location.capability,
                });
            }
        }
        Ok(())
    }
}
