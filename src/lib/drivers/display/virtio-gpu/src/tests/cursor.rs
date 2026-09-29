//! The cursor queue against the fake device: brought up with no interrupt,
//! an image and its moves carried with the resource, and what waits when
//! every slot is taken.

use core::cell::RefCell;
use std::rc::Rc;
use std::vec::Vec;

use ferrix_virtio::gpu::{
    CMD_MOVE_CURSOR, CMD_UPDATE_CURSOR, Command, Format, MemEntry, Rect, backing_entries,
};
use ferrix_virtio::pci::NO_VECTOR;

use super::driver::{TestDriver, build, run};
use super::fake::{Bus, CursorSeen, Device};
use crate::{CURSOR_SLOTS, DevicePages, Rings, SubmitError, Teardown};

/// A 64 × 64 resource numbered `id` whose pixels are on the device: the
/// pixels.
fn cursor_image(
    bus: &Rc<Bus>,
    driver: &mut TestDriver,
    device: &Rc<RefCell<Device>>,
    id: u32,
) -> Vec<u8> {
    let backing = bus.pin(4, false);
    let pixels: Vec<u8> = (0..64u32 * 64)
        .flat_map(|index| (index.wrapping_mul(2_654_435_761) | 0xFF00_0000).to_le_bytes())
        .collect();
    backing.write_bytes(0, &pixels);
    let mut entries = [MemEntry::default(); 4];
    let count = backing_entries(backing.device_pages(), &mut entries).expect("four pages");
    for command in [
        Command::ResourceCreate2d {
            resource_id: id,
            format: Format::B8G8R8X8,
            width: 64,
            height: 64,
        },
        Command::ResourceAttachBacking {
            resource_id: id,
            entries: &entries[..count],
        },
        Command::TransferToHost2d {
            rect: Rect::sized(64, 64),
            offset: 0,
            resource_id: id,
        },
    ] {
        assert!(run(driver, device, &command).result.is_ok());
    }
    pixels
}

fn cursor_doorbells(driver: &TestDriver) -> usize {
    *driver.transport().cursor_doorbells.borrow()
}

fn seen(device: &Rc<RefCell<Device>>) -> Vec<CursorSeen> {
    device.borrow().cursor_log.clone()
}

#[test]
fn the_cursor_queue_comes_up_asking_for_no_interrupt() {
    let (_bus, device, _driver) = build((640, 480));
    let device = device.borrow();
    assert!(
        device.protocol_errors.is_empty(),
        "{:?}",
        device.protocol_errors
    );
    assert_eq!(device.cursor_vector(), NO_VECTOR);
    assert!(!device.cursor_interrupts_wanted());
}

#[test]
fn an_image_and_its_moves_all_name_the_resource() {
    let (bus, device, mut driver) = build((640, 480));
    let pixels = cursor_image(&bus, &mut driver, &device, 7);

    // A move before there is an image tells the device nothing; the update
    // that follows carries the place.
    driver.move_cursor(0, 5, 6).expect("remembered");
    assert_eq!(cursor_doorbells(&driver), 0);
    driver.update_cursor(0, 7, 3, 4).expect("posted");
    device.borrow_mut().serve_cursor();
    assert_eq!(
        seen(&device),
        [CursorSeen {
            code: CMD_UPDATE_CURSOR,
            scanout: 0,
            x: 5,
            y: 6,
            resource: 7,
            hot: (3, 4),
        }]
    );
    assert_eq!(device.borrow().cursor_image, pixels);

    // A move is the same cursor somewhere else, resource and hotspot and
    // all, and a place off the left edge travels as the negative it is.
    driver.move_cursor(0, -2, 300).expect("posted");
    device.borrow_mut().serve_cursor();
    assert_eq!(
        seen(&device).last(),
        Some(&CursorSeen {
            code: CMD_MOVE_CURSOR,
            scanout: 0,
            x: -2,
            y: 300,
            resource: 7,
            hot: (3, 4),
        })
    );
    assert_eq!(cursor_doorbells(&driver), 2);
    assert!(device.borrow().protocol_errors.is_empty());
    // The cursor queue is its own: nothing of it went down the control one.
    assert!(!driver.is_busy());
}

#[test]
fn with_every_slot_taken_the_newest_place_waits_and_an_image_is_not_lost() {
    let (bus, device, mut driver) = build((640, 480));
    let _ = cursor_image(&bus, &mut driver, &device, 7);
    let _ = cursor_image(&bus, &mut driver, &device, 8);
    driver.update_cursor(0, 7, 0, 0).expect("posted");
    for step in 1..CURSOR_SLOTS as i32 + 3 {
        driver.move_cursor(0, step, step).expect("posted or owed");
    }
    // Sixteen slots: the image and fifteen moves are on the queue, and the
    // last three moves are one owed move, to the last place.
    device.borrow_mut().serve_cursor();
    assert_eq!(seen(&device).len(), CURSOR_SLOTS);
    assert_eq!(seen(&device).last().map(|cursor| cursor.x), Some(15));

    // The device is done with every slot, but nothing woke the driver: the
    // owed move goes out when the next command does, and an image asked
    // for while slots were full is not turned into a move by one after it.
    for step in 0..CURSOR_SLOTS as i32 {
        driver.move_cursor(0, 100 + step, 0).expect("posted");
    }
    driver.update_cursor(0, 8, 1, 1).expect("owed");
    driver.move_cursor(0, 500, 501).expect("owed");
    device.borrow_mut().serve_cursor();
    driver.pump_cursor().expect("slots come back");
    device.borrow_mut().serve_cursor();
    assert_eq!(
        seen(&device).last(),
        Some(&CursorSeen {
            code: CMD_UPDATE_CURSOR,
            scanout: 0,
            x: 500,
            y: 501,
            resource: 8,
            hot: (1, 1),
        })
    );
    assert!(device.borrow().protocol_errors.is_empty());
}

#[test]
fn a_control_interrupt_takes_the_cursor_slots_back() {
    let (bus, device, mut driver) = build((640, 480));
    let _ = cursor_image(&bus, &mut driver, &device, 7);
    driver.update_cursor(0, 7, 0, 0).expect("posted");
    for step in 0..CURSOR_SLOTS as i32 {
        driver.move_cursor(0, step, 0).expect("posted or owed");
    }
    device.borrow_mut().serve_cursor();
    let before = seen(&device).len();
    // A frame's command interrupts, and that is when the owed move goes.
    let _ = run(&mut driver, &device, &Command::GetDisplayInfo);
    device.borrow_mut().serve_cursor();
    assert_eq!(seen(&device).len(), before + 1);
    assert_eq!(seen(&device).last().map(|cursor| cursor.x), Some(15));
}

#[test]
fn a_scanout_the_device_has_not_got_is_refused() {
    let (_bus, _device, mut driver) = build((640, 480));
    assert_eq!(
        driver.update_cursor(1, 7, 0, 0),
        Err(SubmitError::NoSuchScanout)
    );
    assert_eq!(
        driver.move_cursor(16, 0, 0),
        Err(SubmitError::NoSuchScanout)
    );
}

#[test]
fn the_cursor_queue_comes_back_on_shutdown() {
    let (_bus, _device, driver) = build((640, 480));
    let Teardown::Released(released) = driver.shutdown() else {
        panic!("the device resets");
    };
    assert!(matches!(released.cursor_rings, Rings::Queue(_)));
    assert_eq!(released.cursor_area.device_pages().len(), 1);
}
