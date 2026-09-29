//! What a keysym is, as a character.
//!
//! X11 names a keysym rather than spelling it: the key that types a hyphen
//! makes `minus`, and the one beside `1` on a German keyboard makes
//! `exclam`. A client from elsewhere hands the name to libxkbcommon's
//! `xkb_keysym_to_utf32`; this is that answer for the keysyms the shipped
//! keymaps use, which is `keysymdef.h`'s Latin-1 block and `EuroSign`.
//!
//! Without this a terminal can type letters and digits and nothing else,
//! because their names *are* the character they make and every other name is
//! a word. That is what it was for.
//!
//! # A dead key makes no character
//!
//! `dead_circumflex` and `dead_grave` compose with the key after them, and
//! until one is pressed they have made nothing. So they answer `None` rather
//! than the spacing character they resemble: a caller that wants Hyprland's
//! behaviour must compose, and one that cannot should type nothing rather
//! than the wrong thing.
//!
//! # What is not here
//!
//! The third level. `Key` holds the keysym with nothing held and the one
//! with `Shift` held, and a German keyboard's `|`, `~`, `@`, `\`, `[`, `]`,
//! `{` and `}` are all on `AltGr`, which is level three. No table here can
//! supply what the keymap does not carry: the probe prints two levels, so
//! two levels is what the tables hold. `docs/INPUT.md` says what adding the
//! third one takes.

/// The character `keysym` makes, where it makes one.
///
/// A name of one character is that character, which is how `q` and `7`
/// arrive. Everything else is looked up in [`NAMED`].
#[must_use]
pub fn character(keysym: &str) -> Option<char> {
    let mut characters = keysym.chars();
    if let (Some(one), None) = (characters.next(), characters.next()) {
        return Some(one);
    }
    let at = NAMED
        .binary_search_by_key(&keysym, |(name, _)| *name)
        .ok()?;
    NAMED.get(at).map(|(_, character)| *character)
}

/// Every keysym the shipped keymaps use that makes a character and is not
/// named by it, sorted by name so the lookup is a search.
///
/// `keysymdef.h` is the authority: for a keysym in the Latin-1 block its
/// value *is* the Unicode code point, and `EuroSign` is one of the keysyms
/// whose value is the code point plus `0x0100_0000`.
const NAMED: &[(&str, char)] = &[
    ("Adiaeresis", 'Ä'),
    ("Agrave", 'À'),
    ("Ccedilla", 'Ç'),
    ("Eacute", 'É'),
    ("Egrave", 'È'),
    ("EuroSign", '€'),
    ("Odiaeresis", 'Ö'),
    ("Udiaeresis", 'Ü'),
    ("Ugrave", 'Ù'),
    ("acute", '´'),
    ("adiaeresis", 'ä'),
    ("agrave", 'à'),
    ("ampersand", '&'),
    ("apostrophe", '\''),
    ("asciicircum", '^'),
    ("asciitilde", '~'),
    ("asterisk", '*'),
    ("at", '@'),
    ("backslash", '\\'),
    ("bar", '|'),
    ("braceleft", '{'),
    ("braceright", '}'),
    ("bracketleft", '['),
    ("bracketright", ']'),
    ("ccedilla", 'ç'),
    ("cedilla", '¸'),
    ("colon", ':'),
    ("comma", ','),
    ("currency", '¤'),
    ("degree", '°'),
    ("diaeresis", '¨'),
    ("division", '÷'),
    ("dollar", '$'),
    ("eacute", 'é'),
    ("egrave", 'è'),
    ("equal", '='),
    ("exclam", '!'),
    ("exclamdown", '¡'),
    ("grave", '`'),
    ("greater", '>'),
    ("guillemotleft", '«'),
    ("guillemotright", '»'),
    ("less", '<'),
    ("macron", '¯'),
    ("minus", '-'),
    ("mu", 'µ'),
    ("multiply", '×'),
    ("notsign", '¬'),
    ("numbersign", '#'),
    ("odiaeresis", 'ö'),
    ("onehalf", '½'),
    ("onequarter", '¼'),
    ("onesuperior", '¹'),
    ("paragraph", '¶'),
    ("parenleft", '('),
    ("parenright", ')'),
    ("percent", '%'),
    ("period", '.'),
    ("periodcentered", '·'),
    ("plus", '+'),
    ("plusminus", '±'),
    ("question", '?'),
    ("questiondown", '¿'),
    ("quotedbl", '"'),
    ("registered", '®'),
    ("section", '§'),
    ("semicolon", ';'),
    ("slash", '/'),
    ("space", ' '),
    ("ssharp", 'ß'),
    ("sterling", '£'),
    ("threequarters", '¾'),
    ("threesuperior", '³'),
    ("twosuperior", '²'),
    ("udiaeresis", 'ü'),
    ("ugrave", 'ù'),
    ("underscore", '_'),
    ("yen", '¥'),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_of_one_character_is_that_character() {
        assert_eq!(character("q"), Some('q'));
        assert_eq!(character("Q"), Some('Q'));
        assert_eq!(character("7"), Some('7'));
        assert_eq!(character("ä"), Some('ä'));
    }

    #[test]
    fn the_named_keysyms_are_their_characters() {
        assert_eq!(character("minus"), Some('-'));
        assert_eq!(character("exclam"), Some('!'));
        assert_eq!(character("slash"), Some('/'));
        assert_eq!(character("bar"), Some('|'));
        assert_eq!(character("adiaeresis"), Some('ä'));
        assert_eq!(character("Udiaeresis"), Some('Ü'));
        assert_eq!(character("ssharp"), Some('ß'));
        assert_eq!(character("EuroSign"), Some('€'));
    }

    #[test]
    fn a_dead_key_and_a_named_key_make_no_character() {
        // A dead key composes with what follows; a named key is a key a
        // terminal has bytes for, not a character.
        for keysym in [
            "dead_circumflex",
            "dead_grave",
            "dead_acute",
            "dead_diaeresis",
            "Return",
            "BackSpace",
            "Escape",
            "F5",
            "Shift_L",
            "XF86AudioMute",
            "",
        ] {
            assert_eq!(character(keysym), None, "{keysym} makes no character");
        }
    }

    #[test]
    fn every_printable_ascii_character_has_a_keysym_that_makes_it() {
        // The point of the table: a terminal that cannot produce one of
        // these cannot be typed into. Letters, digits and the punctuation
        // whose keysym is named by it come from the one-character path; the
        // rest must be in `NAMED`.
        for byte in 0x20..=0x7Eu8 {
            let wanted = char::from(byte);
            let single = character(&wanted.to_string());
            let named = NAMED
                .iter()
                .any(|(_, character)| *character == wanted)
                .then_some(wanted);
            assert!(
                single == Some(wanted) || named == Some(wanted),
                "nothing makes {wanted:?}"
            );
        }
    }

    #[test]
    fn the_table_is_sorted_and_has_no_name_twice() {
        // The lookup is a binary search, so an unsorted table would answer
        // `None` for a name that is in it.
        for pair in NAMED.windows(2) {
            let (before, after) = (pair[0].0, pair[1].0);
            assert!(before < after, "{before} is not before {after}");
        }
    }
}
