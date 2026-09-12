//! Fuzz PCI configuration space: the bus walk, BARs, capabilities, virtio.
//!
//! From stage 10 the kernel walks every bus firmware describes and sizes every
//! BAR it finds, in ring 0, with `overflow-checks` on. What it reads was
//! written by devices, and on a bridge nobody configured, by nothing at all.
//!
//! # The input
//!
//! A sequence of functions, each `bus, device, function`, six BAR masks, and
//! 512 bytes of configuration space — the legacy 256 and the first 256 of the
//! extended space. Bus numbers are taken modulo 8 so that inputs collide into
//! walks with bridges rather than scattering across 256 empty buses. Anything
//! not supplied reads as all ones, the way an empty slot does.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **The walk ends, and finds each function at most once**, however the
//!    bridges point.
//! 2. **Both capability lists end** within the number of entries their part
//!    of the space can hold.
//! 3. **Sizing leaves every register as it found it** and never probes a BAR
//!    while its function is decoding. The BARs are first made consistent with
//!    their masks — hardware cannot hold an address bit its aperture covers —
//!    since restoring an impossible value is not a property anything can have.
//! 4. **A size is a power of two, and its address a multiple of it.**
//! 5. **A BAR's mappable and withheld ranges partition it**: in order,
//!    disjoint, inside the BAR, together covering every byte exactly once,
//!    and every withheld range starting on a page boundary and ending on one
//!    or at the BAR's end — so nothing an MSI-X table touches is mappable.

#![no_main]

use std::collections::{BTreeMap, BTreeSet};

use ferrix_pci::bar::{self, Region};
use ferrix_pci::capability::{Capabilities, ExtendedCapabilities, ID_MSIX, MsiX};
use ferrix_pci::header::{COMMAND, HEADER_TYPE, HeaderKind};
use ferrix_pci::msix;
use ferrix_pci::virtio::Transport;
use ferrix_pci::walk::Walk;
use ferrix_pci::{Address, ConfigSpace};
use libfuzzer_sys::fuzz_target;

/// Bytes of space each function is given; the rest reads zero.
const SPACE: usize = 512;

/// The most functions one input may describe.
const MAX_FUNCTIONS: usize = 32;

/// One function: its space and which BAR bits it implements.
#[derive(Clone)]
struct Function {
    bytes: [u8; SPACE],
    masks: [u32; 6],
}

impl Function {
    fn get(&self, at: usize, width: usize) -> Option<u64> {
        if at >= 4096 || width > 4096 - at {
            return None;
        }
        let mut value = 0_u64;
        for i in 0..width {
            let byte = self.bytes.get(at + i).copied().unwrap_or(0);
            value |= u64::from(byte) << (8 * i);
        }
        Some(value)
    }

    fn put(&mut self, at: usize, width: usize, value: u64) {
        for i in 0..width {
            if let Some(byte) = self.bytes.get_mut(at + i) {
                *byte = (value >> (8 * i)) as u8;
            }
        }
    }

    /// The BAR slot `at` addresses, if it is one on this header.
    fn bar_slot(&self, at: usize) -> Option<usize> {
        let slots = usize::from(
            HeaderKind::from_register(self.bytes[usize::from(HEADER_TYPE)]).bar_slots(),
        );
        let slot = at.checked_sub(0x10)? / 4;
        (at % 4 == 0 && slot < slots).then_some(slot)
    }
}

#[derive(Default)]
struct Bus {
    functions: BTreeMap<Address, Function>,
    probed_while_decoding: bool,
}

impl ConfigSpace for Bus {
    fn read8(&self, function: Address, offset: u16) -> u8 {
        self.read(function, offset, 1) as u8
    }
    fn read16(&self, function: Address, offset: u16) -> u16 {
        self.read(function, offset, 2) as u16
    }
    fn read32(&self, function: Address, offset: u16) -> u32 {
        self.read(function, offset, 4) as u32
    }
    fn write16(&mut self, function: Address, offset: u16, value: u16) {
        self.write(function, offset, 2, u32::from(value));
    }
    fn write32(&mut self, function: Address, offset: u16, value: u32) {
        self.write(function, offset, 4, value);
    }
}

impl Bus {
    fn read(&self, function: Address, offset: u16, width: usize) -> u64 {
        let ones = u64::MAX >> (64 - 8 * width);
        self.functions
            .get(&function)
            .and_then(|f| f.get(usize::from(offset), width))
            .unwrap_or(ones)
    }

    fn write(&mut self, function: Address, offset: u16, width: usize, value: u32) {
        let Some(f) = self.functions.get_mut(&function) else {
            return;
        };
        let at = usize::from(offset);
        match (width, f.bar_slot(at)) {
            (4, Some(slot)) => {
                if f.get(usize::from(COMMAND), 2).unwrap_or(0) & 0x3 != 0 {
                    self.probed_while_decoding = true;
                }
                let old = f.get(at, 4).unwrap_or(0) as u32;
                let mask = f.masks[slot];
                f.put(at, 4, u64::from((value & mask) | (old & !mask & 0xF)));
            }
            _ => f.put(at, width, u64::from(value)),
        }
    }
}

struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn byte(&mut self) -> u8 {
        match self.0.split_first() {
            Some((&b, rest)) => {
                self.0 = rest;
                b
            }
            None => 0,
        }
    }

    fn u32(&mut self) -> u32 {
        u32::from_le_bytes([self.byte(), self.byte(), self.byte(), self.byte()])
    }
}

fn build(data: &[u8]) -> Bus {
    let mut input = Input(data);
    let mut bus = Bus::default();
    while !input.0.is_empty() && bus.functions.len() < MAX_FUNCTIONS {
        let (b, d, f) = (input.byte() % 8, input.byte() % 32, input.byte() % 8);
        let mut function = Function {
            bytes: [0; SPACE],
            masks: [0; 6],
        };
        for mask in &mut function.masks {
            *mask = input.u32();
        }
        for byte in &mut function.bytes {
            *byte = input.byte();
        }
        // Hardware cannot hold an address bit its BAR does not implement.
        for slot in 0..6 {
            let at = 0x10 + 4 * slot;
            let value = function.get(at, 4).unwrap_or(0) as u32;
            function.put(at, 4, u64::from(value & (function.masks[slot] | 0xF)));
        }
        let address = Address::new(0, b, d, f).expect("reduced into range");
        let _ = bus.functions.insert(address, function);
    }
    bus
}

fuzz_target!(|data: &[u8]| {
    let mut bus = build(data);

    let mut found = BTreeSet::new();
    let mut functions = Vec::new();
    for item in Walk::new(&bus, 0, 0..=255) {
        if let Ok(function) = item {
            assert!(
                found.insert(function.address),
                "{} found twice",
                function.address
            );
            functions.push(function);
        }
    }
    assert!(
        found.len() <= bus.functions.len(),
        "found functions nobody put there"
    );

    for function in functions {
        let address = function.address;
        assert!(
            Capabilities::new(&bus, address).take(65).count() <= 64,
            "the standard list outran its space"
        );
        assert!(
            ExtendedCapabilities::new(&bus, address).take(1025).count() <= 1024,
            "the extended list outran its space"
        );
        let table = match ferrix_pci::capability::find(&bus, address, ID_MSIX) {
            Ok(Some(capability)) => MsiX::read(&bus, capability).ok(),
            _ => None,
        };
        if let Some(table) = table {
            let _ = (table.table_len(), table.pending_len());
        }

        let kind = function.identity.kind;
        let before = bus.functions[&address].bytes;
        let mut regions: Vec<Region> = Vec::new();
        for index in 0..kind.bar_slots() {
            if let Ok(Some(region)) = bar::size(&mut bus, address, kind, index) {
                assert!(region.size.is_power_of_two(), "size {:#x}", region.size);
                assert_eq!(region.bar.address() % region.size, 0, "misaligned");
                let _ = region.last();
                regions.push(region);
            }
        }
        assert!(
            !bus.probed_while_decoding,
            "{address}: probed while decoding"
        );
        assert!(
            bus.functions[&address].bytes == before,
            "{address}: not restored"
        );

        for region in &regions {
            partition(region, table.as_ref());
        }

        if let Ok(Some(transport)) = Transport::find(&bus, address) {
            let _ = transport.verify(&regions);
        }
    }
});

/// Property 5, for one BAR.
fn partition(region: &Region, table: Option<&MsiX>) {
    const PAGE: u64 = 0x1000;
    let mappable = msix::mappable(region, table, PAGE);
    let withheld = msix::withheld(region, table, PAGE);
    let mut all: Vec<(u64, u64, bool)> = mappable
        .as_slice()
        .iter()
        .map(|&(offset, len)| (offset, len, false))
        .chain(
            withheld
                .as_slice()
                .iter()
                .map(|&(offset, len)| (offset, len, true)),
        )
        .collect();
    all.sort_unstable();
    let mut cursor = 0_u64;
    for (offset, len, is_withheld) in all {
        assert!(len > 0, "an empty range");
        assert_eq!(offset, cursor, "a gap or an overlap at {offset:#x}");
        let end = offset.checked_add(len).expect("a range wraps");
        assert!(end <= region.size, "a range past the BAR");
        if is_withheld {
            assert_eq!(offset % PAGE, 0, "withheld from mid-page");
            assert!(
                end % PAGE == 0 || end == region.size,
                "withheld to mid-page"
            );
        }
        cursor = end;
    }
    assert_eq!(cursor, region.size, "the ranges do not cover the BAR");
}
