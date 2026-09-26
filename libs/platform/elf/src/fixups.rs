//! Where an image has to be patched to run somewhere other than where it was
//! linked: the loader's half of KASLR.
//!
//! An image says so in one of two ways, and this module reads both into one
//! stream of [`Fixup`]s:
//!
//! * **A position-independent executable** (`ET_DYN`) carries a dynamic
//!   relocation table -- `.rela.dyn` on the 64-bit pair -- which the linker
//!   wrote for exactly this and which holds nothing but `R_*_RELATIVE`
//!   entries in a static PIE. Found by section header rather than through
//!   `PT_DYNAMIC`, because the kernel's link script loads the table without
//!   declaring a dynamic segment, which nothing at run time would read.
//! * **A fixed-address 32-bit Arm executable linked with `--emit-relocs`**,
//!   which keeps every relocation the linker resolved in a section beside the
//!   one it patched (`.rel.text`, `.rel.rodata`). The ARMv7-A kernel is built
//!   this way because the target's precompiled `core` materialises addresses
//!   with `movw`/`movt` pairs, which no dynamic relocation can express, so a
//!   PIE link of it fails. Of those relocations, the absolute ones are the
//!   fixups: a word holding an address (`R_ARM_ABS32`), and the halves of an
//!   address a `movw`/`movt` pair builds. A bias that is a multiple of 64 KiB
//!   leaves every `movw` as it was and adds its top half to every `movt`,
//!   which is why [`Elf::fixup_granule`] asks for that alignment.
//!
//! Anything else in either kind of table is refused rather than skipped: a
//! fixup left out is an image that runs until it follows the one pointer that
//! was never moved.

use crate::{
    Addend, Class, EM_AARCH64, EM_ARM, EM_X86_64, ET_DYN, ET_EXEC, Elf, ElfError,
    R_AARCH64_RELATIVE, R_ARM_RELATIVE, R_X86_64_RELATIVE, symbols::Section, u16_at, u32_at,
    u64_at, word_at,
};

/// Section type of a relocation table whose entries carry their addend.
pub const SHT_RELA: u32 = 4;
/// Section type of a relocation table whose addend is stored at the target.
pub const SHT_REL: u32 = 9;
/// Section flag: the section occupies memory while the image runs.
pub const SHF_ALLOC: u64 = 2;

/// `R_ARM_ABS32`: a word holding `S + A`.
pub const R_ARM_ABS32: u32 = 2;
/// `R_ARM_TARGET1`, which lld resolves as `R_ARM_ABS32`.
pub const R_ARM_TARGET1: u32 = 38;
/// `R_ARM_MOVW_ABS_NC`: the low half of `S + A` in an A32 `movw`.
pub const R_ARM_MOVW_ABS_NC: u32 = 43;
/// `R_ARM_MOVT_ABS`: the high half of `S + A` in an A32 `movt`.
pub const R_ARM_MOVT_ABS: u32 = 44;

/// The 32-bit Arm relocations a bias leaves correct, because each is relative
/// to the place it patches and both move together: `NONE`, `REL32`, `CALL`,
/// `JUMP24`, `V4BX`, `PREL31`, `MOVW_PREL_NC`, `MOVT_PREL`, and the Thumb
/// branches `THM_CALL`, `THM_JUMP24`, `THM_JUMP19`, `THM_JUMP11`, `THM_JUMP8`.
const ARM_PLACE_RELATIVE: [u32; 13] = [0, 3, 28, 29, 40, 42, 45, 46, 10, 30, 51, 102, 103];

/// `st_shndx` of a symbol whose value is a number rather than an address.
const SHN_ABS: u16 = 0xFFF1;

/// The alignment a bias needs when an image has `movw`/`movt` fixups.
pub const MOVW_MOVT_GRANULE: u64 = 0x1_0000;

/// What a fixup does to the word at its place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FixupKind {
    /// `R_*_RELATIVE`: store the bias plus the addend, an address-sized word.
    Relative(Addend),
    /// A 32-bit word holding a link-time address: add the bias to it.
    Word32,
    /// An A32 `movw` building an address's low half, which a bias that is a
    /// multiple of [`MOVW_MOVT_GRANULE`] leaves as it is.
    ArmMovw,
    /// An A32 `movt` building an address's high half: add the bias's high
    /// half to its immediate.
    ArmMovt,
}

/// One place a loader patches, by link-time address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fixup {
    /// Link-time address of the word to patch.
    pub at: u64,
    /// What to do to it.
    pub kind: FixupKind,
}

/// The encoding bits of an A32 `movw` (`cond 0011 0000 ...`) and `movt`
/// (`cond 0011 0100 ...`), with the condition masked off.
const MOV_OPCODE_MASK: u32 = 0x0FF0_0000;
/// See [`MOV_OPCODE_MASK`].
const MOVW_OPCODE: u32 = 0x0300_0000;
/// See [`MOV_OPCODE_MASK`].
const MOVT_OPCODE: u32 = 0x0340_0000;

impl Fixup {
    /// Bytes this fixup rewrites in an image of `class`.
    #[must_use]
    pub const fn width(&self, class: Class) -> usize {
        match (self.kind, class) {
            (FixupKind::Relative(_), Class::Elf64) => 8,
            _ => 4,
        }
    }

    /// What the word at [`Fixup::at`] becomes when the image is moved by
    /// `bias`, given `stored`, what it holds now.
    ///
    /// `None` when this fixup cannot express that bias -- a `movw` or `movt`
    /// with a bias that is not a multiple of [`MOVW_MOVT_GRANULE`] -- or when
    /// `stored` is not the instruction the relocation says it is, which is an
    /// image that does not match its own relocations.
    #[must_use]
    pub const fn apply(&self, bias: u64, stored: u64) -> Option<u64> {
        let aligned = bias.is_multiple_of(MOVW_MOVT_GRANULE);
        match self.kind {
            FixupKind::Relative(addend) => {
                let base = match addend {
                    Addend::Explicit(addend) => addend as u64,
                    Addend::InPlace => stored,
                };
                Some(bias.wrapping_add(base))
            }
            FixupKind::Word32 => Some((stored as u32).wrapping_add(bias as u32) as u64),
            FixupKind::ArmMovw => {
                if !aligned || stored as u32 & MOV_OPCODE_MASK != MOVW_OPCODE {
                    return None;
                }
                Some(stored)
            }
            FixupKind::ArmMovt => {
                let word = stored as u32;
                if !aligned || word & MOV_OPCODE_MASK != MOVT_OPCODE {
                    return None;
                }
                let immediate = ((word >> 4) & 0xF000) | (word & 0x0FFF);
                let moved = immediate as u64 + ((bias >> 16) & 0xFFFF);
                if moved > 0xFFFF {
                    return None;
                }
                let moved = moved as u32;
                Some(((word & 0xFFF0_F000) | ((moved & 0xF000) << 4) | (moved & 0x0FFF)) as u64)
            }
        }
    }
}

impl<'a> Elf<'a> {
    /// Whether this image can be run somewhere other than its link address:
    /// a PIE, or an Arm executable that kept its relocations. A fixed-address
    /// image without them runs only where it was linked.
    #[must_use]
    pub fn is_relocatable(&self) -> bool {
        match self.header.elf_type {
            ET_DYN => true,
            ET_EXEC if self.header.machine == EM_ARM => {
                self.relocation_sections().any(|(_, kept)| kept)
            }
            _ => false,
        }
    }

    /// The alignment a bias must have for every fixup to express it: the
    /// largest segment alignment, or [`MOVW_MOVT_GRANULE`] for an Arm image
    /// with `movw`/`movt` fixups, whichever is larger. At least one.
    ///
    /// # Errors
    ///
    /// Whatever [`Elf::fixups`] refuses.
    pub fn fixup_granule(&self) -> Result<u64, ElfError> {
        let mut granule = self
            .loadable()
            .map(|segment| segment.align)
            .fold(1, u64::max);
        for fixup in self.fixups() {
            if matches!(fixup?.kind, FixupKind::ArmMovw | FixupKind::ArmMovt) {
                granule = granule.max(MOVW_MOVT_GRANULE);
            }
        }
        Ok(granule)
    }

    /// Every place a loader has to patch to run this image moved by some bias.
    ///
    /// Empty for an image that is not [relocatable](Elf::is_relocatable).
    #[must_use]
    pub fn fixups(&self) -> Fixups<'a> {
        Fixups {
            elf: *self,
            section: 0,
            table: None,
        }
    }

    /// Each relocation section, and whether [`Elf::fixups`] reads it.
    fn relocation_sections(&self) -> impl Iterator<Item = (Section, bool)> + '_ {
        let count = self.section_table().map_or(0, |(_, _, count)| count);
        (0..count)
            .filter_map(|index| self.section(index))
            .filter(|section| section.kind == SHT_RELA || section.kind == SHT_REL)
            .map(|section| (section, self.reads(&section)))
    }

    /// Whether `section`, a relocation section, holds fixups: the loaded table
    /// of a PIE, or a kept table patching a loaded section of an Arm
    /// executable. Tables that patch debug information are neither.
    fn reads(&self, section: &Section) -> bool {
        let loaded = section.flags & SHF_ALLOC != 0;
        match self.header.elf_type {
            ET_DYN => loaded,
            ET_EXEC if self.header.machine == EM_ARM && !loaded => self
                .section(u64::from(section.info))
                .is_some_and(|target| target.flags & SHF_ALLOC != 0),
            _ => false,
        }
    }
}

/// One relocation table being read.
#[derive(Clone, Copy, Debug)]
struct Table<'a> {
    entries: &'a [u8],
    entsize: usize,
    explicit: bool,
    next: usize,
    /// The symbol table the entries index, for an executable's kept
    /// relocations; `None` for a PIE's, which name no symbols.
    symbols: Option<(&'a [u8], usize)>,
}

/// Iterator over an image's [`Fixup`]s, section by section.
#[derive(Clone, Copy, Debug)]
pub struct Fixups<'a> {
    elf: Elf<'a>,
    section: u64,
    table: Option<Table<'a>>,
}

impl<'a> Fixups<'a> {
    /// The next relocation section [`Elf::fixups`] reads, opened.
    fn open_next(&mut self) -> Option<Result<Table<'a>, ElfError>> {
        let (_, _, count) = self.elf.section_table()?;
        while self.section < count {
            let index = self.section;
            self.section += 1;
            let Some(section) = self.elf.section(index) else {
                return Some(Err(ElfError::SegmentOutOfBounds));
            };
            if !(section.kind == SHT_RELA || section.kind == SHT_REL) || !self.elf.reads(&section) {
                continue;
            }
            return Some(self.open(&section));
        }
        None
    }

    /// Open `section` for reading.
    fn open(&self, section: &Section) -> Result<Table<'a>, ElfError> {
        let class = self.elf.class();
        let explicit = section.kind == SHT_RELA;
        let minimum = class.relocation_size(explicit);
        let entsize = usize::try_from(section.entsize).map_err(|_| ElfError::SegmentOutOfBounds)?;
        let entsize = if entsize == 0 { minimum } else { entsize };
        if entsize < minimum {
            return Err(ElfError::SegmentMalformed);
        }
        let entries = section
            .bytes(self.elf.image())
            .ok_or(ElfError::SegmentOutOfBounds)?;
        let symbols = if self.elf.header.elf_type == ET_EXEC {
            let table = self
                .elf
                .section(u64::from(section.link))
                .ok_or(ElfError::SegmentOutOfBounds)?;
            let bytes = table
                .bytes(self.elf.image())
                .ok_or(ElfError::SegmentOutOfBounds)?;
            let size = usize::try_from(table.entsize).map_err(|_| ElfError::SegmentOutOfBounds)?;
            Some((bytes, size.max(crate::SYM32_SIZE)))
        } else {
            None
        };
        Ok(Table {
            entries,
            entsize,
            explicit,
            next: 0,
            symbols,
        })
    }

    /// Decode the entry at `base` of `table`: `None` for one that needs no
    /// fixup.
    fn decode(&self, table: &Table<'a>, base: usize) -> Result<Option<Fixup>, ElfError> {
        let class = self.elf.class();
        let bytes = table.entries;
        let at = word_at(class, bytes, base).ok_or(ElfError::SegmentOutOfBounds)?;
        let (kind, symbol, addend) = match class {
            Class::Elf32 => {
                let info = u32_at(bytes, base + 4).ok_or(ElfError::SegmentOutOfBounds)?;
                let addend = if table.explicit {
                    let value = u32_at(bytes, base + 8).ok_or(ElfError::SegmentOutOfBounds)?;
                    Addend::Explicit(i64::from(value as i32))
                } else {
                    Addend::InPlace
                };
                (info & 0xFF, info >> 8, addend)
            }
            Class::Elf64 => {
                let info = u64_at(bytes, base + 8).ok_or(ElfError::SegmentOutOfBounds)?;
                let addend = if table.explicit {
                    let value = u64_at(bytes, base + 16).ok_or(ElfError::SegmentOutOfBounds)?;
                    Addend::Explicit(value as i64)
                } else {
                    Addend::InPlace
                };
                (info as u32, (info >> 32) as u32, addend)
            }
        };
        let Some(symbols) = table.symbols else {
            return dynamic(self.elf.machine(), at, kind, addend);
        };
        if ARM_PLACE_RELATIVE.contains(&kind) || !names_an_address(symbols, symbol) {
            return Ok(None);
        }
        let kind = match kind {
            R_ARM_ABS32 | R_ARM_TARGET1 => FixupKind::Word32,
            R_ARM_MOVW_ABS_NC => FixupKind::ArmMovw,
            R_ARM_MOVT_ABS => FixupKind::ArmMovt,
            other => return Err(ElfError::UnsupportedRelocation(other)),
        };
        Ok(Some(Fixup { at, kind }))
    }
}

/// A PIE's entry: its machine's `R_*_RELATIVE`, or `NONE`, which is padding.
fn dynamic(machine: u16, at: u64, kind: u32, addend: Addend) -> Result<Option<Fixup>, ElfError> {
    let relative = match machine {
        EM_X86_64 => R_X86_64_RELATIVE,
        EM_AARCH64 => R_AARCH64_RELATIVE,
        EM_ARM => R_ARM_RELATIVE,
        other => return Err(ElfError::BadMachine(other)),
    };
    match kind {
        0 => Ok(None),
        kind if kind == relative => Ok(Some(Fixup {
            at,
            kind: FixupKind::Relative(addend),
        })),
        other => Err(ElfError::UnsupportedRelocation(other)),
    }
}

/// Whether symbol `index` of a 32-bit symbol table stands for an address in
/// the image. Not the null symbol, which an absolute addend alone uses; not
/// an undefined one, which a weak reference resolved to zero, and zero must
/// stay zero; and not an absolute one, which is a number such as the link
/// base the build script defines.
fn names_an_address((table, entsize): (&[u8], usize), index: u32) -> bool {
    if index == 0 {
        return false;
    }
    let Some(base) = usize::try_from(index)
        .ok()
        .and_then(|index| index.checked_mul(entsize))
    else {
        return false;
    };
    match u16_at(table, base + 14) {
        Some(0 | SHN_ABS) | None => false,
        Some(_) => true,
    }
}

impl Iterator for Fixups<'_> {
    type Item = Result<Fixup, ElfError>;

    fn next(&mut self) -> Option<Result<Fixup, ElfError>> {
        loop {
            let mut table = match self.table {
                Some(table) => table,
                None => match self.open_next()? {
                    Ok(table) => table,
                    Err(error) => return Some(Err(error)),
                },
            };
            let base = table.next;
            let size = self.elf.class().relocation_size(table.explicit);
            if base
                .checked_add(size)
                .is_none_or(|end| end > table.entries.len())
            {
                self.table = None;
                continue;
            }
            table.next = base.saturating_add(table.entsize);
            self.table = Some(table);
            match self.decode(&table, base) {
                Ok(None) => {}
                Ok(Some(fixup)) => return Some(Ok(fixup)),
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

#[cfg(test)]
mod tests;
