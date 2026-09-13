//! Tests for the ELF-to-PE32 converter.
//!
//! As with the FAT32 writer, the failure this guards against is not a crash.
//! It is firmware that loads the image, relocates the wrong words, and jumps
//! into something that is not the loader — with nothing on the serial port,
//! because the loader's console is one of the things that did not survive. So
//! the output is read back by a reader written from the PE specification rather
//! than from the converter, and every assertion is about bytes.

use ferrix_elf::{
    DT_NULL, DT_REL, DT_RELA, DT_RELASZ, DT_RELENT, DT_RELSZ, ET_DYN, ET_EXEC, PF_R, PF_W, PF_X,
    PT_DYNAMIC, PT_LOAD, R_ARM_RELATIVE, Relocation,
};

use super::*;

// ---------------------------------------------------------------------------
// A synthetic loader
// ---------------------------------------------------------------------------

/// Write a little-endian word into `bytes` at `at`.
fn put32(bytes: &mut [u8], at: u32, value: u32) {
    let at = at as usize;
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Read a little-endian word out of `bytes` at `at`.
fn get32(bytes: &[u8], at: u32) -> u32 {
    let at = at as usize;
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// Read a little-endian half-word out of `bytes` at `at`.
fn get16(bytes: &[u8], at: u32) -> u16 {
    let at = at as usize;
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}

/// One program header of a synthetic image.
struct Program {
    kind: u32,
    flags: u32,
    vaddr: u32,
    filesz: u32,
    memsz: u32,
}

/// An ELF32 static PIE shaped like the one `boot/linker/armv7a.ld` produces.
///
/// Every segment sits at the same file offset as its address, so a test can
/// patch the file by address and the arithmetic stays out of the way.
struct Pie {
    elf_type: u16,
    machine: u16,
    entry: u32,
    programs: Vec<Program>,
    /// The file, which is also the image as linked.
    bytes: Vec<u8>,
}

impl Pie {
    /// Text at 0x1000, read-only data at 0x2000 holding two pointers and the
    /// relocation table, and data at 0x3000 holding a third pointer, the
    /// dynamic table and a page of `.bss`.
    fn loader_shaped() -> Self {
        let mut bytes = vec![0u8; 0x3040];

        // Text: `mov r0, r0`, a few times over.
        for at in (0x1000..0x1100).step_by(4) {
            put32(&mut bytes, at, 0xE1A0_0000);
        }

        // Read-only data: a pointer into the text and one into the data, the
        // way a vtable or a string table holds them.
        put32(&mut bytes, 0x2000, 0x1004);
        put32(&mut bytes, 0x2010, 0x3000);

        // The REL table: three R_ARM_RELATIVE entries, addend in place.
        for (index, site) in [0x2000u32, 0x2010, 0x3008].into_iter().enumerate() {
            let at = 0x2100 + 8 * index as u32;
            put32(&mut bytes, at, site);
            put32(&mut bytes, at + 4, R_ARM_RELATIVE);
        }

        // Data: one pointer back into the read-only data.
        put32(&mut bytes, 0x3008, 0x2000);

        // The dynamic table, inside the data segment where lld puts it.
        let tags = [
            (DT_REL, 0x2100),
            (DT_RELSZ, 24),
            (DT_RELENT, 8),
            (DT_NULL, 0),
        ];
        for (index, (tag, value)) in tags.into_iter().enumerate() {
            let at = 0x3020 + 8 * index as u32;
            put32(&mut bytes, at, tag as u32);
            put32(&mut bytes, at + 4, value);
        }

        Pie {
            elf_type: ET_DYN,
            machine: EM_ARM,
            entry: 0x1000,
            programs: vec![
                Program {
                    kind: PT_LOAD,
                    flags: PF_R | PF_X,
                    vaddr: 0x1000,
                    filesz: 0x100,
                    memsz: 0x100,
                },
                Program {
                    kind: PT_LOAD,
                    flags: PF_R,
                    vaddr: 0x2000,
                    filesz: 0x118,
                    memsz: 0x118,
                },
                Program {
                    kind: PT_LOAD,
                    flags: PF_R | PF_W,
                    vaddr: 0x3000,
                    filesz: 0x40,
                    memsz: 0x1000,
                },
                Program {
                    kind: PT_DYNAMIC,
                    flags: PF_R | PF_W,
                    vaddr: 0x3020,
                    filesz: 0x20,
                    memsz: 0x20,
                },
            ],
            bytes,
        }
    }

    /// The file.
    fn build(&self) -> Vec<u8> {
        let mut bytes = self.bytes.clone();
        bytes[0..4].copy_from_slice(&ferrix_elf::ELF_MAGIC);
        bytes[4] = 1; // ELFCLASS32
        bytes[5] = 1; // ELFDATA2LSB
        bytes[6] = 1; // EI_VERSION
        bytes[16..18].copy_from_slice(&self.elf_type.to_le_bytes());
        bytes[18..20].copy_from_slice(&self.machine.to_le_bytes());
        put32(&mut bytes, 20, 1);
        put32(&mut bytes, 24, self.entry);
        put32(&mut bytes, 28, 52); // e_phoff
        put32(&mut bytes, 36, 0x0500_0200);
        bytes[40..42].copy_from_slice(&52u16.to_le_bytes());
        bytes[42..44].copy_from_slice(&32u16.to_le_bytes());
        bytes[44..46].copy_from_slice(&(self.programs.len() as u16).to_le_bytes());

        for (index, program) in self.programs.iter().enumerate() {
            let base = 52 + 32 * index as u32;
            put32(&mut bytes, base, program.kind);
            put32(&mut bytes, base + 4, program.vaddr); // p_offset
            put32(&mut bytes, base + 8, program.vaddr);
            put32(&mut bytes, base + 12, program.vaddr);
            put32(&mut bytes, base + 16, program.filesz);
            put32(&mut bytes, base + 20, program.memsz);
            put32(&mut bytes, base + 24, program.flags);
            put32(&mut bytes, base + 28, 0x1000);
        }
        bytes
    }
}

// ---------------------------------------------------------------------------
// An independent reader
// ---------------------------------------------------------------------------

/// One section header, as read back.
#[derive(Clone, Copy, Debug)]
struct Header {
    name: [u8; 8],
    virtual_size: u32,
    rva: u32,
    raw_size: u32,
    raw_pointer: u32,
    characteristics: u32,
}

/// A PE image, read from the offsets the specification gives rather than from
/// anything the converter computed.
struct Pe<'a> {
    bytes: &'a [u8],
    coff: u32,
    optional: u32,
    sections: Vec<Header>,
}

impl<'a> Pe<'a> {
    fn read(bytes: &'a [u8]) -> Self {
        assert_eq!(&bytes[0..2], b"MZ", "DOS signature");
        let pe = get32(bytes, 0x3C);
        assert_eq!(
            &bytes[pe as usize..pe as usize + 4],
            b"PE\0\0",
            "PE signature"
        );
        let coff = pe + 4;
        let optional = coff + 20;
        let count = u32::from(get16(bytes, coff + 2));
        let table = optional + u32::from(get16(bytes, coff + 16));

        let sections = (0..count)
            .map(|index| {
                let at = table + 40 * index;
                Header {
                    name: bytes[at as usize..at as usize + 8].try_into().unwrap(),
                    virtual_size: get32(bytes, at + 8),
                    rva: get32(bytes, at + 12),
                    raw_size: get32(bytes, at + 16),
                    raw_pointer: get32(bytes, at + 20),
                    characteristics: get32(bytes, at + 36),
                }
            })
            .collect();

        Pe {
            bytes,
            coff,
            optional,
            sections,
        }
    }

    fn machine(&self) -> u16 {
        get16(self.bytes, self.coff)
    }

    fn optional16(&self, offset: u32) -> u16 {
        get16(self.bytes, self.optional + offset)
    }

    fn optional32(&self, offset: u32) -> u32 {
        get32(self.bytes, self.optional + offset)
    }

    fn entry(&self) -> u32 {
        self.optional32(16)
    }

    fn size_of_image(&self) -> u32 {
        self.optional32(56)
    }

    fn size_of_headers(&self) -> u32 {
        self.optional32(60)
    }

    fn section(&self, name: &str) -> Header {
        let mut padded = [0u8; 8];
        padded[..name.len()].copy_from_slice(name.as_bytes());
        *self
            .sections
            .iter()
            .find(|header| header.name == padded)
            .unwrap_or_else(|| panic!("no {name} section"))
    }

    /// The image as firmware would lay it out in memory before relocating
    /// it: each section's raw bytes at its RVA, the rest zero.
    fn loaded(&self) -> Vec<u8> {
        let mut image = vec![0u8; self.size_of_image() as usize];
        for header in &self.sections {
            let copied = header.raw_size.min(header.virtual_size) as usize;
            let from = header.raw_pointer as usize;
            let to = header.rva as usize;
            image[to..to + copied].copy_from_slice(&self.bytes[from..from + copied]);
        }
        image
    }

    /// Every block of the base relocation directory, as (page, size, entries).
    fn relocation_blocks(&self) -> Vec<(u32, u32, Vec<u16>)> {
        let loaded = self.loaded();
        let directory = 96 + 8 * 5;
        let rva = self.optional32(directory);
        let size = self.optional32(directory + 4);

        let mut blocks = Vec::new();
        let mut at = rva;
        while at < rva + size {
            let page = get32(&loaded, at);
            let block = get32(&loaded, at + 4);
            assert!(
                block >= 8 && block.is_multiple_of(4),
                "a block of {block} bytes"
            );
            let entries = (8..block)
                .step_by(2)
                .map(|offset| get16(&loaded, at + offset))
                .collect();
            blocks.push((page, block, entries));
            at += block;
        }
        blocks
    }

    /// Every word the relocation directory tells firmware to patch.
    fn relocation_sites(&self) -> Vec<u32> {
        let mut sites = Vec::new();
        for (page, _, entries) in self.relocation_blocks() {
            for entry in entries {
                match entry >> 12 {
                    0 => {}
                    3 => sites.push(page + u32::from(entry & 0xFFF)),
                    other => panic!("base relocation type {other}"),
                }
            }
        }
        sites
    }
}

fn converted() -> Vec<u8> {
    convert(&Pie::loader_shaped().build()).unwrap()
}

// ---------------------------------------------------------------------------
// The image
// ---------------------------------------------------------------------------

#[test]
fn a_static_pie_becomes_a_pe32_efi_application() {
    let image = converted();
    let pe = Pe::read(&image);

    assert_eq!(
        pe.machine(),
        0x01C2,
        "ARMTHUMB_MIXED, the UEFI spec's ARM32"
    );
    assert_eq!(pe.optional16(0), 0x010B, "PE32, not PE32+");
    assert_eq!(pe.optional16(68), 10, "EFI_APPLICATION");
    assert_eq!(
        pe.optional32(28),
        0,
        "image base zero: an RVA is a link address"
    );
    assert_eq!(pe.optional32(32), 0x1000, "section alignment");
    assert_eq!(pe.entry(), 0x1000);
    assert_eq!(pe.sections.len(), 4, "text, rdata, data, reloc");
}

#[test]
fn each_segment_is_a_section_at_its_link_address_holding_its_bytes() {
    let pie = Pie::loader_shaped();
    let image = convert(&pie.build()).unwrap();
    let pe = Pe::read(&image);

    for (name, program) in [".text", ".rdata", ".data"].iter().zip(&pie.programs) {
        let header = pe.section(name);
        assert_eq!(header.rva, program.vaddr, "{name}");
        assert_eq!(header.virtual_size, program.memsz, "{name}");
        let raw = &image[header.raw_pointer as usize..][..program.filesz as usize];
        let linked = &pie.bytes[program.vaddr as usize..][..program.filesz as usize];
        assert_eq!(raw, linked, "{name}: the section's bytes are the segment's");
    }
}

#[test]
fn no_section_is_both_writable_and_executable() {
    let image = converted();
    let pe = Pe::read(&image);
    const EXECUTE: u32 = 0x2000_0000;
    const WRITE: u32 = 0x8000_0000;
    const DISCARDABLE: u32 = 0x0200_0000;

    for header in &pe.sections {
        assert!(
            header.characteristics & (EXECUTE | WRITE) != (EXECUTE | WRITE),
            "{:?}",
            header.name
        );
    }
    assert_ne!(pe.section(".text").characteristics & EXECUTE, 0);
    assert_eq!(pe.section(".rdata").characteristics & (EXECUTE | WRITE), 0);
    assert_ne!(pe.section(".data").characteristics & WRITE, 0);
    assert_ne!(
        pe.section(".reloc").characteristics & DISCARDABLE,
        0,
        "nothing reads the relocations once they are applied"
    );
}

#[test]
fn the_headers_fit_below_the_first_section_and_the_image_covers_the_bss() {
    let image = converted();
    let pe = Pe::read(&image);

    assert!(pe.size_of_headers() <= 0x1000);
    assert_eq!(pe.size_of_headers() % 0x200, 0);
    assert_eq!(pe.size_of_image() % 0x1000, 0);

    let data = pe.section(".data");
    assert!(
        data.virtual_size > data.raw_size,
        "the .bss is the part of .data firmware zero-fills"
    );
    assert!(pe.size_of_image() >= data.rva + data.virtual_size);
    for header in &pe.sections {
        assert_eq!(header.raw_pointer % 0x200, 0, "{:?}", header.name);
        assert_eq!(header.rva % 0x1000, 0, "{:?}", header.name);
    }
}

#[test]
fn every_relative_relocation_becomes_a_base_relocation() {
    let image = converted();
    let pe = Pe::read(&image);
    assert_eq!(pe.relocation_sites(), [0x2000, 0x2010, 0x3008]);

    // Two sites on the first page and one on the second: the second block
    // needs a padding entry to keep its size a multiple of four.
    let blocks = pe.relocation_blocks();
    assert_eq!(blocks.len(), 2);
    assert_eq!((blocks[0].0, blocks[0].1), (0x2000, 12));
    assert_eq!((blocks[1].0, blocks[1].1), (0x3000, 12));
    assert_eq!(blocks[1].2[1], 0, "padding is an ABSOLUTE entry");
}

/// The claim the whole conversion rests on, checked end to end: relocating
/// the loaded PE the way firmware does gives every word the value the ELF's
/// own relocations say it should have.
#[test]
fn relocating_the_image_gives_the_pointers_the_elf_meant() {
    let pie = Pie::loader_shaped().build();
    let elf = Elf::parse(&pie).unwrap();
    let pe_bytes = convert(&pie).unwrap();
    let pe = Pe::read(&pe_bytes);

    let delta = 0x4230_0000u32;
    let mut loaded = pe.loaded();
    for site in pe.relocation_sites() {
        let word = get32(&loaded, site);
        put32(&mut loaded, site, word.wrapping_add(delta));
    }

    let relocations: Vec<Relocation> = elf
        .relocations()
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect();
    assert_eq!(relocations.len(), 3);
    for relocation in relocations {
        let site = relocation.offset as u32;
        let expected = relocation.value(u64::from(delta), u64::from(get32(&pie, site)));
        assert_eq!(
            u64::from(get32(&loaded, site)),
            expected,
            "the word at {site:#x}"
        );
    }
    assert_eq!(get32(&loaded, 0x2000), 0x4230_1004, "a pointer into .text");
}

#[test]
fn an_explicit_addend_is_written_where_firmware_reads_it() {
    let mut pie = Pie::loader_shaped();
    // Swap the REL table for a RELA one with one entry, whose site holds a
    // placeholder: the addend lives only in the table.
    put32(&mut pie.bytes, 0x2100, 0x3008);
    put32(&mut pie.bytes, 0x2104, R_ARM_RELATIVE);
    put32(&mut pie.bytes, 0x2108, 0x2040);
    put32(&mut pie.bytes, 0x3008, 0);
    for (index, (tag, value)) in [
        (DT_RELA, 0x2100),
        (DT_RELASZ, 12),
        (DT_NULL, 0),
        (DT_NULL, 0),
    ]
    .into_iter()
    .enumerate()
    {
        let at = 0x3020 + 8 * index as u32;
        put32(&mut pie.bytes, at, tag as u32);
        put32(&mut pie.bytes, at + 4, value);
    }

    let image = convert(&pie.build()).unwrap();
    let pe = Pe::read(&image);
    assert_eq!(pe.relocation_sites(), [0x3008]);
    assert_eq!(get32(&pe.loaded(), 0x3008), 0x2040);
}

#[test]
fn the_conversion_is_reproducible() {
    assert_eq!(
        converted(),
        converted(),
        "two builds of one loader must be the same bytes, as the image they go into is"
    );
}

// ---------------------------------------------------------------------------
// What it refuses
// ---------------------------------------------------------------------------

/// The message a refused conversion carries.
fn refusal(pie: &Pie) -> String {
    convert(&pie.build()).unwrap_err().to_string()
}

#[test]
fn refuses_an_image_that_is_not_32_bit_arm() {
    let mut pie = Pie::loader_shaped();
    pie.machine = ferrix_elf::EM_AARCH64;
    assert!(refusal(&pie).contains("32-bit Arm"));

    // And a 64-bit image, however well formed.
    let mut elf64 = vec![0u8; 64];
    elf64[0..4].copy_from_slice(&ferrix_elf::ELF_MAGIC);
    elf64[4] = 2;
    elf64[5] = 1;
    elf64[16..18].copy_from_slice(&ET_DYN.to_le_bytes());
    elf64[18..20].copy_from_slice(&EM_ARM.to_le_bytes());
    assert!(convert(&elf64).is_err());
}

#[test]
fn refuses_an_image_that_is_not_position_independent() {
    let mut pie = Pie::loader_shaped();
    pie.elf_type = ET_EXEC;
    assert!(refusal(&pie).contains("position independent"));
}

#[test]
fn refuses_a_writable_executable_segment() {
    let mut pie = Pie::loader_shaped();
    pie.programs[0].flags = PF_R | PF_W | PF_X;
    assert!(refusal(&pie).contains("writable and executable"));
}

#[test]
fn refuses_a_segment_that_would_overlap_the_headers() {
    let mut pie = Pie::loader_shaped();
    pie.programs[0].vaddr = 0;
    assert!(refusal(&pie).contains("above the headers"));
}

#[test]
fn refuses_a_gap_between_segments() {
    let mut pie = Pie::loader_shaped();
    pie.programs[2].vaddr = 0x4000;
    pie.programs[3].vaddr = 0x4020;
    pie.bytes.resize(0x4040, 0);
    assert!(
        refusal(&pie).contains("where 0x3000 was expected"),
        "sections must be adjacent, as a PE image's are"
    );
}

#[test]
fn refuses_a_relocation_into_the_bss() {
    let mut pie = Pie::loader_shaped();
    put32(&mut pie.bytes, 0x2110, 0x3800); // the third entry's site
    assert!(refusal(&pie).contains("outside every segment's file contents"));
}

#[test]
fn refuses_a_relocation_it_cannot_express() {
    let mut pie = Pie::loader_shaped();
    put32(&mut pie.bytes, 0x2104, 2); // R_ARM_ABS32, which needs a symbol
    assert!(refusal(&pie).contains("relocation type 2"));
}

#[test]
fn a_switch_block_with_no_relocation_inside_and_under_a_page_passes() {
    assert_eq!(
        switch_violation(0x2000, 0x20B0, &[0x1FFC, 0x20B0, 0x4000]),
        None
    );
}

#[test]
fn a_switch_block_with_a_relocation_inside_is_refused() {
    let inside = switch_violation(0x2000, 0x20B0, &[0x2040]).expect("refused");
    assert!(inside.contains("PC-relative"), "{inside}");

    // A word that starts before the block but reaches into it counts too.
    assert!(switch_violation(0x2000, 0x20B0, &[0x1FFE]).is_some());
}

#[test]
fn a_switch_block_larger_than_a_page_or_empty_is_refused() {
    assert!(switch_violation(0x2000, 0x3001, &[]).is_some());
    assert_eq!(
        switch_violation(0x2000, 0x3000, &[]),
        None,
        "exactly one page fits"
    );
    assert!(switch_violation(0x2000, 0x2000, &[]).is_some());
}

#[test]
fn a_switch_block_off_an_instruction_boundary_is_refused() {
    assert!(switch_violation(0x2002, 0x20B2, &[]).is_some());
}
