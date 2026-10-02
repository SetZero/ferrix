//! Tests for the VT-d queued invalidation and interrupt remapping encodings,
//! against the specification's layouts and QEMU's reserved-bit masks: a
//! descriptor with a reserved bit set stops QEMU's queue (`IQE`), and one
//! with a field in the wrong place invalidates something other than what
//! was meant, which no boot would notice.

use super::queue::{self, Completion, ContextScope, InterruptEntryScope, IotlbScope, QUEUE_LENGTH};
use super::remap::{self, Remap, UnitState};

/// QEMU's `VTD_INV_DESC_CC_RSVD`.
const CC_RESERVED: u64 = 0xfffc_0000_0000_f1c0;
/// QEMU's `VTD_INV_DESC_IOTLB_RSVD_LO` and `_HI`.
const IOTLB_RESERVED: [u64; 2] = [0xffff_ffff_0000_f100, 0xf80];
/// QEMU's `VTD_INV_DESC_IEC_RSVD`.
const IEC_RESERVED: u64 = 0xffff_0000_07ff_f1e0;
/// QEMU's `VTD_INV_DESC_WAIT_RSVD_LO` and `_HI`.
const WAIT_RESERVED: [u64; 2] = [0xffff_f180, 0b11];

/// The type QEMU reads from a descriptor (`VTD_INV_DESC_TYPE`): bits 3:0,
/// and bits 11:9 as bits 6:4.
fn kind(low: u64) -> u64 {
    ((low >> 5) & 0x70) | (low & 0xF)
}

/// Verifies: `L.iommu.47`
#[test]
fn a_context_cache_invalidation_is_laid_out_as_the_specification_says() {
    let global = queue::context(ContextScope::Global);
    assert_eq!(kind(global[0]), 1);
    assert_eq!((global[0] >> 4) & 0b11, 1, "global granularity");
    assert_eq!(global[1], 0);

    let device = queue::context(ContextScope::Device {
        source: 0x0210,
        domain: 0x2A,
    });
    assert_eq!(kind(device[0]), 1);
    assert_eq!((device[0] >> 4) & 0b11, 3, "device-selective granularity");
    assert_eq!((device[0] >> 16) & 0xFFFF, 0x2A, "the domain in bits 31:16");
    assert_eq!(
        (device[0] >> 32) & 0xFFFF,
        0x0210,
        "the source in bits 47:32"
    );
    assert_eq!((device[0] >> 48) & 0b11, 0, "function mask 0: all 16 bits");
    for descriptor in [global, device] {
        assert_eq!(descriptor[0] & CC_RESERVED, 0, "a reserved bit is set");
    }
}

/// Verifies: `L.iommu.47`
#[test]
fn an_iotlb_invalidation_is_laid_out_as_the_specification_says() {
    let global = queue::iotlb(IotlbScope::Global);
    assert_eq!(kind(global[0]), 2);
    assert_eq!((global[0] >> 4) & 0b11, 1, "global granularity");

    let domain = queue::iotlb(IotlbScope::Domain(0xBEEF));
    assert_eq!(kind(domain[0]), 2);
    assert_eq!((domain[0] >> 4) & 0b11, 2, "domain-selective granularity");
    assert_eq!(
        (domain[0] >> 16) & 0xFFFF,
        0xBEEF,
        "the domain in bits 31:16"
    );
    for descriptor in [global, domain] {
        assert_eq!(descriptor[0] & IOTLB_RESERVED[0], 0, "a reserved low bit");
        assert_eq!(descriptor[1] & IOTLB_RESERVED[1], 0, "a reserved high bit");
        assert_eq!(descriptor[1], 0, "no address: not page-selective");
    }
}

/// Verifies: `L.iommu.47`
#[test]
fn an_interrupt_entry_invalidation_is_laid_out_as_the_specification_says() {
    let global = queue::interrupt_entry(InterruptEntryScope::Global);
    assert_eq!(kind(global[0]), 4);
    assert_eq!(global[0] & 1 << 4, 0, "granularity 0: global");

    for index in [0_u16, 63, 255, 0x8000] {
        let one = queue::interrupt_entry(InterruptEntryScope::Index(index));
        assert_eq!(kind(one[0]), 4);
        assert_ne!(one[0] & 1 << 4, 0, "granularity 1: index-selective");
        assert_eq!((one[0] >> 27) & 0x1F, 0, "index mask 0: one entry");
        assert_eq!(
            (one[0] >> 32) & 0xFFFF,
            u64::from(index),
            "the index in 47:32"
        );
        assert_eq!(one[0] & IEC_RESERVED, 0, "a reserved bit is set");
        assert_eq!(one[1], 0);
    }
}

/// Verifies: `L.iommu.47`, `L.iommu.48`
#[test]
fn a_wait_writes_its_sequence_number_or_nothing() {
    let written = queue::wait(Completion::Status {
        address: 0x1234_5000,
        data: 0xDEAD_BEEF,
    });
    assert_eq!(kind(written[0]), 5);
    assert_ne!(written[0] & 1 << 5, 0, "SW: the status is written");
    assert_eq!(written[0] & 1 << 4, 0, "IF clear: no completion event");
    assert_ne!(written[0] & 1 << 6, 0, "FN: fenced");
    assert_eq!(written[0] >> 32, 0xDEAD_BEEF, "the status data in 63:32");
    assert_eq!(written[1], 0x1234_5000, "the status address");

    let unwritten = queue::wait(Completion::Unwritten);
    assert_eq!(kind(unwritten[0]), 5);
    assert_eq!(unwritten[0] & 1 << 5, 0, "SW clear: nothing is written");
    // QEMU refuses a wait with neither SW nor IF as an unknown type, and
    // stops the queue on it.
    assert_ne!(
        unwritten[0] & 1 << 4,
        0,
        "IF set, so the descriptor is valid"
    );
    for descriptor in [written, unwritten] {
        assert_eq!(descriptor[0] & WAIT_RESERVED[0], 0, "a reserved low bit");
        assert_eq!(descriptor[1] & WAIT_RESERVED[1], 0, "a reserved high bit");
    }
}

/// Verifies: `L.iommu.47`
#[test]
fn the_queue_registers_name_slots_of_sixteen_bytes() {
    assert_eq!(
        queue::queue_address(0x7_6543_2000),
        0x7_6543_2000,
        "QS and DW 0"
    );
    assert_eq!(queue::queue_address(0x7_6543_2FFF) & 0xFFF, 0);
    assert_eq!(queue::slot_register(0), 0);
    assert_eq!(queue::slot_register(1), 0x10);
    assert_eq!(queue::slot_register(255), 0xFF0);
    assert_eq!(queue::slot_register(QUEUE_LENGTH), 0, "the queue wraps");
    for slot in 0..QUEUE_LENGTH {
        assert_eq!(queue::slot_of(queue::slot_register(slot)), slot);
    }
}

/// Verifies: `L.iommu.47`
#[test]
fn a_full_queue_is_never_mistaken_for_an_empty_one() {
    assert_eq!(queue::free_slots(0, 0), QUEUE_LENGTH - 1);
    assert_eq!(queue::free_slots(0, 3), QUEUE_LENGTH - 4);
    assert_eq!(queue::free_slots(5, 4), 0, "one slot is always left empty");
    assert_eq!(
        queue::free_slots(250, 2),
        QUEUE_LENGTH - 1 - 8,
        "across the wrap"
    );
}

/// Verifies: `L.iommu.48`
#[test]
fn a_queue_error_is_never_mistaken_for_a_completion() {
    use super::queue::{FSTS_ICE, FSTS_IQE, FSTS_ITE, FSTS_QUEUE_ERRORS, Outcome};
    assert_eq!(queue::outcome(0, true), Outcome::Completed);
    assert_eq!(queue::outcome(0, false), Outcome::TimedOut);
    assert_eq!(
        queue::outcome(1 << 0 | 1 << 1, true),
        Outcome::Completed,
        "PFO and PPF are DMA's"
    );
    assert_eq!(queue::outcome(FSTS_ICE, true), Outcome::CompletionError);
    assert_eq!(queue::outcome(FSTS_ICE, false), Outcome::CompletionError);
    for stopped in [FSTS_IQE, FSTS_ITE, FSTS_IQE | FSTS_ICE, FSTS_ITE | FSTS_ICE] {
        assert_eq!(queue::outcome(stopped, true), Outcome::QueueStopped);
    }
    assert_eq!(FSTS_QUEUE_ERRORS, 0b111 << 4, "IQE, ICE and ITE, bits 6:4");
}

/// QEMU's `VTD_IR_TableEntry` reserved fields: low bits 14:12 and 31:24,
/// high bits 63:20. An entry with one set faults 0x24.
const IRTE_RESERVED: [u64; 2] = [0x0000_0000_FF00_7000, 0xFFFF_FFFF_FFF0_0000];

/// Verifies: `L.iommu.53`, `L.x86_64.132`
#[test]
fn an_entry_carries_exactly_its_fixed_fields() {
    let destination = remap::destination(3).unwrap();
    let [low, high] = remap::entry(Remap {
        vector: 0x41,
        destination,
        level: false,
        source: 0x0200,
    });
    assert_eq!(low & 1, 1, "P");
    assert_eq!(low >> 1 & 1, 0, "FPD 0: faults are recorded");
    assert_eq!(low >> 2 & 1, 0, "DM 0: physical");
    assert_eq!(low >> 3 & 1, 0, "RH 0");
    assert_eq!(low >> 4 & 1, 0, "TM edge for a message");
    assert_eq!(
        low >> 5 & 0b111,
        0,
        "DLM fixed: never NMI, SMI, INIT, ExtINT"
    );
    assert_eq!(low >> 15 & 1, 0, "IM 0: remapped, never posted");
    assert_eq!(low >> 16 & 0xFF, 0x41, "the vector");
    assert_eq!(low >> 32, 3 << 8, "xAPIC destination in DST 15:8");
    assert_eq!(high & 0xFFFF, 0x0200, "SID");
    assert_eq!(high >> 16 & 0b11, 0, "SQ 00: all sixteen bits");
    assert_eq!(high >> 18 & 0b11, 0b01, "SVT 01: verify by SID");
    assert_eq!(low & IRTE_RESERVED[0], 0);
    assert_eq!(high & IRTE_RESERVED[1], 0);
    assert!(remap::is_present(low));
    assert!(!remap::is_present(low & !1));

    let [level, _] = remap::entry(Remap {
        vector: 0x40,
        destination,
        level: true,
        source: 0xFF00,
    });
    assert_eq!(level >> 4 & 1, 1, "TM level for a level-triggered line");
}

/// Verifies: `L.x86_64.132`
#[test]
fn a_destination_above_255_is_refused() {
    assert_eq!(remap::destination(0), Some(0));
    assert_eq!(remap::destination(255), Some(0xFF00));
    assert_eq!(remap::destination(256), None);
    assert_eq!(remap::destination(u32::MAX), None);
}

/// Verifies: `L.iommu.50`
#[test]
fn irta_holds_the_table_in_xapic_format_with_256_entries() {
    let value = remap::table_address(0x1_2345_6000);
    assert_eq!(value & !0xFFF, 0x1_2345_6000);
    assert_eq!(value & 1 << 11, 0, "EIME 0");
    assert_eq!(1u32 << ((value & 0xF) + 1), u32::from(remap::TABLE_ENTRIES));
}

/// Verifies: `L.x86_64.129`
#[test]
fn a_remappable_message_names_its_handle() {
    for handle in [0_u16, 63, 255, 0x7FFF, 0x8000, 0xFFFF] {
        let address = remap::message_address(handle);
        assert_eq!(address >> 20, 0xFEE, "the interrupt range");
        assert_eq!(address >> 4 & 1, 1, "format: remappable");
        assert_eq!(address >> 3 & 1, 0, "SHV 0: no subhandle");
        // QEMU's VTD_IR_MSIAddress: index_l 19:5, index_h bit 2.
        let index = (address >> 2 & 1) << 15 | (address >> 5 & 0x7FFF);
        assert_eq!(index, u64::from(handle));
        assert!(address < 1 << 32, "a 32-bit capability holds it");
    }
}

/// Verifies: `L.x86_64.130`
#[test]
fn a_remappable_redirection_entry_names_its_handle_and_keeps_the_vector() {
    for handle in [0_u16, 63, 0x8000] {
        let entry = remap::redirection_entry(handle, 0x44, false, false, true);
        assert_eq!(entry >> 48 & 1, 1, "format: remappable");
        assert_eq!(
            entry >> 49,
            u64::from(handle & 0x7FFF),
            "handle 14:0 in 63:49"
        );
        assert_eq!(entry >> 11 & 1, u64::from(handle >> 15), "handle 15 in 11");
        assert_eq!(entry >> 8 & 0b111, 0, "bits 10:8 clear");
        assert_eq!(entry & 0xFF, 0x44, "the entry's vector");
        assert_eq!(entry >> 16 & 1, 1, "masked");
        // QEMU's I/O APIC turns bits 63:48 into address bits 19:4 and bit
        // 11 into address bit 2: the remappable message for the handle.
        let address = 0xFEE0_0000 | (entry >> 48) << 4 | (entry >> 11 & 1) << 2;
        assert_eq!(address, remap::message_address(handle));
    }
    let level = remap::redirection_entry(1, 0x50, true, true, false);
    assert_eq!(level >> 15 & 1, 1, "level");
    assert_eq!(level >> 13 & 1, 1, "active low");
    assert_eq!(level >> 16 & 1, 0, "unmasked");
}

/// Verifies: `L.iommu.54`
#[test]
fn a_fault_record_tells_an_interrupt_from_dma() {
    assert!(!remap::is_interrupt_reason(0x05), "a DMA write fault");
    assert!(!remap::is_interrupt_reason(0x1F));
    for reason in 0x20..=0x26 {
        assert!(remap::is_interrupt_reason(reason));
    }
    assert!(!remap::is_interrupt_reason(0x27));
    // QEMU's vtd_report_ir_fault: FR in the high quadword's bits 39:32,
    // the dword at +12's bits 7:0; FI[63:48] the index.
    let flags = 1 << 31 | u32::from(remap::REASON_SOURCE);
    assert_eq!(remap::fault_reason(flags), 0x26);
    assert_eq!(remap::fault_index(0x0003_0000_0000_0000), 3);
}

/// Verifies: `L.iommu.55`
#[test]
fn interrupts_are_isolated_only_when_every_unit_blocks() {
    let blocking = UnitState {
        remapping: true,
        compatibility_blocked: true,
    };
    let refused = UnitState {
        remapping: false,
        compatibility_blocked: false,
    };
    let compatible = UnitState {
        remapping: true,
        compatibility_blocked: false,
    };
    assert!(remap::interrupts_isolated(&[blocking], 0));
    assert!(remap::interrupts_isolated(&[blocking, blocking], 0));
    assert!(
        !remap::interrupts_isolated(&[blocking, refused], 0),
        "a refused unit"
    );
    assert!(
        !remap::interrupts_isolated(&[blocking, compatible], 0),
        "CFIS 1"
    );
    assert!(
        !remap::interrupts_isolated(&[blocking], 1),
        "a function bypassing"
    );
    assert!(!remap::interrupts_isolated(&[], 0), "no unit at all");
}
