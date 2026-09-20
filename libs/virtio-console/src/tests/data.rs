//! Bytes across an open port, both ways.

use std::vec;
use std::vec::Vec;

use super::fake::{Harness, opened};
use crate::{Chunk, Event, ReleaseError, SubmitError};

/// An event slice long enough for anything these tests provoke.
fn room() -> Vec<Event> {
    vec![Event::Sent { id: 0 }; 8]
}

#[test]
fn bytes_from_the_host_arrive_in_a_receive_buffer() {
    let harness = Harness::new();
    let (mut driver, mut device, port) = opened(&harness);
    let mut events = room();

    device.send_port(port, b"hello from the host");
    let drained = driver.on_interrupt(&mut events).expect("a frame is fine");

    assert_eq!(drained.events, 1);
    let Some(&Event::Received {
        buffer,
        offset,
        len,
    }) = events.first()
    else {
        panic!("expected bytes, got {:?}", events.first());
    };
    assert_eq!(len, 19);
    assert_eq!(
        harness.receive_data.read(offset as usize, len as usize),
        b"hello from the host".to_vec(),
        "the bytes are where the event said they were"
    );
    assert_eq!(driver.buffers_held(), 1);
    assert_eq!(buffer, 0, "the first buffer is the one the device filled");
}

#[test]
fn a_buffer_is_out_of_the_ring_until_it_is_released() {
    let harness = Harness::new();
    let (mut driver, mut device, port) = opened(&harness);
    let mut events = room();

    device.send_port(port, b"first");
    let _ = driver.on_interrupt(&mut events).expect("fine");
    let Some(&Event::Received { buffer, .. }) = events.first() else {
        panic!("expected bytes");
    };
    assert_eq!(driver.buffers_held(), 1);

    // Released twice is refused, and so is a buffer that was never handed out.
    driver.release(buffer).expect("it was handed out");
    assert_eq!(driver.buffers_held(), 0);
    assert_eq!(driver.release(buffer), Err(ReleaseError::NotHeld(buffer)));
    assert_eq!(
        driver.release(9_999),
        Err(ReleaseError::OutOfRange(9_999)),
        "and neither is one that does not exist"
    );

    // Once released it goes back in the ring and takes the next frame.
    let _ = driver.refill().expect("the ring is sound");
    device.send_port(port, b"second");
    let _ = driver.on_interrupt(&mut events).expect("fine");
    let Some(&Event::Received { offset, len, .. }) = events.first() else {
        panic!("expected bytes again");
    };
    assert_eq!(
        harness.receive_data.read(offset as usize, len as usize),
        b"second".to_vec()
    );
}

#[test]
fn a_chunk_goes_out_and_is_answered_exactly_once() {
    let harness = Harness::new();
    let (mut driver, mut device, port) = opened(&harness);
    let mut events = room();

    harness.transmit_data.write(0, b"to the host");
    driver
        .submit(&Chunk {
            id: 7,
            offset: 0,
            len: 11,
        })
        .expect("the port is open");
    assert_eq!(driver.chunks_in_flight(), 1);

    assert_eq!(
        device.take_port(port),
        Some(b"to the host".to_vec()),
        "the device read what the caller wrote"
    );

    let drained = driver.on_interrupt(&mut events).expect("fine");
    assert_eq!(drained.events, 1);
    assert_eq!(events.first(), Some(&Event::Sent { id: 7 }));
    assert_eq!(driver.chunks_in_flight(), 0);

    // And nothing else comes back for it.
    let drained = driver.on_interrupt(&mut events).expect("fine");
    assert_eq!(drained.events, 0, "an id is answered once and once only");
}

#[test]
fn a_chunk_is_refused_before_the_port_opens() {
    let harness = Harness::new();
    let mut driver = super::fake::brought_up(&harness);
    assert_eq!(
        driver.submit(&Chunk {
            id: 1,
            offset: 0,
            len: 4
        }),
        Err(SubmitError::NotOpen),
        "§3.3 is not finished, so the port carries nothing yet"
    );
}

#[test]
fn a_chunk_of_no_bytes_and_one_past_the_region_are_both_refused() {
    let harness = Harness::new();
    let (mut driver, _device, _port) = opened(&harness);

    assert_eq!(
        driver.submit(&Chunk {
            id: 1,
            offset: 0,
            len: 0
        }),
        Err(SubmitError::Empty)
    );
    assert_eq!(
        driver.submit(&Chunk {
            id: 2,
            offset: 4_000,
            len: 4_000
        }),
        Err(SubmitError::OutsideData),
        "a chunk running off the end of the region is not published"
    );
    assert_eq!(
        driver.chunks_in_flight(),
        0,
        "and neither id is left in the driver's books"
    );
}

#[test]
fn several_chunks_come_back_in_order_and_each_with_its_own_id() {
    let harness = Harness::new();
    let (mut driver, mut device, port) = opened(&harness);
    let mut events = room();

    for (index, text) in [b"one".as_slice(), b"two", b"three"].iter().enumerate() {
        let offset = index as u64 * 64;
        harness.transmit_data.write(offset as usize, text);
        driver
            .submit(&Chunk {
                id: index as u64 + 1,
                offset,
                len: text.len() as u32,
            })
            .expect("the port is open");
    }
    assert_eq!(driver.chunks_in_flight(), 3);

    assert_eq!(device.take_port(port), Some(b"one".to_vec()));
    assert_eq!(device.take_port(port), Some(b"two".to_vec()));
    assert_eq!(device.take_port(port), Some(b"three".to_vec()));

    let drained = driver.on_interrupt(&mut events).expect("fine");
    assert_eq!(drained.events, 3);
    assert_eq!(
        events.get(..3),
        Some(
            [
                Event::Sent { id: 1 },
                Event::Sent { id: 2 },
                Event::Sent { id: 3 }
            ]
            .as_slice()
        )
    );
    assert_eq!(driver.chunks_in_flight(), 0);
}

#[test]
fn the_ring_is_filled_the_moment_the_port_opens() {
    // The first thing a host does after opening the port is write to it, and a
    // receive queue with nothing posted drops that without a word.
    let harness = Harness::new();
    let (mut driver, mut device, port) = opened(&harness);
    let mut events = room();

    device.send_port(port, b"the very first bytes");
    let drained = driver.on_interrupt(&mut events).expect("fine");
    assert_eq!(
        drained.events, 1,
        "a buffer was posted before the host ever wrote"
    );
}

#[test]
fn no_byte_of_either_direction_touched_unmapped_memory() {
    let harness = Harness::new();
    let (mut driver, mut device, port) = opened(&harness);
    let mut events = room();

    harness.transmit_data.write(0, b"out");
    driver
        .submit(&Chunk {
            id: 1,
            offset: 0,
            len: 3,
        })
        .expect("fine");
    let _ = device.take_port(port);
    device.send_port(port, b"in");
    let _ = driver.on_interrupt(&mut events).expect("fine");

    assert_eq!(
        device.faults, 0,
        "every address the device dereferenced was one the driver had pinned"
    );
    assert!(
        driver.transport().protocol_errors.is_empty(),
        "{:?}",
        driver.transport().protocol_errors
    );
}
