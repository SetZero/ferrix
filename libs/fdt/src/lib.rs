//! Flattened device tree (DTB) reader.
//!
//! On AArch64 the firmware hands the loader a device tree and that blob is the
//! only description of the machine the kernel gets: where RAM is, which UART is
//! the console, where the interrupt controller lives, which processors there
//! are. This crate turns those
//! bytes into answers and nothing else — it does not modify a tree, resolve
//! phandles or apply overlays.
//!
//! # Totality
//!
//! The input is chosen by firmware and parsed in ring 0, so every function here
//! either returns a value or an error for *any* byte string. Fields are pulled
//! out with a bounds-checked slice access and [`u32::from_be_bytes`], which is
//! why the crate is `#![forbid(unsafe_code)]`: casting the blob to a header
//! pointer would be shorter and would put the sharpest surface in the system
//! into an `unsafe` block for nothing.
//!
//! [`Fdt::parse`] walks the whole structure block once and rejects the blob if
//! anything in it is malformed, so the accessors afterwards cannot fail — they
//! answer `None` when a node or property is absent, never when the blob is bad.
//! The walk is iterative over an explicit [`MAX_DEPTH`] frame stack, so a tree
//! that nests forever is refused rather than recursed into.
//!
//! ```
//! # use ferrix_fdt::{Fdt, FdtError};
//! # fn example(blob: &[u8]) -> Result<(), FdtError> {
//! let fdt = Fdt::parse(blob)?;
//! for region in fdt.memory() {
//!     let _ = (region.address, region.size);
//! }
//! if let Some(console) = fdt.console() {
//!     let _ = (console.compatible(), console.reg().next());
//! }
//! # Ok(())
//! # }
//! ```

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

// ---------------------------------------------------------------------------
// Format constants
// ---------------------------------------------------------------------------

/// The word every device tree blob starts with.
pub const FDT_MAGIC: u32 = 0xd00d_feed;

/// Opens a node. A NUL-terminated name, padded to four bytes, follows.
pub const FDT_BEGIN_NODE: u32 = 1;
/// Closes the innermost open node.
pub const FDT_END_NODE: u32 = 2;
/// Introduces a property: a length, a name offset, then that many bytes of
/// value padded to four.
pub const FDT_PROP: u32 = 3;
/// Padding. Skipped wherever a token is expected.
pub const FDT_NOP: u32 = 4;
/// Ends the structure block.
pub const FDT_END: u32 = 9;

/// Size of the header: ten big-endian `u32` fields.
pub const HEADER_SIZE: usize = 40;

/// The structure block layout this parser understands.
///
/// Version 17 is what every producer in use emits (dtc, QEMU, U-Boot, EDK2),
/// and it is the first version carrying `size_dt_struct`. Accepting an older
/// layout would mean guessing where the structure block ends, which is exactly
/// the guess a hostile blob would want us to make.
pub const SUPPORTED_VERSION: u32 = 17;

/// Deepest node nesting accepted, the root included.
///
/// The walk is iterative and keeps one frame per open node, so this bounds both
/// the stack footprint (a little under a kilobyte) and the work a blob can ask
/// for. Real trees are five or six levels deep.
pub const MAX_DEPTH: usize = 64;

/// Most structure block tokens read before a blob is declared hostile.
///
/// Termination does not rest on this — every token advances the cursor by at
/// least four bytes inside a fixed slice — but a blob that is all padding still
/// should not cost more than a bounded amount of time.
pub const MAX_TOKENS: usize = 1 << 22;

/// `#address-cells` in effect when no ancestor declares one.
pub const DEFAULT_ADDRESS_CELLS: u32 = 2;

/// `#size-cells` in effect when no ancestor declares one.
///
/// The specification's stated default is 1. Two is used here because every tree
/// this kernel is handed declares both at the root, and on a 64-bit machine a
/// missing declaration is likelier to mean "as wide as the address" than "a
/// 32-bit size". A caller that cares should read the property itself.
pub const DEFAULT_SIZE_CELLS: u32 = 2;

/// Widest `#address-cells` or `#size-cells` honoured. A wider declaration is
/// ignored in favour of the inherited value.
pub const MAX_CELLS: u32 = 4;

/// `compatible` string of a GICv3 interrupt controller.
pub const GICV3_COMPATIBLE: &str = "arm,gic-v3";

/// `compatible` strings of a GICv2. QEMU's `virt` machine says
/// `cortex-a15-gic` whatever the CPU, the STM32MP1 says `cortex-a7-gic`, and
/// `gic-400` is what most other boards of that generation carry. The register
/// layout is the same for all three.
pub const GICV2_COMPATIBLES: [&str; 3] = ["arm,cortex-a15-gic", "arm,cortex-a7-gic", "arm,gic-400"];

/// `compatible` string of the architected timer on a 64-bit CPU.
pub const TIMER_COMPATIBLE: &str = "arm,armv8-timer";

/// `compatible` string of the same timer on a 32-bit CPU. QEMU lists it after
/// [`TIMER_COMPATIBLE`] for a 64-bit CPU and alone for a 32-bit one, so a
/// reader that knew only the first would find no timer on a Cortex-A7.
pub const TIMER_V7_COMPATIBLE: &str = "arm,armv7-timer";

/// `compatible` strings of PSCI firmware, newest first.
pub const PSCI_COMPATIBLES: [&str; 3] = ["arm,psci-1.0", "arm,psci-0.2", "arm,psci"];

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a blob was rejected.
///
/// Every variant means the same thing operationally — do not trust this tree —
/// but they are distinguished because a truncated blob and a blob that is not a
/// device tree at all call for very different responses from whoever reads the
/// message on the console.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FdtError {
    /// The blob is smaller than a device tree header.
    TooShort,
    /// The first word is not [`FDT_MAGIC`]. Carries what was found.
    BadMagic(u32),
    /// `totalsize` is smaller than a header or larger than the slice given.
    BadTotalSize,
    /// The blob is laid out for a version this parser does not read. Carries
    /// the `version` field.
    UnsupportedVersion(u32),
    /// A block offset or size violates the alignment the format requires.
    Misaligned,
    /// The structure or strings block does not lie inside `totalsize`.
    BlockOutOfBounds,
    /// The memory reservation block does not lie inside `totalsize`.
    ReservationOutOfBounds,
    /// A token that is none of the five defined ones. Carries its value.
    BadToken(u32),
    /// A node or property name runs to the end of its block with no NUL.
    UnterminatedString,
    /// A node or property name is not UTF-8.
    NotUtf8,
    /// A property's value runs past the end of the structure block.
    PropertyOutOfBounds,
    /// Nodes nest deeper than [`MAX_DEPTH`].
    DepthOverflow,
    /// A node ended that never began, or the tree ended with a node open.
    UnbalancedNode,
    /// The structure block runs out before [`FDT_END`].
    MissingEnd,
    /// The structure block holds more than [`MAX_TOKENS`] tokens.
    TokenBudget,
}

impl fmt::Display for FdtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FdtError::TooShort => f.write_str("blob is shorter than a device tree header"),
            FdtError::BadMagic(found) => write!(f, "magic {found:#010x} is not a device tree"),
            FdtError::BadTotalSize => f.write_str("totalsize does not fit the blob"),
            FdtError::UnsupportedVersion(found) => write!(f, "device tree version {found}"),
            FdtError::Misaligned => f.write_str("a block offset is misaligned"),
            FdtError::BlockOutOfBounds => f.write_str("a block lies outside the blob"),
            FdtError::ReservationOutOfBounds => {
                f.write_str("the reservation block lies outside the blob")
            }
            FdtError::BadToken(found) => write!(f, "token {found} is not a device tree token"),
            FdtError::UnterminatedString => f.write_str("a name has no terminator"),
            FdtError::NotUtf8 => f.write_str("a name is not UTF-8"),
            FdtError::PropertyOutOfBounds => {
                f.write_str("a property runs past the structure block")
            }
            FdtError::DepthOverflow => f.write_str("nodes nest too deeply"),
            FdtError::UnbalancedNode => f.write_str("node begin and end tokens are unbalanced"),
            FdtError::MissingEnd => f.write_str("the structure block has no end token"),
            FdtError::TokenBudget => f.write_str("the structure block has too many tokens"),
        }
    }
}

// ---------------------------------------------------------------------------
// Big-endian field access
// ---------------------------------------------------------------------------

/// Read a big-endian `u32` at `offset`, or `None` past the end.
fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let field: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(field))
}

/// Read a big-endian `u64` at `offset`, or `None` past the end.
fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    let field: [u8; 8] = bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(u64::from_be_bytes(field))
}

/// The NUL-terminated string starting at `offset`, without its terminator.
fn cstr_at(bytes: &[u8], offset: usize) -> Result<&str, FdtError> {
    let tail = bytes.get(offset..).ok_or(FdtError::UnterminatedString)?;
    let end = tail
        .iter()
        .position(|&byte| byte == 0)
        .ok_or(FdtError::UnterminatedString)?;
    let text = tail.get(..end).ok_or(FdtError::UnterminatedString)?;
    core::str::from_utf8(text).map_err(|_| FdtError::NotUtf8)
}

/// Round `len` up to the four-byte boundary the token stream pads to.
fn align4(len: usize) -> Option<usize> {
    Some(len.checked_add(3)? & !3)
}

/// The part of a node name before its unit address, so that `memory` names the
/// same node as `memory@40000000`.
fn base_name(name: &str) -> &str {
    name.split('@').next().unwrap_or(name)
}

/// Read `cells` consecutive big-endian words as one integer, advancing
/// `offset`.
///
/// `None` if the words run out or the value does not fit in 64 bits. Cell
/// counts other than one and two are read rather than refused, so that a
/// three-cell PCI-style address with a zero high word still yields an answer.
fn read_cells(value: &[u8], offset: &mut usize, cells: u32) -> Option<u64> {
    let mut accumulated: u64 = 0;
    for _ in 0..cells {
        let word = u32_at(value, *offset)?;
        if accumulated >> 32 != 0 {
            return None;
        }
        accumulated = (accumulated << 32) | u64::from(word);
        *offset = offset.checked_add(4)?;
    }
    Some(accumulated)
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

/// The device tree header, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// Size of the whole blob, magic word included.
    pub totalsize: u32,
    /// Offset of the structure block from the start of the blob.
    pub off_dt_struct: u32,
    /// Offset of the strings block from the start of the blob.
    pub off_dt_strings: u32,
    /// Offset of the memory reservation block from the start of the blob.
    pub off_mem_rsvmap: u32,
    /// Layout version this blob was written to.
    pub version: u32,
    /// Oldest layout version this blob is still readable as.
    pub last_comp_version: u32,
    /// Physical ID of the CPU the firmware is running on.
    pub boot_cpuid_phys: u32,
    /// Size of the strings block in bytes.
    pub size_dt_strings: u32,
    /// Size of the structure block in bytes.
    pub size_dt_struct: u32,
}

impl Header {
    /// Decode and check the header at the start of `blob`.
    pub fn parse(blob: &[u8]) -> Result<Self, FdtError> {
        let magic = u32_at(blob, 0).ok_or(FdtError::TooShort)?;
        if magic != FDT_MAGIC {
            return Err(FdtError::BadMagic(magic));
        }
        if blob.len() < HEADER_SIZE {
            return Err(FdtError::TooShort);
        }
        let header = Header {
            totalsize: u32_at(blob, 4).ok_or(FdtError::TooShort)?,
            off_dt_struct: u32_at(blob, 8).ok_or(FdtError::TooShort)?,
            off_dt_strings: u32_at(blob, 12).ok_or(FdtError::TooShort)?,
            off_mem_rsvmap: u32_at(blob, 16).ok_or(FdtError::TooShort)?,
            version: u32_at(blob, 20).ok_or(FdtError::TooShort)?,
            last_comp_version: u32_at(blob, 24).ok_or(FdtError::TooShort)?,
            boot_cpuid_phys: u32_at(blob, 28).ok_or(FdtError::TooShort)?,
            size_dt_strings: u32_at(blob, 32).ok_or(FdtError::TooShort)?,
            size_dt_struct: u32_at(blob, 36).ok_or(FdtError::TooShort)?,
        };
        header.check(blob.len())?;
        Ok(header)
    }

    /// Reject a header whose own fields already contradict each other or the
    /// `available` bytes, before any block is sliced out of the blob.
    fn check(&self, available: usize) -> Result<(), FdtError> {
        let total = usize::try_from(self.totalsize).map_err(|_| FdtError::BadTotalSize)?;
        if total < HEADER_SIZE || total > available {
            return Err(FdtError::BadTotalSize);
        }
        if self.version < SUPPORTED_VERSION || self.last_comp_version > SUPPORTED_VERSION {
            return Err(FdtError::UnsupportedVersion(self.version));
        }
        // The reservation block holds 64-bit pairs and the structure block
        // 32-bit tokens; the format requires each block to be aligned for the
        // words it holds, and a misaligned one is a malformed blob rather than
        // something to read anyway.
        if !self.off_mem_rsvmap.is_multiple_of(8) {
            return Err(FdtError::Misaligned);
        }
        if !self.off_dt_struct.is_multiple_of(4) || !self.size_dt_struct.is_multiple_of(4) {
            return Err(FdtError::Misaligned);
        }
        // Even an empty reservation list is a terminating pair of zero words,
        // so those sixteen bytes always have to be there.
        let rsvmap_end = self
            .off_mem_rsvmap
            .checked_add(16)
            .ok_or(FdtError::ReservationOutOfBounds)?;
        if self.off_mem_rsvmap < HEADER_SIZE as u32 || rsvmap_end > self.totalsize {
            return Err(FdtError::ReservationOutOfBounds);
        }
        Ok(())
    }
}

/// The slice of `blob` a block header field describes.
fn block(blob: &[u8], offset: u32, size: u32) -> Result<&[u8], FdtError> {
    let start = usize::try_from(offset).map_err(|_| FdtError::BlockOutOfBounds)?;
    let len = usize::try_from(size).map_err(|_| FdtError::BlockOutOfBounds)?;
    let end = start.checked_add(len).ok_or(FdtError::BlockOutOfBounds)?;
    blob.get(start..end).ok_or(FdtError::BlockOutOfBounds)
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// One `address, size` pair out of a `reg` property.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Region {
    /// First address of the region, decoded with the parent's
    /// `#address-cells`.
    pub address: u64,
    /// Length of the region in bytes, decoded with the parent's
    /// `#size-cells`. Zero when the parent declares `#size-cells = <0>`.
    pub size: u64,
}

/// One entry of the memory reservation block: memory the kernel must not hand
/// to the frame allocator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MemoryReservation {
    /// Physical address of the reserved range.
    pub address: u64,
    /// Length of the reserved range in bytes.
    pub size: u64,
}

/// One property of one node. The value is borrowed from the blob and is
/// uninterpreted: the accessors decode it, they never copy it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Property<'a> {
    /// Property name, from the strings block.
    pub name: &'a str,
    /// Raw value bytes, exactly as long as the blob says.
    pub value: &'a [u8],
}

impl<'a> Property<'a> {
    /// Length of the raw value in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.value.len()
    }

    /// True if the value is empty, which is how the format spells a boolean
    /// property that is simply present.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// The value as a single big-endian word, or `None` if it is not exactly
    /// one word long.
    #[must_use]
    pub fn as_u32(&self) -> Option<u32> {
        if self.value.len() != 4 {
            return None;
        }
        u32_at(self.value, 0)
    }

    /// The value as a single big-endian doubleword, or `None` if it is not
    /// exactly two words long.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        if self.value.len() != 8 {
            return None;
        }
        u64_at(self.value, 0)
    }

    /// The value as a string: the bytes up to the first NUL.
    #[must_use]
    pub fn as_str(&self) -> Option<&'a str> {
        cstr_at(self.value, 0).ok()
    }

    /// The value as a list of strings, which is how `compatible` and
    /// `stdout-path` alternatives are encoded.
    #[must_use]
    pub const fn strings(&self) -> Strings<'a> {
        Strings {
            value: self.value,
            offset: 0,
        }
    }

    /// True if the value is a string list holding exactly `needle`.
    #[must_use]
    pub fn contains_string(&self, needle: &str) -> bool {
        self.strings().any(|entry| entry == needle)
    }

    /// The value as a sequence of big-endian words, which is how `interrupts`
    /// and the cell properties are encoded.
    #[must_use]
    pub const fn cells(&self) -> Cells<'a> {
        Cells {
            value: self.value,
            offset: 0,
        }
    }
}

/// The strings in a string-list property, in order.
#[derive(Clone, Copy, Debug)]
pub struct Strings<'a> {
    value: &'a [u8],
    offset: usize,
}

impl<'a> Iterator for Strings<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.offset >= self.value.len() {
            return None;
        }
        let text = cstr_at(self.value, self.offset).ok()?;
        self.offset = self.offset.checked_add(text.len())?.checked_add(1)?;
        Some(text)
    }
}

/// The big-endian words of a property value, in order.
#[derive(Clone, Copy, Debug)]
pub struct Cells<'a> {
    value: &'a [u8],
    offset: usize,
}

impl Iterator for Cells<'_> {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        let word = u32_at(self.value, self.offset)?;
        self.offset = self.offset.checked_add(4)?;
        Some(word)
    }
}

/// The `address, size` pairs of a `reg` property, decoded with the cell counts
/// its parent node declared.
#[derive(Clone, Copy, Debug)]
pub struct Regions<'a> {
    value: &'a [u8],
    offset: usize,
    address_cells: u32,
    size_cells: u32,
}

impl Iterator for Regions<'_> {
    type Item = Region;

    fn next(&mut self) -> Option<Region> {
        // A pair of zero-width fields would consume nothing and never end the
        // iteration, so a parent that declares both as zero has no regions.
        if self.address_cells == 0 && self.size_cells == 0 {
            return None;
        }
        let mut offset = self.offset;
        let address = read_cells(self.value, &mut offset, self.address_cells)?;
        let size = read_cells(self.value, &mut offset, self.size_cells)?;
        self.offset = offset;
        Some(Region { address, size })
    }
}

// ---------------------------------------------------------------------------
// The token cursor
// ---------------------------------------------------------------------------

/// A position in the structure block.
///
/// Every method advances the position by at least four bytes inside a fixed
/// slice, which is what makes every walk in this crate terminate without
/// needing to trust the blob.
#[derive(Clone, Copy, Debug)]
struct Cursor<'a> {
    structs: &'a [u8],
    strings: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    /// A cursor at the start of the structure block.
    const fn new(structs: &'a [u8], strings: &'a [u8]) -> Self {
        Cursor {
            structs,
            strings,
            position: 0,
        }
    }

    /// Read one token and step over it.
    fn token(&mut self) -> Result<u32, FdtError> {
        let token = u32_at(self.structs, self.position).ok_or(FdtError::MissingEnd)?;
        self.position = self.position.checked_add(4).ok_or(FdtError::MissingEnd)?;
        Ok(token)
    }

    /// Read the node name that follows [`FDT_BEGIN_NODE`] and step over it and
    /// its padding.
    fn node_name(&mut self) -> Result<&'a str, FdtError> {
        let name = cstr_at(self.structs, self.position)?;
        let length = name
            .len()
            .checked_add(1)
            .ok_or(FdtError::UnterminatedString)?;
        let step = align4(length).ok_or(FdtError::UnterminatedString)?;
        self.position = self
            .position
            .checked_add(step)
            .ok_or(FdtError::PropertyOutOfBounds)?;
        Ok(name)
    }

    /// Read the length, name and value that follow [`FDT_PROP`] and step over
    /// them and the value's padding.
    fn property(&mut self) -> Result<Property<'a>, FdtError> {
        let len = usize::try_from(self.token()?).map_err(|_| FdtError::PropertyOutOfBounds)?;
        let nameoff = usize::try_from(self.token()?).map_err(|_| FdtError::PropertyOutOfBounds)?;
        let name = cstr_at(self.strings, nameoff)?;
        let end = self
            .position
            .checked_add(len)
            .ok_or(FdtError::PropertyOutOfBounds)?;
        let value = self
            .structs
            .get(self.position..end)
            .ok_or(FdtError::PropertyOutOfBounds)?;
        self.position = self
            .position
            .checked_add(align4(len).ok_or(FdtError::PropertyOutOfBounds)?)
            .ok_or(FdtError::PropertyOutOfBounds)?;
        Ok(Property { name, value })
    }
}

// ---------------------------------------------------------------------------
// Nodes and their properties
// ---------------------------------------------------------------------------

/// One node of the tree.
///
/// A node borrows the blob and holds the offset its properties start at, so it
/// is cheap to copy and reading a property is a fresh walk of that node's own
/// property list rather than of the tree.
#[derive(Clone, Copy, Debug)]
pub struct Node<'a> {
    /// The node's name, unit address included: `pl011@9000000`. The root's
    /// name is empty.
    pub name: &'a str,
    /// Nesting depth, the root being zero.
    pub depth: usize,
    /// `#address-cells` in effect for this node's own `reg`, which is the
    /// value its *parent* declares.
    pub address_cells: u32,
    /// `#size-cells` in effect for this node's own `reg`.
    pub size_cells: u32,
    structs: &'a [u8],
    strings: &'a [u8],
    properties: usize,
}

impl<'a> Node<'a> {
    /// Every property of this node, in blob order.
    #[must_use]
    pub const fn properties(&self) -> Properties<'a> {
        Properties {
            cursor: Cursor {
                structs: self.structs,
                strings: self.strings,
                position: self.properties,
            },
            done: false,
        }
    }

    /// The property called `name`, if the node has one.
    #[must_use]
    pub fn property(&self, name: &str) -> Option<Property<'a>> {
        self.properties().find(|property| property.name == name)
    }

    /// The first entry of the node's `compatible` list, which is the most
    /// specific binding it claims.
    #[must_use]
    pub fn compatible(&self) -> Option<&'a str> {
        self.property("compatible")?.as_str()
    }

    /// True if the node's `compatible` list holds `binding`.
    #[must_use]
    pub fn is_compatible(&self, binding: &str) -> bool {
        self.property("compatible")
            .is_some_and(|property| property.contains_string(binding))
    }

    /// The node's `device_type`, the property `/memory` nodes are found by.
    #[must_use]
    pub fn device_type(&self) -> Option<&'a str> {
        self.property("device_type")?.as_str()
    }

    /// The node's name without its unit address: `pl011` for `pl011@9000000`.
    #[must_use]
    pub fn base_name(&self) -> &'a str {
        base_name(self.name)
    }

    /// The node's unit address as written, without the `@`.
    #[must_use]
    pub fn unit_address(&self) -> Option<&'a str> {
        let mut parts = self.name.split('@');
        let _ = parts.next()?;
        parts.next()
    }

    /// The node's `reg` ranges, decoded with the cell counts its parent
    /// declared. Empty if the node has no `reg`.
    #[must_use]
    pub fn reg(&self) -> Regions<'a> {
        let value = match self.property("reg") {
            Some(property) => property.value,
            None => &[],
        };
        Regions {
            value,
            offset: 0,
            address_cells: self.address_cells,
            size_cells: self.size_cells,
        }
    }

    /// The words of the node's `interrupts` property. Their meaning is the
    /// interrupt controller's to define, so they are handed over undecoded.
    #[must_use]
    pub fn interrupts(&self) -> Option<Cells<'a>> {
        Some(self.property("interrupts")?.cells())
    }
}

/// The properties of one node, in blob order.
#[derive(Clone, Copy, Debug)]
pub struct Properties<'a> {
    cursor: Cursor<'a>,
    done: bool,
}

impl<'a> Iterator for Properties<'a> {
    type Item = Property<'a>;

    fn next(&mut self) -> Option<Property<'a>> {
        while !self.done {
            let Ok(token) = self.cursor.token() else {
                self.done = true;
                return None;
            };
            match token {
                FDT_NOP => {}
                FDT_PROP => {
                    let Ok(property) = self.cursor.property() else {
                        self.done = true;
                        return None;
                    };
                    return Some(property);
                }
                // Properties precede subnodes, so anything else ends the list.
                _ => {
                    self.done = true;
                    return None;
                }
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Walking the tree
// ---------------------------------------------------------------------------

/// One open node on the walker's stack.
///
/// The name is kept as an offset rather than a `&str` so that a frame is twelve
/// bytes: sixty-four of them then cost under a kilobyte of the kernel's stack.
#[derive(Clone, Copy, Debug, Default)]
struct Frame {
    name_offset: u32,
    /// `#address-cells` this node declares *for its children*, inherited from
    /// its parent when it declares none.
    address_cells: u32,
    size_cells: u32,
}

/// A depth-first walk of every node in the tree.
///
/// The iterator stops rather than panicking if it meets something malformed;
/// [`Fdt::parse`] has already rejected such a blob, so in practice it stops
/// only at the end of the tree.
#[derive(Clone, Copy, Debug)]
pub struct Nodes<'a> {
    cursor: Cursor<'a>,
    stack: [Frame; MAX_DEPTH],
    depth: usize,
    tokens: usize,
    done: bool,
}

impl<'a> Nodes<'a> {
    /// A walk positioned at the start of the structure block.
    const fn new(cursor: Cursor<'a>) -> Self {
        Nodes {
            cursor,
            stack: [Frame {
                name_offset: 0,
                address_cells: DEFAULT_ADDRESS_CELLS,
                size_cells: DEFAULT_SIZE_CELLS,
            }; MAX_DEPTH],
            depth: 0,
            tokens: 0,
            done: false,
        }
    }

    /// End the walk. Written as a function returning `None` so that the many
    /// failure paths below stay one line each.
    fn stop(&mut self) -> Option<Node<'a>> {
        self.done = true;
        None
    }

    /// Enter the node whose [`FDT_BEGIN_NODE`] was just read, leaving the
    /// cursor on the first token after its properties.
    fn begin_node(&mut self) -> Option<Node<'a>> {
        let index = self.depth;
        if index >= MAX_DEPTH {
            return self.stop();
        }
        let Ok(name_offset) = u32::try_from(self.cursor.position) else {
            return self.stop();
        };
        let Ok(name) = self.cursor.node_name() else {
            return self.stop();
        };
        // A node's own `reg` is decoded with the counts its parent declared;
        // the root falls back to the format's defaults.
        let (address_cells, size_cells) = match index.checked_sub(1).and_then(|p| self.stack.get(p))
        {
            Some(parent) => (parent.address_cells, parent.size_cells),
            None => (DEFAULT_ADDRESS_CELLS, DEFAULT_SIZE_CELLS),
        };
        let properties = self.cursor.position;
        let (child_address_cells, child_size_cells) =
            self.scan_properties(address_cells, size_cells)?;
        let Some(frame) = self.stack.get_mut(index) else {
            return self.stop();
        };
        *frame = Frame {
            name_offset,
            address_cells: child_address_cells,
            size_cells: child_size_cells,
        };
        self.depth = index.saturating_add(1);
        Some(Node {
            name,
            depth: index,
            address_cells,
            size_cells,
            structs: self.cursor.structs,
            strings: self.cursor.strings,
            properties,
        })
    }

    /// Step the cursor over the current node's properties, returning the cell
    /// counts that apply to its children.
    ///
    /// The counts start at the inherited values, which is what the format means
    /// by a node that declares neither.
    fn scan_properties(&mut self, address: u32, size: u32) -> Option<(u32, u32)> {
        let mut cells = (address, size);
        loop {
            let resume = self.cursor.position;
            let Ok(token) = self.cursor.token() else {
                self.done = true;
                return None;
            };
            match token {
                FDT_NOP => {}
                FDT_PROP => {
                    let Ok(property) = self.cursor.property() else {
                        self.done = true;
                        return None;
                    };
                    declared_cells(&property, &mut cells);
                }
                // Leave the token for the main loop: it opens a child node,
                // closes this one, or ends the tree.
                _ => {
                    self.cursor.position = resume;
                    return Some(cells);
                }
            }
        }
    }

    /// True if the node most recently returned sits exactly at `path`.
    ///
    /// A component written without a unit address matches a node that has one,
    /// so `/memory` finds `/memory@40000000`.
    fn path_is(&self, path: &str) -> bool {
        let mut components = path.split('/').filter(|part| !part.is_empty());
        // Level zero is the root, whose empty name answers to no component.
        for level in 1..self.depth {
            let (Some(want), Some(frame)) = (components.next(), self.stack.get(level)) else {
                return false;
            };
            let offset = frame.name_offset as usize;
            let Ok(have) = cstr_at(self.cursor.structs, offset) else {
                return false;
            };
            if have != want && (want.contains('@') || base_name(have) != want) {
                return false;
            }
        }
        components.next().is_none()
    }

    /// The first node at `path`, or `None` if the tree has none.
    fn find_path(mut self, path: &str) -> Option<Node<'a>> {
        loop {
            let node = self.next()?;
            if self.path_is(path) {
                return Some(node);
            }
        }
    }
}

/// Apply a node's `#address-cells` or `#size-cells` to the counts its children
/// will use.
///
/// A declaration wider than [`MAX_CELLS`] is ignored rather than honoured: it
/// cannot describe an address this machine has, and treating it as a length
/// would let a blob steer how much of a `reg` value is read per entry.
fn declared_cells(property: &Property<'_>, cells: &mut (u32, u32)) {
    let Some(value) = property.as_u32().filter(|value| *value <= MAX_CELLS) else {
        return;
    };
    if property.name == "#address-cells" {
        cells.0 = value;
    } else if property.name == "#size-cells" {
        cells.1 = value;
    }
}

impl<'a> Iterator for Nodes<'a> {
    type Item = Node<'a>;

    fn next(&mut self) -> Option<Node<'a>> {
        while !self.done {
            self.tokens = self.tokens.saturating_add(1);
            if self.tokens > MAX_TOKENS {
                return self.stop();
            }
            let Ok(token) = self.cursor.token() else {
                return self.stop();
            };
            match token {
                FDT_BEGIN_NODE => return self.begin_node(),
                FDT_END_NODE => match self.depth.checked_sub(1) {
                    Some(depth) => self.depth = depth,
                    None => return self.stop(),
                },
                FDT_NOP => {}
                // Only reachable in a tree that puts a property after a
                // subnode; step over it rather than giving up on the rest.
                FDT_PROP => {
                    if self.cursor.property().is_err() {
                        return self.stop();
                    }
                }
                _ => return self.stop(),
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Memory reservations
// ---------------------------------------------------------------------------

/// The entries of the memory reservation block, in order.
#[derive(Clone, Copy, Debug)]
pub struct Reservations<'a> {
    blob: &'a [u8],
    offset: usize,
    done: bool,
}

impl Iterator for Reservations<'_> {
    type Item = MemoryReservation;

    fn next(&mut self) -> Option<MemoryReservation> {
        if self.done {
            return None;
        }
        let (Some(address), Some(size)) = (
            u64_at(self.blob, self.offset),
            u64_at(self.blob, self.offset.checked_add(8)?),
        ) else {
            self.done = true;
            return None;
        };
        // A zero pair terminates the list.
        if address == 0 && size == 0 {
            self.done = true;
            return None;
        }
        match self.offset.checked_add(16) {
            Some(offset) => self.offset = offset,
            None => self.done = true,
        }
        Some(MemoryReservation { address, size })
    }
}

// ---------------------------------------------------------------------------
// Devices the kernel has to find
// ---------------------------------------------------------------------------

/// Which generic interrupt controller a tree describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GicVersion {
    /// A GICv2: the second `reg` range is the CPU interface.
    V2,
    /// A GICv3: the second `reg` range is the redistributor region.
    V3,
}

/// The interrupt controller node, with the version its `compatible` claimed.
#[derive(Clone, Copy, Debug)]
pub struct InterruptController<'a> {
    /// Which architecture version the controller implements.
    pub version: GicVersion,
    /// The node itself, for any property this type does not expose.
    pub node: Node<'a>,
}

impl InterruptController<'_> {
    /// The distributor's `reg` range, which is the first one in both versions.
    #[must_use]
    pub fn distributor(&self) -> Option<Region> {
        self.node.reg().next()
    }

    /// The redistributor `reg` range, or `None` on a GICv2, which has none.
    #[must_use]
    pub fn redistributor(&self) -> Option<Region> {
        if self.version != GicVersion::V3 {
            return None;
        }
        self.node.reg().nth(1)
    }

    /// The CPU interface `reg` range, or `None` on a GICv3, where the CPU
    /// interface is a set of system registers rather than a memory range.
    #[must_use]
    pub fn cpu_interface(&self) -> Option<Region> {
        if self.version != GicVersion::V2 {
            return None;
        }
        self.node.reg().nth(1)
    }
}

/// One of the architected timer's four interrupts, in the order a timer node
/// lists them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimerInterrupt {
    /// The secure physical timer, which belongs to the secure world.
    SecurePhysical = 0,
    /// The non-secure physical timer.
    NonSecurePhysical = 1,
    /// The virtual timer: the one a kernel below any hypervisor owns.
    Virtual = 2,
    /// The hypervisor's own physical timer.
    Hypervisor = 3,
}

/// The instruction a PSCI call is made with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PsciConduit {
    /// `smc`: firmware is a secure monitor.
    Smc,
    /// `hvc`: firmware is a hypervisor, or is being emulated as one.
    Hvc,
}

/// The GIC interrupt identifier a three-cell GIC specifier names.
///
/// The binding numbers the two peripheral kinds from zero each: type 0 is a
/// shared peripheral, identifier 32 upwards; type 1 is a private one,
/// identifiers 16 to 31. Any other type, or a number past the end of its
/// range, is not an interrupt a GIC delivers.
#[must_use]
pub const fn gic_interrupt_id(kind: u32, number: u32) -> Option<u32> {
    match kind {
        0 if number < 988 => Some(32 + number),
        1 if number < 16 => Some(16 + number),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Fdt
// ---------------------------------------------------------------------------

/// A borrowed, validated device tree.
///
/// Nothing is copied and nothing is allocated: the type is three sub-slices of
/// the blob and the decoded header.
#[derive(Clone, Copy, Debug)]
pub struct Fdt<'a> {
    blob: &'a [u8],
    header: Header,
    structs: &'a [u8],
    strings: &'a [u8],
}

impl<'a> Fdt<'a> {
    /// Validate `blob` and borrow it.
    ///
    /// The header, the block extents and the entire token stream are checked
    /// here, so a tree that parses is one every accessor below can walk without
    /// trusting a single further byte of it. Bytes past `totalsize` are never
    /// read, which is what lets a caller hand over a whole page.
    pub fn parse(blob: &'a [u8]) -> Result<Self, FdtError> {
        let header = Header::parse(blob)?;
        let total = usize::try_from(header.totalsize).map_err(|_| FdtError::BadTotalSize)?;
        let blob = blob.get(..total).ok_or(FdtError::BadTotalSize)?;
        let structs = block(blob, header.off_dt_struct, header.size_dt_struct)?;
        let strings = block(blob, header.off_dt_strings, header.size_dt_strings)?;
        let fdt = Fdt {
            blob,
            header,
            structs,
            strings,
        };
        fdt.validate_structure()?;
        Ok(fdt)
    }

    /// Walk the token stream once, rejecting anything malformed.
    ///
    /// This is the reason the iterators can be infallible. It is also where a
    /// blob that nests forever, names a property past the end of the strings
    /// block or never ends is turned into an error instead of a fault.
    fn validate_structure(&self) -> Result<(), FdtError> {
        let mut cursor = Cursor::new(self.structs, self.strings);
        let mut depth = 0usize;
        let mut tokens = 0usize;
        loop {
            tokens = tokens.saturating_add(1);
            if tokens > MAX_TOKENS {
                return Err(FdtError::TokenBudget);
            }
            match cursor.token()? {
                FDT_BEGIN_NODE => {
                    let _ = cursor.node_name()?;
                    depth = depth.saturating_add(1);
                    if depth > MAX_DEPTH {
                        return Err(FdtError::DepthOverflow);
                    }
                }
                FDT_END_NODE => depth = depth.checked_sub(1).ok_or(FdtError::UnbalancedNode)?,
                FDT_PROP => {
                    let _ = cursor.property()?;
                }
                FDT_NOP => {}
                FDT_END => {
                    if depth != 0 {
                        return Err(FdtError::UnbalancedNode);
                    }
                    return Ok(());
                }
                other => return Err(FdtError::BadToken(other)),
            }
        }
    }

    /// The decoded header.
    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    /// The blob, truncated to the `totalsize` the header declared.
    #[must_use]
    pub const fn blob(&self) -> &'a [u8] {
        self.blob
    }

    /// Physical ID of the CPU the firmware booted on.
    #[must_use]
    pub const fn boot_cpu(&self) -> u32 {
        self.header.boot_cpuid_phys
    }

    /// Every node of the tree, depth first.
    #[must_use]
    pub const fn nodes(&self) -> Nodes<'a> {
        Nodes::new(Cursor::new(self.structs, self.strings))
    }

    /// The node at `path`, for example `/chosen` or `/soc/uart@9000000`.
    ///
    /// A component written without a unit address matches a node that has one,
    /// so `/memory` finds `/memory@40000000`.
    #[must_use]
    pub fn find_node(&self, path: &str) -> Option<Node<'a>> {
        self.nodes().find_path(path)
    }

    /// The root node.
    #[must_use]
    pub fn root(&self) -> Option<Node<'a>> {
        self.nodes().next()
    }

    /// The first node whose `compatible` list holds `binding`.
    #[must_use]
    pub fn find_compatible(&self, binding: &str) -> Option<Node<'a>> {
        self.nodes().find(|node| node.is_compatible(binding))
    }

    /// The memory reservation block: ranges the kernel must keep out of the
    /// frame allocator.
    #[must_use]
    pub fn reservations(&self) -> Reservations<'a> {
        Reservations {
            blob: self.blob,
            offset: self.header.off_mem_rsvmap as usize,
            done: false,
        }
    }

    /// Every `reg` range of every `/memory` node, in tree order.
    #[must_use]
    pub fn memory(&self) -> MemoryRegions<'a> {
        MemoryRegions {
            nodes: self.nodes(),
            current: None,
        }
    }

    /// The root's `model`, the machine's own name for itself.
    #[must_use]
    pub fn model(&self) -> Option<&'a str> {
        self.root()?.property("model")?.as_str()
    }

    /// The kernel command line from `/chosen`.
    #[must_use]
    pub fn bootargs(&self) -> Option<&'a str> {
        self.find_node("/chosen")?.property("bootargs")?.as_str()
    }

    /// The path an entry of `/aliases` stands for.
    #[must_use]
    pub fn alias(&self, name: &str) -> Option<&'a str> {
        self.find_node("/aliases")?.property(name)?.as_str()
    }

    /// The raw `stdout-path` from `/chosen`, suffix and all.
    ///
    /// `linux,stdout-path` is accepted as well: it is the older spelling, and
    /// some firmware still emits only that one.
    #[must_use]
    pub fn stdout_path(&self) -> Option<&'a str> {
        let chosen = self.find_node("/chosen")?;
        chosen
            .property("stdout-path")
            .or_else(|| chosen.property("linux,stdout-path"))?
            .as_str()
    }

    /// The console node `/chosen`'s `stdout-path` names.
    ///
    /// The path may carry a `:115200n8` options suffix, which is stripped, and
    /// may be an alias rather than a path, which is resolved through
    /// `/aliases`. Its `compatible` and first `reg` range are what a driver
    /// needs; both come from the returned node.
    #[must_use]
    pub fn console(&self) -> Option<Node<'a>> {
        let stdout = self.stdout_path()?;
        let target = stdout.split(':').next().unwrap_or(stdout);
        if target.starts_with('/') {
            return self.find_node(target);
        }
        self.find_node(self.alias(target)?)
    }

    /// The generic interrupt controller, whichever version the tree describes.
    #[must_use]
    pub fn interrupt_controller(&self) -> Option<InterruptController<'a>> {
        self.nodes().find_map(|node| {
            if node.is_compatible(GICV3_COMPATIBLE) {
                return Some(InterruptController {
                    version: GicVersion::V3,
                    node,
                });
            }
            if GICV2_COMPATIBLES
                .iter()
                .any(|binding| node.is_compatible(binding))
            {
                return Some(InterruptController {
                    version: GicVersion::V2,
                    node,
                });
            }
            None
        })
    }

    /// The architected timer node, whose `interrupts` carry the four PPIs the
    /// kernel programs. Found by either the 64-bit or the 32-bit binding.
    #[must_use]
    pub fn timer(&self) -> Option<Node<'a>> {
        self.nodes().find(|node| {
            node.is_compatible(TIMER_COMPATIBLE) || node.is_compatible(TIMER_V7_COMPATIBLE)
        })
    }

    /// The GIC identifier one of the architected timer's interrupts arrives on.
    ///
    /// `None` if there is no timer, if it lists fewer interrupts than `which`
    /// needs, or if the entry is not one a GIC could deliver. A timer's
    /// `interrupts` are GIC specifiers of three cells each, in the order
    /// [`TimerInterrupt`] gives — the binding's order, and not the order
    /// anybody would guess.
    #[must_use]
    pub fn timer_interrupt(&self, which: TimerInterrupt) -> Option<u32> {
        let mut cells = self.timer()?.interrupts()?.skip(which as usize * 3);
        let kind = cells.next()?;
        let number = cells.next()?;
        // The third cell is the trigger type and CPU mask. It changes how the
        // line is programmed, not which line it is — but it has to be there,
        // or the specifier was cut short and the two before it are suspect.
        let _flags = cells.next()?;
        gic_interrupt_id(kind, number)
    }

    /// How this machine's PSCI firmware is called, or `None` if the tree
    /// describes none or names a conduit this crate does not know.
    ///
    /// The difference is not cosmetic: `smc` on a machine with no secure
    /// monitor, or `hvc` on one with no hypervisor, is an undefined-instruction
    /// exception rather than a call.
    #[must_use]
    pub fn psci_conduit(&self) -> Option<PsciConduit> {
        let node = self.nodes().find(|node| {
            PSCI_COMPATIBLES
                .iter()
                .any(|binding| node.is_compatible(binding))
        })?;
        match node.property("method")?.as_str()? {
            "smc" => Some(PsciConduit::Smc),
            "hvc" => Some(PsciConduit::Hvc),
            _ => None,
        }
    }

    /// Every PCI host bridge whose configuration space is an ECAM window,
    /// in tree order. [`EcamHosts`] says what is read and what is skipped.
    #[must_use]
    pub const fn ecam_hosts(&self) -> EcamHosts<'a> {
        EcamHosts {
            nodes: self.nodes(),
        }
    }

    /// Every processor in `/cpus` that has not failed, in tree order.
    ///
    /// [`Cpus`] says which nodes count as processors and why.
    #[must_use]
    pub const fn cpus(&self) -> Cpus<'a> {
        Cpus {
            nodes: self.nodes(),
            inside: false,
            done: false,
        }
    }
}

/// One processor, from a `cpu` node under `/cpus`.
#[derive(Clone, Copy, Debug)]
pub struct Cpu<'a> {
    /// The node, for anything else a caller wants from it.
    pub node: Node<'a>,
    /// The processor's hardware identifier: its first `reg` entry, decoded
    /// with the `#address-cells` `/cpus` declares. On Arm this is the affinity
    /// fields of `MPIDR` — `Aff3` in the upper cell when there are two — and
    /// so the number PSCI's `CPU_ON` takes.
    pub id: u64,
}

impl<'a> Cpu<'a> {
    /// How the processor is started, if the tree says: `psci`, `spin-table`.
    #[must_use]
    pub fn enable_method(&self) -> Option<&'a str> {
        self.node.property("enable-method")?.as_str()
    }

    /// The node's `status`, or `None` when it has none, which means `okay`.
    ///
    /// **For a processor, `disabled` does not mean absent.** The
    /// specification defines it as quiescent — stopped, waiting to be started
    /// through its enable-method — which is the state every secondary is in
    /// before a kernel starts it. So it is reported rather than filtered, and
    /// the caller decides what to do with one.
    #[must_use]
    pub fn status(&self) -> Option<&'a str> {
        self.node.property("status")?.as_str()
    }
}

/// Every processor in `/cpus` whose `status` is not `fail`.
///
/// A processor is a *direct* child of `/cpus` that is named `cpu` or has
/// `device_type = "cpu"` — both spellings, for the reason [`MemoryRegions`]
/// accepts both of its own — and has a `reg` to be addressed by. Its own
/// children, the caches, are not processors; nor are `/cpus`' other children,
/// `cpu-map` and `idle-states`; nor is a node called `cpu` anywhere else in
/// the tree.
///
/// `fail` is the one status skipped: the specification's word for a processor
/// that is broken, as opposed to one that is merely stopped. Linux draws the
/// line in the same place.
#[derive(Clone, Copy, Debug)]
pub struct Cpus<'a> {
    nodes: Nodes<'a>,
    /// Whether the walk has reached `/cpus`.
    inside: bool,
    done: bool,
}

impl<'a> Iterator for Cpus<'a> {
    type Item = Cpu<'a>;

    fn next(&mut self) -> Option<Cpu<'a>> {
        while !self.done {
            let Some(node) = self.nodes.next() else {
                self.done = true;
                return None;
            };
            if !self.inside {
                self.inside = node.depth == 1 && node.base_name() == "cpus";
                continue;
            }
            // The walk is depth first, so the first node back at `/cpus`' own
            // depth is the end of it.
            if node.depth <= 1 {
                self.done = true;
                return None;
            }
            if let Some(cpu) = as_cpu(node) {
                return Some(cpu);
            }
        }
        None
    }
}

/// `node`, a descendant of `/cpus`, as a processor — if it is one.
fn as_cpu(node: Node<'_>) -> Option<Cpu<'_>> {
    let named = node.base_name() == "cpu";
    let typed = node.device_type() == Some("cpu");
    if node.depth != 2 || !(named || typed) {
        return None;
    }
    let failed = node
        .property("status")
        .and_then(|status| status.as_str())
        .is_some_and(|status| status.starts_with("fail"));
    if failed {
        return None;
    }
    let id = node.reg().next()?.address;
    Some(Cpu { node, id })
}

/// Every `reg` range of every `/memory` node.
#[derive(Clone, Copy, Debug)]
pub struct MemoryRegions<'a> {
    nodes: Nodes<'a>,
    current: Option<Regions<'a>>,
}

impl<'a> MemoryRegions<'a> {
    /// The next child of the root that describes memory.
    ///
    /// Both spellings are accepted: `device_type = "memory"`, which the
    /// specification requires, and a node simply named `memory`, which is what
    /// a hand-written tree tends to have.
    fn next_memory_node(&mut self) -> Option<Regions<'a>> {
        loop {
            let node = self.nodes.next()?;
            let named = node.base_name() == "memory";
            let typed = node.device_type() == Some("memory");
            if node.depth == 1 && (named || typed) {
                return Some(node.reg());
            }
        }
    }
}

impl Iterator for MemoryRegions<'_> {
    type Item = Region;

    fn next(&mut self) -> Option<Region> {
        loop {
            if let Some(region) = self.current.as_mut().and_then(Iterator::next) {
                return Some(region);
            }
            self.current = Some(self.next_memory_node()?);
        }
    }
}

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// PCI host bridges
// ---------------------------------------------------------------------------

/// The binding for a host bridge whose configuration space is plain ECAM,
/// which is what QEMU's `virt` machine describes.
pub const PCI_HOST_ECAM_COMPATIBLE: &str = "pci-host-ecam-generic";

/// Bytes of ECAM window one bus occupies.
const ECAM_BYTES_PER_BUS: u64 = 1 << 20;

/// A PCI host bridge whose configuration space is an ECAM window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EcamHost {
    /// The window: the first region of `reg`. Its address is bus
    /// [`EcamHost::start_bus`]'s, which is *not* the MCFG's convention — an
    /// MCFG allocation's address is bus zero's.
    pub window: Region,
    /// `linux,pci-domain`, or zero when the tree does not say.
    pub segment: u16,
    /// The first bus of `bus-range`, or zero when there is none.
    pub start_bus: u8,
    /// The last bus the window really reaches: `bus-range`'s last, or 255,
    /// cut down to what [`EcamHost::window`] is large enough to hold.
    pub end_bus: u8,
}

/// Every `pci-host-ecam-generic` node that can be used, in tree order.
///
/// A node is skipped if its `status` is anything but `okay`, if it has no
/// `reg`, if its window is smaller than one bus, or if `bus-range` or
/// `linux,pci-domain` does not decode to numbers PCI can have. A `bus-range`
/// larger than the window is cut down to the buses the window holds, as
/// Linux does, rather than letting the kernel map configuration space past
/// the end of what firmware said is there.
#[derive(Clone, Copy, Debug)]
pub struct EcamHosts<'a> {
    /// The walk the hosts are found in.
    nodes: Nodes<'a>,
}

impl Iterator for EcamHosts<'_> {
    type Item = EcamHost;

    fn next(&mut self) -> Option<EcamHost> {
        loop {
            let node = self.nodes.next()?;
            if let Some(host) = ecam_host(&node) {
                return Some(host);
            }
        }
    }
}

/// Decode `node` as an ECAM host bridge, if it is a usable one.
fn ecam_host(node: &Node<'_>) -> Option<EcamHost> {
    if !node.is_compatible(PCI_HOST_ECAM_COMPATIBLE) {
        return None;
    }
    if let Some(status) = node.property("status")
        && !matches!(status.as_str(), Some("okay" | "ok"))
    {
        return None;
    }
    let window = node.reg().next()?;
    let (start_bus, last_bus) = match node.property("bus-range") {
        None => (0, u8::MAX),
        Some(range) => {
            if range.len() != 8 {
                return None;
            }
            let mut cells = range.cells();
            let start = u8::try_from(cells.next()?).ok()?;
            let end = u8::try_from(cells.next()?).ok()?;
            (start, end)
        }
    };
    if last_bus < start_bus {
        return None;
    }
    let segment = match node.property("linux,pci-domain") {
        None => 0,
        Some(domain) => u16::try_from(domain.as_u32()?).ok()?,
    };
    let buses = window.size / ECAM_BYTES_PER_BUS;
    let last_held = u64::from(start_bus).checked_add(buses.checked_sub(1)?)?;
    let end_bus = u8::try_from(last_held).map_or(last_bus, |held| held.min(last_bus));
    Some(EcamHost {
        window,
        segment,
        start_bus,
        end_bus,
    })
}
