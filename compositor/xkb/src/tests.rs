//! The tables against the probe's own output, and the state against what
//! libxkbcommon's state machine did when the probe asked it.

use super::{KEYMAP, XKB_OFFSET, code_of, generated, key};

/// The evdev codes the tests name, from `linux/input-event-codes.h`.
const KEY_ESC: u16 = 1;
const KEY_Q: u16 = 16;
const KEY_ENTER: u16 = 28;
const KEY_LEFTCTRL: u16 = 29;
const KEY_A: u16 = 30;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_LEFTALT: u16 = 56;
const KEY_CAPSLOCK: u16 = 58;
const KEY_NUMLOCK: u16 = 69;
const KEY_LEFTMETA: u16 = 125;

/// The keymap is the text a client compiles, so the one thing that must be
/// true of it is that it is a whole `xkb_keymap` block and nothing else.
#[test]
fn the_keymap_is_a_whole_one() {
    assert!(KEYMAP.starts_with("xkb_keymap {"));
    assert!(KEYMAP.trim_end().ends_with("};"));
    for section in [
        "xkb_keycodes",
        "xkb_types",
        "xkb_compatibility",
        "xkb_symbols",
    ] {
        assert!(KEYMAP.contains(section), "the keymap has no {section}");
    }
    // The client is told the length including a terminating NUL, which the
    // text itself must not already hold.
    assert!(!KEYMAP.contains('\0'));
}

/// The keymap numbers its keys from eight, and Wayland's keycodes are
/// evdev's; a compositor that sent the keymap's numbers would type the letter
/// eight keys along.
#[test]
fn the_keymap_numbers_keys_eight_above_evdev() {
    assert_eq!(XKB_OFFSET, 8);
    assert!(KEYMAP.contains("minimum = 8;"));
    let q = key(KEY_Q).expect("the keymap has Q");
    assert_eq!(q.name, "AD01");
    assert!(KEYMAP.contains(&format!(
        "<{}> = {};",
        q.name,
        u32::from(q.code) + XKB_OFFSET
    )));
}

#[test]
fn a_key_is_found_by_its_code_and_by_its_keysyms_name() {
    let q = key(KEY_Q).expect("Q");
    assert_eq!(q.plain, Some("q"));
    assert_eq!(q.shifted, Some("Q"));
    assert_eq!(code_of("q"), Some(KEY_Q));
    assert_eq!(code_of("Q"), Some(KEY_Q));
    assert_eq!(code_of("Return"), Some(KEY_ENTER));
    assert_eq!(code_of("escape"), Some(KEY_ESC));
    assert_eq!(code_of("no such key"), None);
}

/// The table is searched by code, which only works if it is sorted; a table
/// out of order would answer `None` for a key that is in it.
#[test]
fn the_table_is_in_order_of_the_code_and_has_no_duplicate() {
    let codes: Vec<u16> = generated::KEYS.iter().map(|key| key.code).collect();
    let mut sorted = codes.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(codes, sorted);
    assert!(codes.len() > 100, "only {} keys", codes.len());
}

/// The bits are the keymap's, and the keymap declares them in the order
/// `Shift`, `Lock`, `Control`, `Mod1`..`Mod5`.
#[test]
fn a_modifiers_bit_is_the_one_the_keymap_gives_it() {
    assert_eq!(generated::SHIFT, 1 << 0);
    assert_eq!(generated::LOCK, 1 << 1);
    assert_eq!(generated::CONTROL, 1 << 2);
    assert_eq!(generated::MOD1, 1 << 3);
    assert_eq!(generated::MOD2, 1 << 4);
    assert_eq!(generated::MOD4, 1 << 6);
    assert!(generated::SOURCE.contains("layout us"));
}

/// What libxkbcommon's state machine said when the probe pressed each key.
#[test]
fn a_modifier_key_holds_what_libxkbcommon_said_it_holds() {
    let held = |code| key(code).expect("a key").held;
    assert_eq!(held(KEY_LEFTSHIFT), generated::SHIFT);
    assert_eq!(held(KEY_LEFTCTRL), generated::CONTROL);
    assert_eq!(held(KEY_LEFTALT), generated::MOD1);
    assert_eq!(held(KEY_LEFTMETA), generated::MOD4);
    assert_eq!(held(KEY_A), 0);

    let locked = |code| key(code).expect("a key").locked;
    assert_eq!(locked(KEY_CAPSLOCK), generated::LOCK);
    assert_eq!(locked(KEY_NUMLOCK), generated::MOD2);
    assert_eq!(locked(KEY_LEFTSHIFT), 0);
}

#[test]
fn holding_a_modifier_shows_in_the_masks_and_releasing_it_stops() {
    let mut keyboard = super::Keyboard::new();
    assert_eq!(keyboard.modifiers().depressed, 0);

    assert!(keyboard.key(KEY_LEFTSHIFT, true));
    assert!(keyboard.key(KEY_LEFTCTRL, true));
    assert_eq!(
        keyboard.modifiers().depressed,
        generated::SHIFT | generated::CONTROL
    );
    assert_eq!(keyboard.pressed(), [KEY_LEFTSHIFT, KEY_LEFTCTRL]);

    assert!(keyboard.key(KEY_LEFTSHIFT, false));
    assert_eq!(keyboard.modifiers().depressed, generated::CONTROL);
    assert!(keyboard.is_held(KEY_LEFTCTRL));
}

/// evdev repeats a held key as value 2, which arrives here as another press.
/// `wl_keyboard.key` has no repeat -- the client repeats for itself from
/// `repeat_info` -- so a repeat must change nothing and say so.
#[test]
fn a_repeat_of_a_held_key_changes_nothing() {
    let mut keyboard = super::Keyboard::new();
    assert!(keyboard.key(KEY_A, true));
    assert!(!keyboard.key(KEY_A, true));
    assert_eq!(keyboard.pressed(), [KEY_A]);
    assert!(keyboard.key(KEY_A, false));
    assert!(!keyboard.key(KEY_A, false));
    assert!(keyboard.pressed().is_empty());
}

#[test]
fn a_lock_toggles_on_the_press_and_survives_the_release() {
    let mut keyboard = super::Keyboard::new();
    let _ = keyboard.key(KEY_CAPSLOCK, true);
    assert_eq!(keyboard.modifiers().locked, generated::LOCK);
    let _ = keyboard.key(KEY_CAPSLOCK, false);
    assert_eq!(keyboard.modifiers().locked, generated::LOCK);

    let _ = keyboard.key(KEY_CAPSLOCK, true);
    let _ = keyboard.key(KEY_CAPSLOCK, false);
    assert_eq!(keyboard.modifiers().locked, 0);

    // Two locks are two bits, not one.
    let _ = keyboard.key(KEY_CAPSLOCK, true);
    let _ = keyboard.key(KEY_NUMLOCK, true);
    assert_eq!(
        keyboard.modifiers().locked,
        generated::LOCK | generated::MOD2
    );
}

/// Latched modifiers and layout groups are not kept, and the crate says so;
/// a test that did not pin it would let one appear by accident.
#[test]
fn nothing_is_ever_latched_and_there_is_one_group() {
    let mut keyboard = super::Keyboard::new();
    let _ = keyboard.key(KEY_LEFTSHIFT, true);
    let _ = keyboard.key(KEY_CAPSLOCK, true);
    let modifiers = keyboard.modifiers();
    assert_eq!(modifiers.latched, 0);
    assert_eq!(modifiers.group, 0);

    keyboard.clear();
    assert_eq!(keyboard.modifiers(), super::Modifiers::default());
}
