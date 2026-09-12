//! Tests for the device tree reader.
//!
//! Blobs are assembled by a builder rather than checked in as binaries, so that
//! a failure names the field that is wrong instead of pointing at an opaque
//! blob, and so that every malformed case can be produced by mutating one field
//! of a known-good tree.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// A synthetic device tree under construction.
struct Builder {
    structs: Vec<u8>,
    strings: Vec<u8>,
    reservations: Vec<(u64, u64)>,
    version: u32,
    last_comp_version: u32,
}

impl Builder {
    fn new() -> Self {
        Builder {
            structs: Vec::new(),
            strings: Vec::new(),
            reservations: Vec::new(),
            version: 17,
            last_comp_version: 16,
        }
    }

    fn token(&mut self, token: u32) {
        self.structs.extend_from_slice(&token.to_be_bytes());
    }

    /// Append raw bytes to the structure block, padding to four.
    fn padded(&mut self, bytes: &[u8]) {
        self.structs.extend_from_slice(bytes);
        while !self.structs.len().is_multiple_of(4) {
            self.structs.push(0);
        }
    }

    /// Add `name` to the strings block and return its offset.
    fn intern(&mut self, name: &str) -> u32 {
        let offset = self.strings.len() as u32;
        self.strings.extend_from_slice(name.as_bytes());
        self.strings.push(0);
        offset
    }

    fn begin(&mut self, name: &str) {
        self.token(FDT_BEGIN_NODE);
        let mut bytes = Vec::from(name.as_bytes());
        bytes.push(0);
        self.padded(&bytes);
    }

    fn end(&mut self) {
        self.token(FDT_END_NODE);
    }

    fn prop(&mut self, name: &str, value: &[u8]) {
        let nameoff = self.intern(name);
        self.token(FDT_PROP);
        self.token(value.len() as u32);
        self.token(nameoff);
        self.padded(value);
    }

    fn prop_str(&mut self, name: &str, value: &str) {
        let mut bytes = Vec::from(value.as_bytes());
        bytes.push(0);
        self.prop(name, &bytes);
    }

    fn prop_strings(&mut self, name: &str, values: &[&str]) {
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }
        self.prop(name, &bytes);
    }

    fn prop_u32(&mut self, name: &str, value: u32) {
        self.prop(name, &value.to_be_bytes());
    }

    fn prop_cells(&mut self, name: &str, cells: &[u32]) {
        let mut bytes = Vec::new();
        for cell in cells {
            bytes.extend_from_slice(&cell.to_be_bytes());
        }
        self.prop(name, &bytes);
    }

    fn reserve(&mut self, address: u64, size: u64) {
        self.reservations.push((address, size));
    }

    fn build(&self) -> Vec<u8> {
        self.assemble(true)
    }

    fn build_without_end(&self) -> Vec<u8> {
        self.assemble(false)
    }

    fn assemble(&self, terminate: bool) -> Vec<u8> {
        let mut structs = self.structs.clone();
        if terminate {
            structs.extend_from_slice(&FDT_END.to_be_bytes());
        }

        let mut blob = vec![0u8; HEADER_SIZE];
        let off_mem_rsvmap = blob.len() as u32;
        for &(address, size) in &self.reservations {
            blob.extend_from_slice(&address.to_be_bytes());
            blob.extend_from_slice(&size.to_be_bytes());
        }
        blob.extend_from_slice(&[0u8; 16]);

        let off_dt_struct = blob.len() as u32;
        blob.extend_from_slice(&structs);
        let off_dt_strings = blob.len() as u32;
        blob.extend_from_slice(&self.strings);
        let totalsize = blob.len() as u32;

        let header = [
            FDT_MAGIC,
            totalsize,
            off_dt_struct,
            off_dt_strings,
            off_mem_rsvmap,
            self.version,
            self.last_comp_version,
            0,
            self.strings.len() as u32,
            structs.len() as u32,
        ];
        for (index, field) in header.iter().enumerate() {
            blob[index * 4..index * 4 + 4].copy_from_slice(&field.to_be_bytes());
        }
        blob
    }
}

/// Overwrite one big-endian header word.
fn patch(blob: &mut [u8], offset: usize, value: u32) {
    blob[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

/// Header field offsets, so a patch names what it is corrupting.
const OFF_TOTALSIZE: usize = 4;
const OFF_STRUCT: usize = 8;
const OFF_STRINGS: usize = 12;
const OFF_RSVMAP: usize = 16;
const OFF_VERSION: usize = 20;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A tree shaped like QEMU's `virt` machine, trimmed to what the kernel reads.
fn virt() -> Vec<u8> {
    let mut builder = Builder::new();
    builder.reserve(0x4000_0000, 0x1_0000);
    builder.reserve(0x4001_0000, 0x2000);

    builder.begin("");
    builder.prop_u32("#address-cells", 2);
    builder.prop_u32("#size-cells", 2);
    builder.prop_str("model", "linux,dummy-virt");

    builder.begin("memory@40000000");
    builder.prop_str("device_type", "memory");
    builder.prop_cells("reg", &[0, 0x4000_0000, 0, 0x2000_0000]);
    builder.end();

    builder.begin("pl011@9000000");
    builder.prop_strings("compatible", &["arm,pl011", "arm,primecell"]);
    builder.prop_cells("reg", &[0, 0x0900_0000, 0, 0x1000]);
    builder.prop_cells("interrupts", &[0, 1, 4]);
    builder.end();

    builder.begin("intc@8000000");
    builder.prop_strings("compatible", &["arm,gic-v3"]);
    builder.prop_cells(
        "reg",
        &[0, 0x0800_0000, 0, 0x1_0000, 0, 0x080a_0000, 0, 0x00f6_0000],
    );
    builder.end();

    builder.begin("timer");
    builder.prop_strings("compatible", &["arm,armv8-timer"]);
    builder.prop_cells(
        "interrupts",
        &[1, 13, 0x108, 1, 14, 0x108, 1, 11, 0x108, 1, 10, 0x108],
    );
    builder.end();

    builder.begin("chosen");
    builder.prop_str("stdout-path", "/pl011@9000000:115200n8");
    builder.prop_str("bootargs", "console=ttyAMA0 root=/dev/vda");
    builder.end();

    builder.begin("aliases");
    builder.prop_str("serial0", "/pl011@9000000");
    builder.end();

    builder.end();
    builder.build()
}

/// Parse a fixture that is expected to be well formed.
fn parse(blob: &[u8]) -> Fdt<'_> {
    match Fdt::parse(blob) {
        Ok(fdt) => fdt,
        Err(error) => panic!("fixture should parse, got {error}"),
    }
}

/// The error a blob is rejected with, or `None` if it parses.
///
/// `Fdt` is deliberately not `PartialEq` — comparing two borrowed trees means
/// nothing — so a rejection is asserted on the error alone.
fn rejection(blob: &[u8]) -> Option<FdtError> {
    Fdt::parse(blob).err()
}

// ---------------------------------------------------------------------------
// Header and blocks
// ---------------------------------------------------------------------------

#[test]
fn header_fields_survive_a_round_trip() {
    let blob = virt();
    let fdt = parse(&blob);
    let header = *fdt.header();
    assert_eq!(
        header.totalsize as usize,
        blob.len(),
        "totalsize should cover the whole assembled blob"
    );
    assert_eq!(
        header.version, SUPPORTED_VERSION,
        "the builder emits the version the parser supports"
    );
    assert!(
        header.off_dt_struct.is_multiple_of(4),
        "the structure block must stay four-byte aligned"
    );
    assert_eq!(
        fdt.boot_cpu(),
        0,
        "the fixture boots on CPU 0, as boot_cpuid_phys says"
    );
}

#[test]
fn a_blob_longer_than_totalsize_is_truncated_not_read() {
    let mut blob = virt();
    let declared = blob.len();
    blob.extend_from_slice(&[0xAA; 64]);
    let fdt = parse(&blob);
    assert_eq!(
        fdt.blob().len(),
        declared,
        "bytes past totalsize must not become part of the tree"
    );
}

#[test]
fn bad_magic_is_rejected() {
    let mut blob = virt();
    patch(&mut blob, 0, 0xDEAD_BEEF);
    assert_eq!(
        rejection(&blob),
        Some(FdtError::BadMagic(0xDEAD_BEEF)),
        "a blob that is not a device tree must be named as such"
    );
}

#[test]
fn a_truncated_header_is_rejected() {
    let blob = virt();
    for length in [0usize, 1, 3, 4, 20, 39] {
        assert_eq!(
            rejection(&blob[..length]),
            Some(FdtError::TooShort),
            "a {length}-byte blob is shorter than a header"
        );
    }
}

#[test]
fn a_totalsize_beyond_the_slice_is_rejected() {
    let mut blob = virt();
    let beyond = blob.len() as u32 + 4;
    patch(&mut blob, OFF_TOTALSIZE, beyond);
    assert_eq!(
        rejection(&blob),
        Some(FdtError::BadTotalSize),
        "totalsize must never let the parser read past the slice"
    );
}

#[test]
fn a_totalsize_smaller_than_a_header_is_rejected() {
    let mut blob = virt();
    patch(&mut blob, OFF_TOTALSIZE, 8);
    assert_eq!(
        rejection(&blob),
        Some(FdtError::BadTotalSize),
        "a totalsize that excludes its own header is malformed"
    );
}

#[test]
fn block_offsets_outside_the_blob_are_rejected() {
    let mut struct_out = virt();
    // Rounded up to the alignment the format demands, so that the blob is
    // rejected for the offset and not for a stray low bit.
    let total = (struct_out.len() as u32).next_multiple_of(8);
    patch(&mut struct_out, OFF_STRUCT, total);
    assert_eq!(
        rejection(&struct_out),
        Some(FdtError::BlockOutOfBounds),
        "a structure block starting at the end of the blob cannot hold it"
    );

    let mut strings_out = virt();
    patch(&mut strings_out, OFF_STRINGS, total);
    assert_eq!(
        rejection(&strings_out),
        Some(FdtError::BlockOutOfBounds),
        "a strings block starting at the end of the blob cannot hold it"
    );
}

#[test]
fn a_misaligned_structure_block_is_rejected() {
    let mut blob = virt();
    let offset = u32::from_be_bytes([blob[8], blob[9], blob[10], blob[11]]);
    patch(&mut blob, OFF_STRUCT, offset + 1);
    assert_eq!(
        rejection(&blob),
        Some(FdtError::Misaligned),
        "the token stream is words, so its block must be word aligned"
    );
}

#[test]
fn an_unsupported_version_is_rejected() {
    let mut blob = virt();
    patch(&mut blob, OFF_VERSION, 16);
    assert_eq!(
        rejection(&blob),
        Some(FdtError::UnsupportedVersion(16)),
        "version 16 does not carry size_dt_struct, so it is refused"
    );
}

#[test]
fn a_reservation_block_outside_the_blob_is_rejected() {
    let mut blob = virt();
    // The last eight-byte-aligned position in the blob: a reservation block
    // there has room for at most half of its terminating pair.
    let last = (blob.len() as u32) & !7;
    patch(&mut blob, OFF_RSVMAP, last);
    assert_eq!(
        rejection(&blob),
        Some(FdtError::ReservationOutOfBounds),
        "even an empty reservation list needs sixteen bytes inside the blob"
    );
}

// ---------------------------------------------------------------------------
// Structure block validation
// ---------------------------------------------------------------------------

#[test]
fn a_property_running_past_the_block_is_rejected() {
    let mut builder = Builder::new();
    builder.begin("");
    let nameoff = builder.intern("reg");
    builder.token(FDT_PROP);
    builder.token(0x1000);
    builder.token(nameoff);
    builder.end();
    assert_eq!(
        rejection(&builder.build()),
        Some(FdtError::PropertyOutOfBounds),
        "a length longer than the block must not be believed"
    );
}

#[test]
fn a_property_name_off_the_end_of_the_strings_block_is_rejected() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_u32("#address-cells", 2);
    builder.end();
    let mut blob = builder.build();
    // Drop the NUL that terminates the last (and only) interned name.
    let total = blob.len();
    blob.truncate(total - 1);
    patch(&mut blob, OFF_TOTALSIZE, (total - 1) as u32);
    let size_of_strings = u32::from_be_bytes([blob[32], blob[33], blob[34], blob[35]]);
    patch(&mut blob, 32, size_of_strings - 1);
    assert_eq!(
        rejection(&blob),
        Some(FdtError::UnterminatedString),
        "a name with no terminator must not be read past its block"
    );
}

#[test]
fn an_unterminated_node_name_is_rejected() {
    let mut builder = Builder::new();
    builder.token(FDT_BEGIN_NODE);
    builder.structs.extend_from_slice(b"abcd");
    assert_eq!(
        rejection(&builder.build_without_end()),
        Some(FdtError::UnterminatedString),
        "a node name that runs to the end of the block is malformed"
    );
}

#[test]
fn an_unbalanced_end_token_is_rejected() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.end();
    builder.end();
    assert_eq!(
        rejection(&builder.build()),
        Some(FdtError::UnbalancedNode),
        "a node cannot end that never began"
    );
}

#[test]
fn a_node_left_open_is_rejected() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.begin("soc");
    builder.end();
    assert_eq!(
        rejection(&builder.build()),
        Some(FdtError::UnbalancedNode),
        "the tree must not end with the root still open"
    );
}

#[test]
fn a_missing_end_token_is_rejected() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.end();
    assert_eq!(
        rejection(&builder.build_without_end()),
        Some(FdtError::MissingEnd),
        "the token stream must end with FDT_END, not simply stop"
    );
}

#[test]
fn an_unknown_token_is_rejected() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.token(7);
    builder.end();
    assert_eq!(
        rejection(&builder.build()),
        Some(FdtError::BadToken(7)),
        "only the five defined tokens may appear"
    );
}

#[test]
fn nesting_deeper_than_the_limit_is_refused_not_recursed_into() {
    let mut builder = Builder::new();
    let levels = MAX_DEPTH + 1;
    for level in 0..levels {
        builder.begin(if level == 0 { "" } else { "child" });
    }
    for _ in 0..levels {
        builder.end();
    }
    assert_eq!(
        rejection(&builder.build()),
        Some(FdtError::DepthOverflow),
        "a tree that nests past MAX_DEPTH must be rejected, never recursed into"
    );
}

#[test]
fn nesting_exactly_at_the_limit_is_accepted() {
    let mut builder = Builder::new();
    for level in 0..MAX_DEPTH {
        builder.begin(if level == 0 { "" } else { "child" });
    }
    for _ in 0..MAX_DEPTH {
        builder.end();
    }
    let blob = builder.build();
    let fdt = parse(&blob);
    assert_eq!(
        fdt.nodes().count(),
        MAX_DEPTH,
        "every level of a tree at the depth limit should still be walked"
    );
}

#[test]
fn nop_tokens_are_skipped_everywhere() {
    let mut builder = Builder::new();
    builder.token(FDT_NOP);
    builder.begin("");
    builder.token(FDT_NOP);
    builder.prop_u32("#address-cells", 1);
    builder.token(FDT_NOP);
    builder.prop_u32("#size-cells", 1);
    builder.begin("uart@1000");
    builder.prop_cells("reg", &[0x1000, 0x100]);
    builder.end();
    builder.token(FDT_NOP);
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    let uart = fdt.find_node("/uart@1000").expect("uart should be found");
    assert_eq!(
        uart.reg().next(),
        Some(Region {
            address: 0x1000,
            size: 0x100
        }),
        "padding between properties must not disturb the reg decoding"
    );
}

// ---------------------------------------------------------------------------
// Walking
// ---------------------------------------------------------------------------

#[test]
fn every_node_is_walked_in_tree_order() {
    let blob = virt();
    let fdt = parse(&blob);
    let names: Vec<&str> = fdt.nodes().map(|node| node.name).collect();
    assert_eq!(
        names,
        vec![
            "",
            "memory@40000000",
            "pl011@9000000",
            "intc@8000000",
            "timer",
            "chosen",
            "aliases",
        ],
        "the walk should yield the root and then each child in blob order"
    );
    let depths: Vec<usize> = fdt.nodes().map(|node| node.depth).collect();
    assert_eq!(
        depths,
        vec![0, 1, 1, 1, 1, 1, 1],
        "the root is depth zero and its children depth one"
    );
}

#[test]
fn properties_of_a_node_are_read_back_whole() {
    let blob = virt();
    let fdt = parse(&blob);
    let uart = fdt.find_node("/pl011@9000000").expect("uart should exist");
    let names: Vec<&str> = uart.properties().map(|property| property.name).collect();
    assert_eq!(
        names,
        vec!["compatible", "reg", "interrupts"],
        "a node's property list must stop at its first subnode or its end"
    );
    let compatible: Vec<&str> = uart
        .property("compatible")
        .expect("compatible should exist")
        .strings()
        .collect();
    assert_eq!(
        compatible,
        vec!["arm,pl011", "arm,primecell"],
        "a compatible list is several NUL-terminated strings, not one"
    );
    assert!(
        uart.is_compatible("arm,primecell"),
        "a later entry of the compatible list still matches"
    );
    assert!(
        !uart.is_compatible("arm,pl0"),
        "matching a compatible entry must not be a prefix match"
    );
}

#[test]
fn a_path_component_without_a_unit_address_matches() {
    let blob = virt();
    let fdt = parse(&blob);
    let by_full = fdt.find_node("/memory@40000000");
    let by_base = fdt.find_node("/memory");
    assert!(by_full.is_some(), "the fully written path should be found");
    assert_eq!(
        by_base.map(|node| node.name),
        by_full.map(|node| node.name),
        "/memory should resolve to /memory@40000000"
    );
    assert!(
        fdt.find_node("/memory@1").is_none(),
        "a path with the wrong unit address must not match"
    );
    assert!(
        fdt.find_node("/nothing").is_none(),
        "a path that is not in the tree yields nothing"
    );
    assert_eq!(
        fdt.find_node("/").map(|node| node.depth),
        Some(0),
        "the bare root path names the root"
    );
}

#[test]
fn a_nested_path_matches_only_at_its_own_depth() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_u32("#address-cells", 1);
    builder.prop_u32("#size-cells", 1);
    builder.begin("soc");
    builder.begin("uart@9000000");
    builder.prop_cells("reg", &[0x900_0000, 0x1000]);
    builder.end();
    builder.end();
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    assert!(
        fdt.find_node("/soc/uart@9000000").is_some(),
        "a two-component path should reach a grandchild"
    );
    assert!(
        fdt.find_node("/uart@9000000").is_none(),
        "a child of /soc must not answer to a root-level path"
    );
}

// ---------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------

#[test]
fn memory_is_decoded_with_two_address_and_two_size_cells() {
    let blob = virt();
    let fdt = parse(&blob);
    let regions: Vec<Region> = fdt.memory().collect();
    assert_eq!(
        regions,
        vec![Region {
            address: 0x4000_0000,
            size: 0x2000_0000
        }],
        "the fixture has 512 MiB of RAM at 1 GiB"
    );
}

#[test]
fn memory_is_decoded_with_one_address_and_one_size_cell() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_u32("#address-cells", 1);
    builder.prop_u32("#size-cells", 1);
    builder.begin("memory@80000000");
    builder.prop_str("device_type", "memory");
    builder.prop_cells("reg", &[0x8000_0000, 0x1000_0000, 0x9000_0000, 0x1000]);
    builder.end();
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    let regions: Vec<Region> = fdt.memory().collect();
    assert_eq!(
        regions,
        vec![
            Region {
                address: 0x8000_0000,
                size: 0x1000_0000
            },
            Region {
                address: 0x9000_0000,
                size: 0x1000
            }
        ],
        "one-cell values must be read as single words, not halves of a pair"
    );
}

#[test]
fn several_memory_nodes_are_all_reported() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_u32("#address-cells", 2);
    builder.prop_u32("#size-cells", 2);
    builder.begin("memory@40000000");
    builder.prop_str("device_type", "memory");
    builder.prop_cells("reg", &[0, 0x4000_0000, 0, 0x1000_0000]);
    builder.end();
    builder.begin("memory@100000000");
    builder.prop_str("device_type", "memory");
    builder.prop_cells("reg", &[1, 0, 0, 0x2000_0000]);
    builder.end();
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    let addresses: Vec<u64> = fdt.memory().map(|region| region.address).collect();
    assert_eq!(
        addresses,
        vec![0x4000_0000, 0x1_0000_0000],
        "both memory nodes contribute, and the high word is the upper half"
    );
}

#[test]
fn a_child_overrides_the_cell_counts_of_its_parent() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_u32("#address-cells", 2);
    builder.prop_u32("#size-cells", 2);
    builder.begin("soc");
    builder.prop_u32("#address-cells", 1);
    builder.prop_u32("#size-cells", 1);
    builder.prop_cells("reg", &[0, 0x1000_0000, 0, 0x1000]);
    builder.begin("uart@9000000");
    builder.prop_cells("reg", &[0x900_0000, 0x1000]);
    builder.end();
    builder.end();
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);

    let soc = fdt.find_node("/soc").expect("soc should exist");
    assert_eq!(
        (soc.address_cells, soc.size_cells),
        (2, 2),
        "a node's own reg is decoded with the counts its parent declared"
    );
    assert_eq!(
        soc.reg().next(),
        Some(Region {
            address: 0x1000_0000,
            size: 0x1000
        }),
        "the soc node's reg uses the root's two-cell counts"
    );

    let uart = fdt
        .find_node("/soc/uart@9000000")
        .expect("uart should exist");
    assert_eq!(
        (uart.address_cells, uart.size_cells),
        (1, 1),
        "a child sees the counts its parent declared, not the root's"
    );
    assert_eq!(
        uart.reg().next(),
        Some(Region {
            address: 0x900_0000,
            size: 0x1000
        }),
        "the overridden one-cell counts must decode the child's reg"
    );
}

#[test]
fn cell_counts_are_inherited_by_a_node_that_declares_none() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_u32("#address-cells", 1);
    builder.prop_u32("#size-cells", 1);
    builder.begin("bus");
    builder.begin("uart@2000");
    builder.prop_cells("reg", &[0x2000, 0x40]);
    builder.end();
    builder.end();
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    let uart = fdt.find_node("/bus/uart@2000").expect("uart should exist");
    assert_eq!(
        uart.reg().next(),
        Some(Region {
            address: 0x2000,
            size: 0x40
        }),
        "a bus that declares no counts passes its parent's down unchanged"
    );
}

#[test]
fn a_value_too_wide_for_sixty_four_bits_is_refused() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_u32("#address-cells", 3);
    builder.prop_u32("#size-cells", 1);
    builder.begin("wide@0");
    builder.prop_cells("reg", &[1, 0, 0x1000, 0x10]);
    builder.end();
    builder.begin("narrow@0");
    builder.prop_cells("reg", &[0, 0, 0x1000, 0x10]);
    builder.end();
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    assert_eq!(
        fdt.find_node("/wide@0")
            .expect("wide should exist")
            .reg()
            .next(),
        None,
        "an address that does not fit a u64 must be refused, not truncated"
    );
    assert_eq!(
        fdt.find_node("/narrow@0")
            .expect("narrow should exist")
            .reg()
            .next(),
        Some(Region {
            address: 0x1000,
            size: 0x10
        }),
        "three cells with a zero high word still describe a 64-bit address"
    );
}

#[test]
fn zero_width_cells_yield_no_regions_instead_of_looping() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_u32("#address-cells", 0);
    builder.prop_u32("#size-cells", 0);
    builder.begin("odd");
    builder.prop_cells("reg", &[0, 0, 0, 0]);
    builder.end();
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    let node = fdt.find_node("/odd").expect("odd should exist");
    assert_eq!(
        node.reg().count(),
        0,
        "a pair of zero-width fields consumes nothing, so it yields nothing"
    );
}

#[test]
fn an_absurd_cell_count_is_ignored_in_favour_of_the_inherited_one() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_u32("#address-cells", 1);
    builder.prop_u32("#size-cells", 1);
    builder.begin("bus");
    builder.prop_u32("#address-cells", 0x4000_0000);
    builder.begin("uart@3000");
    builder.prop_cells("reg", &[0x3000, 0x20]);
    builder.end();
    builder.end();
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    let uart = fdt.find_node("/bus/uart@3000").expect("uart should exist");
    assert_eq!(
        uart.address_cells, 1,
        "a count wider than MAX_CELLS is ignored rather than honoured"
    );
    assert_eq!(
        uart.reg().next(),
        Some(Region {
            address: 0x3000,
            size: 0x20
        }),
        "the inherited counts still decode the reg"
    );
}

// ---------------------------------------------------------------------------
// The devices the kernel looks for
// ---------------------------------------------------------------------------

#[test]
fn the_console_is_resolved_through_an_absolute_stdout_path() {
    let blob = virt();
    let fdt = parse(&blob);
    assert_eq!(
        fdt.stdout_path(),
        Some("/pl011@9000000:115200n8"),
        "stdout-path is reported raw, options suffix and all"
    );
    let console = fdt.console().expect("the console should resolve");
    assert_eq!(
        console.compatible(),
        Some("arm,pl011"),
        "the console driver is chosen by the first compatible entry"
    );
    assert_eq!(
        console.reg().next(),
        Some(Region {
            address: 0x900_0000,
            size: 0x1000
        }),
        "the console's first reg range is the MMIO window to map"
    );
}

#[test]
fn the_console_is_resolved_through_an_alias() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_u32("#address-cells", 2);
    builder.prop_u32("#size-cells", 2);
    builder.begin("pl011@9000000");
    builder.prop_strings("compatible", &["arm,pl011"]);
    builder.prop_cells("reg", &[0, 0x900_0000, 0, 0x1000]);
    builder.end();
    builder.begin("chosen");
    builder.prop_str("stdout-path", "serial0:115200n8");
    builder.end();
    builder.begin("aliases");
    builder.prop_str("serial0", "/pl011@9000000");
    builder.end();
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    assert_eq!(
        fdt.alias("serial0"),
        Some("/pl011@9000000"),
        "an alias maps a short name onto a path"
    );
    assert_eq!(
        fdt.console().map(|node| node.name),
        Some("pl011@9000000"),
        "a stdout-path that is an alias must be resolved through /aliases"
    );
    assert!(
        fdt.alias("serial9").is_none(),
        "an alias that does not exist resolves to nothing"
    );
}

#[test]
fn a_stdout_path_without_a_suffix_still_resolves() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.begin("uart");
    builder.prop_strings("compatible", &["ns16550a"]);
    builder.end();
    builder.begin("chosen");
    builder.prop_str("stdout-path", "/uart");
    builder.end();
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    assert_eq!(
        fdt.console().and_then(|node| node.compatible()),
        Some("ns16550a"),
        "the colon suffix is optional, not required"
    );
}

#[test]
fn a_dangling_stdout_path_resolves_to_nothing() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.begin("chosen");
    builder.prop_str("stdout-path", "/nowhere@0:115200n8");
    builder.end();
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    assert!(
        fdt.console().is_none(),
        "a stdout-path naming a node that is not there yields no console"
    );
}

#[test]
fn a_gicv3_reports_its_distributor_and_redistributor() {
    let blob = virt();
    let fdt = parse(&blob);
    let gic = fdt
        .interrupt_controller()
        .expect("the fixture describes a GIC");
    assert_eq!(
        gic.version,
        GicVersion::V3,
        "arm,gic-v3 must be recognised as a GICv3"
    );
    assert_eq!(
        gic.distributor(),
        Some(Region {
            address: 0x800_0000,
            size: 0x1_0000
        }),
        "the first reg range of a GIC is its distributor"
    );
    assert_eq!(
        gic.redistributor(),
        Some(Region {
            address: 0x80a_0000,
            size: 0xf6_0000
        }),
        "the second reg range of a GICv3 is the redistributor region"
    );
    assert!(
        gic.cpu_interface().is_none(),
        "a GICv3 has no memory-mapped CPU interface"
    );
}

#[test]
fn a_gicv2_reports_its_distributor_and_cpu_interface() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_u32("#address-cells", 2);
    builder.prop_u32("#size-cells", 2);
    builder.begin("intc@8000000");
    builder.prop_strings("compatible", &["arm,cortex-a15-gic"]);
    builder.prop_cells(
        "reg",
        &[0, 0x800_0000, 0, 0x1_0000, 0, 0x801_0000, 0, 0x1_0000],
    );
    builder.end();
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    let gic = fdt
        .interrupt_controller()
        .expect("the tree describes a GICv2");
    assert_eq!(
        gic.version,
        GicVersion::V2,
        "arm,cortex-a15-gic must be recognised as a GICv2"
    );
    assert_eq!(
        gic.cpu_interface(),
        Some(Region {
            address: 0x801_0000,
            size: 0x1_0000
        }),
        "the second reg range of a GICv2 is the CPU interface"
    );
    assert!(
        gic.redistributor().is_none(),
        "a GICv2 has no redistributor"
    );
}

#[test]
fn a_tree_without_an_interrupt_controller_reports_none() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.begin("chosen");
    builder.end();
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    assert!(
        fdt.interrupt_controller().is_none(),
        "no GIC node means no interrupt controller, not a wrong one"
    );
}

#[test]
fn the_timer_reports_its_four_interrupts() {
    let blob = virt();
    let fdt = parse(&blob);
    let timer = fdt.timer().expect("the fixture describes a timer");
    assert_eq!(
        timer.name, "timer",
        "the architected timer is found by compatible, not by name"
    );
    let cells: Vec<u32> = timer
        .interrupts()
        .expect("the timer has interrupts")
        .collect();
    assert_eq!(
        cells,
        vec![1, 13, 0x108, 1, 14, 0x108, 1, 11, 0x108, 1, 10, 0x108],
        "the timer's four PPIs are handed over as raw cells"
    );
}

#[test]
fn reservations_are_read_until_the_zero_pair() {
    let blob = virt();
    let fdt = parse(&blob);
    let entries: Vec<MemoryReservation> = fdt.reservations().collect();
    assert_eq!(
        entries,
        vec![
            MemoryReservation {
                address: 0x4000_0000,
                size: 0x1_0000
            },
            MemoryReservation {
                address: 0x4001_0000,
                size: 0x2000
            }
        ],
        "both reserved ranges must be reported, and the terminator must not be"
    );
}

#[test]
fn an_empty_reservation_block_yields_nothing() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    assert_eq!(
        fdt.reservations().count(),
        0,
        "a reservation block that is only its terminator reserves nothing"
    );
}

#[test]
fn root_properties_and_bootargs_are_readable() {
    let blob = virt();
    let fdt = parse(&blob);
    assert_eq!(
        fdt.model(),
        Some("linux,dummy-virt"),
        "the machine's model comes from the root node"
    );
    assert_eq!(
        fdt.bootargs(),
        Some("console=ttyAMA0 root=/dev/vda"),
        "the command line comes from /chosen"
    );
    let root = fdt.root().expect("the root should exist");
    assert_eq!(root.name, "", "the root node has an empty name");
    assert_eq!(
        root.property("#address-cells").and_then(|p| p.as_u32()),
        Some(2),
        "a one-word property reads back as a single big-endian value"
    );
    assert!(
        root.property("nonexistent").is_none(),
        "a property that is not there is absent, not empty"
    );
}

#[test]
fn node_names_split_into_base_and_unit_address() {
    let blob = virt();
    let fdt = parse(&blob);
    let uart = fdt.find_node("/pl011@9000000").expect("uart should exist");
    assert_eq!(
        uart.base_name(),
        "pl011",
        "the base name is everything before the unit address"
    );
    assert_eq!(
        uart.unit_address(),
        Some("9000000"),
        "the unit address is what follows the at sign"
    );
    let timer = fdt.timer().expect("timer should exist");
    assert!(
        timer.unit_address().is_none(),
        "a node without an at sign has no unit address"
    );
}

#[test]
fn property_accessors_refuse_values_of_the_wrong_width() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_cells("pair", &[1, 2]);
    builder.prop("empty", &[]);
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);
    let root = fdt.root().expect("root should exist");
    let pair = root.property("pair").expect("pair should exist");
    assert_eq!(
        pair.as_u32(),
        None,
        "an eight-byte value is not a single word"
    );
    assert_eq!(
        pair.as_u64(),
        Some(0x0000_0001_0000_0002),
        "two cells read back as one big-endian doubleword"
    );
    let empty = root.property("empty").expect("empty should exist");
    assert!(
        empty.is_empty(),
        "a zero-length value is how a boolean property is spelled"
    );
    assert_eq!(empty.len(), 0, "an empty value has no bytes");
    assert_eq!(
        empty.as_str(),
        None,
        "an empty value is not a string, since it has no terminator"
    );
}

#[test]
fn errors_describe_themselves() {
    use std::string::ToString;
    let rendered = FdtError::BadMagic(0xDEAD_BEEF).to_string();
    assert!(
        rendered.contains("0xdeadbeef"),
        "the magic error should name what was found, got {rendered}"
    );
    assert!(
        !FdtError::DepthOverflow.to_string().is_empty(),
        "every error variant must render to something readable"
    );
}

// ---------------------------------------------------------------------------
// Totality
// ---------------------------------------------------------------------------

/// Call every accessor on one blob, requiring only that none of them panics.
fn poke_at_everything(blob: &[u8]) {
    let Ok(fdt) = Fdt::parse(blob) else {
        return;
    };
    let _ = fdt.header();
    let _ = fdt.boot_cpu();
    let _ = fdt.model();
    let _ = fdt.bootargs();
    let _ = fdt.stdout_path();
    let _ = fdt.alias("serial0");
    let _ = fdt.find_node("/chosen");
    let _ = fdt.find_node("/memory");
    let _ = fdt.find_compatible("arm,pl011");
    let _ = fdt.timer();
    let _ = fdt.psci_conduit();
    for which in [
        TimerInterrupt::SecurePhysical,
        TimerInterrupt::NonSecurePhysical,
        TimerInterrupt::Virtual,
        TimerInterrupt::Hypervisor,
    ] {
        let _ = fdt.timer_interrupt(which);
    }

    for region in fdt.memory().take(64) {
        let _ = region;
    }
    for reservation in fdt.reservations().take(64) {
        let _ = reservation;
    }
    for cpu in fdt.cpus().take(64) {
        let _ = (cpu.id, cpu.enable_method(), cpu.status());
    }
    if let Some(console) = fdt.console() {
        let _ = console.compatible();
        let _ = console.reg().take(8).count();
    }
    if let Some(gic) = fdt.interrupt_controller() {
        let _ = gic.distributor();
        let _ = gic.redistributor();
        let _ = gic.cpu_interface();
    }
    for node in fdt.nodes().take(4096) {
        let _ = node.base_name();
        let _ = node.unit_address();
        let _ = node.device_type();
        let _ = node.reg().take(16).count();
        let _ = node.interrupts().map(|cells| cells.take(16).count());
        for property in node.properties().take(64) {
            let _ = property.as_u32();
            let _ = property.as_u64();
            let _ = property.as_str();
            let _ = property.strings().take(16).count();
            let _ = property.cells().take(16).count();
        }
    }
}

#[test]
fn parsing_never_panics_on_corrupted_blobs() {
    // The cheap deterministic version of a fuzz run: walk a known-good blob
    // byte by byte, corrupting each in turn, and require that every accessor
    // either answers or errors. Nothing about the answers is asserted; the
    // property under test is that ring 0 survives whatever firmware hands over.
    for good in [virt(), virt_armv7()] {
        for index in 0..good.len().min(512) {
            for patch_value in [0x00u8, 0x01, 0x7F, 0xFF] {
                let mut blob = good.clone();
                blob[index] = patch_value;
                poke_at_everything(&blob);
            }
        }
    }
}

#[test]
fn parsing_never_panics_on_truncation() {
    let good = virt();
    for length in 0..good.len() {
        poke_at_everything(&good[..length]);
    }
}

#[test]
fn parsing_never_panics_on_arbitrary_bytes() {
    // A header-shaped blob whose every field is hostile, plus a few degenerate
    // slices that have tripped byte parsers before.
    poke_at_everything(&[]);
    poke_at_everything(&[0xFF; 40]);
    let mut hostile = vec![0u8; 128];
    hostile[0..4].copy_from_slice(&FDT_MAGIC.to_be_bytes());
    for offset in (4..40).step_by(4) {
        let mut blob = hostile.clone();
        patch(&mut blob, offset, u32::MAX);
        poke_at_everything(&blob);
        patch(&mut blob, offset, 0);
        poke_at_everything(&blob);
    }
}

// ---------------------------------------------------------------------------
// What a 32-bit Arm machine is described with
// ---------------------------------------------------------------------------

/// A tree shaped like QEMU's `virt` machine with a Cortex-A7, as QEMU 8.2
/// generates it: four processors started through PSCI, the 32-bit timer
/// binding, a GICv2, and PSCI over `hvc`.
fn virt_armv7() -> Vec<u8> {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_u32("#address-cells", 2);
    builder.prop_u32("#size-cells", 2);
    builder.prop_str("compatible", "linux,dummy-virt");

    builder.begin("cpus");
    builder.prop_u32("#address-cells", 1);
    builder.prop_u32("#size-cells", 0);
    for id in 0..4u32 {
        builder.begin(&std::format!("cpu@{id}"));
        builder.prop_str("device_type", "cpu");
        builder.prop_str("compatible", "arm,cortex-a7");
        builder.prop_u32("reg", id);
        builder.prop_str("enable-method", "psci");
        builder.end();
    }
    builder.end();

    builder.begin("psci");
    builder.prop_strings("compatible", &["arm,psci-1.0", "arm,psci-0.2", "arm,psci"]);
    builder.prop_str("method", "hvc");
    builder.end();

    builder.begin("intc@8000000");
    builder.prop_strings("compatible", &["arm,cortex-a15-gic"]);
    builder.prop_cells(
        "reg",
        &[0, 0x800_0000, 0, 0x1_0000, 0, 0x801_0000, 0, 0x1_0000],
    );
    builder.end();

    builder.begin("timer");
    builder.prop_strings("compatible", &["arm,armv7-timer"]);
    builder.prop_cells(
        "interrupts",
        &[1, 13, 0x104, 1, 14, 0x104, 1, 11, 0x104, 1, 10, 0x104],
    );
    builder.end();

    builder.end();
    builder.build()
}

#[test]
fn an_armv7_timer_is_found_by_its_own_binding() {
    let blob = virt_armv7();
    let fdt = parse(&blob);
    let timer = fdt
        .timer()
        .expect("arm,armv7-timer is the architected timer on a 32-bit CPU");
    assert_eq!(timer.name, "timer");
}

#[test]
fn the_timer_interrupts_decode_to_gic_identifiers() {
    for blob in [virt(), virt_armv7()] {
        let fdt = parse(&blob);
        assert_eq!(
            fdt.timer_interrupt(TimerInterrupt::SecurePhysical),
            Some(29)
        );
        assert_eq!(
            fdt.timer_interrupt(TimerInterrupt::NonSecurePhysical),
            Some(30)
        );
        assert_eq!(
            fdt.timer_interrupt(TimerInterrupt::Virtual),
            Some(27),
            "the virtual timer is PPI 11, identifier 27"
        );
        assert_eq!(fdt.timer_interrupt(TimerInterrupt::Hypervisor), Some(26));
    }
}

#[test]
fn a_timer_specifier_cut_short_names_no_interrupt() {
    let mut builder = Builder::new();
    builder.begin("");
    builder.begin("timer");
    builder.prop_strings("compatible", &["arm,armv7-timer"]);
    builder.prop_cells("interrupts", &[1, 13, 0x104, 1, 14]);
    builder.end();
    builder.end();
    let blob = builder.build();
    let fdt = parse(&blob);

    assert_eq!(
        fdt.timer_interrupt(TimerInterrupt::SecurePhysical),
        Some(29)
    );
    assert_eq!(
        fdt.timer_interrupt(TimerInterrupt::NonSecurePhysical),
        None,
        "a specifier missing its flags cell is not trusted"
    );
    assert_eq!(fdt.timer_interrupt(TimerInterrupt::Virtual), None);
}

#[test]
fn gic_specifiers_follow_the_binding() {
    assert_eq!(gic_interrupt_id(1, 11), Some(27));
    assert_eq!(gic_interrupt_id(1, 15), Some(31));
    assert_eq!(
        gic_interrupt_id(1, 16),
        None,
        "there are sixteen private peripherals"
    );
    assert_eq!(gic_interrupt_id(0, 1), Some(33), "the PL011 on virt");
    assert_eq!(gic_interrupt_id(0, 987), Some(1019));
    assert_eq!(
        gic_interrupt_id(0, 988),
        None,
        "identifiers from 1020 up are special, not interrupts"
    );
    assert_eq!(gic_interrupt_id(2, 0), None, "no third kind of peripheral");
}

#[test]
fn every_gicv2_binding_is_recognised() {
    for binding in GICV2_COMPATIBLES {
        let mut builder = Builder::new();
        builder.begin("");
        builder.prop_u32("#address-cells", 1);
        builder.prop_u32("#size-cells", 1);
        builder.begin("interrupt-controller@a0021000");
        builder.prop_strings("compatible", &[binding]);
        builder.prop_cells("reg", &[0xA002_1000, 0x1000, 0xA002_2000, 0x2000]);
        builder.end();
        builder.end();
        let blob = builder.build();
        let fdt = parse(&blob);

        let gic = fdt
            .interrupt_controller()
            .unwrap_or_else(|| panic!("{binding} is a GICv2"));
        assert_eq!(gic.version, GicVersion::V2, "{binding}");
        assert_eq!(
            gic.cpu_interface(),
            Some(Region {
                address: 0xA002_2000,
                size: 0x2000
            }),
            "{binding}: one address cell, as the STM32MP1 writes it"
        );
    }
}

/// The conduit a tree with one PSCI node of this shape reports.
fn psci_conduit_of(compatible: &[&str], method: Option<&str>) -> Option<PsciConduit> {
    let mut builder = Builder::new();
    builder.begin("");
    builder.begin("psci");
    builder.prop_strings("compatible", compatible);
    if let Some(method) = method {
        builder.prop_str("method", method);
    }
    builder.end();
    builder.end();
    let blob = builder.build();
    parse(&blob).psci_conduit()
}

#[test]
fn the_psci_conduit_is_read_from_its_method() {
    assert_eq!(
        parse(&virt_armv7()).psci_conduit(),
        Some(PsciConduit::Hvc),
        "QEMU emulates PSCI as a hypervisor when the machine has no EL2 or EL3"
    );
    assert_eq!(
        psci_conduit_of(&["arm,psci-0.2"], Some("smc")),
        Some(PsciConduit::Smc)
    );
    assert_eq!(
        psci_conduit_of(&["arm,psci"], Some("hvc")),
        Some(PsciConduit::Hvc)
    );
    assert_eq!(
        psci_conduit_of(&["arm,psci-1.0"], Some("mmio")),
        None,
        "an unknown conduit is not guessed at"
    );
    assert_eq!(psci_conduit_of(&["arm,psci-1.0"], None), None);
    assert_eq!(psci_conduit_of(&["vendor,not-psci"], Some("smc")), None);
    assert_eq!(
        parse(&virt()).psci_conduit(),
        None,
        "the AArch64 fixture has no PSCI node"
    );
}

// ---------------------------------------------------------------------------
// Processors
// ---------------------------------------------------------------------------

/// The hardware identifiers of the processors a tree describes, in order.
fn cpu_ids(blob: &[u8]) -> Vec<u64> {
    parse(blob).cpus().map(|cpu| cpu.id).collect()
}

/// A tree whose `/cpus` holds whatever `children` writes, with one address
/// cell and no size cells, as every Arm tree has.
fn with_cpus(children: impl FnOnce(&mut Builder)) -> Vec<u8> {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_u32("#address-cells", 2);
    builder.prop_u32("#size-cells", 2);
    builder.begin("cpus");
    builder.prop_u32("#address-cells", 1);
    builder.prop_u32("#size-cells", 0);
    children(&mut builder);
    builder.end();
    builder.end();
    builder.build()
}

/// One `cpu` node, with `extra` properties written after its `reg`.
fn cpu_node(builder: &mut Builder, name: &str, reg: Option<u32>, extra: &[(&str, &str)]) {
    builder.begin(name);
    builder.prop_str("device_type", "cpu");
    if let Some(reg) = reg {
        builder.prop_u32("reg", reg);
    }
    for (property, value) in extra {
        builder.prop_str(property, value);
    }
    builder.end();
}

#[test]
fn qemu_armv7_virt_describes_four_processors_started_by_psci() {
    let blob = virt_armv7();
    let fdt = parse(&blob);
    assert_eq!(cpu_ids(&blob), [0, 1, 2, 3]);
    for cpu in fdt.cpus() {
        assert_eq!(cpu.enable_method(), Some("psci"), "cpu {}", cpu.id);
        assert_eq!(
            cpu.status(),
            None,
            "cpu {} has no status, so is okay",
            cpu.id
        );
        assert!(cpu.node.is_compatible("arm,cortex-a7"));
    }
}

#[test]
fn a_two_cell_identifier_carries_aff3() {
    // AArch64 trees use two address cells under `/cpus`, with `Aff3` in the
    // upper one: the identifier is the whole 64-bit value, not its low word.
    let mut builder = Builder::new();
    builder.begin("");
    builder.begin("cpus");
    builder.prop_u32("#address-cells", 2);
    builder.prop_u32("#size-cells", 0);
    builder.begin("cpu@100000000");
    builder.prop_str("device_type", "cpu");
    builder.prop_cells("reg", &[0x1, 0x0]);
    builder.end();
    builder.begin("cpu@101");
    builder.prop_str("device_type", "cpu");
    builder.prop_cells("reg", &[0x0, 0x101]);
    builder.end();
    builder.end();
    builder.end();

    assert_eq!(cpu_ids(&builder.build()), [0x1_0000_0000, 0x101]);
}

#[test]
fn only_direct_children_of_cpus_that_are_processors_count() {
    let blob = with_cpus(|builder| {
        // The topology map and idle states live beside the processors and
        // are not processors, however `cpu`-like their children look.
        builder.begin("cpu-map");
        builder.begin("cluster0");
        builder.begin("core0");
        builder.prop_u32("cpu", 1);
        builder.end();
        builder.end();
        builder.end();
        builder.begin("idle-states");
        builder.end();

        // A processor with a cache inside it: the cache is not a second one.
        builder.begin("cpu@0");
        builder.prop_str("device_type", "cpu");
        builder.prop_u32("reg", 0);
        builder.begin("l2-cache");
        builder.prop_str("device_type", "cache");
        builder.prop_u32("reg", 0x77);
        builder.end();
        builder.end();

        // Typed but not named `cpu`: still a processor.
        builder.begin("processor@5");
        builder.prop_str("device_type", "cpu");
        builder.prop_u32("reg", 5);
        builder.end();

        // Named `cpu` but untyped: also still one, the other spelling.
        builder.begin("cpu@6");
        builder.prop_u32("reg", 6);
        builder.end();
    });
    assert_eq!(cpu_ids(&blob), [0, 5, 6]);

    // And a `cpu` node anywhere but directly under `/cpus` is nobody's.
    let mut builder = Builder::new();
    builder.begin("");
    builder.begin("cpus");
    builder.prop_u32("#address-cells", 1);
    builder.prop_u32("#size-cells", 0);
    cpu_node(&mut builder, "cpu@0", Some(0), &[]);
    builder.end();
    builder.begin("soc");
    builder.prop_u32("#address-cells", 1);
    builder.prop_u32("#size-cells", 0);
    cpu_node(&mut builder, "cpu@9", Some(9), &[]);
    builder.end();
    builder.end();
    assert_eq!(
        cpu_ids(&builder.build()),
        [0],
        "the walk must stop at the end of /cpus"
    );
}

#[test]
fn a_failed_processor_is_skipped_and_a_stopped_one_is_not() {
    let blob = with_cpus(|builder| {
        cpu_node(builder, "cpu@0", Some(0), &[("status", "okay")]);
        cpu_node(builder, "cpu@1", Some(1), &[("status", "disabled")]);
        cpu_node(builder, "cpu@2", Some(2), &[("status", "fail")]);
        cpu_node(builder, "cpu@3", Some(3), &[("status", "fail-sss")]);
    });
    let fdt = parse(&blob);
    let found: Vec<(u64, Option<&str>)> = fdt.cpus().map(|cpu| (cpu.id, cpu.status())).collect();
    assert_eq!(
        found,
        [(0, Some("okay")), (1, Some("disabled"))],
        "a disabled processor is quiescent, not absent; a failed one is broken"
    );
}

#[test]
fn a_processor_with_no_reg_cannot_be_addressed_and_is_skipped() {
    let blob = with_cpus(|builder| {
        cpu_node(builder, "cpu@0", None, &[("enable-method", "psci")]);
        cpu_node(
            builder,
            "cpu@1",
            Some(1),
            &[("enable-method", "spin-table")],
        );
    });
    let fdt = parse(&blob);
    let found: Vec<(u64, Option<&str>)> = fdt
        .cpus()
        .map(|cpu| (cpu.id, cpu.enable_method()))
        .collect();
    assert_eq!(found, [(1, Some("spin-table"))]);
}

#[test]
fn a_tree_without_cpus_describes_no_processors() {
    assert!(
        cpu_ids(&virt()).is_empty(),
        "the AArch64 fixture has no /cpus node"
    );
    let blob = with_cpus(|_| {});
    assert!(cpu_ids(&blob).is_empty(), "an empty /cpus holds none");
}

// ---------------------------------------------------------------------------
// PCI host bridges
// ---------------------------------------------------------------------------

/// A root with two address and two size cells, holding whatever `body` adds.
fn tree(body: impl FnOnce(&mut Builder)) -> Vec<u8> {
    let mut builder = Builder::new();
    builder.begin("");
    builder.prop_u32("#address-cells", 2);
    builder.prop_u32("#size-cells", 2);
    body(&mut builder);
    builder.end();
    builder.build()
}

/// An ECAM host node with a window of `size` bytes at `address`.
fn ecam_node(builder: &mut Builder, address: u64, size: u64, extra: impl FnOnce(&mut Builder)) {
    builder.begin("pcie@10000000");
    builder.prop_str("compatible", PCI_HOST_ECAM_COMPATIBLE);
    builder.prop_cells(
        "reg",
        &[
            (address >> 32) as u32,
            address as u32,
            (size >> 32) as u32,
            size as u32,
        ],
    );
    // A host's own children are addressed with three cells, which must not
    // change how its own `reg` is read.
    builder.prop_u32("#address-cells", 3);
    builder.prop_u32("#size-cells", 2);
    extra(builder);
    builder.end();
}

fn hosts(blob: &[u8]) -> Vec<EcamHost> {
    parse(blob).ecam_hosts().collect()
}

#[test]
fn qemu_virt_describes_one_ecam_host_above_four_gibibytes() {
    let blob = tree(|b| {
        ecam_node(b, 0x40_1000_0000, 0x1000_0000, |b| {
            b.prop_cells("bus-range", &[0, 0xff]);
            b.prop_str("device_type", "pci");
        });
    });
    assert_eq!(
        hosts(&blob),
        vec![EcamHost {
            window: Region {
                address: 0x40_1000_0000,
                size: 0x1000_0000
            },
            segment: 0,
            start_bus: 0,
            end_bus: 0xff
        }],
        "the window QEMU's virt machine puts in highmem"
    );
}

#[test]
fn an_ecam_host_without_bus_range_or_domain_takes_the_defaults() {
    let blob = tree(|b| {
        ecam_node(b, 0x3f00_0000, 0x1000_0000, |_| {});
        ecam_node(b, 0x5000_0000, 0x0100_0000, |b| {
            b.prop_u32("linux,pci-domain", 2);
            b.prop_cells("bus-range", &[0x10, 0x1f]);
        });
    });
    let found = hosts(&blob);
    assert_eq!(found.len(), 2, "both hosts, in order");
    assert_eq!(
        (found[0].segment, found[0].start_bus, found[0].end_bus),
        (0, 0, 255),
        "defaults"
    );
    assert_eq!(
        (found[1].segment, found[1].start_bus, found[1].end_bus),
        (2, 0x10, 0x1f),
        "declared"
    );
}

#[test]
fn an_ecam_window_smaller_than_its_bus_range_reaches_only_the_buses_it_holds() {
    let blob = tree(|b| {
        ecam_node(b, 0x3f00_0000, 4 << 20, |b| {
            b.prop_cells("bus-range", &[0x10, 0x20]);
        });
    });
    assert_eq!(hosts(&blob)[0].end_bus, 0x13, "four buses from 0x10");

    let blob = tree(|b| {
        ecam_node(b, 0x3f00_0000, 0x8_0000, |_| {});
    });
    assert!(hosts(&blob).is_empty(), "half a megabyte is not one bus");

    let blob = tree(|b| {
        ecam_node(b, 0x3f00_0000, 1 << 40, |b| {
            b.prop_cells("bus-range", &[0xf0, 0xff]);
        });
    });
    assert_eq!(
        hosts(&blob)[0].end_bus,
        0xff,
        "a huge window holds every bus it names"
    );
}

#[test]
fn an_ecam_host_that_cannot_be_used_is_skipped() {
    let blob = tree(|b| {
        ecam_node(b, 0x1000_0000, 1 << 28, |b| {
            b.prop_str("status", "disabled");
        });
        ecam_node(b, 0x2000_0000, 1 << 28, |b| {
            b.prop_cells("bus-range", &[2, 1]);
        });
        ecam_node(b, 0x3000_0000, 1 << 28, |b| {
            b.prop_cells("bus-range", &[0, 300]);
        });
        ecam_node(b, 0x4000_0000, 1 << 28, |b| {
            b.prop_cells("bus-range", &[0, 1, 2]);
        });
        ecam_node(b, 0x5000_0000, 1 << 28, |b| {
            b.prop_u32("linux,pci-domain", 0x1_0000);
        });
        b.begin("pcie@60000000");
        b.prop_str("compatible", "pci-host-cam-generic");
        b.prop_cells("reg", &[0, 0x6000_0000, 0, 0x100_0000]);
        b.end();
        b.begin("pcie@70000000");
        b.prop_str("compatible", PCI_HOST_ECAM_COMPATIBLE);
        b.end();
        ecam_node(b, 0x8000_0000, 1 << 28, |b| {
            b.prop_str("status", "okay");
        });
    });
    let found = hosts(&blob);
    assert_eq!(found.len(), 1, "only the last: {found:?}");
    assert_eq!(found[0].window.address, 0x8000_0000, "explicitly okay");
}
