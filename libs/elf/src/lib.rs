//! Bounds-checked ELF reader, for both 64-bit and 32-bit images.
//!
//! Used three times: by the loader to place the kernel, by the kernel to place
//! user programs, and by `xtask` to name the functions in a kernel panic's
//! backtrace. It understands what those jobs need — program headers, the load
//! span, the relative relocations a static PIE carries, and the symbol table —
//! and nothing else.
//!
//! # Two classes, one interface
//!
//! An ELF file is 64-bit or 32-bit, and the two differ in the width and order
//! of their header fields rather than in what the fields mean. So both decode
//! into the same [`Header`] and [`Segment`], every address widened to `u64`,
//! and the only thing above this crate that asks which class an image was is
//! the loader, refusing a kernel built for another word width.
//!
//! The relocation formats differ in substance as well as in width. The 64-bit
//! architectures use `RELA`, whose entries carry their addend; 32-bit Arm uses
//! `REL`, whose addend is the word already stored at the target. [`Addend`]
//! says which, so a caller cannot apply one as though it were the other.
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

/// `e_ident[EI_CLASS]` for a 32-bit object.
pub const ELFCLASS32: u8 = 1;
/// `e_ident[EI_CLASS]` for a 64-bit object.
pub const ELFCLASS64: u8 = 2;
/// `e_ident[EI_DATA]` for a little-endian object.
pub const ELFDATA2LSB: u8 = 1;

/// `e_type`: a non-relocatable executable.
pub const ET_EXEC: u16 = 2;
/// `e_type`: a shared object, which is also how a position-independent
/// executable is spelled.
pub const ET_DYN: u16 = 3;

/// `e_machine` for 32-bit Arm.
pub const EM_ARM: u16 = 40;
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
/// `d_tag`: address of the relocation table without addends.
pub const DT_REL: i64 = 17;
/// `d_tag`: size of that table in bytes.
pub const DT_RELSZ: i64 = 18;
/// `d_tag`: size of one entry in that table.
pub const DT_RELENT: i64 = 19;

/// `R_X86_64_RELATIVE`: add the load bias to the addend.
pub const R_X86_64_RELATIVE: u32 = 8;
/// `R_AARCH64_RELATIVE`: add the load bias to the addend.
pub const R_AARCH64_RELATIVE: u32 = 1027;
/// `R_ARM_RELATIVE`: add the load bias to the word at the target.
pub const R_ARM_RELATIVE: u32 = 23;

/// Size of an ELF64 file header.
pub const EHDR_SIZE: usize = 64;
/// Size of an ELF64 program header.
pub const PHDR_SIZE: usize = 56;
/// Size of an ELF64 relocation entry with an addend.
pub const RELA_SIZE: usize = 24;
/// Size of an ELF64 relocation entry without one.
pub const REL_SIZE: usize = 16;
/// Size of an ELF64 dynamic table entry.
pub const DYN_SIZE: usize = 16;

/// Size of an ELF32 file header.
pub const EHDR32_SIZE: usize = 52;
/// Size of an ELF32 program header.
pub const PHDR32_SIZE: usize = 32;
/// Size of an ELF32 relocation entry with an addend.
pub const RELA32_SIZE: usize = 12;
/// Size of an ELF32 relocation entry without one.
pub const REL32_SIZE: usize = 8;
/// Size of an ELF32 dynamic table entry.
pub const DYN32_SIZE: usize = 8;

/// The word width an image was built for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// `ELFCLASS32`: 32-bit addresses and a 52-byte header.
    Elf32,
    /// `ELFCLASS64`: 64-bit addresses and a 64-byte header.
    Elf64,
}

impl Class {
    /// Bytes in the file header.
    #[must_use]
    pub const fn header_size(self) -> usize {
        match self {
            Class::Elf32 => EHDR32_SIZE,
            Class::Elf64 => EHDR_SIZE,
        }
    }

    /// Bytes in one program header.
    #[must_use]
    pub const fn phdr_size(self) -> usize {
        match self {
            Class::Elf32 => PHDR32_SIZE,
            Class::Elf64 => PHDR_SIZE,
        }
    }

    /// Bytes in one dynamic table entry.
    const fn dyn_size(self) -> usize {
        match self {
            Class::Elf32 => DYN32_SIZE,
            Class::Elf64 => DYN_SIZE,
        }
    }

    /// Bytes in one relocation entry, with or without an addend.
    const fn relocation_size(self, explicit: bool) -> usize {
        match (self, explicit) {
            (Class::Elf32, true) => RELA32_SIZE,
            (Class::Elf32, false) => REL32_SIZE,
            (Class::Elf64, true) => RELA_SIZE,
            (Class::Elf64, false) => REL_SIZE,
        }
    }

    /// One past the highest address an image of this class can name, or
    /// `None` when that is the whole of `u64`.
    ///
    /// A 32-bit segment whose end lands past 4 GiB wraps on the machine it
    /// was built for, and must be refused as one that wraps a `u64` would be.
    /// Widening every address to `u64` is what makes that check necessary: a
    /// sum that cannot overflow here would have overflowed there.
    #[must_use]
    pub const fn address_limit(self) -> Option<u64> {
        match self {
            Class::Elf32 => Some(1 << 32),
            Class::Elf64 => None,
        }
    }
}

/// Why an image was rejected.
///
/// Every variant means the same thing operationally — do not load this — but
/// they are distinguished because a wrong-architecture binary and a truncated
/// one call for very different responses from whoever sees the message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElfError {
    /// The image is smaller than its class's file header.
    TooShort,
    /// The first four bytes are not `\x7fELF`.
    BadMagic,
    /// Neither a 32-bit nor a 64-bit object. Carries the class byte found.
    UnsupportedClass(u8),
    /// Not little-endian.
    NotLittleEndian,
    /// Built for a different CPU. Carries the `e_machine` found.
    BadMachine(u16),
    /// Neither an executable nor a shared object. Carries the `e_type` found.
    BadType(u16),
    /// Not an `ET_EXEC`, for a caller that cannot relocate. Carries the
    /// `e_type` found.
    NotFixedAddress(u16),
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
    /// A `PT_INTERP` segment whose contents are not a path: empty, not
    /// terminated by a NUL, or holding one before the end.
    BadInterpreter,
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ElfError::TooShort => f.write_str("image is shorter than an ELF header"),
            ElfError::BadMagic => f.write_str("not an ELF image"),
            ElfError::UnsupportedClass(found) => {
                write!(f, "ELF class {found} is neither 32-bit nor 64-bit")
            }
            ElfError::NotLittleEndian => f.write_str("not a little-endian ELF image"),
            ElfError::BadMachine(found) => write!(f, "built for e_machine {found}"),
            ElfError::BadType(found) => write!(f, "e_type {found} is neither EXEC nor DYN"),
            ElfError::BadInterpreter => {
                f.write_str("the PT_INTERP segment does not hold a NUL-terminated path")
            }
            ElfError::NotFixedAddress(found) => {
                write!(
                    f,
                    "e_type {found} is not EXEC, and this image is not relocated"
                )
            }
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

/// Read an address-sized little-endian field at `offset`, widened to `u64`.
fn word_at(class: Class, bytes: &[u8], offset: usize) -> Option<u64> {
    match class {
        Class::Elf32 => u32_at(bytes, offset).map(u64::from),
        Class::Elf64 => u64_at(bytes, offset),
    }
}

// ---------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------

/// The ELF file header, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// Whether the image is 32-bit or 64-bit.
    pub class: Class,
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

impl Header {
    /// Decode the fields past `e_ident`, at the offsets `class` puts them.
    fn read(class: Class, image: &[u8]) -> Option<Header> {
        // e_type and e_machine are at the same offsets in both classes; the
        // entry point is where the two layouts start to diverge, because it is
        // the first field whose width is the address width.
        let (entry, phoff, phentsize, phnum) = match class {
            Class::Elf32 => (24, 28, 42, 44),
            Class::Elf64 => (24, 32, 54, 56),
        };
        Some(Header {
            class,
            elf_type: u16_at(image, 16)?,
            machine: u16_at(image, 18)?,
            entry: word_at(class, image, entry)?,
            phoff: word_at(class, image, phoff)?,
            phentsize: u16_at(image, phentsize)?,
            phnum: u16_at(image, phnum)?,
        })
    }
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
    /// Decode the program header at `base`.
    ///
    /// The two classes do not merely widen the fields, they reorder them:
    /// ELF64 moved `p_flags` up beside `p_type` so the 64-bit fields after it
    /// are naturally aligned.
    fn read(class: Class, image: &[u8], base: usize) -> Option<Segment> {
        match class {
            Class::Elf32 => Some(Segment {
                kind: u32_at(image, base)?,
                offset: u64::from(u32_at(image, base + 4)?),
                vaddr: u64::from(u32_at(image, base + 8)?),
                filesz: u64::from(u32_at(image, base + 16)?),
                memsz: u64::from(u32_at(image, base + 20)?),
                flags: u32_at(image, base + 24)?,
                align: u64::from(u32_at(image, base + 28)?),
            }),
            Class::Elf64 => Some(Segment {
                kind: u32_at(image, base)?,
                flags: u32_at(image, base + 4)?,
                offset: u64_at(image, base + 8)?,
                vaddr: u64_at(image, base + 16)?,
                filesz: u64_at(image, base + 32)?,
                memsz: u64_at(image, base + 40)?,
                align: u64_at(image, base + 48)?,
            }),
        }
    }

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
    /// `None` if the range would wrap a `u64`, which is a malformed image
    /// rather than an address a loader should try to satisfy. A 32-bit image
    /// can wrap sooner than that; [`Elf::validate_segments`] and
    /// [`Elf::load_span`] apply its class's limit too.
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
        let header = read_header(image)?;
        // The whole program header table has to be inside the image. Checking
        // it once here is what lets `segments()` be infallible.
        if headers_end(&header)? > image.len() as u64 {
            return Err(ElfError::HeaderOutOfBounds);
        }
        Ok(Elf { image, header })
    }

    /// How many leading bytes of a file hold its file header and its whole
    /// program header table: what [`Elf::parse`] has to be handed, for a
    /// caller that reads a file a piece at a time rather than whole.
    ///
    /// `image` need hold only the file header. Nothing bounds the answer --
    /// a table may claim to end anywhere -- so a caller about to read that
    /// much sets a limit of its own first.
    ///
    /// # Errors
    ///
    /// What [`Elf::parse`] refuses about the file header, and
    /// [`ElfError::HeaderOutOfBounds`] for a table whose end wraps or whose
    /// entries are too small to be program headers.
    pub fn headers_len(image: &[u8]) -> Result<u64, ElfError> {
        let header = read_header(image)?;
        Ok(headers_end(&header)?.max(header.class.header_size() as u64))
    }

    /// The decoded file header.
    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    /// Whether the image is 32-bit or 64-bit.
    #[must_use]
    pub const fn class(&self) -> Class {
        self.header.class
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

    /// Reject an image that only runs once it has been relocated, for a caller
    /// that places images at their link-time address and applies nothing.
    ///
    /// [`Elf::check_machine`] accepts `ET_DYN`, and rightly for a program
    /// loader that biases and relocates one. A loader that does neither would
    /// run a position-independent image with every absolute address it holds
    /// still pointing at wherever the linker left it.
    pub const fn check_fixed_address(&self) -> Result<(), ElfError> {
        if self.header.elf_type != ET_EXEC {
            return Err(ElfError::NotFixedAddress(self.header.elf_type));
        }
        Ok(())
    }

    /// Every program header, in file order.
    #[must_use]
    pub const fn segments(&self) -> Segments<'a> {
        Segments {
            image: self.image,
            class: self.header.class,
            offset: self.header.phoff,
            entsize: self.header.phentsize as u64,
            left: self.header.phnum,
        }
    }

    /// Just the segments that have to be loaded into memory.
    pub fn loadable(&self) -> impl Iterator<Item = Segment> + 'a {
        self.segments().filter(Segment::is_load)
    }

    /// The path of the dynamic linker this image asks to be started by,
    /// without its terminating NUL.
    ///
    /// `None` when the image names none, which is every static binary and
    /// every static PIE. `Some(Err(..))` when it names one and what it names
    /// is not a path: a segment whose contents are outside the image, or are
    /// empty, or do not end in a NUL, or hold one before the end. Those are
    /// distinguished from `None` because an image that asks for an interpreter
    /// and cannot say which cannot be run, where one that asks for none runs
    /// perfectly well.
    ///
    /// The first `PT_INTERP` wins, as Linux takes the first. A second is not
    /// an error here: `validate_segments` is where an image's shape is judged,
    /// and this answers a question about the one segment that matters.
    pub fn interpreter(&self) -> Option<Result<&'a [u8], ElfError>> {
        let segment = self.segments().find(|segment| segment.kind == PT_INTERP)?;
        Some(interpreter_path(&segment, self.image))
    }

    /// One past the last address `segment` occupies, or `None` if that wraps
    /// the address space of this image's class.
    fn end_of(&self, segment: &Segment) -> Option<u64> {
        let end = segment.vaddr_end()?;
        match self.header.class.address_limit() {
            Some(limit) if end > limit => None,
            _ => Some(end),
        }
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
            high = high.max(self.end_of(&segment)?);
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
        self.validate_segments_within(self.image.len() as u64)
    }

    /// [`Elf::validate_segments`], for an image that holds only the
    /// beginning of a file of `file_len` bytes: each loadable segment's
    /// contents have to be inside the file rather than inside the image.
    ///
    /// What a loader that maps a file, rather than reading it whole, checks
    /// before it maps anything. Only where the contents are is asked, never
    /// what they hold.
    pub fn validate_segments_within(&self, file_len: u64) -> Result<(), ElfError> {
        for segment in self.segments() {
            if segment.kind != PT_LOAD {
                continue;
            }
            if segment.memsz < segment.filesz {
                return Err(ElfError::SegmentMalformed);
            }
            if self.end_of(&segment).is_none() {
                return Err(ElfError::SegmentMalformed);
            }
            let end = segment
                .offset
                .checked_add(segment.filesz)
                .ok_or(ElfError::SegmentOutOfBounds)?;
            if end > file_len {
                return Err(ElfError::SegmentOutOfBounds);
            }
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
    ///
    /// An image carries a `RELA` table, a `REL` table, or neither; a static
    /// PIE has one. Should a malformed one name both, the `RELA` table is the
    /// one read, and the iterator's [`Addend`]s say so.
    pub fn relocations(&self) -> Result<Relocations<'a>, ElfError> {
        let Some(dynamic) = self.segments().find(|segment| segment.kind == PT_DYNAMIC) else {
            return Ok(Relocations::EMPTY);
        };
        let table = dynamic.data(self.image)?;
        let class = self.header.class;
        let step = class.dyn_size();

        let mut rela = Table::default();
        let mut rel = Table::default();
        for index in 0..(table.len() / step) {
            let base = index * step;
            let (tag, value) = match class {
                Class::Elf32 => (
                    i64::from(u32_at(table, base).ok_or(ElfError::SegmentOutOfBounds)? as i32),
                    u64::from(u32_at(table, base + 4).ok_or(ElfError::SegmentOutOfBounds)?),
                ),
                Class::Elf64 => (
                    u64_at(table, base).ok_or(ElfError::SegmentOutOfBounds)? as i64,
                    u64_at(table, base + 8).ok_or(ElfError::SegmentOutOfBounds)?,
                ),
            };
            match tag {
                DT_NULL => break,
                DT_RELA => rela.addr = value,
                DT_RELASZ => rela.size = value,
                DT_RELAENT => rela.entsize = value,
                DT_REL => rel.addr = value,
                DT_RELSZ => rel.size = value,
                DT_RELENT => rel.entsize = value,
                _ => {}
            }
        }

        let (found, explicit) = if rela.addr != 0 {
            (rela, true)
        } else {
            (rel, false)
        };
        // An absent entry size means the format's own, which is what every
        // linker writes anyway.
        let minimum = class.relocation_size(explicit) as u64;
        let entsize = if found.entsize == 0 {
            minimum
        } else {
            found.entsize
        };
        if found.addr == 0 || found.size == 0 || entsize < minimum {
            return Ok(Relocations::EMPTY);
        }

        let expected = match self.header.machine {
            EM_X86_64 => R_X86_64_RELATIVE,
            EM_AARCH64 => R_AARCH64_RELATIVE,
            EM_ARM => R_ARM_RELATIVE,
            other => return Err(ElfError::BadMachine(other)),
        };

        // DT_RELA and DT_REL are link-time addresses; find the file bytes
        // behind them.
        let bytes = self
            .vaddr_to_bytes(found.addr, found.size)
            .ok_or(ElfError::SegmentOutOfBounds)?;

        Ok(Relocations {
            bytes,
            class,
            explicit,
            entsize: usize::try_from(entsize).map_err(|_| ElfError::SegmentOutOfBounds)?,
            offset: 0,
            expected,
        })
    }
}

/// One relocation table as the dynamic section describes it.
#[derive(Clone, Copy, Default, Debug)]
struct Table {
    addr: u64,
    size: u64,
    entsize: u64,
}

/// Iterator over an image's program headers.
#[derive(Clone, Copy, Debug)]
pub struct Segments<'a> {
    image: &'a [u8],
    class: Class,
    offset: u64,
    entsize: u64,
    left: u16,
}

impl Iterator for Segments<'_> {
    type Item = Segment;

    fn next(&mut self) -> Option<Segment> {
        // `Elf::parse` checked that the whole table is inside the image and
        // that entries are at least a program header long, so every read
        // here succeeds.
        if self.left != 0 {
            let base = usize::try_from(self.offset).ok()?;
            self.left -= 1;
            self.offset = self.offset.checked_add(self.entsize)?;
            return Segment::read(self.class, self.image, base);
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.left as usize))
    }
}

/// Where a relocation's addend comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Addend {
    /// A `RELA` entry: the addend is in the entry itself.
    Explicit(i64),
    /// A `REL` entry: the addend is the word stored at the target, which the
    /// linker wrote there and the loader has to read before overwriting it.
    InPlace,
}

/// One `R_*_RELATIVE` relocation: store `bias + addend` at `bias + offset`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Relocation {
    /// Link-time address of the word to patch.
    pub offset: u64,
    /// The value to add the load bias to, or where to find it.
    pub addend: Addend,
}

impl Relocation {
    /// The link-time address to patch, given a load bias.
    #[must_use]
    pub const fn target(&self, bias: u64) -> u64 {
        self.offset.wrapping_add(bias)
    }

    /// The value to store, given a load bias and `stored`, the word the
    /// target held before relocation.
    ///
    /// `stored` is only read for an [`Addend::InPlace`] entry; for an
    /// explicit one it is ignored, because reading it would be treating a
    /// `RELA` image's placeholder as though it meant something.
    #[must_use]
    pub const fn value(&self, bias: u64, stored: u64) -> u64 {
        let addend = match self.addend {
            Addend::Explicit(addend) => addend as u64,
            Addend::InPlace => stored,
        };
        bias.wrapping_add(addend)
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
    class: Class,
    explicit: bool,
    entsize: usize,
    offset: usize,
    expected: u32,
}

impl Relocations<'_> {
    const EMPTY: Relocations<'static> = Relocations {
        bytes: &[],
        class: Class::Elf64,
        explicit: true,
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
            let size = self.class.relocation_size(self.explicit);
            if base.checked_add(size)? > self.bytes.len() {
                return None;
            }
            self.offset = self.offset.checked_add(self.entsize)?;

            let offset = word_at(self.class, self.bytes, base)?;
            // The type is the low 32 bits of an ELF64 `r_info` and the low 8
            // of an ELF32 one; the rest is a symbol index, which a relative
            // relocation does not use.
            let (kind, addend) = match self.class {
                Class::Elf32 => {
                    let info = u32_at(self.bytes, base + 4)?;
                    let addend = if self.explicit {
                        Addend::Explicit(i64::from(u32_at(self.bytes, base + 8)? as i32))
                    } else {
                        Addend::InPlace
                    };
                    (info & 0xFF, addend)
                }
                Class::Elf64 => {
                    let info = u64_at(self.bytes, base + 8)?;
                    let addend = if self.explicit {
                        Addend::Explicit(u64_at(self.bytes, base + 16)? as i64)
                    } else {
                        Addend::InPlace
                    };
                    (info as u32, addend)
                }
            };

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

mod symbols;
pub use symbols::{
    MAX_LABEL_SPAN, SHDR_SIZE, SHDR32_SIZE, SHT_STRTAB, SHT_SYMTAB, STT_FUNC, STT_NOTYPE,
    STT_OBJECT, SYM_SIZE, SYM32_SIZE, Symbol, Symbols,
};

/// The file header at the start of `image`, checked as far as it can be
/// without the program header table.
fn read_header(image: &[u8]) -> Result<Header, ElfError> {
    let ident = image.get(0..16).ok_or(ElfError::TooShort)?;
    if ident.get(0..4) != Some(&ELF_MAGIC) {
        return Err(ElfError::BadMagic);
    }
    let class = match ident.get(4).copied() {
        Some(ELFCLASS64) => Class::Elf64,
        Some(ELFCLASS32) => Class::Elf32,
        Some(other) => return Err(ElfError::UnsupportedClass(other)),
        None => return Err(ElfError::TooShort),
    };
    if ident.get(5) != Some(&ELFDATA2LSB) {
        return Err(ElfError::NotLittleEndian);
    }
    if image.len() < class.header_size() {
        return Err(ElfError::TooShort);
    }
    Header::read(class, image).ok_or(ElfError::TooShort)
}

/// One past the last byte of the program header table `header` describes,
/// or zero when it describes none; refused when the end wraps or the entries
/// are smaller than a program header of the header's class.
fn headers_end(header: &Header) -> Result<u64, ElfError> {
    if header.phnum == 0 {
        return Ok(0);
    }
    if (header.phentsize as usize) < header.class.phdr_size() {
        return Err(ElfError::HeaderOutOfBounds);
    }
    let span = (header.phnum as u64)
        .checked_mul(header.phentsize as u64)
        .ok_or(ElfError::HeaderOutOfBounds)?;
    header
        .phoff
        .checked_add(span)
        .ok_or(ElfError::HeaderOutOfBounds)
}

/// The path inside a `PT_INTERP` segment: its contents less the NUL, checked.
fn interpreter_path<'a>(segment: &Segment, image: &'a [u8]) -> Result<&'a [u8], ElfError> {
    interpreter_name(segment.data(image)?)
}

/// The path a `PT_INTERP` segment's contents `data` name, less the NUL they
/// end in: [`Elf::interpreter`]'s answer, for a caller that read the
/// segment's contents from the file itself.
///
/// # Errors
///
/// [`ElfError::BadInterpreter`] for contents that are not a path.
pub fn interpreter_name(data: &[u8]) -> Result<&[u8], ElfError> {
    // A path, not a string: it is handed to the same name resolution an
    // `execve` argument is, so the only shapes refused here are the ones no
    // resolution could take. An empty segment names nothing; a segment that
    // does not end in a NUL is a name the image did not finish writing; and a
    // NUL before the end means the bytes after it are unreachable, which is a
    // malformed segment rather than a path with a suffix.
    match data.split_last() {
        Some((0, rest)) if !rest.is_empty() && !rest.contains(&0) => Ok(rest),
        _ => Err(ElfError::BadInterpreter),
    }
}
