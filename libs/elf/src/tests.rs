//! Tests for the ELF64 reader.
//!
//! Images are built by hand rather than checked in as binaries, so that a test
//! failure names the field that is wrong instead of pointing at an opaque blob,
//! and so that the malformed cases can be produced by mutating one field of a
//! known-good image. The same builder backs `fuzz/fuzz_targets/elf_parse.rs`.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;

/// A synthetic ELF64 image under construction.
pub(crate) struct Builder {
    elf_type: u16,
    machine: u16,
    entry: u64,
    segments: Vec<Segment>,
    contents: Vec<Vec<u8>>,
}

impl Builder {
    fn new() -> Self {
        Builder {
            elf_type: ET_EXEC,
            machine: EM_X86_64,
            entry: 0xFFFF_FFFF_8000_1000,
            segments: Vec::new(),
            contents: Vec::new(),
        }
    }

    fn machine(mut self, machine: u16) -> Self {
        self.machine = machine;
        self
    }

    fn elf_type(mut self, elf_type: u16) -> Self {
        self.elf_type = elf_type;
        self
    }

    /// Add a segment whose file contents are `data`, followed by
    /// `zero_fill` bytes of `.bss`.
    fn segment(mut self, kind: u32, flags: u32, vaddr: u64, data: Vec<u8>, zero_fill: u64) -> Self {
        self.segments.push(Segment {
            kind,
            flags,
            offset: 0, // patched in `build`
            vaddr,
            filesz: data.len() as u64,
            memsz: data.len() as u64 + zero_fill,
            align: 0x1000,
        });
        self.contents.push(data);
        self
    }

    fn build(self) -> Vec<u8> {
        let phnum = self.segments.len();
        let phoff = EHDR_SIZE;
        let mut image = vec![0u8; phoff + phnum * PHDR_SIZE];

        image[0..4].copy_from_slice(&ELF_MAGIC);
        image[4] = ELFCLASS64;
        image[5] = ELFDATA2LSB;
        image[6] = 1; // EI_VERSION
        image[16..18].copy_from_slice(&self.elf_type.to_le_bytes());
        image[18..20].copy_from_slice(&self.machine.to_le_bytes());
        image[20..24].copy_from_slice(&1u32.to_le_bytes()); // e_version
        image[24..32].copy_from_slice(&self.entry.to_le_bytes());
        image[32..40].copy_from_slice(&(phoff as u64).to_le_bytes());
        image[52..54].copy_from_slice(&(EHDR_SIZE as u16).to_le_bytes());
        image[54..56].copy_from_slice(&(PHDR_SIZE as u16).to_le_bytes());
        image[56..58].copy_from_slice(&(phnum as u16).to_le_bytes());

        for (index, (mut segment, data)) in self.segments.into_iter().zip(self.contents).enumerate()
        {
            segment.offset = image.len() as u64;
            image.extend_from_slice(&data);

            let base = phoff + index * PHDR_SIZE;
            image[base..base + 4].copy_from_slice(&segment.kind.to_le_bytes());
            image[base + 4..base + 8].copy_from_slice(&segment.flags.to_le_bytes());
            image[base + 8..base + 16].copy_from_slice(&segment.offset.to_le_bytes());
            image[base + 16..base + 24].copy_from_slice(&segment.vaddr.to_le_bytes());
            image[base + 24..base + 32].copy_from_slice(&segment.vaddr.to_le_bytes()); // p_paddr
            image[base + 32..base + 40].copy_from_slice(&segment.filesz.to_le_bytes());
            image[base + 40..base + 48].copy_from_slice(&segment.memsz.to_le_bytes());
            image[base + 48..base + 56].copy_from_slice(&segment.align.to_le_bytes());
        }

        image
    }
}

/// A two-segment kernel-shaped image: read-execute text, read-write data with
/// a `.bss` tail.
fn kernel_image() -> Vec<u8> {
    Builder::new()
        .segment(
            PT_LOAD,
            PF_R | PF_X,
            0xFFFF_FFFF_8000_0000,
            vec![0x90; 0x1000],
            0,
        )
        .segment(
            PT_LOAD,
            PF_R | PF_W,
            0xFFFF_FFFF_8000_1000,
            vec![0xAA; 0x800],
            0x800,
        )
        .build()
}

#[test]
fn parses_a_well_formed_image() {
    let image = kernel_image();
    let elf = Elf::parse(&image).unwrap();

    assert_eq!(elf.machine(), EM_X86_64);
    assert_eq!(elf.entry(), 0xFFFF_FFFF_8000_1000);
    assert!(!elf.is_pie(), "ET_EXEC is not position independent");
    elf.check_machine(EM_X86_64).unwrap();
    elf.validate_segments().unwrap();
    assert_eq!(elf.segments().count(), 2);
    assert_eq!(elf.loadable().count(), 2);
}

#[test]
fn decodes_segment_permissions() {
    let image = kernel_image();
    let elf = Elf::parse(&image).unwrap();
    let loadable: Vec<Segment> = elf.loadable().collect();

    assert!(loadable[0].is_readable() && loadable[0].is_executable());
    assert!(!loadable[0].is_writable(), "text must not be writable");
    assert!(loadable[1].is_readable() && loadable[1].is_writable());
    assert!(!loadable[1].is_executable(), "data must not be executable");
}

#[test]
fn load_span_covers_bss_and_rounds_to_pages() {
    let image = kernel_image();
    let elf = Elf::parse(&image).unwrap();
    let (low, high) = elf.load_span(0x1000).unwrap();

    assert_eq!(low, 0xFFFF_FFFF_8000_0000);
    // Second segment is 0x800 of file plus 0x800 of .bss, so it ends exactly on
    // a page and the span must include all of it.
    assert_eq!(high, 0xFFFF_FFFF_8000_2000);
}

#[test]
fn load_span_rounds_a_partial_page_up() {
    let image = Builder::new()
        .segment(PT_LOAD, PF_R, 0x1000, vec![0; 1], 0)
        .build();
    let elf = Elf::parse(&image).unwrap();
    assert_eq!(elf.load_span(0x1000), Some((0x1000, 0x2000)));
}

#[test]
fn segment_data_is_the_bytes_the_builder_wrote() {
    let image = kernel_image();
    let elf = Elf::parse(&image).unwrap();
    let text = elf.loadable().next().unwrap();

    assert_eq!(text.data(&image).unwrap(), &[0x90u8; 0x1000][..]);
}

#[test]
fn rejects_a_truncated_image() {
    let image = kernel_image();
    for length in [0, 1, 4, 15, 16, EHDR_SIZE - 1] {
        assert!(
            Elf::parse(&image[..length]).is_err(),
            "a {length}-byte image must not parse"
        );
    }
}

#[test]
fn rejects_wrong_magic_class_and_endianness() {
    let good = kernel_image();

    let mut image = good.clone();
    image[1] = b'X';
    assert_eq!(Elf::parse(&image).unwrap_err(), ElfError::BadMagic);

    let mut image = good.clone();
    image[4] = 1; // ELFCLASS32
    assert_eq!(Elf::parse(&image).unwrap_err(), ElfError::NotElf64);

    let mut image = good;
    image[5] = 2; // ELFDATA2MSB
    assert_eq!(Elf::parse(&image).unwrap_err(), ElfError::NotLittleEndian);
}

#[test]
fn rejects_the_wrong_architecture() {
    let image = Builder::new()
        .machine(EM_AARCH64)
        .segment(PT_LOAD, PF_R, 0x1000, vec![0; 16], 0)
        .build();
    let elf = Elf::parse(&image).unwrap();

    assert_eq!(
        elf.check_machine(EM_X86_64).unwrap_err(),
        ElfError::BadMachine(EM_AARCH64),
        "an AArch64 kernel must not load on x86-64"
    );
    elf.check_machine(EM_AARCH64).unwrap();
}

#[test]
fn rejects_an_object_that_is_neither_exec_nor_dyn() {
    let image = Builder::new()
        .elf_type(1) // ET_REL
        .segment(PT_LOAD, PF_R, 0x1000, vec![0; 16], 0)
        .build();
    let elf = Elf::parse(&image).unwrap();
    assert_eq!(
        elf.check_machine(EM_X86_64).unwrap_err(),
        ElfError::BadType(1)
    );
}

#[test]
fn rejects_a_program_header_table_past_the_end() {
    let mut image = kernel_image();
    // Claim 4096 program headers; the table cannot be there.
    image[56..58].copy_from_slice(&4096u16.to_le_bytes());
    assert_eq!(Elf::parse(&image).unwrap_err(), ElfError::HeaderOutOfBounds);
}

#[test]
fn rejects_a_program_header_offset_that_overflows() {
    let mut image = kernel_image();
    image[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(Elf::parse(&image).unwrap_err(), ElfError::HeaderOutOfBounds);
}

#[test]
fn rejects_an_undersized_program_header_entry() {
    let mut image = kernel_image();
    image[54..56].copy_from_slice(&16u16.to_le_bytes());
    assert_eq!(Elf::parse(&image).unwrap_err(), ElfError::HeaderOutOfBounds);
}

#[test]
fn rejects_segment_contents_past_the_end_of_the_image() {
    let mut image = kernel_image();
    // Inflate the first segment's p_filesz past the end of the image, and its
    // p_memsz with it -- otherwise `memsz < filesz` is the first thing wrong
    // and the malformed-size check fires before the bounds check we mean to
    // exercise here.
    let base = EHDR_SIZE + 32;
    image[base..base + 8].copy_from_slice(&0x10_0000u64.to_le_bytes());
    image[base + 8..base + 16].copy_from_slice(&0x10_0000u64.to_le_bytes());
    let elf = Elf::parse(&image).unwrap();
    assert_eq!(
        elf.validate_segments().unwrap_err(),
        ElfError::SegmentOutOfBounds
    );
}

#[test]
fn rejects_memsz_smaller_than_filesz() {
    let mut image = kernel_image();
    let base = EHDR_SIZE + 40; // p_memsz of the first segment
    image[base..base + 8].copy_from_slice(&1u64.to_le_bytes());
    let elf = Elf::parse(&image).unwrap();
    assert_eq!(
        elf.validate_segments().unwrap_err(),
        ElfError::SegmentMalformed,
        "a segment cannot occupy less memory than it has contents"
    );
}

#[test]
fn rejects_a_segment_whose_address_range_wraps() {
    let mut image = kernel_image();
    let base = EHDR_SIZE + 16; // p_vaddr of the first segment
    image[base..base + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    let elf = Elf::parse(&image).unwrap();
    assert_eq!(
        elf.validate_segments().unwrap_err(),
        ElfError::SegmentMalformed
    );
    assert_eq!(elf.load_span(0x1000), None, "a wrapping span has no answer");
}

#[test]
fn an_image_with_no_loadable_segments_has_no_span() {
    let image = Builder::new()
        .segment(PT_NOTE, 0, 0x1000, vec![0; 8], 0)
        .build();
    let elf = Elf::parse(&image).unwrap();
    assert_eq!(elf.load_span(0x1000), None);
    assert_eq!(elf.loadable().count(), 0);
    elf.validate_segments().unwrap();
}

#[test]
fn maps_a_link_time_address_back_to_file_bytes() {
    let image = kernel_image();
    let elf = Elf::parse(&image).unwrap();

    assert_eq!(
        elf.vaddr_to_bytes(0xFFFF_FFFF_8000_0000, 4),
        Some(&[0x90u8; 4][..])
    );
    assert_eq!(
        elf.vaddr_to_bytes(0xFFFF_FFFF_8000_1000, 4),
        Some(&[0xAAu8; 4][..])
    );
    // Inside the .bss tail: no file bytes exist for it.
    assert_eq!(elf.vaddr_to_bytes(0xFFFF_FFFF_8000_1900, 4), None);
    assert_eq!(elf.vaddr_to_bytes(0x1234, 4), None, "unmapped address");
}

#[test]
fn an_image_without_a_dynamic_segment_needs_no_relocation() {
    let image = kernel_image();
    let elf = Elf::parse(&image).unwrap();
    assert_eq!(elf.relocations().unwrap().count(), 0);
}

#[test]
fn reads_relative_relocations_from_a_static_pie() {
    // One PT_LOAD holding both the relocation table and the dynamic table, so
    // that DT_RELA resolves through `vaddr_to_bytes`.
    const BASE: u64 = 0x1000;
    let mut payload = Vec::new();

    // Two R_X86_64_RELATIVE entries at the start of the segment.
    for (offset, addend) in [(0x2000u64, 0x2100i64), (0x2008, 0x2200)] {
        payload.extend_from_slice(&offset.to_le_bytes());
        payload.extend_from_slice(&((R_X86_64_RELATIVE as u64).to_le_bytes()));
        payload.extend_from_slice(&addend.to_le_bytes());
    }
    let rela_size = payload.len() as u64;
    let dyn_offset = payload.len() as u64;

    for (tag, value) in [
        (DT_RELA, BASE),
        (DT_RELASZ, rela_size),
        (DT_RELAENT, RELA_SIZE as u64),
        (DT_NULL, 0),
    ] {
        payload.extend_from_slice(&(tag as u64).to_le_bytes());
        payload.extend_from_slice(&value.to_le_bytes());
    }

    let image = Builder::new()
        .elf_type(ET_DYN)
        .segment(PT_LOAD, PF_R | PF_W, BASE, payload, 0)
        .segment(PT_DYNAMIC, PF_R, BASE + dyn_offset, Vec::new(), 0)
        .build();

    // Point the PT_DYNAMIC header at the dynamic table inside the PT_LOAD.
    let mut image = image;
    let load_offset = u64::from_le_bytes(image[EHDR_SIZE + 8..EHDR_SIZE + 16].try_into().unwrap());
    let dynamic = EHDR_SIZE + PHDR_SIZE;
    image[dynamic + 8..dynamic + 16].copy_from_slice(&(load_offset + dyn_offset).to_le_bytes());
    image[dynamic + 32..dynamic + 40].copy_from_slice(&(4u64 * DYN_SIZE as u64).to_le_bytes());
    image[dynamic + 40..dynamic + 48].copy_from_slice(&(4u64 * DYN_SIZE as u64).to_le_bytes());

    let elf = Elf::parse(&image).unwrap();
    assert!(elf.is_pie());

    let relocations: Vec<Relocation> = elf.relocations().unwrap().map(Result::unwrap).collect();

    assert_eq!(relocations.len(), 2);
    assert_eq!(
        relocations[0],
        Relocation {
            offset: 0x2000,
            addend: 0x2100
        }
    );
    assert_eq!(
        relocations[1],
        Relocation {
            offset: 0x2008,
            addend: 0x2200
        }
    );

    // With a load bias, both the patch site and the value shift by it.
    let bias = 0x40_0000u64;
    assert_eq!(relocations[0].target(bias), 0x2000 + bias);
    assert_eq!(relocations[0].value(bias), 0x2100 + bias);
}

#[test]
fn relocation_helpers_wrap_rather_than_panic() {
    let relocation = Relocation {
        offset: u64::MAX,
        addend: -1,
    };
    assert_eq!(relocation.target(1), 0, "a wrapping target is not a panic");
    assert_eq!(relocation.value(1), 0);
}

/// Exercise every accessor on one image, requiring only that none of them
/// panics. Pulled out of the loop below so the nesting stays readable.
fn poke_at_everything(image: &[u8]) {
    let Ok(elf) = Elf::parse(image) else {
        return;
    };
    let _ = elf.validate_segments();
    let _ = elf.load_span(0x1000);
    let _ = elf.check_machine(EM_X86_64);
    let _ = elf.vaddr_to_bytes(0xFFFF_FFFF_8000_0000, 8);

    for segment in elf.segments() {
        let _ = segment.data(image);
        let _ = segment.vaddr_end();
    }

    if let Ok(relocations) = elf.relocations() {
        for relocation in relocations.take(64) {
            let _ = relocation;
        }
    }
}

#[test]
fn parsing_never_panics_on_arbitrary_bytes() {
    // The cheap deterministic version of the fuzz target: walk a known-good
    // image byte by byte, corrupting each in turn, and require that every
    // accessor either answers or errors. The fuzzer searches further; this runs
    // on every commit.
    let good = kernel_image();
    for index in 0..good.len().min(512) {
        for patch in [0x00u8, 0x01, 0x7F, 0xFF] {
            let mut image = good.clone();
            image[index] = patch;
            poke_at_everything(&image);
        }
    }
}
