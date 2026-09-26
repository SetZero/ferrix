//! Fuzz the ACPI table reader, from the RSDP down and table by table.
//!
//! On x86-64 and on AArch64 servers the kernel finds its processors, its
//! interrupt controllers, its timers, PCI configuration space and its IOMMUs
//! in these tables, in ring 0, before anything else is running. Firmware
//! writes them, and firmware ships tables with wrong lengths, zero-length
//! entries and bad checksums.
//!
//! # The input
//!
//! The input is physical memory: an address is an offset into it. When it
//! begins with an RSDP, the walk the kernel makes is taken from there -- the
//! root table, every table it lists, and each table found by signature. Every
//! offset where a known signature appears is also read as a table on its own,
//! so a seed that is one table and a seed that is a whole machine both reach
//! every decoder.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **A table is exactly the length it declares**, inside the memory it was
//!    read from, with its checksum reported as the bytes actually sum.
//! 2. **Every walk ends within a bound the table's length sets**: root table
//!    entries, MADT, MCFG and DMAR structures, device scopes, IORT nodes and
//!    their ID mappings.
//! 3. **The decoders agree with the bytes**: an entry's type and fields are
//!    the little-endian values at the offsets the specification gives, a
//!    structure is reported malformed exactly when it is too short for its
//!    type, and `check_entries` accepts a MADT exactly when its entries tile
//!    the table.
//! 4. **Lookups agree with walks**: `Acpi::find` returns the first listed
//!    table with the signature, `RootTable::entry(i)` is the walk's `i`th,
//!    `Madt::local_apic_address` is the last override, an IORT node read at
//!    its own offset is the same node, and a translation comes from the first
//!    mapping that covers the identifier.

#![no_main]

use ferrix_acpi::dmar::{Dmar, Structure};
use ferrix_acpi::iort::{Iort, NODE_HEADER_LEN};
use ferrix_acpi::{
    Acpi, Fadt, Gtdt, Hpet, MADT_HEADER_LEN, MCFG_ENTRY_LEN, MCFG_HEADER_LEN, Madt, MadtEntry,
    Mcfg, RSDP_SIGNATURE, RSDP_V1_LEN, RootKind, RootTable, Rsdp, SDT_HEADER_LEN, Table, Tables,
    read_table,
};
use libfuzzer_sys::fuzz_target;

/// The input as physical memory.
struct Memory<'a>(&'a [u8]);

impl Tables for Memory<'_> {
    fn table(&self, physical_address: u64, len: usize) -> Option<&[u8]> {
        let start = usize::try_from(physical_address).ok()?;
        let rest = self.0.get(start..)?;
        // Everything to the end, not just `len`: firmware's mapping is a
        // window, and a table must never read past its own length inside it.
        (rest.len() >= len).then_some(rest)
    }
}

/// The signatures worth reading as a table wherever they appear.
const SIGNATURES: [&[u8; 4]; 9] = [
    b"APIC", b"FACP", b"GTDT", b"HPET", b"MCFG", b"DMAR", b"IORT", b"XSDT", b"RSDT",
];

/// The most tables one input is read as, so a buffer full of signatures does
/// not turn one case into thousands.
const MAX_TABLES: usize = 32;

fn u8_at(bytes: &[u8], at: usize) -> u8 {
    bytes[at]
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

/// Whether `inner` lies inside `outer`; an empty slice reads nothing.
fn inside(outer: &[u8], inner: &[u8]) -> bool {
    let (outer, range) = (outer.as_ptr_range(), inner.as_ptr_range());
    inner.is_empty() || (outer.start <= range.start && range.end <= outer.end)
}

/// Property 1 for any table, then whichever decoder its signature selects.
fn check_table(memory: &[u8], table: Table<'_>) {
    let bytes = table.bytes();
    assert_eq!(
        bytes.len(),
        table.header().length as usize,
        "a table is not its length"
    );
    assert!(bytes.len() >= SDT_HEADER_LEN && inside(memory, bytes));
    assert_eq!(bytes.get(..4), Some(&table.signature()[..]));
    assert_eq!(table.body().len(), bytes.len() - SDT_HEADER_LEN);
    let sum = bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    assert_eq!(
        table.checksum_valid(),
        sum == 0,
        "the checksum is misreported"
    );

    match &table.signature() {
        b"APIC" => check_madt(table),
        b"MCFG" => check_mcfg(table),
        b"DMAR" => check_dmar(table),
        b"IORT" => check_iort(table),
        b"XSDT" => check_root(table, RootKind::Xsdt),
        b"RSDT" => check_root(table, RootKind::Rsdt),
        b"HPET" => {
            if let Ok(hpet) = Hpet::parse(table) {
                assert!(hpet.comparators() <= 32);
                let _ = (
                    hpet.revision(),
                    hpet.counter_is_64_bit(),
                    hpet.supports_legacy_replacement(),
                    hpet.vendor(),
                    hpet.base_address(),
                    hpet.block_number(),
                    hpet.minimum_tick(),
                    hpet.page_protection(),
                );
            }
        }
        b"FACP" => {
            let fadt = Fadt::parse(table).expect("a FACP table is an FADT");
            let _ = (
                fadt.firmware_ctrl(),
                fadt.dsdt_address(),
                fadt.flags(),
                fadt.century(),
                fadt.iapc_boot_arch(),
                fadt.arm_boot_arch(),
                fadt.pm_timer_block(),
                fadt.pm_timer_length(),
                fadt.x_pm_timer_block(),
                fadt.pm_timer(),
            );
        }
        b"GTDT" => {
            if let Ok(gtdt) = Gtdt::parse(table) {
                let _ = (
                    gtdt.counter_control_base(),
                    gtdt.counter_read_base(),
                    gtdt.secure_el1_timer().is_present(),
                    gtdt.non_secure_el1_timer().is_edge_triggered(),
                    gtdt.virtual_el1_timer().is_active_low(),
                    gtdt.non_secure_el2_timer().is_always_on(),
                    gtdt.virtual_el2_timer(),
                    gtdt.platform_timer_count(),
                );
            }
        }
        _ => {}
    }
}

/// The shortest entry of each MADT type the crate decodes.
fn madt_minimum(kind: u8) -> Option<usize> {
    Some(match kind {
        0 => 8,
        1 => 12,
        2 => 10,
        4 => 6,
        5 => 12,
        9 => 16,
        11 => 76,
        12 => 21,
        13 => 24,
        14 => 16,
        15 => 16,
        _ => return None,
    })
}

fn check_madt(table: Table<'_>) {
    let Ok(madt) = Madt::parse(table) else {
        assert!(
            table.bytes().len() < MADT_HEADER_LEN,
            "a long enough MADT was refused"
        );
        return;
    };
    let body = &table.bytes()[MADT_HEADER_LEN..];
    let mut offset = 0;
    let mut expected_apic = u64::from(madt.local_controller_address());
    let mut walked = 0;
    for entry in madt.entries() {
        let kind = u8_at(body, offset);
        let length = usize::from(u8_at(body, offset + 1));
        assert!(
            length >= 2 && offset + length <= body.len(),
            "an entry out of bounds"
        );
        let raw = &body[offset..offset + length];
        match (madt_minimum(kind), entry) {
            (None, MadtEntry::Unknown { kind: k, length: l }) => {
                assert_eq!((k, usize::from(l)), (kind, length));
            }
            (Some(minimum), MadtEntry::Malformed { kind: k, .. }) => {
                assert_eq!(k, kind);
                assert!(length < minimum, "a long enough entry was called malformed");
            }
            (Some(minimum), decoded) => {
                assert!(length >= minimum, "a short entry was decoded");
                match decoded {
                    MadtEntry::LocalApic(apic) => {
                        assert_eq!(
                            (kind, apic.apic_id, apic.flags),
                            (0, raw[3], u32_at(raw, 4))
                        );
                    }
                    MadtEntry::IoApic(io) => {
                        assert_eq!(
                            (kind, io.address, io.gsi_base),
                            (1, u32_at(raw, 4), u32_at(raw, 8))
                        );
                    }
                    MadtEntry::InterruptSourceOverride(over) => {
                        assert_eq!(
                            (kind, over.gsi, over.flags),
                            (2, u32_at(raw, 4), u16_at(raw, 8))
                        );
                    }
                    MadtEntry::LocalApicNmi(nmi) => assert_eq!((kind, nmi.lint), (4, raw[5])),
                    MadtEntry::LocalApicAddressOverride(address) => {
                        assert_eq!((kind, address), (5, u64_at(raw, 4)));
                        expected_apic = address;
                    }
                    MadtEntry::LocalX2Apic(x2) => {
                        assert_eq!((kind, x2.x2apic_id), (9, u32_at(raw, 4)));
                    }
                    MadtEntry::Gicc(gicc) => {
                        assert_eq!((kind, gicc.mpidr), (11, u64_at(raw, 68)));
                        let class = raw.get(76).copied().unwrap_or(0);
                        assert_eq!(gicc.power_efficiency_class, class);
                    }
                    MadtEntry::Gicd(gicd) => {
                        assert_eq!((kind, gicd.physical_base_address), (12, u64_at(raw, 8)));
                    }
                    MadtEntry::GicMsiFrame(frame) => {
                        assert_eq!((kind, frame.base_address), (13, u64_at(raw, 8)));
                    }
                    MadtEntry::Gicr(gicr) => {
                        assert_eq!((kind, gicr.discovery_range_base), (14, u64_at(raw, 4)));
                    }
                    MadtEntry::GicIts(its) => {
                        assert_eq!((kind, its.physical_base_address), (15, u64_at(raw, 8)));
                    }
                    other => panic!("type {kind} decoded as {other:?}"),
                }
            }
            (None, other) => panic!("unknown type {kind} decoded as {other:?}"),
        }
        offset += length;
        walked += 1;
    }
    assert!(walked <= body.len() / 2, "more entries than bytes allow");
    assert_eq!(
        madt.local_apic_address(),
        expected_apic,
        "not the last override"
    );

    // `check_entries` accepts exactly the MADTs whose entries tile the table,
    // within the walk's budget.
    let tiles = offset == body.len();
    match madt.check_entries() {
        Ok(()) => assert!(tiles, "check_entries accepted a walk that stopped early"),
        Err(_) => assert!(!tiles, "check_entries refused a tiled table"),
    }
}

fn check_mcfg(table: Table<'_>) {
    let Ok(mcfg) = Mcfg::parse(table) else {
        return;
    };
    let bytes = table.bytes();
    let mut count = 0;
    for (index, allocation) in mcfg.entries().enumerate() {
        let at = MCFG_HEADER_LEN + index * MCFG_ENTRY_LEN;
        assert_eq!(allocation.base_address, u64_at(bytes, at));
        assert_eq!(allocation.segment, u16_at(bytes, at + 8));
        assert_eq!(
            (allocation.start_bus, allocation.end_bus),
            (bytes[at + 10], bytes[at + 11])
        );
        assert_eq!(
            allocation.window_len().is_none(),
            allocation.end_bus < allocation.start_bus
        );
        let _ = allocation.window_base();
        count += 1;
    }
    assert_eq!(count, (bytes.len() - MCFG_HEADER_LEN) / MCFG_ENTRY_LEN);
}

fn check_dmar(table: Table<'_>) {
    let Ok(dmar) = Dmar::parse(table) else {
        return;
    };
    let _ = (dmar.host_address_width(), dmar.flags());
    let room = table.bytes().len() - 48;
    let mut structures = 0;
    for structure in dmar.structures() {
        structures += 1;
        assert!(structures <= room / 4, "more structures than bytes allow");
        let scopes = match structure {
            Structure::Drhd(drhd) => drhd.device_scopes(),
            Structure::Rmrr(rmrr) => {
                if let Some(size) = rmrr.size() {
                    assert_eq!(rmrr.base.checked_add(size - 1), Some(rmrr.limit));
                }
                rmrr.device_scopes()
            }
            Structure::Malformed { kind, length } => {
                let minimum = if kind == 0 { 16 } else { 24 };
                assert!(kind <= 1 && usize::from(length) < minimum);
                continue;
            }
            Structure::Unknown { kind, .. } => {
                assert!(kind > 1, "a known structure reported as unknown");
                continue;
            }
        };
        let mut count = 0;
        for scope in scopes {
            count += 1;
            assert!(count <= room / 6, "more scopes than bytes allow");
            assert!(
                scope.path().count() <= room / 2,
                "more hops than bytes allow"
            );
            // A single hop names a function on the start bus, and only then.
            if let Some((bus, device, function)) = scope.endpoint() {
                assert_eq!(bus, scope.start_bus);
                assert!(
                    scope.path().eq([(device, function)]),
                    "an endpoint is not its path"
                );
            }
        }
    }
}

fn check_iort(table: Table<'_>) {
    let Ok(iort) = Iort::parse(table) else {
        return;
    };
    let length = table.bytes().len();
    let mut previous = None;
    let mut count = 0usize;
    for node in iort.nodes() {
        count += 1;
        assert!(count <= iort.node_count() as usize && count <= length / NODE_HEADER_LEN);
        assert_eq!(
            iort.node_at(node.offset),
            Some(node),
            "a node read differently at its offset"
        );
        if let Some(earlier) = previous {
            assert!(node.offset > earlier, "nodes did not advance");
        }
        previous = Some(node.offset);
        let _ = (node.smmu_v3(), node.root_complex());

        let mappings: Vec<_> = node.id_mappings().collect();
        assert!(mappings.len() <= length / 20);
        for mapping in mappings.iter().take(64) {
            for id in [
                mapping.input_base,
                mapping.input_base.wrapping_add((mapping.count - 1) as u32),
            ] {
                let first = mappings.iter().find_map(|candidate| {
                    candidate
                        .translate(id)
                        .map(|out| (out, candidate.output_reference))
                });
                assert_eq!(
                    node.translate(id),
                    first,
                    "not the first mapping covering {id}"
                );
            }
        }
    }
}

fn check_root(table: Table<'_>, kind: RootKind) {
    let root = RootTable::parse(table, kind).expect("the signature selected the kind");
    let entries: Vec<u64> = root.entries().collect();
    assert_eq!(entries.len(), root.len());
    assert_eq!(root.len(), table.body().len() / kind.entry_width());
    assert_eq!(root.is_empty(), entries.is_empty());
    for (index, address) in entries.iter().enumerate().take(64) {
        assert_eq!(root.entry(index), Some(*address));
    }
    assert_eq!(root.entry(entries.len()), None);
}

/// The walk the kernel makes, from an RSDP at address zero.
fn check_from_rsdp(data: &[u8], memory: &Memory<'_>) {
    let Ok(rsdp) = Rsdp::read(memory, 0) else {
        return;
    };
    let sum = |bytes: &[u8]| bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    assert_eq!(rsdp.checksum_valid, sum(&data[..RSDP_V1_LEN]) == 0);
    if rsdp.revision < 2 {
        assert_eq!((rsdp.length, rsdp.xsdt_address), (RSDP_V1_LEN as u32, 0));
        assert!(rsdp.extended_checksum_valid);
    }
    let Ok(acpi) = Acpi::from_rsdp(memory, &rsdp) else {
        assert_eq!(
            (
                rsdp.rsdt_address,
                rsdp.revision < 2 || rsdp.xsdt_address == 0
            ),
            (0, true)
        );
        return;
    };
    let Ok(root) = acpi.root() else {
        return;
    };
    check_table(data, *root.table());

    let listed: Vec<Table<'_>> = root
        .entries()
        .take(MAX_TABLES)
        .filter_map(|address| read_table(memory, address).ok())
        .collect();
    for table in &listed {
        check_table(data, *table);
    }
    if root.len() <= MAX_TABLES {
        for signature in SIGNATURES {
            let first = listed.iter().find(|table| table.signature() == *signature);
            match (acpi.find(*signature), first) {
                (Ok(found), Some(first)) => {
                    assert!(
                        core::ptr::eq(found.bytes(), first.bytes()),
                        "not the first table"
                    );
                }
                (Err(_), None) => {}
                (found, first) => panic!("find gave {found:?}, the walk {first:?}"),
            }
        }
        let _ = (
            acpi.madt(),
            acpi.fadt(),
            acpi.gtdt(),
            acpi.hpet(),
            acpi.mcfg(),
        );
        let _ = (acpi.dmar(), acpi.iort());
    }
}

fuzz_target!(|data: &[u8]| {
    let memory = Memory(data);
    if data.starts_with(&RSDP_SIGNATURE) {
        check_from_rsdp(data, &memory);
    }
    let offsets = (0..data.len().saturating_sub(3))
        .filter(|&at| {
            SIGNATURES
                .iter()
                .any(|signature| data[at..at + 4] == signature[..])
        })
        .take(MAX_TABLES);
    for at in offsets {
        if let Ok(table) = Table::parse(&data[at..]) {
            check_table(data, table);
        }
    }
});
