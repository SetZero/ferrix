//! What a bind matches, and what a key does when none does.

use compositor_config::{Config, NoSources};

use super::{Action, Input, Seat};
use compositor_xkb::generated;

/// The evdev codes the tests name.
const KEY_Q: u16 = 16;
const KEY_A: u16 = 30;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_LEFTMETA: u16 = 125;
const BTN_LEFT: u32 = 272;

fn config(text: &str) -> Config {
    compositor_config::parse("test", text, &mut NoSources).config
}

fn seat(text: &str) -> Seat {
    Seat::new(&config(text), 1024, 768)
}

fn press(seat: &mut Seat, code: u16) -> Vec<Action> {
    seat.input(Input::Key {
        code,
        pressed: true,
        repeat: false,
    })
}

fn release(seat: &mut Seat, code: u16) -> Vec<Action> {
    seat.input(Input::Key {
        code,
        pressed: false,
        repeat: false,
    })
}

#[test]
fn a_key_with_no_bind_goes_to_the_window() {
    let mut seat = seat("");
    assert_eq!(
        press(&mut seat, KEY_A),
        [Action::Key {
            code: KEY_A,
            pressed: true
        }]
    );
    assert_eq!(
        release(&mut seat, KEY_A),
        [Action::Key {
            code: KEY_A,
            pressed: false
        }]
    );
}

#[test]
fn a_modifier_key_reports_the_new_state_and_still_reaches_the_window() {
    let mut seat = seat("");
    let actions = press(&mut seat, KEY_LEFTSHIFT);
    assert_eq!(actions.len(), 2, "{actions:?}");
    assert!(matches!(
        actions.first(),
        Some(Action::Modifiers(modifiers)) if modifiers.depressed == generated::SHIFT
    ));
    assert_eq!(
        actions.get(1),
        Some(&Action::Key {
            code: KEY_LEFTSHIFT,
            pressed: true
        })
    );
}

#[test]
fn a_bind_fires_on_its_modifiers_and_eats_the_key() {
    let mut seat = seat("bind = SUPER, Q, killactive\n");
    assert_eq!(seat.binds().0, 1);

    // Without the modifier the key is the window's.
    assert_eq!(
        press(&mut seat, KEY_Q),
        [Action::Key {
            code: KEY_Q,
            pressed: true
        }]
    );
    let _ = release(&mut seat, KEY_Q);

    let _ = press(&mut seat, KEY_LEFTMETA);
    let actions = press(&mut seat, KEY_Q);
    assert_eq!(
        actions,
        [Action::Dispatch {
            name: "killactive".to_owned(),
            argument: String::new()
        }],
        "the bind fired and the key did not reach the window"
    );
    // The release is eaten too, so a client is not told a key came up that
    // it was never told went down.
    assert!(release(&mut seat, KEY_Q).is_empty());
}

/// Hyprland compares the modifiers exactly, which is what lets `SUPER, Q` and
/// `SUPER SHIFT, Q` be two different binds.
#[test]
fn a_bind_does_not_fire_with_a_modifier_it_did_not_ask_for() {
    let mut seat = seat("bind = SUPER, Q, killactive\nbind = SUPER SHIFT, Q, exit\n");
    let _ = press(&mut seat, KEY_LEFTMETA);
    let _ = press(&mut seat, KEY_LEFTSHIFT);
    let actions = press(&mut seat, KEY_Q);
    assert_eq!(
        actions,
        [Action::Dispatch {
            name: "exit".to_owned(),
            argument: String::new()
        }]
    );
}

/// A lock a person left on must not stop every bind working.
#[test]
fn caps_lock_is_not_compared() {
    let mut seat = seat("bind = SUPER, Q, killactive\n");
    let _ = press(&mut seat, 58);
    let _ = release(&mut seat, 58);
    assert_eq!(seat.keyboard().modifiers().locked, generated::LOCK);
    let _ = press(&mut seat, KEY_LEFTMETA);
    assert_eq!(press(&mut seat, KEY_Q).len(), 1, "the bind still fires");
}

#[test]
fn the_flags_decide_when_a_bind_fires_and_whether_the_key_goes_on() {
    // `r`: on the release. `n`: the key reaches the client too.
    let mut seat = seat("bindrn = , A, exec, true\n");
    assert_eq!(
        press(&mut seat, KEY_A),
        [Action::Key {
            code: KEY_A,
            pressed: true
        }],
        "a release bind does not fire on the press"
    );
    let actions = release(&mut seat, KEY_A);
    assert_eq!(actions.len(), 2, "{actions:?}");
    assert!(matches!(actions.first(), Some(Action::Dispatch { .. })));
    assert_eq!(
        actions.get(1),
        Some(&Action::Key {
            code: KEY_A,
            pressed: false
        })
    );
}

/// `wl_keyboard.key` has no way to say a repeat, and the client repeats for
/// itself; a compositor that forwarded evdev's repeats would double every
/// held key.
#[test]
fn a_repeat_reaches_no_window_and_only_a_bind_that_asked() {
    let mut seat = seat("binde = , A, resizeactive, 10 0\nbind = , Q, exit\n");
    let repeat = |seat: &mut Seat, code| {
        seat.input(Input::Key {
            code,
            pressed: true,
            repeat: true,
        })
    };
    let _ = press(&mut seat, KEY_A);
    let actions = repeat(&mut seat, KEY_A);
    assert_eq!(
        actions,
        [Action::Dispatch {
            name: "resizeactive".to_owned(),
            argument: "10 0".to_owned()
        }]
    );

    let _ = press(&mut seat, KEY_Q);
    assert!(
        repeat(&mut seat, KEY_Q).is_empty(),
        "a bind without `e` fires once and the repeat goes nowhere"
    );
}

#[test]
fn a_bind_on_a_key_the_keymap_does_not_have_is_reported_not_dropped() {
    let seat = seat("bind = SUPER, NoSuchKey, exit\n");
    let (live, unresolved) = seat.binds();
    assert_eq!(live, 0);
    assert_eq!(unresolved.len(), 1);
    assert!(unresolved[0].contains("NoSuchKey"), "{unresolved:?}");
}

/// `code:24` is `q` on a `us` keyboard, because XKB numbers keys eight above
/// evdev and `xev` prints XKB's number.
#[test]
fn a_bind_by_keycode_is_read_as_xkb_numbers_one() {
    let mut seat = seat("bind = , code:24, exit\n");
    assert_eq!(seat.binds().0, 1);
    assert_eq!(
        press(&mut seat, KEY_Q),
        [Action::Dispatch {
            name: "exit".to_owned(),
            argument: String::new()
        }]
    );
}

#[test]
fn the_pointer_starts_in_the_middle_and_stays_on_the_screen() {
    let mut seat = seat("");
    assert_eq!(seat.pointer(), (512.0, 384.0));

    assert_eq!(
        seat.input(Input::Motion {
            dx: 10.0,
            dy: -20.0
        }),
        [Action::Pointer { x: 522.0, y: 364.0 }]
    );
    // Past the edge in both directions, and past the other edge.
    let _ = seat.input(Input::Motion {
        dx: -10_000.0,
        dy: -10_000.0,
    });
    assert_eq!(seat.pointer(), (0.0, 0.0));
    let _ = seat.input(Input::Motion {
        dx: 10_000.0,
        dy: 10_000.0,
    });
    assert_eq!(seat.pointer(), (1023.0, 767.0));

    // A tablet says where it is, as a fraction of the screen.
    let _ = seat.input(Input::Absolute { x: 0.5, y: 0.25 });
    assert_eq!(seat.pointer(), (512.0, 192.0));
    // A device that reported nonsense must not make the position nonsense.
    let _ = seat.input(Input::Absolute {
        x: f64::NAN,
        y: 0.5,
    });
    assert_eq!(seat.pointer(), (0.0, 384.0));
}

#[test]
fn a_button_bind_fires_and_the_button_still_reaches_the_window() {
    let mut seat = seat("bindm = SUPER, mouse:272, movewindow\n");
    let _ = press(&mut seat, KEY_LEFTMETA);
    let actions = seat.input(Input::Button {
        button: BTN_LEFT,
        pressed: true,
    });
    assert_eq!(actions.len(), 2, "{actions:?}");
    assert!(matches!(actions.first(), Some(Action::Dispatch { .. })));
    assert_eq!(
        actions.get(1),
        Some(&Action::Button {
            button: BTN_LEFT,
            pressed: true
        })
    );
}

#[test]
fn a_wheel_bind_fires_on_the_direction_it_asked_for() {
    let mut seat = seat("bind = SUPER, mouse_down, workspace, e+1\n");
    let _ = press(&mut seat, KEY_LEFTMETA);
    let vertical = compositor_protocol::core::wl_pointer::axis::VERTICAL_SCROLL;
    let down = seat.input(Input::Axis {
        axis: vertical,
        value: 15.0,
    });
    assert!(
        matches!(down.first(), Some(Action::Dispatch { .. })),
        "{down:?}"
    );
    let up = seat.input(Input::Axis {
        axis: vertical,
        value: -15.0,
    });
    assert!(matches!(up.first(), Some(Action::Axis { .. })), "{up:?}");
}

#[test]
fn a_bind_in_a_submap_is_not_in_the_global_map() {
    let seat = seat("submap = resize\nbind = , Q, exit\nsubmap = reset\n");
    assert_eq!(seat.binds().0, 0);
}

/// The modifier is usually let go before the key is, so the release cannot be
/// judged on its own: a client told a key came up that it was never told went
/// down has that key stuck down for ever.
#[test]
fn the_release_of_an_eaten_key_is_eaten_even_after_the_modifier_went() {
    let mut seat = seat("bind = SUPER, Q, killactive\n");
    let _ = press(&mut seat, KEY_LEFTMETA);
    assert_eq!(press(&mut seat, KEY_Q).len(), 1, "the bind fired");
    // The person lets go of SUPER first, which is what a hand does.
    let _ = release(&mut seat, KEY_LEFTMETA);
    assert!(
        release(&mut seat, KEY_Q).is_empty(),
        "the release reached the window"
    );
    // And the next press, with no modifier, is the window's again.
    assert_eq!(
        press(&mut seat, KEY_Q),
        [Action::Key {
            code: KEY_Q,
            pressed: true
        }]
    );
}
