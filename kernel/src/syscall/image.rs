//! A synthetic ELF image, for the loader's self-check.
//!
//! The loader has to be exercised on the machine, because what it produces is
//! page tables and there is no way to check those from the host. That needs an
//! image, and the alternatives to building one here are worse: a binary
//! committed to the tree would be three binaries, one per architecture, none
//! of them readable in a diff, and all of them stale the moment the loader's
//! assumptions changed.
//!
//! So the check builds its own, for whichever architecture it is running on,
//! with the shape that matters — a read-execute segment covering the headers
//! and a read-write one with more `p_memsz` than `p_filesz`, which is how
//! `.bss` is expressed and the one part of loading with nothing to copy.
//!
//! Deliberately not a general ELF writer. It emits exactly what the check
//! needs and is wrong for anything else: no sections, no dynamic table, no
//! alignment congruence between file offset and virtual address.

use alloc::vec;
use alloc::vec::Vec;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_elf::{Class, PF_R, PF_W, PF_X, PT_LOAD};

/// Where the synthetic image is linked. Well above zero and well below
/// anything the kernel uses, on every architecture including the 32-bit one.
pub(crate) const BASE: u64 = 0x0040_0000;

/// The entry point the image declares: a little way into the text segment.
pub(crate) const ENTRY: u64 = BASE + 0x100;

/// Where the writable segment is linked.
pub(crate) const DATA_VADDR: u64 = BASE + 0x0001_0000;

/// Bytes of the data segment that come from the file; the rest is `.bss`.
pub(crate) const DATA_FILESZ: usize = 16;

/// Bytes the data segment occupies in memory.
pub(crate) const DATA_MEMSZ: u64 = PAGE_SIZE + 32;

/// The pattern the data segment's file contents carry, so the check can
/// prove the bytes landed at the address the program headers asked for and
/// not merely somewhere.
pub(crate) const DATA_MARK: [u8; DATA_FILESZ] = *b"stage7-loaded-ok";

/// How the image should be built, for the checks that want a broken one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    /// Valid: read-execute text, read-write data with a `.bss` tail.
    Good,
    /// `ET_DYN`, which needs relocations nothing applies yet.
    PositionIndependent,
    /// A machine this kernel is not.
    ForeignMachine,
    /// Text and data linked into the same page, so the page would have to be
    /// writable and executable at once.
    WriteExecute,
}

/// Build one.
pub(crate) fn build(class: Class, machine: u16, shape: Shape) -> Vec<u8> {
    build_with(class, machine, shape, &[])
}

/// Build one carrying `code` at its entry point.
///
/// The payload is what makes the image a *program* rather than a shape: the
/// loader checks use an empty one, and the check that actually enters ring 3
/// passes the architecture's own machine code.
pub(crate) fn build_with(class: Class, machine: u16, shape: Shape, code: &[u8]) -> Vec<u8> {
    let machine = match shape {
        // 0x3E is x86-64 and 0xB7 is AArch64; whichever this is not.
        Shape::ForeignMachine if machine == 0x003E => 0x00B7,
        Shape::ForeignMachine => 0x003E,
        _ => machine,
    };
    let data_vaddr = match shape {
        // Land the writable segment inside the executable one's last page.
        Shape::WriteExecute => BASE + 0x200,
        _ => DATA_VADDR,
    };

    let header = class.header_size();
    let phentsize = class.phdr_size();
    let phoff = header;
    let text_filesz = phoff + phentsize * 2 + 0x400;
    let data_offset = usize::try_from(PAGE_SIZE).unwrap_or(4096);

    let mut file = vec![0_u8; data_offset + DATA_FILESZ];

    write_header(&mut file, class, machine, shape, phoff);
    // The text segment covers the file from zero, which is what puts the
    // program headers at a real address and so gives `AT_PHDR` something to
    // point at -- exactly as a linked binary does.
    write_phdr(
        &mut file,
        class,
        phoff,
        &Phdr {
            flags: PF_R | PF_X,
            offset: 0,
            vaddr: BASE,
            filesz: text_filesz as u64,
            memsz: text_filesz as u64,
        },
    );
    write_phdr(
        &mut file,
        class,
        phoff + phentsize,
        &Phdr {
            flags: PF_R | PF_W,
            offset: data_offset as u64,
            vaddr: data_vaddr,
            filesz: DATA_FILESZ as u64,
            memsz: DATA_MEMSZ,
        },
    );
    if let Some(slot) = file.get_mut(data_offset..data_offset + DATA_FILESZ) {
        slot.copy_from_slice(&DATA_MARK);
    }

    // The entry point is `ENTRY - BASE` into the text segment, and that
    // segment starts at file offset zero, so the payload goes at the same
    // offset in the file.
    let at = usize::try_from(ENTRY - BASE).unwrap_or(0);
    if let Some(slot) = file.get_mut(at..at + code.len()) {
        slot.copy_from_slice(code);
    }
    file
}

/// One program header, before it is written out.
struct Phdr {
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
}

/// `e_ident` and the fields after it, at the offsets the class puts them.
fn write_header(file: &mut [u8], class: Class, machine: u16, shape: Shape, phoff: usize) {
    put(file, 0, &[0x7F, b'E', b'L', b'F']);
    let (class_byte, header_size, phentsize) = match class {
        Class::Elf32 => (1_u8, 52_u16, 32_u16),
        Class::Elf64 => (2, 64, 56),
    };
    put(file, 4, &[class_byte, 1, 1, 0]);

    // ET_DYN is 3 and ET_EXEC is 2. The loader refuses the former because
    // nothing applies relocations yet.
    let elf_type: u16 = if shape == Shape::PositionIndependent {
        3
    } else {
        2
    };
    put(file, 16, &elf_type.to_le_bytes());
    put(file, 18, &machine.to_le_bytes());
    put(file, 20, &1_u32.to_le_bytes());

    match class {
        Class::Elf32 => {
            put(file, 24, &(ENTRY as u32).to_le_bytes());
            put(file, 28, &(phoff as u32).to_le_bytes());
            put(file, 40, &header_size.to_le_bytes());
            put(file, 42, &phentsize.to_le_bytes());
            put(file, 44, &2_u16.to_le_bytes());
        }
        Class::Elf64 => {
            put(file, 24, &ENTRY.to_le_bytes());
            put(file, 32, &(phoff as u64).to_le_bytes());
            put(file, 52, &header_size.to_le_bytes());
            put(file, 54, &phentsize.to_le_bytes());
            put(file, 56, &2_u16.to_le_bytes());
        }
    }
}

/// One program header. The two classes reorder the fields as well as widening
/// them, which is the thing this and `libs/elf` have to agree about.
fn write_phdr(file: &mut [u8], class: Class, at: usize, phdr: &Phdr) {
    match class {
        Class::Elf32 => {
            put(file, at, &PT_LOAD.to_le_bytes());
            put(file, at + 4, &(phdr.offset as u32).to_le_bytes());
            put(file, at + 8, &(phdr.vaddr as u32).to_le_bytes());
            put(file, at + 12, &(phdr.vaddr as u32).to_le_bytes());
            put(file, at + 16, &(phdr.filesz as u32).to_le_bytes());
            put(file, at + 20, &(phdr.memsz as u32).to_le_bytes());
            put(file, at + 24, &phdr.flags.to_le_bytes());
            put(file, at + 28, &(PAGE_SIZE as u32).to_le_bytes());
        }
        Class::Elf64 => {
            put(file, at, &PT_LOAD.to_le_bytes());
            put(file, at + 4, &phdr.flags.to_le_bytes());
            put(file, at + 8, &phdr.offset.to_le_bytes());
            put(file, at + 16, &phdr.vaddr.to_le_bytes());
            put(file, at + 24, &phdr.vaddr.to_le_bytes());
            put(file, at + 32, &phdr.filesz.to_le_bytes());
            put(file, at + 40, &phdr.memsz.to_le_bytes());
            put(file, at + 48, &PAGE_SIZE.to_le_bytes());
        }
    }
}

/// Write bytes at an offset, doing nothing if they would not fit.
///
/// Silent rather than panicking: the buffer is sized by this module and a
/// short write would fail the check that reads the image back, which is a
/// better place to notice than a panic in a helper.
fn put(file: &mut [u8], at: usize, bytes: &[u8]) {
    if let Some(slot) = file.get_mut(at..at + bytes.len()) {
        slot.copy_from_slice(bytes);
    }
}
