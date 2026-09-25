//! Finding the Generic Interrupt Controller on `AArch64`, and which of two
//! drivers runs it.
//!
//! A GICv2 is `crate::arch::gicv2`, shared with ARMv7-A; a GICv3 is
//! [`super::gicv3`], which is this architecture's alone because its CPU
//! interface is system registers. Where the registers are comes from the MADT
//! on a machine with ACPI and from the device tree on one without -- the
//! Pixel 7's loader hands over a tree and no RSDP. Every other caller goes
//! through the functions at the bottom, which send it to the driver `init`
//! chose.
//!
//! A version neither driver knows is refused by name, rather than writing one
//! version's layouts into the other's registers and producing a machine that
//! takes no interrupts for reasons nothing explains.

use core::sync::atomic::{AtomicU8, Ordering};

use ferrix_acpi::{Acpi, MadtEntry};
use ferrix_fdt::{Fdt, GicVersion};

use super::gicv3;
use crate::acpi::DirectMap;
use crate::arch::gicv2;

/// Which driver [`init`] brought up: 2 or 3, and 0 before it has.
static VERSION: AtomicU8 = AtomicU8::new(0);

/// Where the two register blocks are, according to firmware.
struct Layout {
    /// Physical address of the distributor.
    distributor: u64,
    /// Physical address of the CPU interface, which every core shares. Zero on
    /// a GICv3, which has none.
    cpu_interface: u64,
    /// The first redistributor discovery range, a GICv3's: address and bytes.
    redistributors: Option<(u64, u64)>,
    /// The architecture version firmware reports, or zero for "probe it".
    version: u8,
    /// The first `GICv2m` frame: its address, and its SPI range if firmware
    /// states one.
    msi_frame: Option<(u64, Option<(u32, u32)>)>,
}

/// Read the distributor and CPU interface addresses out of the MADT.
fn layout(acpi: &Acpi<'_, DirectMap>) -> Result<Layout, &'static str> {
    let madt = acpi.madt().map_err(|_| "the machine has no MADT")?;

    let mut distributor = 0;
    let mut cpu_interface = 0;
    let mut version = 0;
    let mut banked = true;
    let mut msi_frame = None;
    let mut redistributors = None;

    for entry in madt.entries() {
        match entry {
            MadtEntry::Gicd(gicd) => {
                distributor = gicd.physical_base_address;
                version = gicd.gic_version;
            }
            // A GICv2 CPU interface is banked: every core reaches its own
            // through the same address, which is why one mapping serves them
            // all. Firmware lists it once per core, and every copy is
            // required to agree.
            MadtEntry::Gicc(gicc) if gicc.is_enabled() => {
                if cpu_interface == 0 {
                    cpu_interface = gicc.physical_base_address;
                } else if gicc.physical_base_address != cpu_interface {
                    banked = false;
                }
            }
            MadtEntry::Gicr(gicr) if redistributors.is_none() => {
                redistributors = Some((
                    gicr.discovery_range_base,
                    u64::from(gicr.discovery_range_length),
                ));
            }
            MadtEntry::GicMsiFrame(frame) if msi_frame.is_none() => {
                let spis = frame
                    .spis()
                    .map(|(base, count)| (u32::from(base), u32::from(count)));
                msi_frame = Some((frame.base_address, spis));
            }
            _ => {}
        }
    }

    if distributor == 0 {
        return Err("the MADT describes no GIC distributor");
    }
    if !banked {
        return Err("the cores' GIC CPU interfaces are at different addresses, not banked");
    }
    Ok(Layout {
        distributor,
        cpu_interface,
        redistributors,
        version,
        msi_frame,
    })
}

/// The architecture version the MADT's layout describes.
///
/// Version 0 means firmware declined to say and the OS should probe. On a
/// machine that also gave a CPU interface address, that is a GICv2 layout:
/// GICv3 has no CPU interface to give an address for, and has a redistributor
/// range instead.
const fn described_version(layout: &Layout) -> u8 {
    match layout.version {
        0 if layout.cpu_interface != 0 => 2,
        0 if layout.redistributors.is_some() => 3,
        other => other,
    }
}

/// Bring up the GICv2 the MADT describes, and its `GICv2m` frame if it has
/// one.
///
/// # Safety
///
/// `gicv2::init`'s contract.
unsafe fn init_v2(layout: &Layout) -> Result<(), &'static str> {
    if layout.cpu_interface == 0 {
        return Err("the MADT describes a GICv2 with no CPU interface");
    }
    // SAFETY: the caller's contract is `gicv2::init`'s, and these are the two
    // register blocks firmware describes, checked above to be a GICv2's.
    unsafe { gicv2::init(layout.distributor, layout.cpu_interface)? };
    // A frame that cannot be used leaves the machine without MSI vectors and
    // nothing else: `gicv2::msi_allocate` says so to whoever asks for one.
    if let Some((base, spis)) = layout.msi_frame {
        let _ = gicv2::init_msi_frame(base, spis);
    }
    Ok(())
}

/// Bring up the GICv3 the MADT describes.
///
/// # Safety
///
/// `gicv3::init`'s contract.
unsafe fn init_v3(layout: &Layout) -> Result<(), &'static str> {
    let (base, len) = layout
        .redistributors
        .ok_or("the MADT describes a GICv3 with no redistributor range")?;
    // SAFETY: the caller's contract is `gicv3::init`'s.
    unsafe { gicv3::init(layout.distributor, base, len) }
}

/// Find the controller in the MADT and bring it up, returning its version.
///
/// # Safety
///
/// Both drivers' `init` contract: once, on the boot CPU, after the vector
/// table is installed and while interrupts are masked.
pub(crate) unsafe fn init(acpi: &Acpi<'_, DirectMap>) -> Result<u8, &'static str> {
    let layout = layout(acpi)?;
    let version = match described_version(&layout) {
        // SAFETY: the caller's contract, passed on.
        2 => unsafe { init_v2(&layout) }.map(|()| 2)?,
        // A `GICv4` is a GICv3 with virtual interrupts added, which a kernel
        // that is not a hypervisor never touches.
        // SAFETY: as above.
        3 | 4 => unsafe { init_v3(&layout) }.map(|()| 3)?,
        _ => return Err("this GIC is neither a GICv2 nor a GICv3"),
    };
    VERSION.store(version, Ordering::Relaxed);
    Ok(version)
}

/// Find the controller in the device tree and bring it up, returning its
/// version.
///
/// # Safety
///
/// As [`init`].
pub(crate) unsafe fn init_from_tree(tree: &Fdt<'_>) -> Result<u8, &'static str> {
    let gic = tree
        .interrupt_controller()
        .ok_or("the device tree describes no interrupt controller")?;
    let distributor = gic
        .distributor()
        .ok_or("the device tree's GIC has no distributor")?;
    let version = match gic.version {
        GicVersion::V2 => {
            let cpu_interface = gic
                .cpu_interface()
                .ok_or("the device tree's GICv2 has no CPU interface")?;
            // SAFETY: the caller's contract is `gicv2::init`'s.
            unsafe { gicv2::init(distributor.address, cpu_interface.address)? };
            if let Some(frame) = tree.gicv2m_frames().next() {
                let _ = gicv2::init_msi_frame(
                    frame.region.address,
                    frame.spi_base.zip(frame.spi_count),
                );
            }
            2
        }
        GicVersion::V3 => {
            let redistributors = gic
                .redistributor()
                .ok_or("the device tree's GICv3 has no redistributor range")?;
            // SAFETY: the caller's contract is `gicv3::init`'s.
            unsafe {
                gicv3::init(
                    distributor.address,
                    redistributors.address,
                    redistributors.size,
                )?;
            }
            3
        }
    };
    VERSION.store(version, Ordering::Relaxed);
    Ok(version)
}

/// Whether [`init`] chose the GICv3 driver.
fn is_v3() -> bool {
    VERSION.load(Ordering::Relaxed) == 3
}

/// Identifiers from here up are not interrupts, on either version.
pub(crate) const FIRST_SPECIAL_ID: u32 = gicv2::FIRST_SPECIAL_ID;

/// The software-generated interrupt inter-processor interrupts arrive on.
pub(crate) const IPI_SGI: u32 = gicv2::IPI_SGI;

const _: () = assert!(
    gicv3::FIRST_SPECIAL_ID == FIRST_SPECIAL_ID && gicv3::IPI_SGI == IPI_SGI,
    "the two drivers number the same things the same way"
);

/// Let interrupt `id` through.
pub(crate) fn enable(id: u32) {
    if is_v3() {
        gicv3::enable(id);
    } else {
        gicv2::enable(id);
    }
}

/// Stop delivering `id`.
pub(crate) fn disable(id: u32) {
    if is_v3() {
        gicv3::disable(id);
    } else {
        gicv2::disable(id);
    }
}

/// Claim the interrupt that arrived, or `None` if there was none.
pub(crate) fn claim() -> Option<(u32, u32)> {
    if is_v3() {
        gicv3::claim()
    } else {
        gicv2::claim()
    }
}

/// Retire an interrupt [`claim`] returned.
pub(crate) fn complete(acknowledgement: u32) {
    if is_v3() {
        gicv3::complete(acknowledgement);
    } else {
        gicv2::complete(acknowledgement);
    }
}

/// Bring up this core's side of the controller, on a secondary.
///
/// A GICv3 can fail here where a GICv2 cannot -- no redistributor for this
/// core, or one that will not wake -- and a secondary has nobody to report
/// to yet, so the failure is the one a GICv2 would give silently: a core that
/// runs and takes no interrupts. `crate::smp`'s check that every core is
/// preempted is what finds it.
pub(crate) fn init_this_cpu() {
    if is_v3() {
        let _ = gicv3::init_this_cpu();
    } else {
        gicv2::init_this_cpu();
    }
}

/// Interrupt every core but this one on [`IPI_SGI`].
pub(crate) fn send_sgi_to_others() {
    if is_v3() {
        gicv3::send_sgi_to_others();
    } else {
        gicv2::send_sgi_to_others();
    }
}

/// Take an interrupt the device whose writes carry requester ID `_device` can
/// raise by message. A GICv3 has none to give until its ITS has a driver.
pub(crate) fn msi_allocate(_device: u32) -> Result<crate::irq::Msi, &'static str> {
    if is_v3() {
        return Err("this GICv3's ITS has no driver, so there are no MSI vectors");
    }
    gicv2::msi_allocate()
}

/// The page a device's MSI writes land in, if there is one.
pub(crate) fn msi_doorbell() -> Option<u64> {
    if is_v3() { None } else { gicv2::msi_doorbell() }
}
