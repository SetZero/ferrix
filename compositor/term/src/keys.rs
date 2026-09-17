//! What a key sends.
//!
//! A terminal turns a key into bytes, and which bytes is the terminal's own
//! decision: `xterm`'s, here, because `TERM=xterm` is what the child is
//! told.
//!
//! The key arrives as an evdev code on `wl_keyboard.key`, and what it means
//! is the keymap's to say. A client from elsewhere would read the keymap the
//! compositor sends it and ask libxkbcommon; this one asks
//! `compositor/xkb`, which *is* that keymap -- the compositor sends what
//! that crate holds, so the terminal and the compositor cannot disagree
//! about what a key is.

/// What the key named `keysym` sends, with `control` held or not.
///
/// A keysym that makes a character sends it; the rest are the named keys a
/// terminal has bytes for. `None` for a key that sends nothing, which is
/// what a modifier does.
///
/// What a keysym's character is belongs to `compositor/xkb`, not here: X11
/// names a keysym rather than spelling it, so `minus` is a hyphen and
/// `exclam` is an exclamation mark. This used to take a name of one
/// character as that character and let every other name fall through to
/// [`named`], which knows only the keys with escape sequences -- so a
/// terminal could be typed into with letters and digits and nothing else.
#[must_use]
pub fn bytes(keysym: &str, control: bool) -> Option<Vec<u8>> {
    if let Some(one) = compositor_xkb::character(keysym) {
        // A character key. With control held it is the control character,
        // which is how every terminal has made one since the teletype:
        // control-C is three, the interrupt character the line discipline
        // looks for.
        if control {
            return control_byte(one).map(|byte| vec![byte]);
        }
        let mut bytes = [0u8; 4];
        return Some(one.encode_utf8(&mut bytes).as_bytes().to_vec());
    }
    named(keysym).map(<[u8]>::to_vec)
}

/// The bytes a named key sends.
#[must_use]
pub fn named(keysym: &str) -> Option<&'static [u8]> {
    let bytes: &[u8] = match keysym {
        // Return and the keypad's both send a carriage return: the line
        // discipline's `ICRNL` turns it into the newline a program reads.
        "Return" | "KP_Enter" => b"\r",
        // Backspace sends DEL, as every terminal has since the VT220, and
        // the discipline's `VERASE` is DEL to match.
        "BackSpace" => b"\x7F",
        "Tab" => b"\t",
        "Escape" => b"\x1B",
        "space" => b" ",
        // The arrows and the keys around them, in the "normal" mode a
        // terminal starts in.
        "Up" => b"\x1B[A",
        "Down" => b"\x1B[B",
        "Right" => b"\x1B[C",
        "Left" => b"\x1B[D",
        "Home" => b"\x1B[H",
        "End" => b"\x1B[F",
        "Insert" => b"\x1B[2~",
        "Delete" => b"\x1B[3~",
        "Prior" => b"\x1B[5~",
        "Next" => b"\x1B[6~",
        _ => return None,
    };
    Some(bytes)
}

/// The byte control and `character` make.
#[must_use]
pub fn control_byte(character: char) -> Option<u8> {
    let byte = u8::try_from(u32::from(character)).ok()?;
    match byte {
        b'@'..=b'_' => Some(byte & 0x1F),
        b'a'..=b'z' => Some((byte - 0x20) & 0x1F),
        b'?' => Some(0x7F),
        b' ' => Some(0),
        _ => None,
    }
}
