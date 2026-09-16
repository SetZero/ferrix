//! Turning the ARMv7-A loader into a UEFI application.
//!
//! The other two loaders come out of rustc as PE32+ images, because their
//! targets are UEFI targets. rustc has no 32-bit Arm UEFI target, so this
//! loader is linked as an ELF32 static PIE by `boot/linker/armv7a.ld` and
//! converted here: each loadable segment becomes a section at the same
//! address, and each `R_ARM_RELATIVE` relocation becomes a PE base relocation
//! at the same address. That is what systemd's `elf2efi.py` does for its own
//! 32-bit Arm stub, for the same reason.
//!
//! The image base is zero, so a relative virtual address *is* a link-time
//! address and nothing is rebased: firmware loads the image wherever it likes
//! and adds that address to every word the relocation table names. For a
//! `REL` image that word already holds the link-time value, because that is
//! where `REL` keeps its addend — which is also exactly what a PE base
//! relocation expects to find there.
//!
//! # What firmware checks
//!
//! U-Boot and EDK2 both read the machine type, the optional header's magic and
//! subsystem, the section table and the base relocation directory, and nothing
//! else of consequence. The machine is `ARMTHUMB_MIXED`, the value the UEFI
//! specification gives 32-bit Arm; the code inside is Arm-state, and an even
//! entry point is how an interworking call knows to stay there.
//!
//! # What this refuses
//!
//! An image that breaks the link script's contract: a segment that is both
//! writable and executable, a first segment that would overlap the headers,
//! a gap between segments, a relocation of any other type or outside the
//! file-backed bytes. Each is a build failure with a message, because each
//! would otherwise be a boot that stops with nothing on the serial port.

use ferrix_elf::{Addend, Class, EM_ARM, Elf};

use crate::{Error, Result};

/// `IMAGE_FILE_MACHINE_ARMTHUMB_MIXED`: what the UEFI specification calls a
/// 32-bit Arm image, and what Linux's own 32-bit Arm EFI stub declares.
const MACHINE_ARMTHUMB_MIXED: u16 = 0x01C2;

/// `IMAGE_NT_OPTIONAL_HDR32_MAGIC`: a PE32 optional header, not PE32+.
const PE32_MAGIC: u16 = 0x010B;

/// `IMAGE_SUBSYSTEM_EFI_APPLICATION`.
const SUBSYSTEM_EFI_APPLICATION: u16 = 10;

/// Sections start on page boundaries in memory, which is what lets firmware
/// give each its own permissions.
const SECTION_ALIGNMENT: u32 = 0x1000;

/// Sections start on sector boundaries in the file.
const FILE_ALIGNMENT: u32 = 0x200;

/// Where the PE signature goes, and so the DOS header's `e_lfanew`: straight
/// after the 64-byte DOS header, with no stub program between.
const PE_OFFSET: u32 = 0x40;

/// Bytes in the COFF file header.
const COFF_HEADER_SIZE: u32 = 20;

/// Data directories in the optional header: all sixteen, zeroed except one.
const DATA_DIRECTORIES: u32 = 16;

/// Bytes in a PE32 optional header with sixteen data directories.
const OPTIONAL_HEADER_SIZE: u32 = 96 + 8 * DATA_DIRECTORIES;

/// Bytes in one section header.
const SECTION_HEADER_SIZE: u32 = 40;

/// Index of the base relocation table among the data directories.
const BASE_RELOCATION_DIRECTORY: u32 = 5;

/// Base relocation type: padding, which firmware skips.
const REL_BASED_ABSOLUTE: u16 = 0;
/// Base relocation type: add the load address to a 32-bit word.
const REL_BASED_HIGHLOW: u16 = 3;

/// File characteristics: an executable image, 32-bit, and stripped of the
/// line numbers, symbols and debug information a COFF file could carry.
const FILE_CHARACTERISTICS: u16 = 0x0002 | 0x0004 | 0x0008 | 0x0100 | 0x0200;

/// `DYNAMIC_BASE`, because the image can be loaded anywhere, and `NX_COMPAT`,
/// because no section is both writable and executable, so firmware that
/// enforces section permissions may.
const DLL_CHARACTERISTICS: u16 = 0x0040 | 0x0100;

/// Section characteristics.
const SCN_CNT_CODE: u32 = 0x0000_0020;
const SCN_CNT_INITIALIZED_DATA: u32 = 0x0000_0040;
const SCN_MEM_DISCARDABLE: u32 = 0x0200_0000;
const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const SCN_MEM_READ: u32 = 0x4000_0000;
const SCN_MEM_WRITE: u32 = 0x8000_0000;

/// An error with the context every conversion error shares.
fn fail(reason: impl std::fmt::Display) -> Error {
    Error::new(format!(
        "cannot convert the ARMv7-A loader to PE32: {reason}"
    ))
}

/// One section of the image being built.
#[derive(Debug)]
struct Section {
    /// The name, NUL padded. Nothing reads it; it is for a human with
    /// `objdump`.
    name: [u8; 8],
    /// Where it goes in memory, relative to wherever firmware loads us.
    rva: u32,
    /// Bytes it occupies in memory, which exceeds `data` by its `.bss`.
    virtual_size: u32,
    /// Its contents in the file.
    data: Vec<u8>,
    /// What it contains, and what it may be used for.
    characteristics: u32,
}

impl Section {
    /// One past its last byte in memory.
    const fn end(&self) -> u32 {
        self.rva + self.virtual_size
    }

    /// True if it is executable code.
    const fn is_code(&self) -> bool {
        self.characteristics & SCN_CNT_CODE != 0
    }
}

/// Convert the ARMv7-A loader's ELF image into a PE32 UEFI application.
pub(crate) fn convert(elf_image: &[u8]) -> Result<Vec<u8>> {
    let elf = Elf::parse(elf_image).map_err(fail)?;
    if elf.class() != Class::Elf32 || elf.machine() != EM_ARM {
        return Err(fail("the loader is not a 32-bit Arm image"));
    }
    if !elf.is_pie() {
        return Err(fail(
            "the loader is not position independent, so firmware could not relocate it",
        ));
    }
    elf.validate_segments().map_err(fail)?;

    let mut sections = segments(&elf)?;
    let sites = relocation_sites(&elf, &mut sections)?;
    let entry = entry_point(&elf, &sections)?;

    let table = base_relocations(&sites);
    let last = sections.last().map_or(SECTION_ALIGNMENT, Section::end);
    let rva = align(last, SECTION_ALIGNMENT)?;
    let size = narrow(table.len() as u64, "the relocation table")?;
    sections.push(Section {
        name: *b".reloc\0\0",
        rva,
        virtual_size: size,
        data: table,
        characteristics: SCN_CNT_INITIALIZED_DATA | SCN_MEM_READ | SCN_MEM_DISCARDABLE,
    });

    layout(&sections, entry, (rva, size))
}

/// Every loadable segment as a section, checked against the link script.
fn segments(elf: &Elf<'_>) -> Result<Vec<Section>> {
    let mut sections = Vec::new();
    // The first page belongs to the headers; every later segment belongs on
    // the page after the one before it.
    let mut next = SECTION_ALIGNMENT;

    for segment in elf.loadable() {
        let rva = narrow(segment.vaddr, "a segment's address")?;
        let virtual_size = narrow(segment.memsz, "a segment's size")?;
        if rva != next {
            return Err(fail(format!(
                "a segment at {rva:#x} where {next:#x} was expected. The first must start \
                 at {SECTION_ALIGNMENT:#x}, above the headers, and each must follow the \
                 last on the next page; see boot/linker/armv7a.ld"
            )));
        }

        let (name, characteristics) = match (segment.is_executable(), segment.is_writable()) {
            (true, true) => {
                return Err(fail(format!(
                    "the segment at {rva:#x} is writable and executable"
                )));
            }
            (true, false) => (
                *b".text\0\0\0",
                SCN_CNT_CODE | SCN_MEM_EXECUTE | SCN_MEM_READ,
            ),
            (false, false) => (*b".rdata\0\0", SCN_CNT_INITIALIZED_DATA | SCN_MEM_READ),
            (false, true) => (
                *b".data\0\0\0",
                SCN_CNT_INITIALIZED_DATA | SCN_MEM_READ | SCN_MEM_WRITE,
            ),
        };

        let end = rva
            .checked_add(virtual_size)
            .ok_or_else(|| fail("a segment wraps the 32-bit address space"))?;
        next = align(end, SECTION_ALIGNMENT)?;
        sections.push(Section {
            name,
            rva,
            virtual_size,
            data: segment.data(elf.image()).map_err(fail)?.to_vec(),
            characteristics,
        });
    }

    if sections.is_empty() {
        return Err(fail("the loader loads nothing"));
    }
    Ok(sections)
}

/// The size a copy of the switch has to fit: one page of the loader's own.
const SWITCH_PAGE: u64 = 4096;

/// Check the loader's switch block can be copied to a trampoline page.
///
/// `boot/src/arch/armv7a.rs` runs its switch either where it is or from a copy
/// on a page below the split. A copy runs only if nothing in the block names
/// an absolute address — which, in a static PIE, is exactly a relocation site
/// inside it — and if it fits the one page the loader allocates. Both are
/// properties of what the compiler and linker emitted, not of the source, so
/// they are checked on every build rather than trusted.
pub(crate) fn check_switch(elf_image: &[u8]) -> Result<()> {
    let elf = Elf::parse(elf_image).map_err(fail)?;
    let symbols = elf
        .symbols()
        .ok_or_else(|| fail("the loader has no symbol table to find its switch block in"))?;
    let find = |name: &[u8]| {
        symbols
            .clone()
            .find(|symbol| symbol.name == name)
            .map(|symbol| symbol.value)
    };
    let (Some(start), Some(end)) = (find(b"ferrix_switch_start"), find(b"ferrix_switch_end"))
    else {
        return Err(fail(
            "the loader has no ferrix_switch_start/ferrix_switch_end block",
        ));
    };
    let mut sites = Vec::new();
    for relocation in elf.relocations().map_err(fail)? {
        sites.push(relocation.map_err(fail)?.offset);
    }
    if let Some(reason) = switch_violation(start, end, &sites) {
        return Err(fail(reason));
    }
    println!(
        "  switch block {} bytes at {start:#x}, no relocation inside",
        end - start
    );
    Ok(())
}

/// What is wrong with a switch block at `start..end` in an image whose
/// relocated words are at `sites`, if anything.
fn switch_violation(start: u64, end: u64, sites: &[u64]) -> Option<String> {
    if end <= start {
        return Some(format!(
            "the switch block {start:#x}..{end:#x} is empty or reversed"
        ));
    }
    if !start.is_multiple_of(4) {
        return Some(format!(
            "the switch block starts at {start:#x}, not on an instruction boundary"
        ));
    }
    if end - start > SWITCH_PAGE {
        return Some(format!(
            "the switch block is {} bytes, more than the one page a trampoline holds",
            end - start
        ));
    }
    // A relocated word is four bytes; any of them inside the block is an
    // absolute address a copy would carry to the wrong place.
    sites
        .iter()
        .find(|&&site| site < end && site + 4 > start)
        .map(|site| {
            format!(
                "the switch block {start:#x}..{end:#x} holds a relocation at {site:#x}, \
                 so a copy of it would not run: every reference in it must be PC-relative"
            )
        })
}

/// The address of every word firmware has to relocate, sorted.
///
/// A `REL` entry's addend is already in the word, which is the only form PE
/// has. A `RELA` entry's is in the table, so it is written into the word here,
/// where firmware will find it — the linker left a placeholder there.
fn relocation_sites(elf: &Elf<'_>, sections: &mut [Section]) -> Result<Vec<u32>> {
    let mut sites = Vec::new();

    for relocation in elf.relocations().map_err(fail)? {
        let relocation = relocation.map_err(fail)?;
        let site = narrow(relocation.offset, "a relocation's address")?;
        let section = sections
            .iter_mut()
            .find(|section| {
                site >= section.rva
                    && u64::from(site) + 4 <= u64::from(section.rva) + section.data.len() as u64
            })
            .ok_or_else(|| {
                fail(format!(
                    "a relocation at {site:#x} is outside every segment's file contents"
                ))
            })?;

        if let Addend::Explicit(addend) = relocation.addend {
            let at = (site - section.rva) as usize;
            let word = section
                .data
                .get_mut(at..at + 4)
                .ok_or_else(|| fail(format!("a relocation at {site:#x} runs off its segment")))?;
            word.copy_from_slice(&(addend as u32).to_le_bytes());
        }
        sites.push(site);
    }

    sites.sort_unstable();
    sites.dedup();
    Ok(sites)
}

/// The entry point, checked to be in executable code.
fn entry_point(elf: &Elf<'_>, sections: &[Section]) -> Result<u32> {
    let entry = narrow(elf.entry(), "the entry point")?;
    let in_code = sections
        .iter()
        .any(|section| section.is_code() && entry >= section.rva && entry < section.end());
    if !in_code {
        return Err(fail(format!(
            "the entry point {entry:#x} is not in executable code"
        )));
    }
    Ok(entry)
}

/// The contents of the `.reloc` section.
///
/// One block per 4 KiB page that has anything to patch: the page's address,
/// the block's size, and a 16-bit entry per word — the type in the top four
/// bits and the offset within the page in the other twelve. A block is padded
/// to a multiple of four bytes with an entry of the type firmware skips.
fn base_relocations(sites: &[u32]) -> Vec<u8> {
    let mut table = Vec::new();
    let page_of = |site: u32| site & !(SECTION_ALIGNMENT - 1);

    let mut rest = sites;
    while let Some(&first) = rest.first() {
        let page = page_of(first);
        let count = rest
            .iter()
            .take_while(|site| page_of(**site) == page)
            .count();
        let (block, remainder) = rest.split_at_checked(count).unwrap_or((rest, &[]));
        rest = remainder;

        let padded = count + count % 2;
        table.extend_from_slice(&page.to_le_bytes());
        table.extend_from_slice(&(8 + 2 * padded as u32).to_le_bytes());
        for site in block {
            let offset = (site & (SECTION_ALIGNMENT - 1)) as u16;
            table.extend_from_slice(&((REL_BASED_HIGHLOW << 12) | offset).to_le_bytes());
        }
        if count % 2 == 1 {
            table.extend_from_slice(&REL_BASED_ABSOLUTE.to_le_bytes());
        }
    }

    // An image with nothing to relocate still gets a directory, holding one
    // empty block. Firmware reads a missing directory as "this image cannot be
    // moved", and would then insist on loading it at address zero.
    if table.is_empty() {
        table.extend_from_slice(&0u32.to_le_bytes());
        table.extend_from_slice(&8u32.to_le_bytes());
    }
    table
}

/// Lay the finished image out: headers, then each section's bytes.
fn layout(sections: &[Section], entry: u32, relocations: (u32, u32)) -> Result<Vec<u8>> {
    let count = u16::try_from(sections.len()).map_err(|_| fail("too many sections"))?;
    let headers = PE_OFFSET
        + 4
        + COFF_HEADER_SIZE
        + OPTIONAL_HEADER_SIZE
        + u32::from(count) * SECTION_HEADER_SIZE;
    let size_of_headers = align(headers, FILE_ALIGNMENT)?;
    let first = sections.first().map_or(0, |section| section.rva);
    if size_of_headers > first {
        return Err(fail("the headers do not fit below the first section"));
    }

    // Where each section's bytes go in the file, and how many of them.
    let mut placements = Vec::new();
    let mut offset = size_of_headers;
    for section in sections {
        let raw = align(
            narrow(section.data.len() as u64, "a section")?,
            FILE_ALIGNMENT,
        )?;
        placements.push(if raw == 0 { (0, 0) } else { (offset, raw) });
        offset = offset
            .checked_add(raw)
            .ok_or_else(|| fail("the image is larger than 4 GiB"))?;
    }

    let raw_size = |code: bool| -> u32 {
        sections
            .iter()
            .zip(&placements)
            .filter(|(section, _)| section.is_code() == code)
            .map(|(_, (_, raw))| raw)
            .sum()
    };
    let base_of = |code: bool| {
        sections
            .iter()
            .find(|section| section.is_code() == code)
            .map_or(0, |section| section.rva)
    };
    let totals = Totals {
        code: raw_size(true),
        data: raw_size(false),
        base_of_code: base_of(true),
        base_of_data: base_of(false),
        size_of_image: align(
            sections.last().map_or(first, Section::end),
            SECTION_ALIGNMENT,
        )?,
        size_of_headers,
    };

    let mut out = Writer::default();

    // The DOS header: `MZ`, and `e_lfanew` at 0x3C pointing at the PE header.
    // Nothing else in it is read by anything that runs a UEFI application.
    out.bytes(b"MZ");
    out.pad_to(0x3C);
    out.u32(PE_OFFSET);
    out.pad_to(PE_OFFSET);
    out.bytes(b"PE\0\0");

    // The COFF file header. No timestamp: two builds of the same loader are
    // the same bytes, as the FAT image they go into is.
    out.u16(MACHINE_ARMTHUMB_MIXED);
    out.u16(count);
    out.u32(0); // TimeDateStamp
    out.u32(0); // PointerToSymbolTable
    out.u32(0); // NumberOfSymbols
    out.u16(OPTIONAL_HEADER_SIZE as u16);
    out.u16(FILE_CHARACTERISTICS);

    optional_header(&mut out, &totals, entry, relocations);

    // The section table.
    for (section, &(pointer, raw)) in sections.iter().zip(&placements) {
        out.bytes(&section.name);
        out.u32(section.virtual_size);
        out.u32(section.rva);
        out.u32(raw);
        out.u32(pointer);
        out.u32(0); // PointerToRelocations
        out.u32(0); // PointerToLinenumbers
        out.u16(0); // NumberOfRelocations
        out.u16(0); // NumberOfLinenumbers
        out.u32(section.characteristics);
    }
    out.pad_to(size_of_headers);

    for (section, &(pointer, raw)) in sections.iter().zip(&placements) {
        if raw == 0 {
            continue;
        }
        out.pad_to(pointer);
        out.bytes(&section.data);
        out.pad_to(pointer + raw);
    }

    Ok(out.finish())
}

/// What the optional header states about the image as a whole.
#[derive(Clone, Copy, Debug)]
struct Totals {
    /// File bytes of executable sections.
    code: u32,
    /// File bytes of every other section.
    data: u32,
    /// Where the first executable section starts.
    base_of_code: u32,
    /// Where the first other section starts.
    base_of_data: u32,
    /// Bytes of memory the loaded image occupies, `.bss` and all.
    size_of_image: u32,
    /// File bytes before the first section's.
    size_of_headers: u32,
}

/// The PE32 optional header: where to enter, how large the image is, what
/// kind of program it is, and where its relocations are.
fn optional_header(out: &mut Writer, totals: &Totals, entry: u32, relocations: (u32, u32)) {
    out.u16(PE32_MAGIC);
    out.u16(0); // MajorLinkerVersion, MinorLinkerVersion
    out.u32(totals.code); // SizeOfCode
    out.u32(totals.data); // SizeOfInitializedData
    out.u32(0); // SizeOfUninitializedData: .bss is the tail of .data
    out.u32(entry);
    out.u32(totals.base_of_code);
    out.u32(totals.base_of_data);
    out.u32(0); // ImageBase: an RVA is a link-time address
    out.u32(SECTION_ALIGNMENT);
    out.u32(FILE_ALIGNMENT);
    for _ in 0..6 {
        out.u16(0); // operating system, image and subsystem versions
    }
    out.u32(0); // Win32VersionValue
    out.u32(totals.size_of_image);
    out.u32(totals.size_of_headers);
    out.u32(0); // CheckSum: firmware does not verify one
    out.u16(SUBSYSTEM_EFI_APPLICATION);
    out.u16(DLL_CHARACTERISTICS);
    for _ in 0..4 {
        out.u32(0); // stack and heap sizes, which firmware ignores
    }
    out.u32(0); // LoaderFlags
    out.u32(DATA_DIRECTORIES);
    for index in 0..DATA_DIRECTORIES {
        let (rva, size) = if index == BASE_RELOCATION_DIRECTORY {
            relocations
        } else {
            (0, 0)
        };
        out.u32(rva);
        out.u32(size);
    }
}

/// A file being written front to back.
#[derive(Debug, Default)]
struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn bytes(&mut self, data: &[u8]) {
        self.bytes.extend_from_slice(data);
    }

    fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    /// Zero-fill up to file offset `to`.
    ///
    /// Never truncates. `layout` sizes everything before it writes any of it,
    /// so a write that has run past where the next thing belongs is a bug in
    /// that arithmetic, and the test that reads the headers back is where it
    /// would show.
    fn pad_to(&mut self, to: u32) {
        let to = to as usize;
        if self.bytes.len() < to {
            self.bytes.resize(to, 0);
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// `value` as a 32-bit quantity, or an error naming `what` did not fit.
fn narrow(value: u64, what: &str) -> Result<u32> {
    u32::try_from(value).map_err(|_| fail(format!("{what} ({value:#x}) does not fit 32 bits")))
}

/// `value` rounded up to a multiple of `to`.
fn align(value: u32, to: u32) -> Result<u32> {
    value
        .checked_next_multiple_of(to)
        .ok_or_else(|| fail("the image is larger than 4 GiB"))
}

#[cfg(test)]
mod tests;
