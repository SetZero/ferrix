//! The in-band representation of shell words.
//!
//! zsh does not parse a word into a tree. The lexer copies it into a byte
//! string in which the characters that matter to expansion are replaced by
//! token bytes (`$` becomes [`STRING`], a single quote [`SNULL`], and so on),
//! and every later stage — parameter expansion, globbing, `${(z)}`, `eval` —
//! works on that string. zinc keeps the same representation, because the
//! semantics of those stages are defined by it: `${~x}` is "tokenize the
//! value", `(q)` is "quote what would be a token".
//!
//! Byte values `META..=MARKER` cannot appear literally, so a byte in that range
//! from input is stored as [`META`] followed by the byte XOR 32 ("metafied"),
//! exactly as zsh stores it.

/// Escape for a literal byte in the token range.
pub(crate) const META: u8 = 0x83;
/// `#` in a pattern.
pub(crate) const POUND: u8 = 0x84;
/// `$` outside double quotes.
pub(crate) const STRING: u8 = 0x85;
/// `^` in a pattern.
pub(crate) const HAT: u8 = 0x86;
/// `*` in a pattern.
pub(crate) const STAR: u8 = 0x87;
/// `(` that is not a literal.
pub(crate) const INPAR: u8 = 0x88;
/// `((` of `$((`.
pub(crate) const INPARMATH: u8 = 0x89;
/// `)` that is not a literal.
pub(crate) const OUTPAR: u8 = 0x8a;
/// `))` of `$((...))`.
pub(crate) const OUTPARMATH: u8 = 0x8b;
/// `$` inside double quotes.
pub(crate) const QSTRING: u8 = 0x8c;
/// `=` at the start of a word, or in an assignment.
pub(crate) const EQUALS: u8 = 0x8d;
/// `|` inside a pattern group.
pub(crate) const BAR: u8 = 0x8e;
/// `{`.
pub(crate) const INBRACE: u8 = 0x8f;
/// `}`.
pub(crate) const OUTBRACE: u8 = 0x90;
/// `[`.
pub(crate) const INBRACK: u8 = 0x91;
/// `]`.
pub(crate) const OUTBRACK: u8 = 0x92;
/// A backquote outside double quotes.
pub(crate) const TICK: u8 = 0x93;
/// `<` of `<(` or a numeric glob.
pub(crate) const INANG: u8 = 0x94;
/// `>` closing a numeric glob.
pub(crate) const OUTANG: u8 = 0x95;
/// `>` of `>(`.
pub(crate) const OUTANGPROC: u8 = 0x96;
/// `?` in a pattern.
pub(crate) const QUEST: u8 = 0x97;
/// `~`.
pub(crate) const TILDE: u8 = 0x98;
/// A backquote inside double quotes.
pub(crate) const QTICK: u8 = 0x99;
/// `,` in a brace expansion.
pub(crate) const COMMA: u8 = 0x9a;
/// `-`, special only in a bracket range.
pub(crate) const DASH: u8 = 0x9b;
/// `!`, special only in a bracket range.
pub(crate) const BANG: u8 = 0x9c;
/// Marks where a single quote was.
pub(crate) const SNULL: u8 = 0x9d;
/// Marks where a double quote was.
pub(crate) const DNULL: u8 = 0x9e;
/// Marks where a backslash was; the next byte is literal.
pub(crate) const BNULL: u8 = 0x9f;
/// A backslash kept when the string is made printable.
pub(crate) const BNULLKEEP: u8 = 0xa0;
/// An empty argument that corresponds to no character.
pub(crate) const NULARG: u8 = 0xa1;
/// Local marker, never escapes the stage that sets it.
pub(crate) const MARKER: u8 = 0xa2;

/// The characters the tokens `POUND..=BNULLKEEP` stand for, in order (zsh's
/// `ztokens`).
const ZTOKENS: &[u8; 29] = b"#$^*(())$=|{}[]`<>>?~`,-!'\"\\\\";

/// zsh's `itok`: a token byte, the quote placeholders included.
pub(crate) fn is_tok(c: u8) -> bool {
    (POUND..=NULARG).contains(&c)
}

/// True for a quote placeholder (`SNULL..=NULARG`).
pub(crate) fn is_null(c: u8) -> bool {
    (SNULL..=NULARG).contains(&c)
}

/// True for a byte that must be metafied to be stored literally.
pub(crate) fn is_meta(c: u8) -> bool {
    c == 0 || (META..=MARKER).contains(&c)
}

/// The character a token byte stands for, or the byte itself.
pub(crate) fn detok(c: u8) -> u8 {
    if is_tok(c) {
        ZTOKENS.get(usize::from(c - POUND)).copied().unwrap_or(c)
    } else {
        c
    }
}

/// True if `s` contains any token byte.
pub(crate) fn has_token(s: &[u8]) -> bool {
    let mut i = 0;
    while let Some(&c) = s.get(i) {
        if c == META {
            i += 2;
            continue;
        }
        if is_tok(c) {
            return true;
        }
        i += 1;
    }
    false
}

/// Metafy raw bytes from outside the shell.
pub(crate) fn metafy(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    for &c in raw {
        if is_meta(c) {
            out.push(META);
            out.push(c ^ 32);
        } else {
            out.push(c);
        }
    }
    out
}

/// Turn a metafied string back into the bytes it stands for.
pub(crate) fn unmetafy(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut it = s.iter();
    while let Some(&c) = it.next() {
        if c == META {
            if let Some(&n) = it.next() {
                out.push(n ^ 32);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Replace token bytes by the characters they stand for, in place, and drop
/// nothing: quote placeholders stay (zsh's `untokenize` turns them into the
/// quote characters only when printing; see [`untokenize`]).
pub(crate) fn untokenize(s: &mut Vec<u8>) {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while let Some(&c) = s.get(i) {
        if c == META {
            out.push(c);
            if let Some(&n) = s.get(i + 1) {
                out.push(n);
            }
            i += 2;
            continue;
        }
        if c != NULARG {
            out.push(detok(c));
        }
        i += 1;
    }
    *s = out;
}

/// Remove quote placeholders and turn remaining tokens into characters: what
/// is left of a word after every expansion (zsh's `remnulargs` followed by
/// `untokenize` on an expanded argument).
pub(crate) fn remove_nulls(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while let Some(&c) = s.get(i) {
        if c == META {
            out.push(c);
            if let Some(&n) = s.get(i + 1) {
                out.push(n);
            }
            i += 2;
            continue;
        }
        if c == BNULLKEEP {
            out.push(b'\\');
        } else if is_null(c) {
            // dropped
        } else if is_tok(c) {
            out.push(detok(c));
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metafy_round_trips_every_byte() {
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(unmetafy(&metafy(&all)), all);
        assert!(!metafy(&all).iter().any(|&c| is_tok(c)));
    }

    #[test]
    fn tokens_stand_for_their_characters() {
        assert_eq!(detok(STRING), b'$');
        assert_eq!(detok(BANG), b'!');
        assert_eq!(detok(OUTANGPROC), b'>');
        let mut s = vec![DNULL, STRING, b'x', DNULL];
        untokenize(&mut s);
        assert_eq!(s, b"\"$x\"");
        assert_eq!(remove_nulls(&[SNULL, b'a', STAR, SNULL]), b"a*");
    }
}
