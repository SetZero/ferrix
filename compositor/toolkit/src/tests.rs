//! Host tests of what needs no compositor.

use compositor_xkb::generated::{LAYOUTS, SHIFT};

use crate::keyboard::read_key;
use crate::{Anchor, Output};

fn layout(name: &str, variant: &str) -> &'static compositor_xkb::generated::Layout {
    LAYOUTS
        .iter()
        .find(|layout| layout.name == name && layout.variant == variant)
        .unwrap_or(&LAYOUTS[0])
}

#[test]
fn a_letter_is_its_keysym_and_its_text() {
    let read = read_key(Some(layout("us", "")), 0, 30);
    assert_eq!(read.keysym, Some("a"));
    assert_eq!(read.text, "a");
    assert_eq!(read.plain, ["a"]);
    let shifted = read_key(Some(layout("us", "")), SHIFT, 30);
    assert_eq!(shifted.keysym, Some("A"));
    assert_eq!(shifted.text, "A");
    assert_eq!(shifted.consumed & SHIFT, SHIFT, "shift chose the level");
}

#[test]
fn a_german_shift_seven_is_a_slash_and_says_shift_was_used() {
    // KEY_7 is evdev 8.
    let read = read_key(Some(layout("de", "")), SHIFT, 8);
    assert_eq!(read.keysym, Some("slash"));
    assert_eq!(read.text, "/");
    assert_eq!(read.plain, ["7"]);
    assert_eq!(read.consumed & SHIFT, SHIFT);
}

#[test]
fn shift_tab_is_left_tab_on_a_tab_key_whose_plain_level_is_tab() {
    // KEY_TAB is evdev 15.
    let read = read_key(Some(layout("us", "")), SHIFT, 15);
    assert_eq!(read.keysym, Some("ISO_Left_Tab"));
    assert_eq!(read.plain, ["Tab"]);
    assert!(read.text.is_empty(), "a tab types nothing printable");
}

#[test]
fn a_function_key_types_nothing() {
    // KEY_F1 is evdev 59; Return is 28.
    assert!(read_key(None, 0, 59).text.is_empty());
    assert_eq!(read_key(None, 0, 28).keysym, Some("Return"));
    assert!(read_key(None, 0, 28).text.is_empty());
}

#[test]
fn hyprlocks_monitor_rule_is_upstreams() {
    let output = Output {
        name: "DP-2".to_owned(),
        description: "Lenovo Group Limited R27qe Gen2 UTP03KBB (DP-2)".to_owned(),
        ..Output::default()
    };
    assert!(output.matches_hyprland(""));
    assert!(output.matches_hyprland("DP-2"));
    assert!(!output.matches_hyprland("DP-1"));
    assert!(output.matches_hyprland("desc:Lenovo Group Limited R27qe Gen2 UTP03KBB"));
    assert!(output.matches_hyprland("Lenovo Group"));
    assert!(!output.matches_hyprland("desc:Dell"));
}

#[test]
fn a_screen_is_as_big_as_its_mode_turned_and_scaled() {
    let output = Output {
        mode: (2560, 1440),
        scale: 2,
        transform: crate::Transform::Rotated90,
        physical_mm: (597, 336),
        ..Output::default()
    };
    assert_eq!(output.logical_size(), (720, 1280));
    let dpi = output.dpi().unwrap_or(0.0);
    assert!((dpi - 108.9).abs() < 0.5, "{dpi}");
    assert!(Anchor::TOP.with(Anchor::LEFT).contains(Anchor::LEFT));
}

#[test]
fn a_frame_is_swizzled_into_wl_shm_order() {
    let rgba = [10u8, 20, 30, 40, 1, 2, 3, 4];
    let mut argb = [0u8; 8];
    crate::buffer::swizzle_into(&rgba, &mut argb);
    assert_eq!(argb, [30, 20, 10, 40, 3, 2, 1, 4]);
    assert_eq!(crate::buffer::length(0, 10), None);
    assert_eq!(crate::buffer::length(2, 3), Some(24));
}
