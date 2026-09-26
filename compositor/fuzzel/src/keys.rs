//! What a key press does: fuzzel's `keyboard_key`, without libxkbcommon.
//!
//! fuzzel looks a key up three ways, in order, and the first binding found
//! wins:
//!
//! 1. *Untranslated*: a binding that has modifiers, whose modifiers are
//!    exactly the ones held, and whose keysym is one the key makes at its
//!    first level. This is how `Control+Shift+BackSpace` is found although
//!    Shift is held.
//! 2. *Translated*: a binding whose keysym is the one the key makes now,
//!    and whose modifiers are the held ones less those the key consumed to
//!    make it. This is how `Shift+Tab` finds `ISO_Left_Tab` (bound with no
//!    modifiers) and how a German `Shift+7` is a `slash`.
//! 3. The raw keycode, for bindings written as one -- which `fuzzel.ini`
//!    cannot express, so it is not here.
//!
//! A key no binding takes types its text, provided no modifier is held that
//! the key did not consume (Shift on the space bar excepted, as fuzzel
//! excepts it).

use compositor_xkb::generated::{LOCK, MOD2, SHIFT};

use crate::config::{Action, Binding};

/// One key press, as the seat reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Press<'a> {
    /// The keysym the key makes with the modifiers held.
    pub keysym: Option<&'a str>,
    /// The keysyms it makes at its first level.
    pub plain: &'a [&'a str],
    /// The effective modifiers, as `compositor/xkb`'s mask.
    pub mods: u32,
    /// The modifiers the key used to choose its level.
    pub consumed: u32,
    /// The text it types.
    pub text: &'a str,
}

/// What the press does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A binding's action.
    Action(Action),
    /// Text to insert.
    Text(String),
    /// Nothing.
    Nothing,
}

/// Look `press` up in `bindings`.
#[must_use]
pub fn resolve(bindings: &[Binding], press: &Press<'_>) -> Outcome {
    // Locked modifiers -- Caps Lock, Num Lock -- are not held modifiers.
    let locked = LOCK | MOD2;
    let mods = press.mods & !locked;
    let consumed = press.consumed & !locked;

    for binding in bindings {
        let wanted = binding.mods.mask();
        if wanted != mods || wanted == 0 {
            continue;
        }
        if press.plain.contains(&binding.sym) {
            return Outcome::Action(binding.action);
        }
    }
    if let Some(keysym) = press.keysym {
        let unconsumed = mods & !consumed;
        for binding in bindings {
            if binding.sym == keysym && binding.mods.mask() == unconsumed {
                return Outcome::Action(binding.action);
            }
        }
    }

    let mut unconsumed = mods & !consumed;
    if matches!(press.keysym, Some("space" | "KP_Space")) {
        unconsumed &= !SHIFT;
    }
    if unconsumed != 0 || press.text.is_empty() {
        return Outcome::Nothing;
    }
    // A control character is not text a prompt takes.
    if press.text.chars().any(char::is_control) {
        return Outcome::Nothing;
    }
    Outcome::Text(press.text.to_owned())
}

#[cfg(test)]
mod tests {
    use compositor_xkb::generated::{CONTROL, MOD1, SHIFT};

    use super::{Outcome, Press, resolve};
    use crate::config::{Action, default_bindings};

    fn press<'a>(
        keysym: &'a str,
        plain: &'a [&'a str],
        mods: u32,
        consumed: u32,
        text: &'a str,
    ) -> Outcome {
        resolve(
            &default_bindings(),
            &Press {
                keysym: Some(keysym),
                plain,
                mods,
                consumed,
                text,
            },
        )
    }

    #[test]
    fn the_default_bindings() {
        assert_eq!(
            press("Return", &["Return"], 0, 0, "\r"),
            Outcome::Action(Action::Execute)
        );
        assert_eq!(
            press("Escape", &["Escape"], 0, 0, "\x1b"),
            Outcome::Action(Action::Cancel)
        );
        assert_eq!(
            press("Down", &["Down"], 0, 0, ""),
            Outcome::Action(Action::Next)
        );
        assert_eq!(
            press("n", &["n"], CONTROL, 0, "\x0e"),
            Outcome::Action(Action::Next)
        );
        assert_eq!(
            press("p", &["p"], CONTROL, 0, "\x10"),
            Outcome::Action(Action::Prev)
        );
        assert_eq!(
            press("Tab", &["Tab"], 0, 0, "\t"),
            Outcome::Action(Action::ExecuteOrNext)
        );
        assert_eq!(
            press("BackSpace", &["BackSpace"], CONTROL, 0, ""),
            Outcome::Action(Action::DeletePrevWord)
        );
        assert_eq!(
            press("BackSpace", &["BackSpace"], CONTROL | SHIFT, 0, ""),
            Outcome::Action(Action::DeleteLine)
        );
        assert_eq!(
            press("Return", &["Return"], SHIFT, 0, "\r"),
            Outcome::Action(Action::ExecuteInput)
        );
        assert_eq!(
            press("1", &["1"], MOD1, 0, "1"),
            Outcome::Action(Action::Custom(1))
        );
    }

    #[test]
    fn shift_tab_is_translated() {
        assert_eq!(
            press("ISO_Left_Tab", &["Tab"], SHIFT, SHIFT, ""),
            Outcome::Action(Action::PrevWithWrap)
        );
    }

    #[test]
    fn text_on_a_german_keyboard() {
        // Shift+7 is `/`, and Shift is consumed.
        assert_eq!(
            press("slash", &["7"], SHIFT, SHIFT, "/"),
            Outcome::Text("/".to_owned())
        );
        assert_eq!(
            press("odiaeresis", &["odiaeresis"], 0, 0, "ö"),
            Outcome::Text("ö".to_owned())
        );
        assert_eq!(
            press("F", &["f"], SHIFT, SHIFT, "F"),
            Outcome::Text("F".to_owned())
        );
        // Control held and not consumed, and no binding: nothing.
        assert_eq!(press("x", &["x"], CONTROL, 0, "\x18"), Outcome::Nothing);
        // Shift on the space bar is not consumed but still types a space.
        assert_eq!(
            press("space", &["space"], SHIFT, 0, " "),
            Outcome::Text(" ".to_owned())
        );
        // Caps Lock does not stop typing.
        assert_eq!(
            press("A", &["a"], super::LOCK, super::LOCK, "A"),
            Outcome::Text("A".to_owned())
        );
    }
}
