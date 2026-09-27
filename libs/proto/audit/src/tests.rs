use super::{CONFIG, Class, Event, MAX_RING, NO_UID, RECORD_BYTES, Record, START, SUPPRESSED};

/// Ids that exercise every word of the 128 bits.
const IDS: [u128; 5] = [
    0,
    1,
    u128::MAX,
    0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210,
    1 << 127 | 1 << 95 | 1 << 63 | 1 << 31,
];

#[test]
fn a_start_up_record_carries_the_whole_id_and_both_ring_lengths() {
    for id in IDS {
        for (high, refusals) in [(512, 4096), (0, 0), (MAX_RING, MAX_RING), (1, MAX_RING)] {
            let record = Record::start(id, high, refusals).expect("lengths that fit");
            assert!(record.is(START));
            assert_eq!(record.start_fields(), Some((id, high, refusals)));
            let read_back = Record::from_bytes(&record.to_bytes());
            assert_eq!(read_back.start_fields(), Some((id, high, refusals)));
        }
    }
}

#[test]
fn a_ring_longer_than_a_half_word_is_refused_rather_than_cut() {
    assert_eq!(Record::start(7, MAX_RING + 1, 1), None);
    assert_eq!(Record::start(7, 1, MAX_RING + 1), None);
}

#[test]
fn only_a_start_up_record_says_what_a_start_up_record_says() {
    let mut record = Record::start(9, 4, 8).expect("lengths that fit");
    record.code = CONFIG.code;
    assert_eq!(record.start_fields(), None);
    record.code = START.code;
    record.class = Class::Granted as u16;
    assert_eq!(record.start_fields(), None);
}

#[test]
fn codes_are_numbered_within_a_class() {
    let grant = Event::new(Class::Granted, START.code);
    let mut record = Record::EMPTY;
    record.class = Class::Granted as u16;
    record.code = grant.code;
    assert!(record.is(grant));
    assert!(!record.is(START));
    record.class = Class::Refused as u16;
    record.code = SUPPRESSED.code;
    assert!(record.is(SUPPRESSED));
}

#[test]
fn a_record_is_its_sixty_four_bytes_in_order_little_endian() {
    let mut record = Record {
        sequence: 0x0807_0605_0403_0201,
        time: 0x100F_0E0D_0C0B_0A09,
        class: 0x1211,
        code: 0x1413,
        outcome: 0x1615,
        status: 0x1817,
        pid: 0x1C1B_1A19,
        uid: NO_UID,
        job: 0x2827_2625_2423_2221,
        target_kind: 0x2C2B_2A29,
        target_id: [0, 0],
        detail: [0x3635_3433, 0x3A39_3837, 0x3E3D_3C3B],
    };
    record.set_target(0x3433_3231_302F_2E2D);
    let bytes = record.to_bytes();
    assert_eq!(bytes.len(), RECORD_BYTES);
    assert_eq!(bytes[..8], [1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(
        bytes[16..24],
        [0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18]
    );
    assert_eq!(bytes[28..32], [0xFF; 4]);
    assert_eq!(
        bytes[44..52],
        [0x2D, 0x2E, 0x2F, 0x30, 0x31, 0x32, 0x33, 0x34]
    );
    assert_eq!(Record::from_bytes(&bytes), record);
    assert_eq!(record.target(), 0x3433_3231_302F_2E2D);
}

#[test]
fn a_negative_status_survives_the_bytes() {
    let mut record = Record::EMPTY;
    record.status = -13;
    assert_eq!(Record::from_bytes(&record.to_bytes()).status, -13);
}
