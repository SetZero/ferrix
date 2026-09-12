//! Tests for the symbol table reader.
//!
//! As in the crate's other tests, images are built by hand, so that a failure
//! names the field that is wrong and a malformed case is one mutated field of
//! a good image.

extern crate std;

use std::vec;
use std::vec::Vec;

use crate::*;

/// One symbol for a synthetic image: name, value, size, type.
type Entry<'a> = (&'a str, u64, u64, u8);

/// A section header's fields: type, offset, size, link, entry size.
type Fields = (u32, usize, usize, u32, usize);

/// Symbols that exercise each rule `function_at` applies.
const TABLE: &[Entry<'static>] = &[
    (
        "_ZN6kernel5kmain17h0123456789abcdefE",
        0x1000,
        0x80,
        STT_FUNC,
    ),
    ("data", 0x1800, 0x10, STT_OBJECT),
    ("vectors", 0x2000, 0, STT_NOTYPE),
    ("$x", 0x2100, 0, STT_NOTYPE),
];

/// Write `bytes` at `at`, growing the buffer as needed.
fn put(buffer: &mut Vec<u8>, at: usize, bytes: &[u8]) {
    if buffer.len() < at + bytes.len() {
        buffer.resize(at + bytes.len(), 0);
    }
    buffer[at..at + bytes.len()].copy_from_slice(bytes);
}

/// Encode one symbol table entry of `class`, naming `offset` in the strings.
fn entry(class: Class, offset: u32, &(_, value, size, kind): &Entry<'_>) -> Vec<u8> {
    let mut entry = Vec::new();
    put(&mut entry, 0, &offset.to_le_bytes());
    match class {
        Class::Elf64 => {
            put(&mut entry, 4, &[kind, 0]);
            put(&mut entry, 6, &1_u16.to_le_bytes());
            put(&mut entry, 8, &value.to_le_bytes());
            put(&mut entry, 16, &size.to_le_bytes());
        }
        Class::Elf32 => {
            put(&mut entry, 4, &(value as u32).to_le_bytes());
            put(&mut entry, 8, &(size as u32).to_le_bytes());
            put(&mut entry, 12, &[kind, 0]);
            put(&mut entry, 14, &1_u16.to_le_bytes());
        }
    }
    entry
}

/// Write a section header of `class` at `base`.
fn section(out: &mut Vec<u8>, class: Class, base: usize, fields: Fields) {
    let (kind, offset, size, link, entsize) = fields;
    put(out, base + 4, &kind.to_le_bytes());
    match class {
        Class::Elf64 => {
            put(out, base + 24, &(offset as u64).to_le_bytes());
            put(out, base + 32, &(size as u64).to_le_bytes());
            put(out, base + 40, &link.to_le_bytes());
            put(out, base + 56, &(entsize as u64).to_le_bytes());
        }
        Class::Elf32 => {
            put(out, base + 16, &(offset as u32).to_le_bytes());
            put(out, base + 20, &(size as u32).to_le_bytes());
            put(out, base + 24, &link.to_le_bytes());
            put(out, base + 36, &(entsize as u32).to_le_bytes());
        }
    }
}

/// An image of `class` for `machine` holding: the file header, a string
/// table, a symbol table of the null symbol and `entries`, and a section
/// header table of a null section, the symbol table and the string table.
fn image(class: Class, machine: u16, entries: &[Entry<'_>]) -> Vec<u8> {
    let (ehdr, shdr, sym) = match class {
        Class::Elf64 => (EHDR_SIZE, SHDR_SIZE, SYM_SIZE),
        Class::Elf32 => (EHDR32_SIZE, SHDR32_SIZE, SYM32_SIZE),
    };

    let mut strings = vec![0_u8];
    let mut symbols = vec![0_u8; sym];
    for symbol in entries {
        let encoded = entry(class, strings.len() as u32, symbol);
        strings.extend_from_slice(symbol.0.as_bytes());
        strings.push(0);
        symbols.extend_from_slice(&encoded);
    }

    let strings_at = ehdr;
    let symbols_at = strings_at + strings.len();
    let table_at = symbols_at + symbols.len();

    let mut out = vec![0_u8; table_at + 3 * shdr];
    let class_byte = match class {
        Class::Elf64 => ELFCLASS64,
        Class::Elf32 => ELFCLASS32,
    };
    put(&mut out, 0, &ELF_MAGIC);
    put(&mut out, 4, &[class_byte, ELFDATA2LSB, 1]);
    put(&mut out, 16, &ET_EXEC.to_le_bytes());
    put(&mut out, 18, &machine.to_le_bytes());
    match class {
        Class::Elf64 => {
            put(&mut out, 40, &(table_at as u64).to_le_bytes());
            put(&mut out, 58, &(shdr as u16).to_le_bytes());
            put(&mut out, 60, &3_u16.to_le_bytes());
        }
        Class::Elf32 => {
            put(&mut out, 32, &(table_at as u32).to_le_bytes());
            put(&mut out, 46, &(shdr as u16).to_le_bytes());
            put(&mut out, 48, &3_u16.to_le_bytes());
        }
    }
    put(&mut out, strings_at, &strings);
    put(&mut out, symbols_at, &symbols);
    let symtab = (SHT_SYMTAB, symbols_at, symbols.len(), 2, sym);
    section(&mut out, class, table_at + shdr, symtab);
    let strtab = (SHT_STRTAB, strings_at, strings.len(), 0, 0);
    section(&mut out, class, table_at + 2 * shdr, strtab);
    out
}

/// The names `symbols()` yields for `bytes`.
fn names(bytes: &[u8]) -> Vec<Vec<u8>> {
    let elf = Elf::parse(bytes).unwrap();
    elf.symbols()
        .unwrap()
        .map(|symbol| symbol.name.to_vec())
        .collect()
}

#[test]
fn an_address_inside_a_function_names_it_and_the_offset() {
    for class in [Class::Elf64, Class::Elf32] {
        let bytes = image(class, EM_X86_64, TABLE);
        let (symbol, offset) = Elf::parse(&bytes).unwrap().function_at(0x101C).unwrap();
        assert_eq!(symbol.name, b"_ZN6kernel5kmain17h0123456789abcdefE");
        assert_eq!(offset, 0x1C);
    }
}

#[test]
fn the_first_byte_of_a_function_is_offset_zero_and_its_end_is_outside_it() {
    let bytes = image(Class::Elf64, EM_X86_64, &[("f", 0x1000, 0x10, STT_FUNC)]);
    let elf = Elf::parse(&bytes).unwrap();
    assert_eq!(elf.function_at(0x1000).map(|(_, offset)| offset), Some(0));
    assert_eq!(elf.function_at(0x100F).map(|(_, offset)| offset), Some(0xF));
    assert_eq!(elf.function_at(0x1010), None);
    assert_eq!(elf.function_at(0x0FFF), None);
}

#[test]
fn a_data_object_is_never_taken_for_code() {
    let bytes = image(Class::Elf64, EM_X86_64, TABLE);
    // Inside `data`, and past the end of the sized function below it.
    assert_eq!(Elf::parse(&bytes).unwrap().function_at(0x1804), None);
}

#[test]
fn an_unsized_label_covers_what_follows_it_but_a_mapping_symbol_does_not() {
    let bytes = image(Class::Elf64, EM_AARCH64, TABLE);
    // Past `$x` at 0x2100, which must not win for being nearer.
    let (symbol, offset) = Elf::parse(&bytes).unwrap().function_at(0x2204).unwrap();
    assert_eq!(symbol.name, b"vectors");
    assert_eq!(offset, 0x204);
}

#[test]
fn an_unsized_label_does_not_reach_beyond_a_function_s_worth_of_code() {
    let bytes = image(Class::Elf64, EM_AARCH64, TABLE);
    let elf = Elf::parse(&bytes).unwrap();
    let last = 0x2000 + MAX_LABEL_SPAN;
    assert_eq!(
        elf.function_at(last).map(|(symbol, _)| symbol.name),
        Some(&b"vectors"[..])
    );
    assert_eq!(elf.function_at(last + 1), None);
}

#[test]
fn the_thumb_bit_is_cleared_on_arm_and_only_there() {
    let thumb = image(Class::Elf32, EM_ARM, &[("t", 0x1001, 0x20, STT_FUNC)]);
    let (symbol, offset) = Elf::parse(&thumb).unwrap().function_at(0x1008).unwrap();
    assert_eq!((symbol.value, offset), (0x1000, 8));

    let odd = image(Class::Elf64, EM_AARCH64, &[("o", 0x1001, 0x20, STT_FUNC)]);
    let (symbol, _) = Elf::parse(&odd).unwrap().function_at(0x1008).unwrap();
    assert_eq!(symbol.value, 0x1001);
}

#[test]
fn every_defined_symbol_is_listed_in_table_order_without_the_null_one() {
    for class in [Class::Elf64, Class::Elf32] {
        let expected: Vec<Vec<u8>> = TABLE.iter().map(|e| e.0.as_bytes().to_vec()).collect();
        assert_eq!(names(&image(class, EM_ARM, TABLE)), expected);
    }
}

#[test]
fn an_image_with_no_sections_has_no_symbols() {
    let mut bytes = image(Class::Elf64, EM_X86_64, TABLE);
    put(&mut bytes, 60, &0_u16.to_le_bytes());
    let elf = Elf::parse(&bytes).unwrap();
    assert!(elf.symbols().is_none());
    assert_eq!(elf.function_at(0x101C), None);
}

#[test]
fn a_name_outside_the_string_table_skips_only_that_entry() {
    let mut bytes = image(
        Class::Elf64,
        EM_X86_64,
        &[("a", 0x1000, 8, STT_FUNC), ("b", 0x2000, 8, STT_FUNC)],
    );
    // The first real entry follows the null one, after the four bytes of
    // strings "\0a\0b\0" minus the trailing one: header, strings, null entry.
    let first = EHDR_SIZE + b"\0a\0b\0".len() + SYM_SIZE;
    put(&mut bytes, first, &0xFFFF_u32.to_le_bytes());
    assert_eq!(names(&bytes), [b"b".to_vec()]);
}

#[test]
fn a_symbol_entry_too_small_for_its_class_is_refused() {
    let mut bytes = image(Class::Elf64, EM_X86_64, TABLE);
    let table_at = u64::from_le_bytes(bytes[40..48].try_into().unwrap()) as usize;
    put(&mut bytes, table_at + SHDR_SIZE + 56, &8_u64.to_le_bytes());
    assert!(Elf::parse(&bytes).unwrap().symbols().is_none());
}

#[test]
fn a_symbol_table_linked_to_something_other_than_strings_is_refused() {
    let mut bytes = image(Class::Elf32, EM_ARM, TABLE);
    let table_at = u32::from_le_bytes(bytes[32..36].try_into().unwrap()) as usize;
    // Point the symbol table's link at itself.
    put(
        &mut bytes,
        table_at + SHDR32_SIZE + 24,
        &1_u32.to_le_bytes(),
    );
    assert!(Elf::parse(&bytes).unwrap().symbols().is_none());
}

#[test]
fn truncating_the_image_anywhere_gives_nothing_rather_than_a_panic() {
    for class in [Class::Elf64, Class::Elf32] {
        let bytes = image(class, EM_ARM, TABLE);
        for len in 0..bytes.len() {
            if let Ok(elf) = Elf::parse(&bytes[..len]) {
                let _ = elf.function_at(0x101C);
                let _ = elf.symbols().map(Iterator::count);
            }
        }
    }
}
