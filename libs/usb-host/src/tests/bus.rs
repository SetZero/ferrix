//! The bus against the model of a DK board: the hub, a full-speed mouse and
//! a low-speed keyboard behind it.

use std::vec;
use std::vec::Vec;

use ferrix_inputctl::message::{Events, Message, PORT_RIGHTS, RawEvent};
use ferrix_inputctl::session::Session;
use ferrix_linux_abi::input::{
    BTN_LEFT, BTN_RIGHT, BUS_USB, EV_KEY, EV_REL, EV_SYN, REL_WHEEL, REL_X, REL_Y, SYN_REPORT,
};
use ferrix_native_abi::types::DEVICE_NOT_PCI;

use super::model::{Memory, Regs, Shared, Time, dk_board};
use crate::bus::{Bus, Note, Output, Parent};
use crate::hid::Kind;
use crate::usb::Speed;
use crate::{MILLISECOND, Parts};

type TestBus = Bus<Regs, Memory, Time>;

const KEY_A: u16 = 30;
const KEY_B: u16 = 48;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_LEFTCTRL: u16 = 29;

fn parts(model: &Shared) -> Parts<Regs, Memory, Time> {
    Parts {
        registers: Regs(model.clone()),
        memory: Memory(model.clone()),
        clock: Time(model.clone()),
    }
}

fn started(model: &Shared) -> TestBus {
    Bus::start(parts(model))
        .map_err(|(error, _)| error)
        .expect("the controller starts")
}

fn outputs(bus: &mut TestBus) -> Vec<Output> {
    std::iter::from_fn(|| bus.next_output()).collect()
}

fn attached(outputs: &[Output]) -> Vec<usize> {
    outputs
        .iter()
        .filter_map(|output| match output {
            Output::Attached(function) => Some(*function),
            _ => None,
        })
        .collect()
}

/// Let `ms` frames run, then take the interrupt.
fn frames(model: &Shared, bus: &mut TestBus, ms: u64) {
    for _ in 0..ms {
        crate::Clock::sleep_nanos(&mut Time(model.clone()), MILLISECOND);
    }
    bus.on_interrupt().expect("the controller runs");
}

fn events(outputs: &[Output], wanted: usize) -> Vec<Vec<RawEvent>> {
    outputs
        .iter()
        .filter_map(|output| match output {
            Output::Events { function, events } if *function == wanted => {
                Some(events.as_slice().to_vec())
            }
            _ => None,
        })
        .collect()
}

fn key(code: u16, down: bool) -> RawEvent {
    RawEvent::new(EV_KEY, code, i32::from(down))
}

fn syn() -> RawEvent {
    RawEvent::new(EV_SYN, SYN_REPORT, 0)
}

/// The board, started and polled once: the bus, and the keyboard's and the
/// mouse's functions.
fn board() -> (Shared, TestBus, usize, usize, usize, usize) {
    let (model, _, mouse, keyboard) = dk_board();
    let mut bus = started(&model);
    bus.poll();
    let found = attached(&outputs(&mut bus));
    let of = |kind| {
        *found
            .iter()
            .find(|&&function| bus.kind(function) == Some(kind))
            .expect("the function")
    };
    let (keys, pointer) = (of(Kind::Keyboard), of(Kind::Mouse));
    (model, bus, keyboard, mouse, keys, pointer)
}

#[test]
fn the_dk_boards_bus_gives_a_keyboard_and_a_mouse() {
    let (model, mut bus, keyboard, mouse, keys, pointer) = board();
    let m = model.borrow();
    assert!(m.violations.is_empty(), "{:?}", m.violations);
    // Addresses in order found: the hub, then its ports in order.
    assert_eq!(m.devices[0].address, 1);
    assert_eq!(m.devices[mouse].address, 2);
    assert_eq!(m.devices[keyboard].address, 3);
    for device in [0, mouse, keyboard] {
        assert_eq!(
            m.devices[device].configured, 1,
            "device {device} configured"
        );
    }
    assert!(m.devices[mouse].boot_protocol[0]);
    assert!(m.devices[keyboard].boot_protocol[0]);
    // The second, non-boot HID interface of each is left alone.
    assert!(!m.devices[mouse].boot_protocol[1]);
    drop(m);

    let hello = bus.hello(keys, DEVICE_NOT_PCI).expect("a HELLO");
    assert_eq!(hello.name.as_bytes(), b"SEM USB Keyboard");
    assert_eq!(
        (
            hello.id.bustype,
            hello.id.vendor,
            hello.id.product,
            hello.id.version
        ),
        (BUS_USB, 0x1A2C, 0x2124, 0x0116)
    );
    assert_eq!(hello.location, DEVICE_NOT_PCI);
    let _ = Session::accept(&hello, &[PORT_RIGHTS]).expect("the core takes the keyboard");

    let hello = bus.hello(pointer, DEVICE_NOT_PCI).expect("a HELLO");
    assert_eq!(hello.name.as_bytes(), b"Logitech G502 HERO Gaming Mouse");
    let _ = Session::accept(&hello, &[PORT_RIGHTS]).expect("the core takes the mouse");

    let notes: Vec<Note> = std::iter::from_fn(|| bus.next_note()).collect();
    assert!(notes.contains(&Note::Device {
        address: 1,
        speed: Speed::High,
        vendor: 0x0424,
        product: 0x2514,
        hub_ports: Some(4),
        functions: 0,
    }));
    assert!(notes.contains(&Note::Device {
        address: 3,
        speed: Speed::Low,
        vendor: 0x1A2C,
        product: 0x2124,
        hub_ports: None,
        functions: 1,
    }));
}

#[test]
fn a_second_poll_finds_nothing_new() {
    let (model, mut bus, ..) = board();
    let transfers = model.borrow().async_runs;
    bus.poll();
    assert!(outputs(&mut bus).is_empty());
    // Four GET_STATUS, one per hub port, and nothing else.
    assert_eq!(model.borrow().async_runs - transfers, 4);
}

#[test]
fn a_key_is_pressed_and_let_go() {
    let (model, mut bus, keyboard, _, keys, _) = board();
    let mut session =
        Session::accept(&bus.hello(keys, DEVICE_NOT_PCI).unwrap(), &[PORT_RIGHTS]).unwrap();

    model
        .borrow_mut()
        .report(keyboard, 1, &[0, 0, 0x04, 0, 0, 0, 0, 0]);
    frames(&model, &mut bus, 2);
    model
        .borrow_mut()
        .report(keyboard, 1, &[0, 0, 0, 0, 0, 0, 0, 0]);
    frames(&model, &mut bus, 2);
    let got = events(&outputs(&mut bus), keys);
    assert_eq!(
        got,
        [
            vec![key(KEY_A, true), syn()],
            vec![key(KEY_A, false), syn()]
        ]
    );

    for batch in got {
        let message = Message::Events(Events::new(&batch).unwrap());
        let _ = session
            .receive(&message, 0, |_| {})
            .expect("the core takes the report");
    }
    assert!(
        model.borrow().violations.is_empty(),
        "{:?}",
        model.borrow().violations
    );
}

#[test]
fn modifiers_come_first_and_a_roll_over_changes_nothing() {
    let (model, mut bus, keyboard, _, keys, _) = board();
    let reports: [[u8; 8]; 4] = [
        [0x02, 0, 0x04, 0, 0, 0, 0, 0],
        [0x02, 0, 0x04, 0x05, 0, 0, 0, 0],
        [0x02, 0, 1, 1, 1, 1, 1, 1],
        [0x01, 0, 0x05, 0, 0, 0, 0, 0],
    ];
    for report in reports {
        model.borrow_mut().report(keyboard, 1, &report);
        frames(&model, &mut bus, 2);
    }
    let got = events(&outputs(&mut bus), keys);
    assert_eq!(
        got,
        [
            vec![key(KEY_LEFTSHIFT, true), key(KEY_A, true), syn()],
            vec![key(KEY_B, true), syn()],
            vec![
                key(KEY_LEFTCTRL, true),
                key(KEY_LEFTSHIFT, false),
                key(KEY_A, false),
                syn()
            ],
        ]
    );
}

#[test]
fn the_mouse_moves_clicks_and_scrolls() {
    let (model, mut bus, _, mouse, _, pointer) = board();
    let mut session =
        Session::accept(&bus.hello(pointer, DEVICE_NOT_PCI).unwrap(), &[PORT_RIGHTS]).unwrap();
    for report in [[0x01_u8, 5, 0xFD, 0], [0x03, 0, 0, 0x01], [0x00, 0, 0, 0]] {
        model.borrow_mut().report(mouse, 1, &report);
        frames(&model, &mut bus, 2);
    }
    let got = events(&outputs(&mut bus), pointer);
    let rel = |code, value| RawEvent::new(EV_REL, code, value);
    assert_eq!(
        got,
        [
            vec![key(BTN_LEFT, true), rel(REL_X, 5), rel(REL_Y, -3), syn()],
            vec![key(BTN_RIGHT, true), rel(REL_WHEEL, 1), syn()],
            vec![key(BTN_LEFT, false), key(BTN_RIGHT, false), syn()],
        ]
    );
    for batch in got {
        let message = Message::Events(Events::new(&batch).unwrap());
        let _ = session
            .receive(&message, 0, |_| {})
            .expect("the core takes the report");
    }
}

#[test]
fn many_reports_between_interrupts_keep_their_order() {
    let (model, mut bus, keyboard, _, keys, _) = board();
    for usage in 0x04..0x0A_u8 {
        model
            .borrow_mut()
            .report(keyboard, 1, &[0, 0, usage, 0, 0, 0, 0, 0]);
    }
    // Two qTDs per pipe: one interrupt, at most two reports taken; the
    // rest wait in the device, as they would on the wire.
    for _ in 0..8 {
        frames(&model, &mut bus, 1);
    }
    let got = events(&outputs(&mut bus), keys);
    let pressed: Vec<u16> = got
        .iter()
        .flatten()
        .filter(|event| event.kind == EV_KEY && event.value == 1)
        .map(|event| event.code)
        .collect();
    assert_eq!(pressed, [30, 48, 46, 32, 18, 33]);
}

#[test]
fn unplugging_the_keyboard_detaches_it_and_plugging_it_back_attaches_it() {
    let (model, mut bus, keyboard, _, keys, pointer) = board();
    model.borrow_mut().unplug(0, 2);
    bus.poll();
    assert_eq!(outputs(&mut bus), [Output::Detached(keys)]);
    assert!(std::iter::from_fn(|| bus.next_note()).any(|note| note == Note::Gone { address: 3 }));

    model.borrow_mut().plug(0, 2, keyboard);
    bus.poll();
    let again = attached(&outputs(&mut bus));
    assert_eq!(again.len(), 1);
    assert_ne!(again[0], pointer);
    assert_eq!(bus.kind(again[0]), Some(Kind::Keyboard));
    // Its old address was free again.
    assert_eq!(model.borrow().devices[keyboard].address, 3);

    model
        .borrow_mut()
        .report(keyboard, 1, &[0, 0, 0x04, 0, 0, 0, 0, 0]);
    frames(&model, &mut bus, 2);
    assert_eq!(
        events(&outputs(&mut bus), again[0]),
        [vec![key(KEY_A, true), syn()]]
    );
    assert!(
        model.borrow().violations.is_empty(),
        "{:?}",
        model.borrow().violations
    );
}

#[test]
fn the_mouse_still_reports_while_the_keyboard_comes_and_goes() {
    let (model, mut bus, keyboard, mouse, _, pointer) = board();
    model.borrow_mut().unplug(0, 2);
    bus.poll();
    model.borrow_mut().plug(0, 2, keyboard);
    bus.poll();
    let _ = outputs(&mut bus);
    model.borrow_mut().report(mouse, 1, &[0, 1, 1, 0]);
    frames(&model, &mut bus, 2);
    assert_eq!(events(&outputs(&mut bus), pointer).len(), 1);
}

#[test]
fn a_device_that_refuses_the_boot_protocol_gives_no_function() {
    let (model, _, _, keyboard) = dk_board();
    model.borrow_mut().devices[keyboard].stall_protocol = true;
    let mut bus = started(&model);
    bus.poll();
    let found = attached(&outputs(&mut bus));
    assert_eq!(found.len(), 1, "only the mouse");
    assert_eq!(bus.kind(found[0]), Some(Kind::Mouse));
}

#[test]
fn a_device_that_cannot_be_set_up_is_left_until_it_is_unplugged() {
    let (model, _, _, keyboard) = dk_board();
    model.borrow_mut().devices[keyboard].device_descriptor[7] = 7;
    let mut bus = started(&model);
    bus.poll();
    let notes: Vec<Note> = std::iter::from_fn(|| bus.next_note()).collect();
    assert!(notes.iter().any(|note| matches!(
        note,
        Note::Failed {
            parent: Parent::Hub { port: 2, .. },
            ..
        }
    )));
    let transfers = model.borrow().async_runs;
    bus.poll();
    assert_eq!(
        model.borrow().async_runs - transfers,
        4,
        "no retry, only the status polls"
    );

    model.borrow_mut().devices[keyboard].device_descriptor[7] = 8;
    model.borrow_mut().unplug(0, 2);
    bus.poll();
    model.borrow_mut().plug(0, 2, keyboard);
    bus.poll();
    assert_eq!(
        attached(&outputs(&mut bus)).len(),
        2,
        "the mouse, then the keyboard at last"
    );
}

#[test]
fn the_controller_stops_and_gives_its_memory_back() {
    let (model, bus, ..) = board();
    assert!(bus.shutdown().is_ok());
    assert_ne!(model.borrow().read_status() & (1 << 12), 0, "halted");
}

#[test]
fn a_controller_that_will_not_halt_keeps_its_memory() {
    let (model, bus, ..) = board();
    model.borrow_mut().wedged = true;
    assert!(bus.shutdown().is_err());
}

#[test]
fn memory_whose_pages_the_controller_cannot_reach_is_refused() {
    #[derive(Debug)]
    struct High(Memory);
    impl crate::Dma for High {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn read32(&self, offset: usize) -> u32 {
            self.0.read32(offset)
        }
        fn write32(&mut self, offset: usize, value: u32) {
            self.0.write32(offset, value);
        }
        fn read8(&self, offset: usize) -> u8 {
            self.0.read8(offset)
        }
        fn write8(&mut self, offset: usize, value: u8) {
            self.0.write8(offset, value);
        }
        fn device_address(&self, offset: usize) -> Option<u32> {
            // The last page is above 4 GiB.
            (offset < 3 * 4096)
                .then(|| self.0.device_address(offset))
                .flatten()
        }
        fn barrier(&self) {}
    }
    let (model, ..) = dk_board();
    let parts = Parts {
        registers: Regs(model.clone()),
        memory: High(Memory(model.clone())),
        clock: Time(model),
    };
    let refused = Bus::start(parts).map(drop).map_err(|(error, _)| error);
    assert_eq!(refused, Err(crate::ehci::Error::Memory));
}
