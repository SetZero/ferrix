//! The serial port's bytes, both ways, once the host has enumerated it.

use std::vec;
use std::vec::Vec;

use ferrix_usb_device::acm::{DATA_IN, DATA_OUT, NOTIFY_IN, serial_state};

use super::rig::Rig;
use crate::layout::ENDPOINT_BYTES;

/// Physical endpoints: 0x81, 0x02, 0x82.
const NOTIFY: usize = 3;
const OUT: usize = 4;
const IN: usize = 5;

fn enumerated(high_speed: bool) -> Rig {
    let mut rig = Rig::new();
    let _ = rig.enumerate(high_speed);
    rig.notice = crate::Notice::default();
    rig
}

fn reads(rig: &Rig, physical: usize) -> Vec<Vec<u8>> {
    rig.model.borrow().in_reads[physical].clone()
}

fn counting(length: usize) -> Vec<u8> {
    (0..length).map(|at| (at % 251) as u8).collect()
}

#[test]
fn bytes_to_the_host() {
    let mut rig = enumerated(true);
    assert_eq!(rig.controller.write(DATA_IN, b"hello\n"), Ok(6), "taken");
    rig.pump();
    assert!(rig.notice.sent, "sent");
    assert_eq!(reads(&rig, IN), [b"hello\n".to_vec()], "the host read them");
    assert_eq!(rig.controller.pending(DATA_IN), 0, "nothing waits");
    rig.assert_clean();
}

#[test]
fn a_whole_number_of_packets_ends_with_a_zero_length_packet() {
    let mut rig = enumerated(true);
    let bytes = counting(1024);
    assert_eq!(rig.controller.write(DATA_IN, &bytes), Ok(1024), "taken");
    rig.pump();
    // Without the zero-length packet the host's read would still be open.
    assert_eq!(reads(&rig, IN), [bytes], "one read of two packets");
    rig.assert_clean();
}

#[test]
fn the_ring_wraps_and_fills() {
    let mut rig = enumerated(true);
    let first = counting(3000);
    assert_eq!(rig.controller.write(DATA_IN, &first), Ok(3000), "3000");
    // The transfer is started; the ring holds the rest of a page.
    assert_eq!(
        rig.controller.write(DATA_IN, &counting(2000)),
        Ok(ENDPOINT_BYTES - 3000),
        "only what fits"
    );
    assert_eq!(rig.controller.write(DATA_IN, b"x"), Ok(0), "full");
    rig.pump();
    let second = counting(3000);
    assert_eq!(
        rig.controller.write(DATA_IN, &second),
        Ok(3000),
        "room again, wrapping"
    );
    rig.pump();
    let got: Vec<u8> = reads(&rig, IN).concat();
    let mut sent = first;
    sent.extend_from_slice(&counting(ENDPOINT_BYTES - 3000));
    sent.extend_from_slice(&second);
    assert_eq!(got, sent, "everything, in order");
    rig.assert_clean();
}

#[test]
fn a_write_into_an_empty_ring_is_one_transfer() {
    // adb's host reads a message a part at a time, so a part split at the
    // ring's end would be taken for the whole of it (run adbusb2).
    let mut rig = enumerated(true);
    assert_eq!(rig.controller.write(DATA_IN, &counting(3000)), Ok(3000));
    rig.pump();
    assert_eq!(rig.controller.pending(DATA_IN), 0, "all gone");
    let part = counting(2000);
    assert_eq!(rig.controller.write(DATA_IN, &part), Ok(2000));
    rig.pump();
    let transfers = reads(&rig, IN);
    assert_eq!(
        transfers.last().map(Vec::len),
        Some(2000),
        "one transfer of 2000, not 1096 and 904: {:?}",
        transfers.iter().map(Vec::len).collect::<Vec<_>>()
    );
    rig.assert_clean();
}

#[test]
fn an_endpoint_told_so_ends_whole_packets_without_a_zero_length_packet() {
    // adb's host reads a payload of exactly the length its header gave,
    // and an empty packet after it would land where it reads the next
    // header (run adbusb3's pull).
    let mut rig = enumerated(true);
    rig.controller.set_zero_length_packets(DATA_IN, false);
    assert_eq!(rig.controller.write(DATA_IN, &counting(1024)), Ok(1024));
    rig.pump();
    // The model's host reads as a serial port's does, until a short packet,
    // so with no empty packet sent its read is still open, holding both.
    assert!(reads(&rig, IN).is_empty(), "no empty packet ended the read");
    assert_eq!(
        rig.model.borrow().in_partial[IN].len(),
        1024,
        "both packets"
    );
    rig.controller.set_zero_length_packets(DATA_IN, true);
    assert_eq!(rig.controller.write(DATA_IN, &counting(512)), Ok(512));
    rig.pump();
    assert_eq!(
        reads(&rig, IN).iter().map(Vec::len).collect::<Vec<_>>(),
        [1536],
        "back on, the next whole packet ends with an empty one"
    );
    rig.assert_clean();
}

#[test]
fn bytes_from_the_host() {
    let mut rig = enumerated(true);
    rig.model.borrow_mut().host_write(OUT, b"ls -l\n", 512);
    rig.pump();
    assert!(rig.notice.received, "received");
    let mut buffer = [0; 64];
    assert_eq!(
        rig.controller.read(DATA_OUT, &mut buffer),
        Ok(6),
        "six bytes"
    );
    assert_eq!(&buffer[..6], b"ls -l\n", "these");
    assert_eq!(rig.controller.read(DATA_OUT, &mut buffer), Ok(0), "no more");
    rig.assert_clean();
}

#[test]
fn a_whole_packet_from_the_host_arrives_without_a_zero_length_packet() {
    let mut rig = enumerated(true);
    let bytes = counting(612);
    rig.model.borrow_mut().host_write(OUT, &bytes, 512);
    rig.pump();
    let mut got = Vec::new();
    let mut buffer = [0; 100];
    for _ in 0..20 {
        let count = rig.controller.read(DATA_OUT, &mut buffer).expect("read");
        got.extend_from_slice(&buffer[..count]);
        rig.pump();
    }
    assert_eq!(got, bytes, "both packets, the first as soon as it filled");
    rig.assert_clean();
}

#[test]
fn full_speed_packets() {
    let mut rig = enumerated(false);
    let bytes = counting(64);
    assert_eq!(rig.controller.write(DATA_IN, &bytes), Ok(64), "a packet");
    rig.pump();
    assert_eq!(
        reads(&rig, IN),
        std::slice::from_ref(&bytes),
        "ended by a zero-length packet"
    );
    rig.model.borrow_mut().host_write(OUT, &bytes, 64);
    rig.pump();
    let mut buffer = [0; 128];
    assert_eq!(
        rig.controller.read(DATA_OUT, &mut buffer),
        Ok(64),
        "a full-speed packet"
    );
    rig.assert_clean();
}

#[test]
fn a_notification_is_one_message() {
    let mut rig = enumerated(true);
    let state = serial_state(true, true);
    assert_eq!(
        rig.controller.write(NOTIFY_IN, &state),
        Ok(10),
        "taken whole"
    );
    assert_eq!(
        rig.controller.write(NOTIFY_IN, &state),
        Ok(0),
        "not while one waits"
    );
    assert_eq!(
        rig.controller.write(NOTIFY_IN, &[0; 17]),
        Ok(0),
        "never more than a packet"
    );
    rig.pump();
    assert_eq!(reads(&rig, NOTIFY), [state.to_vec()], "the host has it");
    assert_eq!(rig.controller.write(NOTIFY_IN, &state), Ok(10), "the next");
    rig.assert_clean();
}

#[test]
fn a_halted_endpoint_holds_its_bytes_until_cleared() {
    let mut rig = enumerated(true);
    assert_eq!(
        rig.control(0x02, 3, 0, 0x82, 0, &[]),
        Ok(vec![]),
        "SET_FEATURE halt"
    );
    assert!(rig.model.borrow().stalled(IN), "Set Stall on 0x82");
    assert_eq!(
        rig.control(0x82, 0, 0, 0x82, 2, &[]),
        Ok(vec![1, 0]),
        "GET_STATUS halted"
    );
    assert_eq!(rig.controller.write(DATA_IN, b"held"), Ok(4), "queued");
    rig.pump();
    assert!(reads(&rig, IN).is_empty(), "not sent");
    assert_eq!(
        rig.control(0x02, 1, 0, 0x82, 0, &[]),
        Ok(vec![]),
        "CLEAR_FEATURE halt"
    );
    rig.pump();
    assert!(!rig.model.borrow().stalled(IN), "Clear Stall");
    assert_eq!(reads(&rig, IN), [b"held".to_vec()], "sent now");
    rig.assert_clean();
}

#[test]
fn halting_a_busy_endpoint_ends_its_transfer() {
    let mut rig = enumerated(true);
    // The OUT endpoint always has a packet asked for.
    assert_eq!(
        rig.control(0x02, 3, 0, 0x02, 0, &[]),
        Ok(vec![]),
        "halt 0x02"
    );
    assert!(rig.model.borrow().stalled(OUT), "stalled");
    assert_eq!(
        rig.control(0x02, 1, 0, 0x02, 0, &[]),
        Ok(vec![]),
        "clear it"
    );
    rig.pump();
    rig.model.borrow_mut().host_write(OUT, b"again", 512);
    rig.pump();
    let mut buffer = [0; 8];
    assert_eq!(
        rig.controller.read(DATA_OUT, &mut buffer),
        Ok(5),
        "a packet again"
    );
    rig.assert_clean();
}

#[test]
fn a_disconnect_ends_everything_and_enumeration_starts_over() {
    let mut rig = enumerated(true);
    assert_eq!(rig.controller.write(DATA_IN, b"before"), Ok(6), "taken");
    rig.pump();
    rig.model.borrow_mut().detach();
    rig.pump();
    assert!(rig.notice.disconnected, "reported");
    assert!(!rig.controller.is_configured(), "no endpoints");
    assert!(!rig.function.dtr(), "DTR gone with the host");
    assert_eq!(
        rig.controller.write(DATA_IN, b"lost"),
        Ok(0),
        "nowhere to go"
    );
    let _ = rig.enumerate(true);
    assert_eq!(
        rig.controller.write(DATA_IN, b"after"),
        Ok(5),
        "taken again"
    );
    rig.pump();
    assert_eq!(
        reads(&rig, IN),
        [b"before".to_vec(), b"after".to_vec()],
        "both sessions"
    );
    rig.assert_clean();
}

#[test]
fn discard_drops_what_waits() {
    let mut rig = enumerated(true);
    assert_eq!(
        rig.control(0x02, 3, 0, 0x82, 0, &[]),
        Ok(vec![]),
        "halt, so it waits"
    );
    assert_eq!(rig.controller.write(DATA_IN, b"stale"), Ok(5), "queued");
    rig.controller.discard(DATA_IN);
    assert_eq!(rig.controller.pending(DATA_IN), 0, "gone");
    let _ = rig.control(0x02, 1, 0, 0x82, 0, &[]);
    rig.pump();
    assert!(reads(&rig, IN).is_empty(), "nothing sent");
    rig.assert_clean();
}
