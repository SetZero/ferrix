//! The keyboard as a program reads it: keysyms and text, through
//! `compositor/xkb`, with the repeat the client is owed.

/// Which modifiers are in force, from `wl_keyboard.modifiers` read against
/// the keymap's real modifier masks.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub control: bool,
    /// Alt (`Mod1`).
    pub alt: bool,
    /// Super, the logo key (`Mod4`).
    pub logo: bool,
    /// Caps Lock is on.
    pub caps_lock: bool,
    /// Num Lock is on (`Mod2`).
    pub num_lock: bool,
    /// The raw depressed | latched | locked mask, for a program that wants
    /// a modifier this struct does not name.
    pub mask: u32,
}

impl Modifiers {
    /// From the combined mask, with the core X11 bit meanings every keymap
    /// `compositor/xkb` ships uses.
    #[must_use]
    pub const fn from_mask(mask: u32) -> Self {
        use compositor_xkb::generated::{CONTROL, LOCK, MOD1, MOD2, MOD4, SHIFT};
        Self {
            shift: mask & SHIFT != 0,
            control: mask & CONTROL != 0,
            alt: mask & MOD1 != 0,
            logo: mask & MOD4 != 0,
            caps_lock: mask & LOCK != 0,
            num_lock: mask & MOD2 != 0,
            mask,
        }
    }
}

/// One key going down, coming up, or repeating.
#[derive(Clone, Debug)]
pub struct Key {
    /// The surface with the keyboard focus.
    pub surface: Option<crate::SurfaceId>,
    /// The evdev code (`KEY_A` is 30), as `wl_keyboard.key` carries it.
    pub code: u32,
    /// The keysym's name at the level the modifiers select, as xkb names it
    /// (`"a"`, `"A"`, `"Return"`, `"BackSpace"`, `"Escape"`, `"Up"`), or
    /// `None` for a key the keymap does not have.
    pub keysym: Option<&'static str>,
    /// What the key types, if it types anything: `"a"`, `"ä"`, `"\r"` is not
    /// here -- only printable text is. Empty for a modifier or a function key.
    pub text: String,
    /// The keysyms of the key's first level in the group in force, which
    /// libxkbcommon's `xkb_keymap_key_get_syms_by_level(…, 0)` gives: what
    /// an "untranslated" binding (fuzzel's `Shift+Tab`) is matched against.
    pub plain: &'static [&'static str],
    /// The modifiers this key used to choose its level, as
    /// `xkb_state_key_get_consumed_mods2(XKB_CONSUMED_MODE_XKB)` has them:
    /// the modifiers in force that the key's type declares at any of its
    /// levels. A "translated" binding compares `modifiers.mask & !consumed`.
    pub consumed: u32,
    /// The keymap layout of the group in force, for a program that wants to
    /// read more of the key than this says.
    pub layout: Option<&'static compositor_xkb::generated::Layout>,
    /// Down (`true`) or up.
    pub pressed: bool,
    /// Whether this is the runtime retyping a held key, rather than the
    /// compositor saying it went down.
    pub repeat: bool,
    /// The modifiers in force when it was pressed.
    pub modifiers: Modifiers,
    /// The event's serial, which some requests ask for.
    pub serial: u32,
    /// The compositor's timestamp in milliseconds.
    pub time: u32,
}

impl PartialEq for Key {
    /// Field by field, the layout by identity: a layout is a static table
    /// and two keys from the same one point at the same place.
    fn eq(&self, other: &Self) -> bool {
        let same_layout = match (self.layout, other.layout) {
            (Some(one), Some(two)) => core::ptr::eq(one, two),
            (None, None) => true,
            _ => false,
        };
        same_layout
            && self.surface == other.surface
            && self.code == other.code
            && self.keysym == other.keysym
            && self.text == other.text
            && self.plain == other.plain
            && self.consumed == other.consumed
            && self.pressed == other.pressed
            && self.repeat == other.repeat
            && self.modifiers == other.modifiers
            && self.serial == other.serial
            && self.time == other.time
    }
}

impl Eq for Key {}

/// What one key means in one layout under one modifier mask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Read {
    pub(crate) keysym: Option<&'static str>,
    pub(crate) text: String,
    pub(crate) plain: &'static [&'static str],
    pub(crate) consumed: u32,
}

/// Read evdev key `code` in `layout` (the default keymap where there is
/// none) with `mask` in force.
pub(crate) fn read_key(
    layout: Option<&'static compositor_xkb::generated::Layout>,
    mask: u32,
    code: u32,
) -> Read {
    let found = u16::try_from(code).ok().and_then(|code| match layout {
        Some(layout) => layout.key(code),
        None => compositor_xkb::key(code),
    });
    let keysym = found.and_then(|key| key.keysym(mask));
    let text = keysym
        .and_then(compositor_xkb::character)
        .filter(|character| !character.is_control())
        .map(String::from)
        .unwrap_or_default();
    let plain = found
        .and_then(|key| key.levels.first())
        .map_or(&[][..], |level| level.keysyms);
    let declared = found.map_or(0, |key| {
        key.levels
            .iter()
            .flat_map(|level| level.masks.iter())
            .fold(0, |all, mask| all | mask)
    });
    Read {
        keysym,
        text,
        plain,
        consumed: mask & declared,
    }
}
