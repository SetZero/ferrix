//! The messages, byte for byte.

use super::{
    DEVICES, DEVICES_HEADER_BYTES, DEVICES_MAX_BYTES, DIED, Devices, DevicesView, FLAG_FIRST,
    MAX_DRIVERS, Malformed, Message, NAME_BYTES, PUBLISHED, REPORT, RESTARTED, SHORT_BYTES,
};

fn name(text: &str) -> [u8; NAME_BYTES] {
    let mut name = [0; NAME_BYTES];
    name[..text.len()].copy_from_slice(text.as_bytes());
    name
}

#[test]
fn devices_round_trips_and_counts_its_handles() {
    let names = [name("blk"), name("gpu")];
    let message = Devices {
        devices: 6,
        more: 30,
        first: true,
        names: &names,
    };
    let mut bytes = [0_u8; DEVICES_MAX_BYTES];
    let len = message.encode(&mut bytes).expect("fits");
    assert_eq!(len, DEVICES_HEADER_BYTES + 2 * NAME_BYTES, "length");
    assert_eq!(
        message.handles(),
        1 + 12 + 2,
        "job, two per device, one per driver"
    );
    assert_eq!(bytes[0..4], DEVICES.to_le_bytes(), "type");
    assert_eq!(bytes[4..8], (len as u32).to_le_bytes(), "length");
    assert_eq!(bytes[8..12], 6_u32.to_le_bytes(), "devices");
    assert_eq!(bytes[12..16], 2_u32.to_le_bytes(), "drivers");
    assert_eq!(bytes[16..20], 30_u32.to_le_bytes(), "more");
    assert_eq!(bytes[20..24], FLAG_FIRST.to_le_bytes(), "flags");
    assert_eq!(&bytes[24..27], b"blk", "first name");
    assert_eq!(bytes[27], 0, "NUL-padded");
    assert_eq!(&bytes[56..59], b"gpu", "second name");

    let view = DevicesView::decode(&bytes[..len]).expect("decodes");
    assert_eq!((view.devices, view.drivers, view.more), (6, 2, 30));
    assert!(view.first, "first");
    assert_eq!(view.handles(), message.handles());
    assert_eq!(view.name(0), Some(name("blk")));
    assert_eq!(view.name_bytes(1), Some(&b"gpu"[..]));
    assert_eq!(view.name(2), None, "no third driver");
}

#[test]
fn a_continuation_carries_devices_only() {
    let message = Devices {
        devices: 30,
        more: 0,
        first: false,
        names: &[],
    };
    let mut bytes = [0_u8; DEVICES_MAX_BYTES];
    let len = message.encode(&mut bytes).expect("fits");
    assert_eq!(len, DEVICES_HEADER_BYTES);
    assert_eq!(message.handles(), 60, "two per device and nothing else");
    let view = DevicesView::decode(&bytes[..len]).expect("decodes");
    assert!(!view.first);
    assert_eq!(view.handles(), 60);
    let named = [name("blk")];
    assert_eq!(
        Devices {
            devices: 1,
            more: 0,
            first: false,
            names: &named
        }
        .encode(&mut bytes),
        None,
        "names ride only in the first message"
    );
}

#[test]
fn devices_refuses_too_many_drivers_and_a_small_buffer() {
    let names = [name("x"); MAX_DRIVERS + 1];
    let mut bytes = [0_u8; DEVICES_MAX_BYTES + NAME_BYTES];
    assert_eq!(
        Devices {
            devices: 0,
            more: 0,
            first: true,
            names: &names
        }
        .encode(&mut bytes),
        None,
        "over MAX_DRIVERS"
    );
    let one = [name("blk")];
    let mut small = [0_u8; DEVICES_HEADER_BYTES];
    assert_eq!(
        Devices {
            devices: 1,
            more: 0,
            first: true,
            names: &one
        }
        .encode(&mut small),
        None,
        "a buffer without room for the name"
    );
}

#[test]
fn the_short_messages_round_trip_and_are_sixteen_bytes() {
    for message in [
        Message::Report {
            started: 1,
            failed: 0,
        },
        Message::Published {
            location: 0x0001_0318,
        },
        Message::Died {
            location: 0x0001_0318,
            status: -1,
        },
        Message::Restarted {
            location: 0x0001_0318,
            restarts: 2,
        },
    ] {
        let bytes = message.encode();
        assert_eq!(
            bytes[4..8],
            (SHORT_BYTES as u32).to_le_bytes(),
            "{message:?}"
        );
        assert_eq!(Message::decode(&bytes), Ok(message), "{message:?}");
    }
    let died = Message::Died {
        location: 7,
        status: 137,
    }
    .encode();
    assert_eq!(died[0..4], DIED.to_le_bytes());
    assert_eq!(died[12..16], 137_u32.to_le_bytes(), "status");
    assert_eq!(
        Message::Report {
            started: 2,
            failed: 3
        }
        .encode()[0..4],
        REPORT.to_le_bytes()
    );
    assert_eq!(
        Message::Published { location: 0 }.encode()[0..4],
        PUBLISHED.to_le_bytes()
    );
    let restarted = Message::Restarted {
        location: 7,
        restarts: 3,
    }
    .encode();
    assert_eq!(restarted[0..4], RESTARTED.to_le_bytes());
    assert_eq!(restarted[12..16], 3_u32.to_le_bytes(), "restarts");
}

#[test]
fn bytes_that_are_not_a_message_are_malformed() {
    assert_eq!(Message::decode(&[1, 0, 0]), Err(Malformed::Short));
    assert_eq!(
        Message::decode(&[9, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        Err(Malformed::UnknownType(9))
    );
    assert_eq!(
        Message::decode(&[2, 0, 0, 0, 16, 0, 0, 0]),
        Err(Malformed::Length),
        "a short REPORT"
    );
    let devices = Devices {
        devices: 1,
        more: 0,
        first: true,
        names: &[],
    };
    let mut bytes = [0_u8; DEVICES_MAX_BYTES];
    let len = devices.encode(&mut bytes).expect("fits");
    assert_eq!(
        Message::decode(&bytes[..len]),
        Err(Malformed::UnknownType(DEVICES)),
        "DEVICES has its own decoder"
    );
    assert_eq!(
        DevicesView::decode(&Message::Published { location: 0 }.encode()),
        Err(Malformed::UnknownType(PUBLISHED))
    );
    let mut lying = bytes;
    lying[12..16].copy_from_slice(&1_u32.to_le_bytes());
    assert_eq!(
        DevicesView::decode(&lying[..len]),
        Err(Malformed::Length),
        "a name count the bytes do not carry"
    );
}
