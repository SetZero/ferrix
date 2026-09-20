//! A device that lies, and what the driver does about it.

use std::vec;
use std::vec::Vec;

use ferrix_virtio::console;
use ferrix_virtio::pci::{CommonConfig, DEVICE_STATUS, STATUS_FAILED};

use super::fake::{Harness, opened};
use ferrix_virtio::QueueError;

use crate::{ConsoleError, Event};

/// An event slice long enough for anything these tests provoke.
fn room() -> Vec<Event> {
    vec![Event::Sent { id: 0 }; 8]
}

#[test]
fn a_device_claiming_it_wrote_more_than_the_buffer_holds_is_refused() {
    let harness = Harness::new();
    let (mut driver, mut device, port) = opened(&harness);
    let mut events = room();
    let capacity = driver.info().receive_stride;

    // The one lie that would have the caller read past the end of its own
    // buffer, which is why it is checked before the event is made.
    device.send_port_claiming(port, b"short", capacity + 4_096);
    let error = driver
        .on_interrupt(&mut events)
        .expect_err("a length past the buffer is not believed");
    assert!(matches!(error, ConsoleError::Overrun { .. }), "{error}");
    assert_eq!(driver.fault(), Some(error));
    assert!(
        driver.transport().read8(DEVICE_STATUS) & STATUS_FAILED != 0,
        "and the device is told it has failed"
    );
}

#[test]
fn a_control_message_claiming_more_than_its_slot_is_refused() {
    let harness = Harness::new();
    let mut driver = super::fake::brought_up(&harness);
    let mut device = super::fake::Device::new(&harness, driver.info().queue_size);
    let _ = device.take_controls();
    let mut events = room();

    device.send_control_claiming(&[0; 8], 4_096);
    let error = driver
        .on_interrupt(&mut events)
        .expect_err("a control message longer than its slot is not read");
    assert!(matches!(error, ConsoleError::Overrun { .. }), "{error}");
}

#[test]
fn a_control_message_too_short_to_be_one_is_refused() {
    let harness = Harness::new();
    let mut driver = super::fake::brought_up(&harness);
    let mut device = super::fake::Device::new(&harness, driver.info().queue_size);
    let _ = device.take_controls();
    let mut events = room();

    device.send_control_bytes(&[1, 2, 3]);
    let error = driver
        .on_interrupt(&mut events)
        .expect_err("three bytes are not a control message");
    assert!(matches!(error, ConsoleError::Wire(_)), "{error}");
}

#[test]
fn a_control_message_about_a_port_the_device_does_not_have_is_refused() {
    let harness = Harness::new();
    let mut driver = super::fake::brought_up(&harness);
    let mut device = super::fake::Device::new(&harness, driver.info().queue_size);
    let _ = device.take_controls();
    let mut events = room();

    // `max_nr_ports` is 31, so port 31 names queues the device does not have.
    device.send_control(31, console::PORT_ADD, 0, &[]);
    let error = driver
        .on_interrupt(&mut events)
        .expect_err("a port at or above max_nr_ports has no queues");
    assert!(matches!(error, ConsoleError::Wire(_)), "{error}");
}

#[test]
fn a_completion_for_a_chain_never_published_fails_the_driver() {
    let harness = Harness::new();
    let (mut driver, mut device, port) = opened(&harness);
    let mut events = room();

    // Nothing has been submitted, so no chain on the transmit queue is in
    // flight and descriptor 0 is on the free list. The rings catch it before
    // the driver's own records are consulted -- `SplitQueue::take_used`
    // refuses a head that is not a chain in flight -- which is why this comes
    // back as a corrupt ring and not as `UnknownChain`. Either way nothing is
    // accounted to a chunk that was never sent.
    device.complete_unknown(port, 0);
    let error = driver
        .on_interrupt(&mut events)
        .expect_err("a completion for nothing is not accounted to something");
    assert_eq!(
        error,
        ConsoleError::Queue(QueueError::NotAChainHead),
        "{error}"
    );
    assert_eq!(driver.fault(), Some(error));
}

#[test]
fn a_device_that_needs_a_reset_is_not_drained() {
    let harness = Harness::new();
    let (mut driver, mut device, port) = opened(&harness);
    let mut events = room();

    device.send_port(port, b"bytes that will not be taken");
    // The device gives up between the interrupt and the drain.
    harness.wedge.set(true);

    let error = driver
        .on_interrupt(&mut events)
        .expect_err("nothing is taken from a device that has given up");
    assert_eq!(error, ConsoleError::NeedsReset);
    assert_eq!(driver.fault(), Some(ConsoleError::NeedsReset));
}

#[test]
fn a_failed_driver_stays_failed() {
    let harness = Harness::new();
    let (mut driver, mut device, port) = opened(&harness);
    let mut events = room();

    device.complete_unknown(port, 0);
    let first = driver
        .on_interrupt(&mut events)
        .expect_err("the first fault");
    let second = driver
        .on_interrupt(&mut events)
        .expect_err("and every call after it");
    assert_eq!(first, second, "the fault is remembered, not re-derived");
    assert_eq!(
        driver.refill().expect_err("and nothing more is posted"),
        first
    );
}
