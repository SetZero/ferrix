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
//! So the keymap has to be a real one. [`generated::LAYOUTS`] holds the ones
//! this compositor ships, each printed by libxkbcommon itself for the
//! `evdev` rules through the committed probe in `probe/`. Nothing here parses
//! XKB; the client's libxkbcommon does that, and this crate's job is to send
//! the right text and to keep the modifier state that goes with it.
//!
//! # Which keymap a person gets
//!
//! [`layout`] answers `input:kb_layout`, `input:kb_variant` and
//! `input:kb_model`, the way Hyprland's own defaults do: nothing named is
//! `us`, which is what libxkbcommon resolves an empty layout to.
//!
//! A layout this compositor does not ship is not a layout it can make up.
//! libxkbcommon compiles a keymap out of the XKB data files, which are tens
//! of megabytes of a desktop distribution and are not on a machine running
//! Ferrix; so the keymaps are generated on a host that has them, committed,
//! and shipped. [`layout`] says which one it gave, and a caller that asked
//! for one that is not here is told so rather than quietly typing English on
//! a German keyboard.
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

mod character;

pub use character::character;

mod merge;

pub use merge::merged;

mod state;

pub use state::{Keyboard, Modifiers};

/// The keymap the compositor sends when nothing named another.
///
/// `us`, which is what libxkbcommon resolves Hyprland's empty default
/// `input:kb_layout` to.
pub const KEYMAP: &str = generated::LAYOUTS[0].keymap;

/// What [`layout`] answers if the generated table were ever empty, which it
/// cannot be: a keymap with no keys, so a keyboard that types nothing rather
/// than a compositor that stops.
static FALLBACK: generated::Layout = generated::Layout {
    name: "us",
    variant: "",
    source: "no generated keymap",
    label: "none",
    keymap: "",
    keys: &[],
};

/// The keymap nothing has chosen: `us`, which is Hyprland's own default.
///
/// A `Default` for the type so that a seat can be derived, and the same
/// layout [`layout`] gives a configuration that says nothing.
impl Default for &'static generated::Layout {
    fn default() -> Self {
        layout("", "").0
    }
}

/// The keymap a configuration asked for, and whether it is the one it asked
/// for.
///
/// Hyprland hands `input:kb_layout`, `input:kb_variant`, `input:kb_model`,
/// `input:kb_rules` and `input:kb_options` to libxkbcommon and gets a keymap
/// compiled out of the XKB data files. This compositor ships the keymaps it
/// was given rather than compiling one, so the answer is the shipped layout
/// whose name and variant match, or `us` with `false` beside it.
///
/// A layout is matched on its name and its variant. A configuration that
/// names a variant this compositor does not ship falls back to the same
/// layout's plain form before it falls back to `us`: a German keyboard with
/// the wrong dead keys is far closer to right than an American one.
#[must_use]
pub fn layout(name: &str, variant: &str) -> (&'static generated::Layout, bool) {
    let (name, variant) = (name.trim(), variant.trim());
    let named = |wanted: &str, with: &str| {
        generated::LAYOUTS
            .iter()
            .find(|layout| layout.name == wanted && layout.variant == with)
    };
    // `us` is what an empty layout means, and it is the first of the table.
    // The generator refuses to write a table without it; a `get` rather than
    // an index all the same, because a crate that cannot be made to panic
    // cannot take a compositor's clients down with it.
    let fallback = generated::LAYOUTS.first().unwrap_or(&FALLBACK);
    if name.is_empty() {
        return (fallback, variant.is_empty());
    }
    if let Some(exact) = named(name, variant) {
        return (exact, true);
    }
    if let Some(plain) = named(name, "") {
        return (plain, false);
    }
    (fallback, false)
}

/// Every layout a configuration asks for, in order, each with whether it is
/// the one that was asked for.
///
/// `input:kb_layout = de,us` is a keyboard with two groups, and
/// `input:kb_variant = nodeadkeys,` gives the first of them a variant: both
/// options are comma-separated lists read side by side, which is XKB's own
/// grammar and so libxkbcommon's and so Hyprland's, since Hyprland hands the
/// strings on unread.
///
/// A name with no variant beside it has none. Whitespace around either is
/// ignored, because a person aligning a configuration file is not changing
/// what it says.
///
/// At most [`MAX_GROUPS`] of them, for the reason that constant gives. An
/// empty list is one group, the fallback, which is what an unset
/// `kb_layout` means.
#[must_use]
pub fn layouts(names: &str, variants: &str) -> Vec<(&'static generated::Layout, bool)> {
    if names.trim().is_empty() {
        return vec![layout("", variants)];
    }
    let mut variants = variants.split(',');
    names
        .split(',')
        .take(MAX_GROUPS)
        .map(|name| layout(name, variants.next().unwrap_or_default()))
        .collect()
}

/// The shipped layouts a keymap's own text names, in group order.
///
/// A client of any other compositor compiles the keymap it is handed and
/// asks libxkbcommon what each key means. This compositor's own clients --
/// the terminal above all -- cannot: libxkbcommon is not on the machine, and
/// the tables here are what they read instead. So they need to know *which*
/// tables, and the keymap says: libxkbcommon's own printer writes
/// `name[1]="German";` in `xkb_symbols`, one line a group, and
/// [`generated::Layout::label`] is that name.
///
/// This is how a client follows `input:kb_layout` rather than always
/// reading the first table, and how it follows a keymap with several groups:
/// the group index in `wl_keyboard.modifiers` indexes what this returns.
///
/// A group whose name is not one this compositor ships is skipped rather
/// than guessed at, and a keymap that names none gives an empty list -- a
/// caller falls back to the default, which is what it did before it asked.
#[must_use]
pub fn groups_of(keymap: &str) -> Vec<&'static generated::Layout> {
    let mut named: Vec<(usize, &'static generated::Layout)> = Vec::new();
    for line in keymap.lines() {
        // `name[1]="German";`, and not `level_name[1]= "Any";`, which is why
        // the test is on the whole first word.
        let line = line.trim();
        let Some(rest) = line.strip_prefix("name[") else {
            continue;
        };
        let Some((index, rest)) = rest.split_once(']') else {
            continue;
        };
        let Ok(index) = index.trim().parse::<usize>() else {
            continue;
        };
        let Some(label) = rest
            .trim_start()
            .trim_start_matches('=')
            .trim()
            .strip_prefix('"')
            .and_then(|rest| rest.split('"').next())
        else {
            continue;
        };
        if let Some(layout) = generated::LAYOUTS
            .iter()
            .find(|layout| layout.label == label)
        {
            named.push((index, layout));
        }
    }
    // The group numbering in the text is one-based and need not be in order.
    named.sort_by_key(|(index, _)| *index);
    named.into_iter().map(|(_, layout)| layout).collect()
}

/// How many groups a keymap may have.
///
/// XKB's own limit, which libxkbcommon spells `XKB_MAX_GROUPS` and enforces
/// by refusing the fifth: a `wl_keyboard.modifiers` group is two bits in the
/// protocol's own reckoning, and a keymap with more would have groups no
/// client could be told about. A configuration naming more gets the first
/// four, which is what libxkbcommon gives Hyprland.
pub const MAX_GROUPS: usize = 4;

/// What XKB adds to an evdev keycode to get its own.
pub const XKB_OFFSET: u32 = 8;

/// What a key makes at one of its levels.
///
/// A level is not "how many shifts": on a German keyboard level two is
/// `AltGr` (`Mod5`) and on every keyboard the function keys have a level that
/// `Control+Alt` reaches. So each level carries the modifier masks that
/// select it, as `xkb_keymap_key_get_mods_for_level` reported them, and
/// choosing a level is a lookup rather than an implementation of XKB's key
/// types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    /// The keysyms it makes, which is usually one and is empty for a level
    /// the keymap leaves blank -- keycodes 196 to 199 have an empty first
    /// level and `Alt_L` on their second, and an empty level is not the end
    /// of a key's levels.
    pub keysyms: &'static [&'static str],
    /// Every modifier mask that selects this level. More than one because a
    /// mask names the modifiers the key's *type* cares about together with
    /// the combinations of the ones it ignores, so `FK01`'s first level is
    /// reached by nothing, by `Control` and by `Mod1` alike.
    pub masks: &'static [u32],
}

/// One key of the keymap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    /// Its evdev code, which is what `wl_keyboard.key` carries.
    pub code: u16,
    /// Its name in the keymap, such as `AD01`.
    pub name: &'static str,
    /// Every level it has, the plainest first.
    pub levels: &'static [Level],
    /// The modifiers held down while this key is held.
    pub held: u32,
    /// The modifiers it leaves locked once pressed and released.
    pub locked: u32,
}

impl Key {
    /// The keysym it makes with nothing held, such as `q`.
    #[must_use]
    pub fn plain(&self) -> Option<&'static str> {
        self.keysym_at(0)
    }

    /// The keysym it makes with `Shift` held, such as `Q`.
    #[must_use]
    pub fn shifted(&self) -> Option<&'static str> {
        self.keysym_at(1)
    }

    /// The first keysym of level `level`, if the key has that level and the
    /// level is not blank.
    #[must_use]
    pub fn keysym_at(&self, level: usize) -> Option<&'static str> {
        self.levels
            .get(level)
            .and_then(|level| level.keysyms.first().copied())
    }

    /// Which level `modifiers` selects.
    ///
    /// The rule is libxkbcommon's, and it is not "the mask that matches":
    /// XKB first narrows the active modifiers to the ones this key's type
    /// declares -- every modifier named at any of its levels -- and a
    /// combination that then matches nothing is level zero rather than
    /// nothing at all. So `Shift+Mod4` on a key whose type knows only
    /// `Shift` is the shifted level, and `Mod5` on a key with no third level
    /// is the plain one.
    #[must_use]
    pub fn level(&self, modifiers: u32) -> usize {
        let declared = self
            .levels
            .iter()
            .flat_map(|level| level.masks.iter())
            .fold(0, |all, mask| all | mask);
        let wanted = modifiers & declared;
        self.levels
            .iter()
            .position(|level| level.masks.contains(&wanted))
            .unwrap_or(0)
    }

    /// The keysym `modifiers` makes of this key.
    ///
    /// `None` for a key that makes nothing at that level, which is what a
    /// modifier itself does.
    #[must_use]
    pub fn keysym(&self, modifiers: u32) -> Option<&'static str> {
        self.keysym_at(self.level(modifiers))
    }
}

/// The key with this evdev code in the default keymap, if it has one.
#[must_use]
pub fn key(code: u16) -> Option<&'static Key> {
    generated::LAYOUTS
        .first()
        .and_then(|layout| layout.key(code))
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
///
/// The levels above those two are deliberately not searched. Hyprland
/// resolves a bind against a state with no modifiers applied at all
/// (`CKeybindManager::m_xkbTranslationState`, never given a mask), so no bind
/// of its resolves above the first level either, and a bind on `bracketleft`
/// -- `AltGr+8` on a German keyboard -- is one Hyprland would not resolve
/// and neither does this. `code:NN` names such a key.
#[must_use]
pub fn code_of(name: &str) -> Option<u16> {
    generated::LAYOUTS
        .first()
        .and_then(|layout| layout.code_of(name))
}

impl generated::Layout {
    /// The key with this evdev code, if this keymap has one.
    #[must_use]
    pub fn key(&self, code: u16) -> Option<&'static Key> {
        // The table is in order of the code, so a search rather than a scan.
        let at = self.keys.binary_search_by_key(&code, |key| key.code).ok()?;
        self.keys.get(at)
    }

    /// The evdev code of the key `name` names in this keymap.
    ///
    /// [`code_of`] against the default keymap, and the same rules.
    #[must_use]
    pub fn code_of(&self, name: &str) -> Option<u16> {
        let matches =
            |candidate: Option<&str>| candidate.is_some_and(|text| text.eq_ignore_ascii_case(name));
        self.keys
            .iter()
            .find(|key| matches(key.plain()) || matches(key.shifted()) || matches(Some(key.name)))
            .map(|key| key.code)
    }

    /// How a person would write this layout: its name, and its variant after
    /// a comma where it has one.
    #[must_use]
    pub fn described(&self) -> String {
        if self.variant.is_empty() {
            self.name.to_owned()
        } else {
            format!("{}, {}", self.name, self.variant)
        }
    }
}

#[cfg(test)]
mod tests;
