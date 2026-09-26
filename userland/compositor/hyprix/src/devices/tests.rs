//! evdev's numbering turned into the seat's, without a device.

use ferrix_linux_abi::input::{
    ABS_X, ABS_Y, BTN_LEFT, BTN_TOUCH, EV_ABS, EV_KEY, EV_MSC, EV_REL, EV_SYN, Event, KEY_A,
    MSC_SCAN, REL_HWHEEL, REL_WHEEL, REL_X, REL_Y, SYN_REPORT,
};

use super::{Axes, WHEEL_STEP, fraction, translate};
use crate::seat::Input;

fn event(kind: u16, code: u16, value: i32) -> Event {
    Event {
        sec: 0,
        usec: 0,
        r#type: kind,
        code,
        value,
    }
}

/// What one event sends, the move it may leave gathered included.
fn of(axes: &mut Axes, kind: u16, code: u16, value: i32) -> Option<Input> {
    let mut out = read(axes, &[(kind, code, value)]);
    assert!(out.len() <= 1, "one event sent {out:?}");
    out.pop()
}

/// What a read of `events` sends, as `Devices::read_ready` does it: each
/// event, then the move left gathered.
fn read(axes: &mut Axes, events: &[(u16, u16, i32)]) -> Vec<Input> {
    let mut out = Vec::new();
    for &(kind, code, value) in events {
        translate(axes, event(kind, code, value), &mut out);
    }
    axes.flush(&mut out);
    out
}

#[test]
fn a_key_is_a_key_and_a_mouse_button_is_a_button() {
    let mut axes = Axes::default();
    assert_eq!(
        of(&mut axes, EV_KEY, KEY_A, 1),
        Some(Input::Key {
            code: KEY_A,
            pressed: true,
            repeat: false
        })
    );
    // evdev repeats a held key as value 2.
    assert_eq!(
        of(&mut axes, EV_KEY, KEY_A, 2),
        Some(Input::Key {
            code: KEY_A,
            pressed: true,
            repeat: true
        })
    );
    assert_eq!(
        of(&mut axes, EV_KEY, BTN_LEFT, 1),
        Some(Input::Button {
            button: u32::from(BTN_LEFT),
            pressed: true
        })
    );
}

/// A pen coming near a tablet reports `BTN_TOUCH`, which is not a pointer
/// button; sending it as one would click whatever the pointer was over.
#[test]
fn a_tool_button_is_not_a_pointer_button() {
    let mut axes = Axes::default();
    assert_eq!(
        of(&mut axes, EV_KEY, BTN_TOUCH, 1),
        Some(Input::Key {
            code: BTN_TOUCH,
            pressed: true,
            repeat: false
        })
    );
}

#[test]
fn a_relative_axis_is_pixels_and_a_wheel_is_a_click() {
    let mut axes = Axes::default();
    assert_eq!(
        of(&mut axes, EV_REL, REL_X, 7),
        Some(Input::Motion { dx: 7.0, dy: 0.0 })
    );
    assert_eq!(
        of(&mut axes, EV_REL, REL_Y, -3),
        Some(Input::Motion { dx: 0.0, dy: -3.0 })
    );
    // A wheel click up is evdev's +1 and the surface content moving
    // backwards, which `wl_pointer.axis` writes as a negative distance.
    assert_eq!(
        of(&mut axes, EV_REL, REL_WHEEL, 1),
        Some(Input::Axis {
            axis: compositor_protocol::core::wl_pointer::axis::VERTICAL_SCROLL,
            value: -WHEEL_STEP
        })
    );
    assert_eq!(
        of(&mut axes, EV_REL, REL_HWHEEL, 1),
        Some(Input::Axis {
            axis: compositor_protocol::core::wl_pointer::axis::HORIZONTAL_SCROLL,
            value: WHEEL_STEP
        })
    );
}

/// A tablet reports where it is in its own units, and only the device knows
/// what they are worth.
#[test]
fn an_absolute_axis_is_a_fraction_of_the_devices_own_range() {
    let mut axes = Axes {
        range: Some(((0, 32767), (0, 32767))),
        ..Axes::default()
    };
    assert_eq!(
        of(&mut axes, EV_ABS, ABS_X, 16384),
        Some(Input::Absolute {
            x: 16384.0 / 32767.0,
            y: 0.0
        })
    );
    // The other axis keeps what it was: a report may carry one and not the
    // other, and the pointer has to go somewhere in both.
    assert_eq!(
        of(&mut axes, EV_ABS, ABS_Y, 32767),
        Some(Input::Absolute {
            x: 16384.0 / 32767.0,
            y: 1.0
        })
    );
    // A multi-touch axis is not the pointer.
    assert_eq!(of(&mut axes, EV_ABS, 0x35, 100), None);
    // A device with no range reports nothing rather than dividing by zero.
    let mut none = Axes::default();
    assert_eq!(of(&mut none, EV_ABS, ABS_X, 100), None);
}

#[test]
fn a_range_that_is_empty_or_a_value_outside_it_is_held_inside() {
    assert_eq!(fraction(5, 0, 0), 0.0);
    assert_eq!(fraction(5, 10, 0), 0.0);
    assert_eq!(fraction(-5, 0, 100), 0.0);
    assert_eq!(fraction(500, 0, 100), 1.0);
    assert_eq!(fraction(50, 0, 100), 0.5);
    // A range that does not start at zero, as a touchpad's does not.
    assert_eq!(fraction(150, 100, 200), 0.5);
}

/// The sync ends a report and the seat has no use for it; the scan code and
/// the switches are what libinput drops.
#[test]
fn what_the_seat_has_no_use_for_is_dropped() {
    let mut axes = Axes::default();
    assert_eq!(of(&mut axes, EV_SYN, SYN_REPORT, 0), None);
    assert_eq!(of(&mut axes, EV_MSC, MSC_SCAN, 30), None);
    assert_eq!(of(&mut axes, 0x05, 0, 1), None);
}

/// The LEDs follow the locked mask, are written once per change, and start
/// by clearing what a keyboard was left showing.
#[test]
fn the_lights_follow_the_locked_mask_once_per_change() {
    use compositor_xkb::generated::{LOCK, MOD2};
    use ferrix_linux_abi::input::{LED_CAPSL, LED_NUML};

    let mut lights = super::Lights::default();
    assert_eq!(
        lights.change(0),
        Some([(LED_CAPSL, false), (LED_NUML, false)])
    );
    assert_eq!(lights.change(0), None);
    assert_eq!(
        lights.change(LOCK),
        Some([(LED_CAPSL, true), (LED_NUML, false)])
    );
    assert_eq!(lights.change(LOCK), None);
    assert_eq!(
        lights.change(LOCK | MOD2),
        Some([(LED_CAPSL, true), (LED_NUML, true)])
    );
    assert_eq!(
        lights.change(MOD2),
        Some([(LED_CAPSL, false), (LED_NUML, true)])
    );
}

/// A tablet reports x and y as two events in one report; the pointer moves
/// once, to both, never to the new x with the old y.
#[test]
fn a_report_of_both_axes_is_one_move_to_both() {
    let mut axes = Axes {
        range: Some(((0, 32767), (0, 32767))),
        ..Axes::default()
    };
    let moved = read(
        &mut axes,
        &[
            (EV_ABS, ABS_X, 32767),
            (EV_ABS, ABS_Y, 32767),
            (EV_SYN, SYN_REPORT, 0),
        ],
    );
    assert_eq!(moved, [Input::Absolute { x: 1.0, y: 1.0 }]);
}

/// Many reports queued between two reads are one move, to the last place,
/// and a relative device's distances add up: a thousand-report mouse must
/// not become a thousand moves a client redraws for.
#[test]
fn a_reads_many_reports_are_one_move_to_the_last() {
    let mut tablet = Axes {
        range: Some(((0, 100), (0, 100))),
        ..Axes::default()
    };
    let mut reports = Vec::new();
    for at in 1..=50 {
        reports.extend([
            (EV_ABS, ABS_X, at),
            (EV_ABS, ABS_Y, at * 2),
            (EV_SYN, SYN_REPORT, 0),
        ]);
    }
    assert_eq!(
        read(&mut tablet, &reports),
        [Input::Absolute { x: 0.5, y: 1.0 }]
    );

    let mut mouse = Axes::default();
    let mut reports = Vec::new();
    for _ in 0..10 {
        reports.extend([
            (EV_REL, REL_X, 3),
            (EV_REL, REL_Y, -1),
            (EV_SYN, SYN_REPORT, 0),
        ]);
    }
    assert_eq!(
        read(&mut mouse, &reports),
        [Input::Motion {
            dx: 30.0,
            dy: -10.0
        }]
    );
}

/// A click lands where the pointer was when the button went down: the move
/// gathered before it is sent first, and one after it is sent after.
#[test]
fn a_button_follows_the_move_before_it_and_precedes_the_one_after() {
    let mut axes = Axes {
        range: Some(((0, 100), (0, 100))),
        ..Axes::default()
    };
    let sent = read(
        &mut axes,
        &[
            (EV_ABS, ABS_X, 10),
            (EV_ABS, ABS_Y, 20),
            (EV_SYN, SYN_REPORT, 0),
            (EV_KEY, BTN_LEFT, 1),
            (EV_SYN, SYN_REPORT, 0),
            (EV_ABS, ABS_X, 50),
            (EV_SYN, SYN_REPORT, 0),
            (EV_KEY, BTN_LEFT, 0),
            (EV_SYN, SYN_REPORT, 0),
        ],
    );
    assert_eq!(
        sent,
        [
            Input::Absolute { x: 0.1, y: 0.2 },
            Input::Button {
                button: u32::from(BTN_LEFT),
                pressed: true
            },
            Input::Absolute { x: 0.5, y: 0.2 },
            Input::Button {
                button: u32::from(BTN_LEFT),
                pressed: false
            },
        ]
    );
}
