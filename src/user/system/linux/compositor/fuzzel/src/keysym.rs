//! Keysym names, as `[key-bindings]` writes them.
//!
//! fuzzel hands a binding's key to `xkb_keysym_from_name(key, 0)`: the name
//! must be one XKB knows, spelled with XKB's own case, and two spellings of
//! one keysym are the same key. There is no libxkbcommon here, so "a name
//! XKB knows" is every keysym the keymaps in `src/user/linux/compositor/xkb` make -- which
//! is every key a keyboard on this compositor can press -- and the aliases
//! `keysymdef.h` gives those keys a second name under.

use compositor_xkb::generated::LAYOUTS;

/// Names `keysymdef.h` defines as another name's value, as `(alias,
/// the name the keymaps print)`. libxkbcommon prints the first name a value
/// has, and a person writes whichever the man page taught them: fuzzel's own
/// defaults say `Page_Up`, the keymaps say `Prior`.
const ALIASES: &[(&str, &str)] = &[
    ("KP_Page_Down", "KP_Next"),
    ("KP_Page_Up", "KP_Prior"),
    ("Page_Down", "Next"),
    ("Page_Up", "Prior"),
];

/// The keysym `name` names, spelled the way the keymaps spell it, or `None`
/// for a name XKB would answer `XKB_KEY_NoSymbol` to.
#[must_use]
pub fn canonical(name: &str) -> Option<&'static str> {
    let wanted = ALIASES
        .iter()
        .find(|(alias, _)| *alias == name)
        .map_or(name, |(_, real)| real);
    LAYOUTS
        .iter()
        .flat_map(|layout| layout.keys.iter())
        .flat_map(|key| key.levels.iter())
        .flat_map(|level| level.keysyms.iter())
        .find(|keysym| **keysym == wanted)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::canonical;

    #[test]
    fn names_are_case_sensitive_and_aliases_resolve() {
        assert_eq!(canonical("Return"), Some("Return"));
        assert_eq!(canonical("return"), None);
        assert_eq!(canonical("Page_Up"), Some("Prior"));
        assert_eq!(canonical("KP_Page_Down"), Some("KP_Next"));
        assert_eq!(canonical("bracketleft"), Some("bracketleft"));
        assert_eq!(canonical("ISO_Left_Tab"), Some("ISO_Left_Tab"));
        assert_eq!(canonical("XF86Paste"), Some("XF86Paste"));
        assert_eq!(canonical("NotAKey"), None);
    }
}
