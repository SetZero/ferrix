//! Bring-up, and the conversation of `docs/CLIPBOARD.md` §3.3 that opens a
//! port.

use std::vec;
use std::vec::Vec;

use ferrix_virtio::console;
use ferrix_virtio::pci::{FEATURE_VERSION_1, TransportError};

use super::fake::{Device, Fake, Harness, OFFERED, brought_up, opened};
use crate::{ConsoleError, Driver, Event, InitError, Options, Port, QUEUE_COUNT, SubmitError};

/// An event slice long enough for anything these tests provoke.
fn room() -> Vec<Event> {
    vec![Event::Sent { id: 0 }; 8]
}

#[test]
fn bring_up_enables_every_queue_and_announces_the_driver() {
    let harness = Harness::new();
    let driver = brought_up(&harness);

    assert!(
        driver.transport().driver_ok(),
        "the driver says DRIVER_OK once the queues are up"
    );
    assert!(
        driver.transport().protocol_errors.is_empty(),
        "bring-up broke a rule: {:?}",
        driver.transport().protocol_errors
    );
    assert_eq!(
        driver.port(),
        Port::Waiting,
        "no port is open until it has been named and opened"
    );

    let mut device = Device::new(&harness, driver.info().queue_size);
    let messages = device.take_controls();
    assert_eq!(messages.len(), 1, "one message, and it is DEVICE_READY");
    let ready = messages.first().expect("just counted");
    assert_eq!(
        ready.get(4..6),
        Some(console::DEVICE_READY.to_le_bytes().as_slice()),
        "the first thing the driver says is DEVICE_READY"
    );
    assert_eq!(
        ready.get(6..8),
        Some(1_u16.to_le_bytes().as_slice()),
        "and it says the driver is ready, not that it is not"
    );
}

#[test]
fn the_control_queues_are_the_pair_the_specification_numbers() {
    let harness = Harness::new();
    let driver = brought_up(&harness);
    // The doorbells rung at bring-up name the control receive queue, which is
    // 2 and not 0. A driver that posted its control buffers on queue 0 would
    // pass every other test here and never hear from the device.
    let rung: Vec<u16> = driver
        .transport()
        .notifications
        .iter()
        .map(|(queue, _)| *queue)
        .collect();
    assert!(
        rung.contains(&console::CONTROL_RECEIVE_QUEUE),
        "the control buffers go on queue 2, not queue 0: {rung:?}"
    );
    for (queue, notify_off) in &driver.transport().notifications {
        assert_eq!(
            *notify_off, *queue,
            "each queue is rung through its own notification offset"
        );
    }
}

#[test]
fn a_device_without_multiport_is_refused() {
    let harness = Harness::new();
    let plain = Fake::new(31, 64).offering(FEATURE_VERSION_1);
    let failure = Driver::init(
        harness.parts_for(plain, QUEUE_COUNT * 64),
        Options::default(),
    )
    .expect_err("a console with one nameless stream is not a port this can find");
    assert!(
        matches!(
            failure.error,
            InitError::Transport(TransportError::MissingFeatures { .. })
        ),
        "MULTIPORT is required, not merely wanted: {}",
        failure.error
    );
}

#[test]
fn a_configuration_block_that_stops_before_max_nr_ports_is_refused() {
    let harness = Harness::new();
    let short = Fake::new(31, 64).with_config_len(console::CONFIG_MAX_NR_PORTS);
    let failure = Driver::init(
        harness.parts_for(short, QUEUE_COUNT * 64),
        Options::default(),
    )
    .expect_err("a block that does not reach max_nr_ports is not this device's");
    assert!(
        matches!(failure.error, InitError::Config(_)),
        "{}",
        failure.error
    );
}

#[test]
fn a_device_that_will_not_keep_the_vector_stops_bring_up() {
    let harness = Harness::new();
    let stubborn = Fake::new(31, 64).refusing_vectors();
    let failure = Driver::init(
        harness.parts_for(stubborn, QUEUE_COUNT * 64),
        Options::default(),
    )
    .expect_err("a queue that cannot have its vector is a queue whose interrupts go nowhere");
    assert!(
        matches!(failure.error, InitError::VectorRefused { .. }),
        "{}",
        failure.error
    );
}

#[test]
fn a_port_is_found_by_its_name_and_not_by_its_number() {
    // Port 0, whose queues are 0 and 1, so that a driver which had hardcoded
    // the `2N + 2` of port 1 would fail here.
    let harness = Harness::new();
    let mut driver = brought_up(&harness);
    let mut device = Device::new(&harness, driver.info().queue_size);
    let _ = device.take_controls();
    let mut events = room();

    device.send_control(0, console::PORT_ADD, 0, &[]);
    let _ = driver.on_interrupt(&mut events).expect("PORT_ADD is fine");
    device.send_control(0, console::PORT_NAME, 0, console::SPICE_PORT_NAME);
    let drained = driver.on_interrupt(&mut events).expect("PORT_NAME is fine");

    assert_eq!(drained.events, 1);
    assert_eq!(events.first(), Some(&Event::Named { port: 0 }));
    assert_eq!(driver.port(), Port::Named { port: 0 });

    device.send_control(0, console::PORT_OPEN, 1, &[]);
    let drained = driver.on_interrupt(&mut events).expect("PORT_OPEN is fine");
    assert_eq!(drained.events, 1);
    assert_eq!(events.first(), Some(&Event::Opened { port: 0 }));
    assert!(driver.port().is_open());
}

#[test]
fn a_port_named_something_else_is_answered_and_then_left_alone() {
    let harness = Harness::new();
    let mut driver = brought_up(&harness);
    let mut device = Device::new(&harness, driver.info().queue_size);
    let _ = device.take_controls();
    let mut events = room();

    device.send_control(1, console::PORT_ADD, 0, &[]);
    let _ = driver.on_interrupt(&mut events).expect("PORT_ADD is fine");
    let answers = device.take_controls();
    assert_eq!(answers.len(), 1, "every port added is answered PORT_READY");

    device.send_control(1, console::PORT_NAME, 0, b"org.qemu.guest_agent.0");
    let drained = driver.on_interrupt(&mut events).expect("a name is a name");
    assert_eq!(drained.events, 0, "another agent's port is not this one's");
    assert_eq!(driver.port(), Port::Waiting);

    // And its opening is ignored too, rather than taken for the wanted port's.
    device.send_control(1, console::PORT_OPEN, 1, &[]);
    let drained = driver.on_interrupt(&mut events).expect("still fine");
    assert_eq!(drained.events, 0);
    assert_eq!(driver.port(), Port::Waiting);
}

#[test]
fn a_port_added_beyond_the_queues_set_up_is_answered_not_ready() {
    let harness = Harness::new();
    let mut driver = brought_up(&harness);
    let mut device = Device::new(&harness, driver.info().queue_size);
    let _ = device.take_controls();
    let mut events = room();

    device.send_control(7, console::PORT_ADD, 0, &[]);
    let _ = driver.on_interrupt(&mut events).expect("PORT_ADD is fine");
    let answers = device.take_controls();
    let ready = answers.first().expect("the port is still answered");
    assert_eq!(
        ready.get(6..8),
        Some(0_u16.to_le_bytes().as_slice()),
        "a port whose queues were never set up is answered `ready` 0"
    );
}

#[test]
fn the_wanted_port_beyond_the_queues_set_up_fails_the_driver() {
    let harness = Harness::new();
    let mut driver = brought_up(&harness);
    let mut device = Device::new(&harness, driver.info().queue_size);
    let _ = device.take_controls();
    let mut events = room();

    device.send_control(7, console::PORT_ADD, 0, &[]);
    let _ = driver.on_interrupt(&mut events).expect("PORT_ADD is fine");
    device.send_control(7, console::PORT_NAME, 0, console::SPICE_PORT_NAME);
    let error = driver
        .on_interrupt(&mut events)
        .expect_err("the clipboard is on a port this driver cannot reach");
    assert_eq!(error, ConsoleError::PortNotPrepared(7));
    assert_eq!(
        driver.fault(),
        Some(ConsoleError::PortNotPrepared(7)),
        "and the driver stops rather than driving the wrong queues"
    );
}

#[test]
fn the_host_end_closing_forgets_the_port_and_lets_it_open_again() {
    let harness = Harness::new();
    let (mut driver, mut device, port) = opened(&harness);
    let mut events = room();

    device.send_control(port, console::PORT_OPEN, 0, &[]);
    let drained = driver
        .on_interrupt(&mut events)
        .expect("a close is not a fault");
    assert_eq!(drained.events, 1);
    assert_eq!(events.first(), Some(&Event::Closed { port, abandoned: 0 }));
    assert_eq!(
        driver.port(),
        Port::Named { port },
        "the port is still named; it is its host that went away"
    );
    assert_eq!(
        driver.submit(&crate::Chunk {
            id: 1,
            offset: 0,
            len: 4
        }),
        Err(SubmitError::NotOpen),
        "nothing is sent into a port whose host has gone"
    );

    device.send_control(port, console::PORT_OPEN, 1, &[]);
    let drained = driver.on_interrupt(&mut events).expect("it opens again");
    assert_eq!(drained.events, 1);
    assert_eq!(events.first(), Some(&Event::Opened { port }));
    assert!(driver.port().is_open());
}

#[test]
fn a_closed_port_abandons_what_was_in_flight() {
    let harness = Harness::new();
    let (mut driver, mut device, port) = opened(&harness);
    let mut events = room();

    harness.transmit_data.write(0, b"half sent");
    driver
        .submit(&crate::Chunk {
            id: 42,
            offset: 0,
            len: 9,
        })
        .expect("the port is open");
    assert_eq!(driver.chunks_in_flight(), 1);

    device.send_control(port, console::PORT_OPEN, 0, &[]);
    let _ = driver
        .on_interrupt(&mut events)
        .expect("a close is not a fault");
    assert_eq!(
        events.first(),
        Some(&Event::Closed { port, abandoned: 1 }),
        "the chunk the old host never read is named as lost"
    );
    assert_eq!(
        driver.chunks_in_flight(),
        0,
        "and is no longer waiting for an answer that will not come"
    );
}

#[test]
fn removing_the_port_ends_it() {
    let harness = Harness::new();
    let (mut driver, mut device, port) = opened(&harness);
    let mut events = room();

    device.send_control(port, console::PORT_REMOVE, 0, &[]);
    let drained = driver
        .on_interrupt(&mut events)
        .expect("a removal is not a fault");
    assert_eq!(drained.events, 1);
    assert_eq!(events.first(), Some(&Event::Removed { port }));
    assert_eq!(driver.port(), Port::Removed { port });

    // And it does not come back: a `PORT_OPEN` after a removal is the device
    // contradicting itself, and is ignored rather than believed.
    device.send_control(port, console::PORT_OPEN, 1, &[]);
    let drained = driver.on_interrupt(&mut events).expect("still fine");
    assert_eq!(drained.events, 0);
    assert_eq!(driver.port(), Port::Removed { port });
}

#[test]
fn the_driver_answers_the_devices_port_open_with_its_own() {
    let harness = Harness::new();
    let mut driver = brought_up(&harness);
    let mut device = Device::new(&harness, driver.info().queue_size);
    let _ = device.take_controls();
    let mut events = room();

    device.send_control(1, console::PORT_ADD, 0, &[]);
    let _ = driver.on_interrupt(&mut events).expect("fine");
    device.send_control(1, console::PORT_NAME, 0, console::SPICE_PORT_NAME);
    let _ = driver.on_interrupt(&mut events).expect("fine");
    let _ = device.take_controls();

    device.send_control(1, console::PORT_OPEN, 1, &[]);
    let _ = driver.on_interrupt(&mut events).expect("fine");

    let answers = device.take_controls();
    let open = answers.first().expect("§3.3 step 6: the driver answers");
    assert_eq!(
        open.get(4..6),
        Some(console::PORT_OPEN.to_le_bytes().as_slice())
    );
    assert_eq!(
        open.get(6..8),
        Some(1_u16.to_le_bytes().as_slice()),
        "and says the port is open, which is what lets bytes flow"
    );
}

#[test]
fn every_feature_taken_was_one_the_device_offered() {
    let harness = Harness::new();
    let driver = brought_up(&harness);
    assert_eq!(
        driver.transport().accepted & !OFFERED,
        0,
        "the driver accepted a feature that was never on the table"
    );
    assert!(
        driver.info().features & console::FEATURE_MULTIPORT != 0,
        "and it did take multiport, without which there is no named port"
    );
}
