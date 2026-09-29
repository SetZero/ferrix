//! The configuration header: the first 64 bytes of every function.
//!
//! The first sixteen bytes are common to every header type and say what the
//! function is. What follows depends on the header type: an endpoint (type 0)
//! has six BARs and its interrupt pin, a PCI-to-PCI bridge (type 1) has two
//! BARs and the bus numbers that say which buses are behind it. `CardBus`
//! bridges (type 2) are read far enough to be recognised and no further.

use crate::{Address, ConfigSpace, PciError};

/// Offset of the vendor ID.
pub const VENDOR_ID: u16 = 0x00;
/// Offset of the device ID.
pub const DEVICE_ID: u16 = 0x02;
/// Offset of the command register.
pub const COMMAND: u16 = 0x04;
/// Offset of the status register.
pub const STATUS: u16 = 0x06;
/// Offset of the revision ID; the class code is the three bytes above it.
pub const REVISION_ID: u16 = 0x08;
/// Offset of the header type.
pub const HEADER_TYPE: u16 = 0x0E;
/// Offset of the first BAR, on every header type that has one.
pub const BAR0: u16 = 0x10;
/// Offset of a type 1 header's primary bus number.
pub const PRIMARY_BUS: u16 = 0x18;
/// Offset of a type 1 header's secondary bus number.
pub const SECONDARY_BUS: u16 = 0x19;
/// Offset of a type 1 header's subordinate bus number.
pub const SUBORDINATE_BUS: u16 = 0x1A;
/// Offset of a type 0 header's subsystem vendor ID.
pub const SUBSYSTEM_VENDOR_ID: u16 = 0x2C;
/// Offset of a type 0 header's subsystem ID.
pub const SUBSYSTEM_ID: u16 = 0x2E;
/// Offset of the capabilities pointer in type 0 and type 1 headers.
pub const CAPABILITIES_POINTER: u16 = 0x34;
/// Offset of a `CardBus` header's capabilities pointer, which is elsewhere.
pub const CARDBUS_CAPABILITIES_POINTER: u16 = 0x14;
/// Offset of the interrupt line: what firmware routed the pin to.
pub const INTERRUPT_LINE: u16 = 0x3C;
/// Offset of the interrupt pin: 0 for none, 1 to 4 for INTA# to INTD#.
pub const INTERRUPT_PIN: u16 = 0x3D;

/// Command: the function decodes its I/O BARs.
pub const COMMAND_IO_SPACE: u16 = 1 << 0;
/// Command: the function decodes its memory BARs.
pub const COMMAND_MEMORY_SPACE: u16 = 1 << 1;
/// Command: the function may initiate DMA.
pub const COMMAND_BUS_MASTER: u16 = 1 << 2;
/// Command: the function's legacy interrupt pin is masked.
pub const COMMAND_INTERRUPT_DISABLE: u16 = 1 << 10;

/// Status: the capabilities pointer is valid.
pub const STATUS_CAPABILITIES_LIST: u16 = 1 << 4;

/// Header type: the device has functions other than function 0.
pub const HEADER_TYPE_MULTIFUNCTION: u8 = 1 << 7;

/// Class: mass storage controller.
pub const CLASS_MASS_STORAGE: u8 = 0x01;
/// Class: bridge.
pub const CLASS_BRIDGE: u8 = 0x06;
/// Subclass of [`CLASS_BRIDGE`]: host bridge.
pub const SUBCLASS_HOST_BRIDGE: u8 = 0x00;
/// Subclass of [`CLASS_BRIDGE`]: PCI-to-PCI bridge.
pub const SUBCLASS_PCI_BRIDGE: u8 = 0x04;

/// Whether a vendor and device ID pair, read as one 32-bit register, is what
/// an empty slot answers with.
///
/// All ones is the answer of a slot with nothing in it. All zeros, and either
/// half being all ones, are not valid IDs either, and Linux treats all four
/// the same way for the same reason: some bridges answer an empty slot with
/// them.
#[must_use]
pub const fn is_absent(ids: u32) -> bool {
    matches!(ids, 0 | u32::MAX | 0x0000_FFFF | 0xFFFF_0000)
}

/// What the rest of a header looks like.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HeaderKind {
    /// Type 0: an ordinary function.
    Endpoint,
    /// Type 1: a PCI-to-PCI bridge, with buses behind it.
    Bridge,
    /// Type 2: a `CardBus` bridge.
    CardBus,
    /// Any other value of the low seven bits, which is reserved.
    Reserved(u8),
}

impl HeaderKind {
    /// The kind a header type register describes.
    #[must_use]
    pub const fn from_register(header_type: u8) -> Self {
        match header_type & !HEADER_TYPE_MULTIFUNCTION {
            0 => HeaderKind::Endpoint,
            1 => HeaderKind::Bridge,
            2 => HeaderKind::CardBus,
            other => HeaderKind::Reserved(other),
        }
    }

    /// The low seven bits this kind was decoded from.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            HeaderKind::Endpoint => 0,
            HeaderKind::Bridge => 1,
            HeaderKind::CardBus => 2,
            HeaderKind::Reserved(code) => code,
        }
    }

    /// How many BAR slots the header has.
    #[must_use]
    pub const fn bar_slots(self) -> u8 {
        match self {
            HeaderKind::Endpoint => 6,
            HeaderKind::Bridge => 2,
            HeaderKind::CardBus | HeaderKind::Reserved(_) => 0,
        }
    }

    /// Where the header keeps its capabilities pointer, if it is a header
    /// whose layout is known.
    #[must_use]
    pub const fn capabilities_pointer(self) -> Option<u16> {
        match self {
            HeaderKind::Endpoint | HeaderKind::Bridge => Some(CAPABILITIES_POINTER),
            HeaderKind::CardBus => Some(CARDBUS_CAPABILITIES_POINTER),
            HeaderKind::Reserved(_) => None,
        }
    }
}

/// The class code: what kind of function this is, in three bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Class {
    /// The base class.
    pub base: u8,
    /// The subclass.
    pub sub: u8,
    /// The programming interface.
    pub interface: u8,
}

impl Class {
    /// The class code in the top three bytes of the register at
    /// [`REVISION_ID`].
    #[must_use]
    pub const fn from_register(register: u32) -> Self {
        let [_, interface, sub, base] = register.to_le_bytes();
        Class {
            base,
            sub,
            interface,
        }
    }
}

/// The part of the header every function has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Identity {
    /// The vendor ID.
    pub vendor: u16,
    /// The device ID.
    pub device: u16,
    /// The revision ID.
    pub revision: u8,
    /// The class code.
    pub class: Class,
    /// The header's layout.
    pub kind: HeaderKind,
    /// Whether the device has functions beyond function 0. Only meaningful
    /// when read from function 0.
    pub multifunction: bool,
}

impl Identity {
    /// Read `function`'s identity, or `None` if nothing answers there.
    pub fn read<C: ConfigSpace + ?Sized>(space: &C, function: Address) -> Option<Self> {
        let ids = space.read32(function, VENDOR_ID);
        if is_absent(ids) {
            return None;
        }
        let class = space.read32(function, REVISION_ID);
        let header_type = space.read8(function, HEADER_TYPE);
        let [vendor_low, vendor_high, device_low, device_high] = ids.to_le_bytes();
        Some(Identity {
            vendor: u16::from_le_bytes([vendor_low, vendor_high]),
            device: u16::from_le_bytes([device_low, device_high]),
            revision: class.to_le_bytes()[0],
            class: Class::from_register(class),
            kind: HeaderKind::from_register(header_type),
            multifunction: header_type & HEADER_TYPE_MULTIFUNCTION != 0,
        })
    }
}

/// A PCI-to-PCI bridge's bus numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BusNumbers {
    /// The bus the bridge is on.
    pub primary: u8,
    /// The bus directly behind the bridge.
    pub secondary: u8,
    /// The highest-numbered bus anywhere behind the bridge.
    pub subordinate: u8,
}

impl BusNumbers {
    /// Read a bridge's bus numbers.
    ///
    /// # Errors
    ///
    /// [`PciError::HeaderType`] if `function` does not have a type 1 header.
    /// The numbers are returned as read: whether firmware configured them at
    /// all is the caller's question, and [`crate::walk`] asks it.
    pub fn read<C: ConfigSpace + ?Sized>(space: &C, function: Address) -> Result<Self, PciError> {
        let kind = HeaderKind::from_register(space.read8(function, HEADER_TYPE));
        if kind != HeaderKind::Bridge {
            return Err(PciError::HeaderType {
                function,
                kind: kind.code(),
            });
        }
        Ok(BusNumbers {
            primary: space.read8(function, PRIMARY_BUS),
            secondary: space.read8(function, SECONDARY_BUS),
            subordinate: space.read8(function, SUBORDINATE_BUS),
        })
    }
}

/// A type 0 header's fields past the BARs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Endpoint {
    /// Who built the board the function is on, as opposed to its chip.
    pub subsystem_vendor: u16,
    /// The board's own ID. For a transitional virtio device, this is the
    /// virtio device type.
    pub subsystem: u16,
    /// The legacy interrupt pin, 1 to 4 for INTA# to INTD#, or 0 for none.
    pub interrupt_pin: u8,
    /// What firmware says it routed the pin to. Meaningful on a PC only.
    pub interrupt_line: u8,
}

impl Endpoint {
    /// Read an endpoint's fields.
    ///
    /// # Errors
    ///
    /// [`PciError::HeaderType`] if `function` does not have a type 0 header.
    pub fn read<C: ConfigSpace + ?Sized>(space: &C, function: Address) -> Result<Self, PciError> {
        let kind = HeaderKind::from_register(space.read8(function, HEADER_TYPE));
        if kind != HeaderKind::Endpoint {
            return Err(PciError::HeaderType {
                function,
                kind: kind.code(),
            });
        }
        Ok(Endpoint {
            subsystem_vendor: space.read16(function, SUBSYSTEM_VENDOR_ID),
            subsystem: space.read16(function, SUBSYSTEM_ID),
            interrupt_pin: space.read8(function, INTERRUPT_PIN),
            interrupt_line: space.read8(function, INTERRUPT_LINE),
        })
    }
}
