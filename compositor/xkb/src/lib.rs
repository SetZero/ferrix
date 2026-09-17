//! The keymap a `wl_keyboard` hands its client, and the state behind it.
//!
//! # Why a compositor needs one at all
//!
//! `wl_keyboard.key` carries a keycode and nothing else. What letter that is
//! depends on the keymap, and the keymap is the compositor's to choose and to
//! send: `wl_keyboard.keymap` hands the client the text of one through a
//! descriptor, the client compiles it with libxkbcommon, and from then on the
//! two agree about what every key means. A compositor that sends `no_keymap`
//! is one where nothing can be typed, because the client has no way to turn a
//! keycode into a character.
//!
//! So the keymap has to be a real one. [`KEYMAP`] is
//! `compositor/xkb/src/us.xkb`, which libxkbcommon itself printed for the
//! `evdev` rules with the `pc105` model and the `us` layout -- Hyprland's own
//! defaults -- through the committed probe in `probe/`. Nothing here parses
//! XKB; the client's libxkbcommon does that, and this crate's job is to send
//! the same text every time and to keep the modifier state that goes with it.
//!
//! # The modifier state
//!
//! `wl_keyboard.modifiers` carries four masks -- depressed, latched, locked
//! and the group -- and the bit each modifier has is decided by the keymap,
//! not by the protocol. [`generated`] holds those bits, and for every key,
//! the modifiers libxkbcommon's own state machine makes depressed while it is
//! held and leaves locked once it has been pressed and released. [`Keyboard`]
//! plays those back: a mask of what is held, and a lock mask each lock key
//! toggles.
//!
//! **What this is not.** Latched modifiers and layout groups are not kept:
//! both masks are always zero, which is what a keyboard with one layout and
//! no sticky keys reports, and which is the truth for the keymap above. A
//! second layout would need the real state machine, and that is
//! `docs/COMPOSITOR.md`'s business rather than this crate's.
//!
//! # Keycodes
//!
//! Wayland's keycodes are evdev's, and an XKB keymap numbers its keys from 8,
//! so the keymap's `<AD01>` = 24 is evdev's `KEY_Q` = 16. [`XKB_OFFSET`] is
//! that eight. The tables here are in evdev's numbering, which is what
//! `/dev/input/eventN` gives and what `wl_keyboard.key` carries.

pub mod generated;

mod state;

pub use state::{Keyboard, Modifiers};

/// The keymap the compositor sends, as libxkbcommon printed it.
pub const KEYMAP: &str = include_str!("us.xkb");

/// What XKB adds to an evdev keycode to get its own.
pub const XKB_OFFSET: u32 = 8;

/// One key of the keymap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    /// Its evdev code, which is what `wl_keyboard.key` carries.
    pub code: u16,
    /// Its name in the keymap, such as `AD01`.
    pub name: &'static str,
    /// The keysym it makes with nothing held, such as `q`.
    pub plain: Option<&'static str>,
    /// The keysym it makes with `Shift` held, such as `Q`.
    pub shifted: Option<&'static str>,
    /// The modifiers held down while this key is held.
    pub held: u32,
    /// The modifiers it leaves locked once pressed and released.
    pub locked: u32,
}

/// The key with this evdev code, if the keymap has one.
#[must_use]
pub fn key(code: u16) -> Option<&'static Key> {
    // The table is in order of the code, so a search rather than a scan.
    let at = generated::KEYS
        .binary_search_by_key(&code, |key| key.code)
        .ok()?;
    generated::KEYS.get(at)
}

/// The evdev code of the key `name` names, matched as Hyprland matches one.
///
/// A bind is written `bind = SUPER, Q, killactive`, and the key is a keysym's
/// name: Hyprland passes it to `xkb_keysym_from_name` with
/// `XKB_KEYSYM_CASE_INSENSITIVE`, so `Q`, `q` and `Return` all work and so
/// does `XF86AudioRaiseVolume`. Matching is against the keysym the key makes
/// with nothing held and with `Shift` held, and then against the keymap's own
/// name for the key, which is what lets `bind = , Escape, ...` and a bind on
/// a key with no keysym both resolve.
#[must_use]
pub fn code_of(name: &str) -> Option<u16> {
    let matches =
        |candidate: Option<&str>| candidate.is_some_and(|text| text.eq_ignore_ascii_case(name));
    generated::KEYS
        .iter()
        .find(|key| matches(key.plain) || matches(key.shifted) || matches(Some(key.name)))
        .map(|key| key.code)
}

#[cfg(test)]
mod tests;
