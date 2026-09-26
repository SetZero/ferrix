//! Tests for the fixup reader.
//!
//! As in the crate's other tests, images are built by hand: here only a file
//! header and section headers, since nothing about a fixup is read from the
//! program headers.

extern crate std;

use std::vec;
use std::vec::Vec;

use crate::*;

/// One section: type, flags, link, info, entry size, contents.
type Spec = (u32, u64, u32, u32, u64, Vec<u8>);

/// Write `bytes` at `at`, growing the buffer as needed.
fn put(buffer: &mut Vec<u8>, at: usize, bytes: &[u8]) {
    if buffer.len() < at + bytes.len() {
        buffer.resize(at + bytes.len(), 0);
    }
    buffer[at..at + bytes.len()].copy_from_slice(bytes);
}

/// An image of `class` holding a null section and then `sections`.
fn image(class: Class, elf_type: u16, machine: u16, sections: &[Spec]) -> Vec<u8> {
    let (header, shdr) = match class {
        Class::Elf32 => (EHDR32_SIZE, SHDR32_SIZE),
        Class::Elf64 => (EHDR_SIZE, SHDR_SIZE),
    };
    let mut out = vec![0_u8; header];
    put(&mut out, 0, &ELF_MAGIC);
    let class_byte = if class == Class::Elf64 {
        ELFCLASS64
    } else {
        ELFCLASS32
    };
    put(&mut out, 4, &[class_byte, ELFDATA2LSB, 1]);
    put(&mut out, 16, &elf_type.to_le_bytes());
    put(&mut out, 18, &machine.to_le_bytes());

    // Contents first, then the table, each section's offset recorded.
    let mut offsets = Vec::new();
    for (_, _, _, _, _, contents) in sections {
        offsets.push(out.len());
        out.extend_from_slice(contents);
        while out.len() % 8 != 0 {
            out.push(0);
        }
    }
    let table = out.len();
    out.resize(table + shdr * (sections.len() + 1), 0);
    for (index, (kind, flags, link, info, entsize, contents)) in sections.iter().enumerate() {
        let at = table + shdr * (index + 1);
        let offset = offsets[index] as u64;
        let size = contents.len() as u64;
        match class {
            Class::Elf32 => {
                put(&mut out, at + 4, &kind.to_le_bytes());
                put(&mut out, at + 8, &(*flags as u32).to_le_bytes());
                put(&mut out, at + 16, &(offset as u32).to_le_bytes());
                put(&mut out, at + 20, &(size as u32).to_le_bytes());
                put(&mut out, at + 24, &link.to_le_bytes());
                put(&mut out, at + 28, &info.to_le_bytes());
                put(&mut out, at + 36, &(*entsize as u32).to_le_bytes());
            }
            Class::Elf64 => {
                put(&mut out, at + 4, &kind.to_le_bytes());
                put(&mut out, at + 8, &flags.to_le_bytes());
                put(&mut out, at + 24, &offset.to_le_bytes());
                put(&mut out, at + 32, &size.to_le_bytes());
                put(&mut out, at + 40, &link.to_le_bytes());
                put(&mut out, at + 44, &info.to_le_bytes());
                put(&mut out, at + 56, &entsize.to_le_bytes());
            }
        }
    }
    let count = (sections.len() + 1) as u16;
    match class {
        Class::Elf32 => {
            put(&mut out, 32, &(table as u32).to_le_bytes());
            put(&mut out, 46, &(shdr as u16).to_le_bytes());
            put(&mut out, 48, &count.to_le_bytes());
        }
        Class::Elf64 => {
            put(&mut out, 40, &(table as u64).to_le_bytes());
            put(&mut out, 58, &(shdr as u16).to_le_bytes());
            put(&mut out, 60, &count.to_le_bytes());
        }
    }
    out
}

/// One ELF64 `RELA` entry.
fn rela(at: u64, kind: u32, addend: i64) -> Vec<u8> {
    let mut entry = Vec::new();
    put(&mut entry, 0, &at.to_le_bytes());
    put(&mut entry, 8, &u64::from(kind).to_le_bytes());
    put(&mut entry, 16, &addend.to_le_bytes());
    entry
}

/// One ELF32 `REL` entry against symbol `symbol`.
fn rel(at: u32, kind: u32, symbol: u32) -> Vec<u8> {
    let mut entry = Vec::new();
    put(&mut entry, 0, &at.to_le_bytes());
    put(&mut entry, 4, &((symbol << 8) | kind).to_le_bytes());
    entry
}

/// An ELF32 symbol table entry in section `shndx`.
fn symbol(shndx: u16) -> Vec<u8> {
    let mut entry = vec![0_u8; SYM32_SIZE];
    put(&mut entry, 14, &shndx.to_le_bytes());
    entry
}

/// An x86-64 PIE whose loaded table holds `entries`, and a kept table for
/// debug information beside it that must be ignored.
fn pie(entries: &[Vec<u8>]) -> Vec<u8> {
    image(
        Class::Elf64,
        ET_DYN,
        EM_X86_64,
        &[
            (SHT_RELA, SHF_ALLOC, 0, 0, 24, entries.concat()),
            (1, 0, 0, 0, 0, vec![0; 16]),
            (SHT_RELA, 0, 0, 2, 24, rela(0x10, 1, 0)),
        ],
    )
}

/// A 32-bit Arm executable: `.text` (1), a symbol table (2) whose entries 1, 2
/// and 3 are defined, absolute and undefined, `.rel.text` (3) holding
/// `entries`, and a debug section (4) with a kept table (5) that is not read.
fn arm(entries: &[Vec<u8>]) -> Vec<u8> {
    let symbols = [symbol(0), symbol(1), symbol(0xFFF1), symbol(0)].concat();
    image(
        Class::Elf32,
        ET_EXEC,
        EM_ARM,
        &[
            (1, SHF_ALLOC | 4, 0, 0, 0, vec![0; 64]),
            (2, 0, 0, 0, SYM32_SIZE as u64, symbols),
            (SHT_REL, 0, 2, 1, 8, entries.concat()),
            (1, 0, 0, 0, 0, vec![0; 16]),
            (SHT_REL, 0, 2, 4, 8, rel(0, R_ARM_ABS32, 1)),
        ],
    )
}

/// Every fixup of `bytes`, or the first refusal.
fn collect(bytes: &[u8]) -> Result<Vec<Fixup>, ElfError> {
    let elf = Elf::parse(bytes).map_err(|_| ElfError::TooShort)?;
    elf.fixups().collect()
}

#[test]
fn a_pie_yields_its_relative_entries_and_skips_padding() {
    let bytes = pie(&[
        rela(0xFFFF_FFFF_8000_1000, R_X86_64_RELATIVE, 0x2000),
        rela(0, 0, 0),
        rela(0xFFFF_FFFF_8000_1008, R_X86_64_RELATIVE, -8),
    ]);
    let elf = Elf::parse(&bytes).unwrap();
    assert!(elf.is_relocatable());
    assert_eq!(
        collect(&bytes).unwrap(),
        vec![
            Fixup {
                at: 0xFFFF_FFFF_8000_1000,
                kind: FixupKind::Relative(Addend::Explicit(0x2000)),
            },
            Fixup {
                at: 0xFFFF_FFFF_8000_1008,
                kind: FixupKind::Relative(Addend::Explicit(-8)),
            },
        ],
        "the NONE entry is padding, and the debug section's table is not read"
    );
    assert_eq!(elf.fixup_granule(), Ok(1));
}

#[test]
fn a_pie_with_a_symbolic_relocation_is_refused() {
    let bytes = pie(&[rela(0x1000, 1, 0)]);
    assert_eq!(collect(&bytes), Err(ElfError::UnsupportedRelocation(1)));
}

#[test]
fn an_arm_executable_yields_its_absolute_relocations_against_addresses() {
    let bytes = arm(&[
        rel(0xF000_0000, R_ARM_ABS32, 1),
        rel(0xF000_0004, R_ARM_ABS32, 2),
        rel(0xF000_0008, R_ARM_ABS32, 3),
        rel(0xF000_000C, R_ARM_ABS32, 0),
        rel(0xF000_0010, R_ARM_MOVW_ABS_NC, 1),
        rel(0xF000_0014, R_ARM_MOVT_ABS, 1),
        rel(0xF000_0018, 28, 1),
        rel(0xF000_001C, R_ARM_TARGET1, 1),
    ]);
    let elf = Elf::parse(&bytes).unwrap();
    assert!(elf.is_relocatable());
    let at = |at: u64, kind: FixupKind| Fixup { at, kind };
    assert_eq!(
        collect(&bytes).unwrap(),
        vec![
            at(0xF000_0000, FixupKind::Word32),
            at(0xF000_0010, FixupKind::ArmMovw),
            at(0xF000_0014, FixupKind::ArmMovt),
            at(0xF000_001C, FixupKind::Word32),
        ],
        "an absolute, an undefined and a null symbol are numbers, a call is \
         place-relative, and the debug table is not read"
    );
    assert_eq!(elf.fixup_granule(), Ok(MOVW_MOVT_GRANULE));
}

#[test]
fn an_arm_relocation_nobody_resolves_is_refused() {
    // R_ARM_THM_MOVW_ABS_NC: a Thumb movw, which this reader does not patch.
    let bytes = arm(&[rel(0xF000_0000, 47, 1)]);
    assert_eq!(collect(&bytes), Err(ElfError::UnsupportedRelocation(47)));
}

#[test]
fn a_fixed_address_image_without_kept_relocations_is_not_relocatable() {
    let x86 = image(
        Class::Elf64,
        ET_EXEC,
        EM_X86_64,
        &[(1, SHF_ALLOC, 0, 0, 0, vec![0; 8])],
    );
    let elf = Elf::parse(&x86).unwrap();
    assert!(!elf.is_relocatable());
    assert_eq!(elf.fixups().count(), 0);

    let bare_arm = image(
        Class::Elf32,
        ET_EXEC,
        EM_ARM,
        &[(1, SHF_ALLOC, 0, 0, 0, vec![0; 8])],
    );
    assert!(!Elf::parse(&bare_arm).unwrap().is_relocatable());
}

#[test]
fn applying_a_fixup_moves_what_it_names() {
    let relative = Fixup {
        at: 0,
        kind: FixupKind::Relative(Addend::Explicit(0x1000)),
    };
    assert_eq!(relative.apply(0x20_0000, 0), Some(0x20_1000));
    let in_place = Fixup {
        at: 0,
        kind: FixupKind::Relative(Addend::InPlace),
    };
    assert_eq!(in_place.apply(0x10, 0x1000), Some(0x1010));

    let word = Fixup {
        at: 0,
        kind: FixupKind::Word32,
    };
    assert_eq!(word.apply(0x0010_0000, 0xF000_1000), Some(0xF010_1000));

    // movt r0, #0xf012, moved by 0x0123_0000: #0xf135.
    let movt = Fixup {
        at: 0,
        kind: FixupKind::ArmMovt,
    };
    assert_eq!(movt.apply(0x0123_0000, 0xE34F_0012), Some(0xE34F_0135));
    assert_eq!(
        movt.apply(0x0123_4000, 0xE34F_0012),
        None,
        "not 64 KiB aligned"
    );
    assert_eq!(movt.apply(0x0FFF_0000, 0xE34F_0012), None, "past 4 GiB");
    assert_eq!(
        movt.apply(0x0001_0000, 0xE300_0012),
        None,
        "a movw, not a movt"
    );

    let movw = Fixup {
        at: 0,
        kind: FixupKind::ArmMovw,
    };
    assert_eq!(movw.apply(0x0123_0000, 0xE300_0012), Some(0xE300_0012));
    assert_eq!(
        movw.apply(0x0123_0000, 0xE34F_0012),
        None,
        "a movt, not a movw"
    );

    assert_eq!(relative.width(Class::Elf64), 8);
    assert_eq!(relative.width(Class::Elf32), 4);
    assert_eq!(movt.width(Class::Elf64), 4);
}
