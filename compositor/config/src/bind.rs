//! Key bindings: `bind` and its flagged forms, and `unbind`.
//!
//! Follows Hyprland's `handleBind` and `stringToModMask`. A binding names its
//! dispatcher by string; which dispatchers exist is the dispatcher model's to
//! say, not the configuration's.

use core::fmt;

/// A set of modifiers, as the bits xkbcommon's modifier indices give on a
/// standard keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods(pub u32);

impl Mods {
    /// Shift.
    pub const SHIFT: u32 = 1 << 0;
    /// Caps Lock.
    pub const CAPS: u32 = 1 << 1;
    /// Control.
    pub const CTRL: u32 = 1 << 2;
    /// Alt, `Mod1`.
    pub const ALT: u32 = 1 << 3;
    /// `Mod2`, usually Num Lock.
    pub const MOD2: u32 = 1 << 4;
    /// `Mod3`.
    pub const MOD3: u32 = 1 << 5;
    /// Super, the logo key, `Mod4`.
    pub const LOGO: u32 = 1 << 6;
    /// `Mod5`.
    pub const MOD5: u32 = 1 << 7;

    /// Parse a modifier list the way Hyprland does: upper-cased, and each
    /// modifier present if its name appears anywhere in the text, so
    /// `SUPER_SHIFT`, `SUPER SHIFT` and `SUPERSHIFT` are all the same.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let text = text.to_uppercase();
        let has = |names: &[&str]| names.iter().any(|name| text.contains(name));
        let mut mask = 0;
        for (names, bit) in [
            (&["SHIFT"][..], Self::SHIFT),
            (&["CAPS"], Self::CAPS),
            (&["CTRL", "CONTROL"], Self::CTRL),
            (&["ALT", "MOD1"], Self::ALT),
            (&["MOD2"], Self::MOD2),
            (&["MOD3"], Self::MOD3),
            (&["SUPER", "WIN", "LOGO", "MOD4", "META"], Self::LOGO),
            (&["MOD5"], Self::MOD5),
        ] {
            if has(names) {
                mask |= bit;
            }
        }
        Self(mask)
    }
}

/// The key a binding fires on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// A keysym name, such as `Q`, `Return` or `XF86AudioMute`.
    Sym(String),
    /// A keycode, from `code:NN`.
    Code(u32),
    /// A mouse button, from `mouse:NNN`.
    Mouse(u32),
    /// The scroll wheel, from `mouse_up`, `mouse_down`, `mouse_left` or
    /// `mouse_right`, kept by that name.
    Wheel(String),
}

impl Key {
    /// Parse a key field.
    pub fn parse(text: &str) -> Result<Self, String> {
        let number = |digits: &str| {
            digits
                .trim()
                .parse::<u32>()
                .map_err(|_| format!("Invalid key: {text}"))
        };
        if let Some(code) = text.strip_prefix("code:") {
            return number(code).map(Self::Code);
        }
        if let Some(button) = text.strip_prefix("mouse:") {
            return number(button).map(Self::Mouse);
        }
        if matches!(
            text,
            "mouse_up" | "mouse_down" | "mouse_left" | "mouse_right"
        ) {
            return Ok(Self::Wheel(text.to_owned()));
        }
        Ok(Self::Sym(text.to_owned()))
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sym(name) | Self::Wheel(name) => f.write_str(name),
            Self::Code(code) => write!(f, "code:{code}"),
            Self::Mouse(button) => write!(f, "mouse:{button}"),
        }
    }
}

/// The letters that may follow `bind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per letter Hyprland accepts, each independent"
)]
pub struct BindFlags {
    /// `l`: works while the session is locked.
    pub locked: bool,
    /// `r`: fires on release.
    pub release: bool,
    /// `o`: fires on a long press.
    pub long_press: bool,
    /// `e`: repeats while held.
    pub repeat: bool,
    /// `n`: the key still reaches the focused client.
    pub non_consuming: bool,
    /// `m`: a mouse binding, which drags.
    pub mouse: bool,
    /// `t`: transparent, other bindings of the key fire too.
    pub transparent: bool,
    /// `i`: fires whatever modifiers are held.
    pub ignore_mods: bool,
    /// `s`: the key field is a combination of several keys.
    pub separate: bool,
    /// `d`: a description field follows the key.
    pub description: bool,
    /// `p`: bypasses an app's shortcut inhibitor.
    pub bypass: bool,
    /// `c`: fires on a click.
    pub click: bool,
    /// `g`: fires on a drag.
    pub drag: bool,
    /// `u`: fires in every submap.
    pub submap_universal: bool,
}

impl BindFlags {
    /// Parse the letters after `bind`.
    pub fn parse(letters: &str) -> Result<Self, String> {
        let mut flags = Self::default();
        for letter in letters.chars() {
            let flag = match letter {
                'l' => &mut flags.locked,
                'r' => &mut flags.release,
                'o' => &mut flags.long_press,
                'e' => &mut flags.repeat,
                'n' => &mut flags.non_consuming,
                'm' => &mut flags.mouse,
                't' => &mut flags.transparent,
                'i' => &mut flags.ignore_mods,
                's' => &mut flags.separate,
                'd' => &mut flags.description,
                'p' => &mut flags.bypass,
                'c' => &mut flags.click,
                'g' => &mut flags.drag,
                'u' => &mut flags.submap_universal,
                _ => return Err(format!("bind: invalid flag {letter}")),
            };
            *flag = true;
        }
        Ok(flags)
    }
}

/// One binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bind {
    /// The letters after `bind`.
    pub flags: BindFlags,
    /// The modifiers.
    pub mods: Mods,
    /// The key.
    pub key: Key,
    /// The description, empty unless the `d` flag gave one.
    pub description: String,
    /// The dispatcher's name, lower-cased; `mouse` for a mouse binding.
    pub dispatcher: String,
    /// The dispatcher's argument, as written.
    pub arg: String,
    /// The submap the binding belongs to; `None` is the global map.
    pub submap: Option<String>,
}

/// Split `text` at commas into at most `limit` fields, the last keeping any
/// commas left, each trimmed: hyprutils' `CVarList` with a limit.
pub(crate) fn split_fields(text: &str, limit: usize) -> Vec<&str> {
    text.splitn(limit, ',').map(str::trim).collect()
}

/// Parse the value of `bind<letters> = …`. `Ok(None)` is a binding Hyprland
/// accepts and adds nothing for: one with an empty key.
pub(crate) fn parse(
    letters: &str,
    value: &str,
    submap: Option<&str>,
) -> Result<Option<Bind>, String> {
    let flags = BindFlags::parse(letters)?;
    let count = if flags.description { 5 } else { 4 };
    let fields = split_fields(value, count);
    let field = |index: usize| fields.get(index).copied().unwrap_or("");

    let mods_text = field(0);
    let mods = Mods::parse(mods_text);
    let key_text = field(1);
    let (description, mut dispatcher, arg) = if flags.description {
        (field(2), field(3), field(4))
    } else {
        ("", field(2), field(3))
    };
    // A mouse binding's third field is its action, and the dispatcher is
    // always `mouse`.
    let arg = if flags.mouse {
        let action = dispatcher;
        dispatcher = "mouse";
        action
    } else {
        arg
    };

    if mods.0 == 0 && !mods_text.is_empty() {
        return Err(format!("Invalid mod: {mods_text}"));
    }
    if key_text.is_empty() {
        return Ok(None);
    }
    Ok(Some(Bind {
        flags,
        mods,
        key: Key::parse(key_text)?,
        description: description.to_owned(),
        dispatcher: dispatcher.to_lowercase(),
        arg: arg.to_owned(),
        submap: submap.map(str::to_owned),
    }))
}

/// Parse the value of `unbind = MODS, KEY` into what it removes.
pub(crate) fn parse_unbind(value: &str) -> Result<(Mods, Key), String> {
    let fields = split_fields(value, 2);
    let mods_text = fields.first().copied().unwrap_or("");
    let key_text = fields.get(1).copied().unwrap_or("");
    let mods = Mods::parse(mods_text);
    if mods.0 == 0 && !mods_text.is_empty() {
        return Err(format!("Invalid mod: {mods_text}"));
    }
    Ok((mods, Key::parse(key_text)?))
}
