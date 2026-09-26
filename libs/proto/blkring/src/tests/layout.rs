//! Every field at the offset the specification gives it.

use super::support::{Shared, Who, device};
use crate::driver::DriverSide;
use crate::layout::{
    COMPLETION_BYTES, HEADER_BYTES, HeaderError, RawCompletion, RawSubmission, RingLayout,
    SUBMISSION_BYTES, completion, header, submission,
};

#[test]
fn header_fields_sit_where_the_specification_puts_them() {
    let offsets = [
        header::MAGIC,
        header::VERSION,
        header::FLAGS,
        header::ENTRIES,
        header::SUB_OFFSET,
        header::COMP_OFFSET,
        header::SUB_TAIL,
        header::SUB_HEAD,
        header::COMP_TAIL,
        header::COMP_HEAD,
        header::SUB_WANT_BELL,
        header::COMP_WANT_BELL,
        header::RESERVED,
    ];
    assert_eq!(
        offsets,
        [0, 4, 6, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44],
        "header offsets"
    );
    assert_eq!(
        header::RESERVED + header::RESERVED_BYTES,
        HEADER_BYTES,
        "reserved runs to the end"
    );
    assert_eq!(HEADER_BYTES, 64, "header size");
}

#[test]
fn a_fresh_header_is_written_at_those_offsets() {
    let layout = RingLayout::standard(8).expect("valid");
    assert_eq!(
        layout.ring_bytes(),
        64 + 8 * 32 + 8 * 24,
        "standard ring size"
    );
    let shared = Shared::new(layout.ring_bytes());
    shared.poke(0, &[0xAA; 64]);
    let _driver = DriverSide::new(
        shared.memory(Who::Driver, layout),
        layout.ring_bytes(),
        layout,
        device(),
    )
    .expect("fits");
    let bytes = shared.peek(0, 64);
    assert_eq!(&bytes[0..4], b"FXBR", "magic");
    assert_eq!(bytes[4..6], 1_u16.to_le_bytes(), "version");
    assert_eq!(bytes[6..8], [0, 0], "flags");
    assert_eq!(bytes[8..12], 8_u32.to_le_bytes(), "entries");
    assert_eq!(bytes[12..16], 64_u32.to_le_bytes(), "sub_offset");
    assert_eq!(
        bytes[16..20],
        (64 + 8 * 32_u32).to_le_bytes(),
        "comp_offset"
    );
    assert!(
        bytes[20..64].iter().all(|&byte| byte == 0),
        "indices, want-bells and reserved bytes start at zero"
    );
}

#[test]
fn a_submission_is_32_bytes_laid_out_as_specified() {
    let offsets = [
        submission::ID,
        submission::SECTOR,
        submission::DATA_OFFSET,
        submission::COUNT,
        submission::OP,
        submission::FLAGS,
        submission::RESERVED,
    ];
    assert_eq!(offsets, [0, 8, 16, 24, 28, 29, 30], "submission offsets");
    assert_eq!(SUBMISSION_BYTES, 32, "submission size");

    let layout = RingLayout::standard(2).expect("valid");
    let shared = Shared::new(layout.ring_bytes());
    let mut memory = shared.memory(Who::Kernel, layout);
    let raw = RawSubmission {
        id: 0x0102_0304_0506_0708,
        sector: 0x1112_1314_1516_1718,
        data_offset: 0x2122_2324_2526_2728,
        count: 0x3132_3334,
        op: 0x41,
        flags: 0x51,
        reserved: 0x6162,
    };
    let at = layout.submission_at(1);
    assert_eq!(at, 64 + 32, "the second slot");
    raw.write_to(&mut memory, at);
    let bytes = shared.peek(at, 32);
    assert_eq!(bytes[0..8], raw.id.to_le_bytes(), "id");
    assert_eq!(bytes[8..16], raw.sector.to_le_bytes(), "sector");
    assert_eq!(bytes[16..24], raw.data_offset.to_le_bytes(), "data_offset");
    assert_eq!(bytes[24..28], raw.count.to_le_bytes(), "count");
    assert_eq!(bytes[28..30], [0x41, 0x51], "op and flags");
    assert_eq!(bytes[30..32], raw.reserved.to_le_bytes(), "reserved");
    assert_eq!(
        shared.peek(at - 32, 32),
        [0; 32],
        "nothing before the entry"
    );
    assert_eq!(shared.peek(at + 32, 8), [0; 8], "nothing after the entry");
    assert_eq!(RawSubmission::read_from(&memory, at), raw, "round trip");
}

#[test]
fn a_completion_is_24_bytes_laid_out_as_specified() {
    let offsets = [
        completion::ID,
        completion::BYTES_DONE,
        completion::STATUS,
        completion::RESERVED,
    ];
    assert_eq!(offsets, [0, 8, 16, 20], "completion offsets");
    assert_eq!(COMPLETION_BYTES, 24, "completion size");

    let layout = RingLayout::standard(2).expect("valid");
    let shared = Shared::new(layout.ring_bytes());
    let mut memory = shared.memory(Who::Driver, layout);
    let raw = RawCompletion {
        id: 0x0102_0304_0506_0708,
        bytes_done: 0x1112_1314_1516_1718,
        status: 0x2122_2324,
        reserved: 0x3132_3334,
    };
    let at = layout.completion_at(0);
    assert_eq!(at, 64 + 2 * 32, "the completions follow the submissions");
    raw.write_to(&mut memory, at);
    let bytes = shared.peek(at, 24);
    assert_eq!(bytes[0..8], raw.id.to_le_bytes(), "id");
    assert_eq!(bytes[8..16], raw.bytes_done.to_le_bytes(), "bytes_done");
    assert_eq!(bytes[16..20], raw.status.to_le_bytes(), "status");
    assert_eq!(bytes[20..24], raw.reserved.to_le_bytes(), "reserved");
    assert_eq!(shared.peek(at + 24, 24), [0; 24], "nothing after the entry");
    assert_eq!(RawCompletion::read_from(&memory, at), raw, "round trip");
}

#[test]
fn indices_wrap_onto_slots_modulo_entries() {
    let layout = RingLayout::standard(8).expect("valid");
    for index in [0_u32, 3, 7, 8, 11, u32::MAX] {
        let slot = (index % 8) as usize;
        assert_eq!(
            layout.submission_at(index),
            64 + slot * 32,
            "submission {index}"
        );
        assert_eq!(
            layout.completion_at(index),
            64 + 8 * 32 + slot * 24,
            "completion {index}"
        );
    }
}

#[test]
fn a_layout_must_fit_its_ring_and_keep_its_arrays_apart() {
    let fits = RingLayout::standard(4).expect("valid").ring_bytes();
    assert_eq!(fits, 288, "64 + 4 × 32 + 4 × 24");
    for entries in [0, 1, 3, 6, 8192, u32::MAX] {
        assert_eq!(
            RingLayout::new(entries, 64, 4096, 1 << 20),
            Err(HeaderError::BadEntries),
            "entries {entries}"
        );
    }
    assert!(
        RingLayout::new(4096, 64, 64 + 4096 * 32, 1 << 20).is_ok(),
        "4096 entries"
    );
    let cases = [
        (63, 192, fits, HeaderError::OverlapsHeader),
        (64, 0, fits, HeaderError::OverlapsHeader),
        (64, 192, fits - 1, HeaderError::OutsideRing),
        (64, 191, 1 << 20, HeaderError::ArraysOverlap),
        (200, 105, 1 << 20, HeaderError::ArraysOverlap),
    ];
    for (sub, comp, ring, error) in cases {
        assert_eq!(
            RingLayout::new(4, sub, comp, ring),
            Err(error),
            "sub {sub} comp {comp} ring {ring}"
        );
    }
    assert!(
        RingLayout::new(4, 160, 64, fits).is_ok(),
        "completions first, touching"
    );
    assert!(
        RingLayout::new(4, 64, 192, fits).is_ok(),
        "the standard layout, touching"
    );
}
