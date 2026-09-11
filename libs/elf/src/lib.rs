//! Bounds-checked ELF64 reader.
//!
//! Used twice: by the loader to place the kernel, and by the kernel to place
//! user programs. It understands what those two jobs need — program headers,
//! the load span, and the relative relocations a static PIE carries — and
//! nothing else.
//!
//! # No unsafe
//!
//! Every field is pulled out of a byte slice with a bounds check and
//! [`u64::from_le_bytes`], so the crate is `#![forbid(unsafe_code)]`. That is
//! worth the small cost: this code parses headers that came from whatever the
//! user just ran, with the MMU on and nothing above the kernel to contain a
//! mistake. Casting the slice to `*const Ehdr` and reading it unaligned would
//! be shorter and would put the sharpest surface in the system inside an
//! `unsafe` block for no gain.
//!
//! ```
//! # use ferrix_elf::{Elf, ElfError};
//! # fn example(image: &[u8]) -> Result<(), ElfError> {
//! let elf = Elf::parse(image)?;
//! elf.check_machine(ferrix_elf::EM_X86_64)?;
//! for segment in elf.loadable() {
//!     let _ = (segment.vaddr, segment.memsz, segment.flags);
//! }
//! # Ok(())
//! # }
//! ```

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

/// The four bytes every ELF file starts with.
pub const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

/// `e_ident[EI_CLASS]` for a 64-bit object.
pub const ELFCLASS64: u8 = 2;
/// `e_ident[EI_DATA]` for a little-endian object.
pub const ELFDATA2LSB: u8 = 1;

/// `e_type`: a non-relocatable executable.
pub const ET_EXEC: u16 = 2;
/// `e_type`: a shared object, which is also how a position-independent
/// executable is spelled.
pub const ET_DYN: u16 = 3;

/// `e_machine` for x86-64.
pub const EM_X86_64: u16 = 62;
/// `e_machine` for AArch64.
pub const EM_AARCH64: u16 = 183;

/// `p_type`: unused entry.
pub const PT_NULL: u32 = 0;
/// `p_type`: a segment to load into memory.
pub const PT_LOAD: u32 = 1;
/// `p_type`: the dynamic linking table.
pub const PT_DYNAMIC: u32 = 2;
/// `p_type`: path of the interpreter this binary wants.
pub const PT_INTERP: u32 = 3;
/// `p_type`: auxiliary information.
pub const PT_NOTE: u32 = 4;
/// `p_type`: the program header table itself.
pub const PT_PHDR: u32 = 6;
/// `p_type`: the thread-local storage template.
pub const PT_TLS: u32 = 7;
/// `p_type`: the unwind tables.
pub const PT_GNU_EH_FRAME: u32 = 0x6474_E550;
/// `p_type`: whether the stack should be executable.
pub const PT_GNU_STACK: u32 = 0x6474_E551;
/// `p_type`: the region to make read-only after relocation.
pub const PT_GNU_RELRO: u32 = 0x6474_E552;

/// `p_flags`: executable.
pub const PF_X: u32 = 1;
/// `p_flags`: writable.
pub const PF_W: u32 = 2;
/// `p_flags`: readable.
pub const PF_R: u32 = 4;

/// `d_tag`: end of the dynamic table.
pub const DT_NULL: i64 = 0;
/// `d_tag`: address of the relocation table with addends.
pub const DT_RELA: i64 = 7;
/// `d_tag`: size of that table in bytes.
pub const DT_RELASZ: i64 = 8;
/// `d_tag`: size of one entry in that table.
pub const DT_RELAENT: i64 = 9;

/// `R_X86_64_RELATIVE`: add the load bias to the addend.
pub const R_X86_64_RELATIVE: u32 = 8;
/// `R_AARCH64_RELATIVE`: add the load bias to the addend.
pub const R_AARCH64_RELATIVE: u32 = 1027;

/// Size of an ELF64 file header.
pub const EHDR_SIZE: usize = 64;
/// Size of an ELF64 program header.
pub const PHDR_SIZE: usize = 56;
/// Size of an ELF64 relocation entry with an addend.
pub const RELA_SIZE: usize = 24;
/// Size of an ELF64 dynamic table entry.
pub const DYN_SIZE: usize = 16;

/// Why an image was rejected.
///
/// Every variant means the same thing operationally — do not load this — but
/// they are distinguished because a wrong-architecture binary and a truncated
/// one call for very different responses from whoever sees the message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElfError {
    /// The image is smaller than an ELF64 header.
    TooShort,
    /// The first four bytes are not `\x7fELF`.
    BadMagic,
    /// Not a 64-bit object.
    NotElf64,
    /// Not little-endian.
    NotLittleEndian,
    /// Built for a different CPU. Carries the `e_machine` found.
    BadMachine(u16),
    /// Neither an executable nor a shared object. Carries the `e_type` found.
    BadType(u16),
    /// The program header table runs past the end of the image, or its entries
    /// are too small to be program headers.
    HeaderOutOfBounds,
    /// A segment's file contents run past the end of the image.
    SegmentOutOfBounds,
    /// A segment's memory size is smaller than its file size, or its address
    /// range wraps.
    SegmentMalformed,
    /// A relocation type this loader does not resolve. Carries the type.
    UnsupportedRelocation(u32),
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ElfError::TooShort => f.write_str("image is shorter than an ELF64 header"),
            ElfError::BadMagic => f.write_str("not an ELF image"),
            ElfError::NotElf64 => f.write_str("not a 64-bit ELF image"),
            ElfError::NotLittleEndian => f.write_str("not a little-endian ELF image"),
            ElfError::BadMachine(found) => write!(f, "built for e_machine {found}"),
            ElfError::BadType(found) => write!(f, "e_type {found} is neither EXEC nor DYN"),
            ElfError::HeaderOutOfBounds => f.write_str("program header table is out of bounds"),
            ElfError::SegmentOutOfBounds => f.write_str("segment contents are out of bounds"),
            ElfError::SegmentMalformed => f.write_str("segment has an impossible size or address"),
            ElfError::UnsupportedRelocation(kind) => {
                write!(f, "relocation type {kind} is not supported")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Little-endian field access
// ---------------------------------------------------------------------------

/// Read a little-endian `u16` at `offset`, or `None` past the end.
fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    let field: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(field))
}

/// Read a little-endian `u32` at `offset`, or `None` past the end.
fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let field: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(field))
}

/// Read a little-endian `u64` at `offset`, or `None` past the end.
fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    let field: [u8; 8] = bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(u64::from_le_bytes(field))
}

// ---------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------

/// The ELF64 file header, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// Object type: [`ET_EXEC`] or [`ET_DYN`].
    pub elf_type: u16,
    /// Target architecture.
    pub machine: u16,
    /// Virtual address of the entry point, before any load bias.
    pub entry: u64,
    /// File offset of the program header table.
    pub phoff: u64,
    /// Size of one program header table entry.
    pub phentsize: u16,
    /// Number of program header table entries.
    pub phnum: u16,
}

/// One program header, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Segment {
    /// What the segment is: [`PT_LOAD`], [`PT_DYNAMIC`], and so on.
    pub kind: u32,
    /// Permission bits: [`PF_R`], [`PF_W`], [`PF_X`].
    pub flags: u32,
    /// Offset of the segment's contents in the file.
    pub offset: u64,
    /// Virtual address the segment is linked at.
    pub vaddr: u64,
    /// Bytes of contents present in the file.
    pub filesz: u64,
    /// Bytes the segment occupies in memory; the excess over `filesz` is
    /// zero-filled, which is how `.bss` is expressed.
    pub memsz: u64,
    /// Required alignment of `vaddr`, a power of two.
    pub align: u64,
}

impl Segment {
    /// True if the segment must be loaded into memory.
    #[must_use]
    pub const fn is_load(&self) -> bool {
        self.kind == PT_LOAD && self.memsz != 0
    }

    /// True if the segment is executable.
    #[must_use]
    pub const fn is_executable(&self) -> bool {
        self.flags & PF_X != 0
    }

    /// True if the segment is writable.
    #[must_use]
    pub const fn is_writable(&self) -> bool {
        self.flags & PF_W != 0
    }

    /// True if the segment is readable.
    #[must_use]
    pub const fn is_readable(&self) -> bool {
        self.flags & PF_R != 0
    }

    /// One past the last virtual address the segment occupies.
    ///
    /// `None` if the range would wrap, which is a malformed image rather than
    /// an address a loader should try to satisfy.
    #[must_use]
    pub const fn vaddr_end(&self) -> Option<u64> {
        self.vaddr.checked_add(self.memsz)
    }

    /// The bytes of `image` holding this segment's file contents.
    pub fn data<'a>(&self, image: &'a [u8]) -> Result<&'a [u8], ElfError> {
        let start = usize::try_from(self.offset).map_err(|_| ElfError::SegmentOutOfBounds)?;
        let len = usize::try_from(self.filesz).map_err(|_| ElfError::SegmentOutOfBounds)?;
        let end = start.checked_add(len).ok_or(ElfError::SegmentOutOfBounds)?;
        image.get(start..end).ok_or(ElfError::SegmentOutOfBounds)
    }
}

/// A borrowed ELF image.
///
/// Nothing is copied. Construction validates the file header and the extent of
/// the program header table, so iterating segments cannot fail; each segment's
/// *contents* are checked when they are asked for.
#[derive(Clone, Copy, Debug)]
pub struct Elf<'a> {
    image: &'a [u8],
    header: Header,
}

impl<'a> Elf<'a> {
    /// Validate the file header of `image` and borrow it.
    pub fn parse(image: &'a [u8]) -> Result<Self, ElfError> {
        let ident = image.get(0..16).ok_or(ElfError::TooShort)?;
        if ident.get(0..4) != Some(&ELF_MAGIC) {
            return Err(ElfError::BadMagic);
        }
        if ident.get(4) != Some(&ELFCLASS64) {
            return Err(ElfError::NotElf64);
        }
        if ident.get(5) != Some(&ELFDATA2LSB) {
            return Err(ElfError::NotLittleEndian);
        }
        if image.len() < EHDR_SIZE {
            return Err(ElfError::TooShort);
        }

        let header = Header {
            elf_type: u16_at(image, 16).ok_or(ElfError::TooShort)?,
            machine: u16_at(image, 18).ok_or(ElfError::TooShort)?,
            entry: u64_at(image, 24).ok_or(ElfError::TooShort)?,
            phoff: u64_at(image, 32).ok_or(ElfError::TooShort)?,
            phentsize: u16_at(image, 54).ok_or(ElfError::TooShort)?,
            phnum: u16_at(image, 56).ok_or(ElfError::TooShort)?,
        };

        // The whole program header table has to be inside the image, and its
        // entries at least as large as a program header. Checking it once here
        // is what lets `segments()` be infallible.
        if header.phnum != 0 {
            if (header.phentsize as usize) < PHDR_SIZE {
                return Err(ElfError::HeaderOutOfBounds);
            }
            let span = (header.phnum as u64)
                .checked_mul(header.phentsize as u64)
                .ok_or(ElfError::HeaderOutOfBounds)?;
            let end = header
                .phoff
                .checked_add(span)
                .ok_or(ElfError::HeaderOutOfBounds)?;
            if end > image.len() as u64 {
                return Err(ElfError::HeaderOutOfBounds);
            }
        }

        Ok(Elf { image, header })
    }

    /// The decoded file header.
    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    /// Virtual address of the entry point, before any load bias.
    #[must_use]
    pub const fn entry(&self) -> u64 {
        self.header.entry
    }

    /// The architecture this image was built for.
    #[must_use]
    pub const fn machine(&self) -> u16 {
        self.header.machine
    }

    /// True if the image is position-independent and may be placed anywhere.
    #[must_use]
    pub const fn is_pie(&self) -> bool {
        self.header.elf_type == ET_DYN
    }

    /// The whole image.
    #[must_use]
    pub const fn image(&self) -> &'a [u8] {
        self.image
    }

    /// Reject an image built for the wrong CPU, or of a type we cannot load,
    /// before anything is mapped.
    pub fn check_machine(&self, expected: u16) -> Result<(), ElfError> {
        if self.header.machine != expected {
            return Err(ElfError::BadMachine(self.header.machine));
        }
        if self.header.elf_type != ET_EXEC && self.header.elf_type != ET_DYN {
            return Err(ElfError::BadType(self.header.elf_type));
        }
        Ok(())
    }

    /// Every program header, in file order.
    #[must_use]
    pub const fn segments(&self) -> Segments<'a> {
        Segments {
            image: self.image,
            offset: self.header.phoff,
            entsize: self.header.phentsize as u64,
            left: self.header.phnum,
        }
    }

    /// Just the segments that have to be loaded into memory.
    pub fn loadable(&self) -> impl Iterator<Item = Segment> + 'a {
        self.segments().filter(Segment::is_load)
    }

    /// Lowest and highest virtual address touched by loadable segments, page
    /// aligned to `page_size`: the span a loader has to reserve.
    ///
    /// `None` if the image loads nothing, or if a segment's range wraps.
    #[must_use]
    pub fn load_span(&self, page_size: u64) -> Option<(u64, u64)> {
        let mut low = u64::MAX;
        let mut high = 0u64;
        for segment in self.loadable() {
            low = low.min(segment.vaddr);
            high = high.max(segment.vaddr_end()?);
        }
        if low == u64::MAX {
            return None;
        }
        let mask = page_size.checked_sub(1)?;
        Some((low & !mask, high.checked_add(mask)? & !mask))
    }

    /// Check that every loadable segment is internally consistent and inside
    /// the image.
    ///
    /// A loader should call this once before it maps anything, so that a
    /// malformed image fails before it has had any effect.
    pub fn validate_segments(&self) -> Result<(), ElfError> {
        for segment in self.segments() {
            if segment.kind != PT_LOAD {
                continue;
            }
            if segment.memsz < segment.filesz {
                return Err(ElfError::SegmentMalformed);
            }
            if segment.vaddr_end().is_none() {
                return Err(ElfError::SegmentMalformed);
            }
            let _ = segment.data(self.image)?;
        }
        Ok(())
    }

    /// Map a link-time virtual address back to the file bytes holding it.
    #[must_use]
    pub fn vaddr_to_bytes(&self, vaddr: u64, len: u64) -> Option<&'a [u8]> {
        for segment in self.segments() {
            if segment.kind != PT_LOAD {
                continue;
            }
            let file_end = segment.vaddr.checked_add(segment.filesz)?;
            if vaddr < segment.vaddr || vaddr.checked_add(len)? > file_end {
                continue;
            }
            let delta = vaddr - segment.vaddr;
            let start = usize::try_from(segment.offset.checked_add(delta)?).ok()?;
            let end = start.checked_add(usize::try_from(len).ok()?)?;
            return self.image.get(start..end);
        }
        None
    }

    /// The relative relocations this image needs, if it is a static PIE.
    ///
    /// Returns an empty iterator for a non-relocatable executable, which is
    /// not an error — it is the common case for the kernel itself.
    pub fn relocations(&self) -> Result<Relocations<'a>, ElfError> {
        let Some(dynamic) = self.segments().find(|segment| segment.kind == PT_DYNAMIC) else {
            return Ok(Relocations::EMPTY);
        };
        let table = dynamic.data(self.image)?;

        let mut addr = 0u64;
        let mut size = 0u64;
        let mut entsize = RELA_SIZE as u64;
        for index in 0..(table.len() / DYN_SIZE) {
            let base = index * DYN_SIZE;
            let tag = u64_at(table, base).ok_or(ElfError::SegmentOutOfBounds)? as i64;
            let value = u64_at(table, base + 8).ok_or(ElfError::SegmentOutOfBounds)?;
            match tag {
                DT_NULL => break,
                DT_RELA => addr = value,
                DT_RELASZ => size = value,
                DT_RELAENT => entsize = value,
                _ => {}
            }
        }

        if addr == 0 || size == 0 || entsize < RELA_SIZE as u64 {
            return Ok(Relocations::EMPTY);
        }

        let expected = match self.header.machine {
            EM_X86_64 => R_X86_64_RELATIVE,
            EM_AARCH64 => R_AARCH64_RELATIVE,
            other => return Err(ElfError::BadMachine(other)),
        };

        // DT_RELA is a link-time address; find the file bytes behind it.
        let bytes = self
            .vaddr_to_bytes(addr, size)
            .ok_or(ElfError::SegmentOutOfBounds)?;

        Ok(Relocations {
            bytes,
            entsize: usize::try_from(entsize).map_err(|_| ElfError::SegmentOutOfBounds)?,
            offset: 0,
            expected,
        })
    }
}

/// Iterator over an image's program headers.
#[derive(Clone, Copy, Debug)]
pub struct Segments<'a> {
    image: &'a [u8],
    offset: u64,
    entsize: u64,
    left: u16,
}

impl Iterator for Segments<'_> {
    type Item = Segment;

    fn next(&mut self) -> Option<Segment> {
        // `Elf::parse` checked that the whole table is inside the image and
        // that entries are at least PHDR_SIZE, so every read here succeeds.
        if self.left != 0 {
            let base = usize::try_from(self.offset).ok()?;
            self.left -= 1;
            self.offset = self.offset.checked_add(self.entsize)?;

            let segment = Segment {
                kind: u32_at(self.image, base)?,
                flags: u32_at(self.image, base + 4)?,
                offset: u64_at(self.image, base + 8)?,
                vaddr: u64_at(self.image, base + 16)?,
                filesz: u64_at(self.image, base + 32)?,
                memsz: u64_at(self.image, base + 40)?,
                align: u64_at(self.image, base + 48)?,
            };
            return Some(segment);
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.left as usize))
    }
}

/// One `R_*_RELATIVE` relocation: store `bias + addend` at `bias + offset`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Relocation {
    /// Link-time address of the word to patch.
    pub offset: u64,
    /// Value to add the load bias to.
    pub addend: i64,
}

impl Relocation {
    /// The link-time address to patch, given a load bias.
    #[must_use]
    pub const fn target(&self, bias: u64) -> u64 {
        self.offset.wrapping_add(bias)
    }

    /// The value to store, given a load bias.
    #[must_use]
    pub const fn value(&self, bias: u64) -> u64 {
        bias.wrapping_add(self.addend as u64)
    }
}

/// Iterator over an image's relative relocations.
///
/// Yields an error rather than stopping if it meets a relocation type this
/// loader cannot resolve: silently skipping one would produce an image that
/// runs until it follows the pointer that was never patched.
#[derive(Clone, Copy, Debug)]
pub struct Relocations<'a> {
    bytes: &'a [u8],
    entsize: usize,
    offset: usize,
    expected: u32,
}

impl Relocations<'_> {
    const EMPTY: Relocations<'static> = Relocations {
        bytes: &[],
        entsize: RELA_SIZE,
        offset: 0,
        expected: 0,
    };
}

impl Iterator for Relocations<'_> {
    type Item = Result<Relocation, ElfError>;

    fn next(&mut self) -> Option<Result<Relocation, ElfError>> {
        loop {
            let base = self.offset;
            if base.checked_add(RELA_SIZE)? > self.bytes.len() {
                return None;
            }
            self.offset = self.offset.checked_add(self.entsize)?;

            let offset = u64_at(self.bytes, base)?;
            let info = u64_at(self.bytes, base + 8)?;
            let addend = u64_at(self.bytes, base + 16)? as i64;

            let kind = info as u32;
            if kind == 0 {
                continue; // R_*_NONE, which is padding rather than work.
            }
            if kind != self.expected {
                return Some(Err(ElfError::UnsupportedRelocation(kind)));
            }
            return Some(Ok(Relocation { offset, addend }));
        }
    }
}

#[cfg(test)]
mod tests;
