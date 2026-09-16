//! zsh's `utils.c`: character types, splitting and joining, quoting, key
//! strings, metafication and the "nice" printable forms of strings.
//!
//! Strings here are metafied byte strings, as in zsh. Where zsh walks a
//! NUL-terminated string, the port reads past the end as a NUL through
//! [`at`], which keeps the loops the same shape as the C they follow.

use crate::options::*;
use crate::shell::Shell;
use crate::tok::{self, BNULL, DASH, INBRACE, INBRACK, INPAR, INPARMATH, META, NULARG};
use crate::tok::{
    OUTBRACE, OUTBRACK, OUTPAR, OUTPARMATH, POUND, QSTRING, QTICK, SNULL, STRING, TICK,
};

/// The byte at `i`, or NUL past the end: C's view of a string.
#[inline]
pub(crate) fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

/// Set a character's `typtab` entry.
fn tset(t: &mut [u16; 256], c: u8, v: u16) {
    if let Some(s) = t.get_mut(usize::from(c)) {
        *s = v;
    }
}

/// Add bits to a character's `typtab` entry.
fn tor(t: &mut [u16; 256], c: u8, v: u16) {
    if let Some(s) = t.get_mut(usize::from(c)) {
        *s |= v;
    }
}

/// Mask a character's `typtab` entry.
fn tand(t: &mut [u16; 256], c: u8, v: u16) {
    if let Some(s) = t.get_mut(usize::from(c)) {
        *s &= v;
    }
}

fn tget(t: &[u16; 256], c: u8) -> u16 {
    t.get(usize::from(c)).copied().unwrap_or(0)
}

/// The string from `i` on (empty past the end).
#[inline]
pub(crate) fn from(s: &[u8], i: usize) -> &[u8] {
    s.get(i..).unwrap_or(&[])
}

/// `s[a..b]`, clamped.
#[inline]
pub(crate) fn sub(s: &[u8], a: usize, b: usize) -> &[u8] {
    let b = b.min(s.len());
    s.get(a.min(b)..b).unwrap_or(&[])
}

// The ztypes table (ztype.h).
pub(crate) const IDIGIT: u16 = 1 << 0;
pub(crate) const IALNUM: u16 = 1 << 1;
pub(crate) const IBLANK: u16 = 1 << 2;
pub(crate) const INBLANK: u16 = 1 << 3;
pub(crate) const ITOK: u16 = 1 << 4;
pub(crate) const ISEP: u16 = 1 << 5;
pub(crate) const IALPHA: u16 = 1 << 6;
pub(crate) const IIDENT: u16 = 1 << 7;
pub(crate) const IUSER: u16 = 1 << 8;
pub(crate) const ICNTRL: u16 = 1 << 9;
pub(crate) const IWORD: u16 = 1 << 10;
pub(crate) const ISPECIAL: u16 = 1 << 11;
pub(crate) const IMETA: u16 = 1 << 12;
pub(crate) const IWSEP: u16 = 1 << 13;
pub(crate) const INULL: u16 = 1 << 14;
pub(crate) const IPATTERN: u16 = 1 << 15;

const ZTF_INIT: u32 = 1;
const ZTF_INTERACT: u32 = 2;
const ZTF_SP_COMMA: u32 = 4;
const ZTF_BANGCHAR: u32 = 8;

pub(crate) const DEFAULT_IFS: &[u8] = b" \t\n\x83 ";
pub(crate) const DEFAULT_IFS_SH: &[u8] = b" \t\n";
pub(crate) const DEFAULT_WORDCHARS: &[u8] = b"*?_-.[]~=/&;!#$%^(){}<>";
const SPECCHARS: &[u8] = b"#$^*()=|{}[]`<>?~;&\n\t \\'\"";
const PATCHARS: &[u8] = b"#^*()|[]<>?~\\";

/// The last normal token (`LAST_NORMAL_TOK`, `Bang`).
const LAST_NORMAL_TOK: u8 = tok::BANG;

impl Shell {
    #[inline]
    pub(crate) fn zistype(&self, c: u8, t: u16) -> bool {
        self.typtab.get(usize::from(c)).is_some_and(|&v| v & t != 0)
    }
    #[inline]
    pub(crate) fn idigit(&self, c: u8) -> bool {
        c.is_ascii_digit()
    }
    #[inline]
    pub(crate) fn iblank(&self, c: u8) -> bool {
        self.zistype(c, IBLANK)
    }
    #[inline]
    pub(crate) fn inblank(&self, c: u8) -> bool {
        self.zistype(c, INBLANK)
    }
    #[inline]
    pub(crate) fn isep(&self, c: u8) -> bool {
        self.zistype(c, ISEP)
    }
    #[inline]
    pub(crate) fn iwsep(&self, c: u8) -> bool {
        self.zistype(c, IWSEP)
    }
    #[inline]
    pub(crate) fn iident(&self, c: u8) -> bool {
        self.zistype(c, IIDENT)
    }
    #[inline]
    pub(crate) fn iword(&self, c: u8) -> bool {
        self.zistype(c, IWORD)
    }
    #[inline]
    pub(crate) fn ispecial(&self, c: u8) -> bool {
        self.zistype(c, ISPECIAL)
    }
    #[inline]
    pub(crate) fn ipattern(&self, c: u8) -> bool {
        self.zistype(c, IPATTERN)
    }
    #[inline]
    pub(crate) fn iuser(&self, c: u8) -> bool {
        self.zistype(c, IUSER)
    }
    #[inline]
    pub(crate) fn ialnum(&self, c: u8) -> bool {
        self.zistype(c, IALNUM)
    }
    #[inline]
    pub(crate) fn ialpha(&self, c: u8) -> bool {
        self.zistype(c, IALPHA)
    }

    /// zsh's `inittyptab`.
    pub(crate) fn inittyptab(&mut self) {
        if self.typtab_flags & ZTF_INIT == 0 {
            self.typtab_flags = ZTF_INIT;
            if self.isset(INTERACTIVE) && self.isset(SHINSTDIN) {
                self.typtab_flags |= ZTF_INTERACT;
            }
        }
        let mut t = [0u16; 256];
        for c in 0..32u8 {
            tset(&mut t, c, ICNTRL);
            tset(&mut t, c + 128, ICNTRL);
        }
        t[127] = ICNTRL;
        for c in b'0'..=b'9' {
            tset(&mut t, c, IDIGIT | IALNUM | IWORD | IIDENT | IUSER);
        }
        for c in b'a'..=b'z' {
            let v = IALPHA | IALNUM | IIDENT | IUSER | IWORD;
            tset(&mut t, c, v);
            tset(&mut t, c - b'a' + b'A', v);
        }
        tset(&mut t, b'_', IIDENT | IUSER);
        tset(&mut t, b'-', IUSER);
        tset(&mut t, b'.', IUSER);
        tset(&mut t, DASH, IUSER);
        tor(&mut t, b' ', IBLANK | INBLANK);
        tor(&mut t, b'\t', IBLANK | INBLANK);
        tor(&mut t, b'\n', INBLANK);
        t[0] |= IMETA;
        tor(&mut t, META, IMETA);
        tor(&mut t, tok::MARKER, IMETA);
        for c in POUND..=LAST_NORMAL_TOK {
            tor(&mut t, c, ITOK | IMETA);
        }
        for c in SNULL..=NULARG {
            tor(&mut t, c, ITOK | IMETA | INULL);
        }
        let ifs = match &self.ifs {
            Some(i) => i.clone(),
            None if self.emulation_is(EMULATE_KSH | EMULATE_SH) => DEFAULT_IFS_SH.to_vec(),
            None => DEFAULT_IFS.to_vec(),
        };
        let mut i = 0;
        while i < ifs.len() {
            let mut c = at(&ifs, i);
            if c == META {
                i += 1;
                c = at(&ifs, i) ^ 32;
            }
            if c.is_ascii() {
                if tget(&t, c) & INBLANK != 0 {
                    if at(&ifs, i + 1) == c {
                        i += 1;
                    } else {
                        tor(&mut t, c, IWSEP);
                    }
                }
                tor(&mut t, c, ISEP);
            }
            i += 1;
        }
        let wc = self
            .wordchars
            .clone()
            .unwrap_or_else(|| DEFAULT_WORDCHARS.to_vec());
        let mut i = 0;
        while i < wc.len() {
            let mut c = at(&wc, i);
            if c == META {
                i += 1;
                c = at(&wc, i) ^ 32;
            }
            if c.is_ascii() {
                tor(&mut t, c, IWORD);
            }
            i += 1;
        }
        for &c in SPECCHARS {
            tor(&mut t, c, ISPECIAL);
        }
        if self.typtab_flags & ZTF_SP_COMMA != 0 {
            tor(&mut t, b',', ISPECIAL);
        }
        if self.isset(BANGHIST) && self.bangchar != 0 && self.typtab_flags & ZTF_INTERACT != 0 {
            self.typtab_flags |= ZTF_BANGCHAR;
            tor(&mut t, self.bangchar, ISPECIAL);
        } else {
            self.typtab_flags &= !ZTF_BANGCHAR;
        }
        for &c in PATCHARS {
            tor(&mut t, c, IPATTERN);
        }
        self.typtab = t;
    }

    pub(crate) fn makecommaspecial(&mut self, yes: bool) {
        if yes {
            self.typtab_flags |= ZTF_SP_COMMA;
            tor(&mut self.typtab, b',', ISPECIAL);
        } else {
            self.typtab_flags &= !ZTF_SP_COMMA;
            tand(&mut self.typtab, b',', !ISPECIAL);
        }
    }

    pub(crate) fn makebangspecial(&mut self, yes: bool) {
        let b = self.bangchar;
        if !yes {
            tand(&mut self.typtab, b, !ISPECIAL);
        } else if self.typtab_flags & ZTF_BANGCHAR != 0 {
            tor(&mut self.typtab, b, ISPECIAL);
        }
    }

    /// The IFS characters as wide characters, for non-ASCII separators.
    fn ifs_has_wide(&self, wc: u32) -> bool {
        let ifs = self.ifs.clone().unwrap_or_else(|| DEFAULT_IFS.to_vec());
        String::from_utf8_lossy(&tok::unmetafy(&ifs))
            .chars()
            .any(|c| u32::from(c) == wc)
    }

    fn wordchars_has_wide(&self, wc: u32) -> bool {
        let w = self
            .wordchars
            .clone()
            .unwrap_or_else(|| DEFAULT_WORDCHARS.to_vec());
        String::from_utf8_lossy(&tok::unmetafy(&w))
            .chars()
            .any(|c| u32::from(c) == wc)
    }

    /// zsh's `wcsitype`.
    pub(crate) fn wcsitype(&self, c: u32, itype: u16) -> bool {
        if !self.isset(MULTIBYTE) || c < 0x80 {
            return u8::try_from(c).is_ok_and(|b| self.zistype(b, itype));
        }
        match itype {
            IIDENT => !self.isset(POSIXIDENTIFIERS) && iswalnum(c),
            IWORD => iswalnum(c) || is_combining(c) || self.wordchars_has_wide(c),
            ISEP => self.ifs_has_wide(c),
            _ => iswalnum(c),
        }
    }

    /// zsh's `itype_end`: the index just past the run of `itype`
    /// characters starting at `i` (one character only with `once`).
    pub(crate) fn itype_end(&self, s: &[u8], mut i: usize, itype: u16, once: bool) -> usize {
        if self.isset(MULTIBYTE) && (itype != IIDENT || !self.isset(POSIXIDENTIFIERS)) {
            while at(s, i) != 0 {
                let c = at(s, i);
                let len;
                if tok::is_tok(c) {
                    len = 1;
                    if !self.zistype(c, itype) {
                        break;
                    }
                } else {
                    let (l, wc) = mb_metacharlenconv(self, from(s, i));
                    len = l;
                    if len == 0 {
                        break;
                    }
                    match wc {
                        None => {
                            let chr = if c == META { at(s, i + 1) ^ 32 } else { c };
                            if chr > 127 || !self.zistype(chr, itype) {
                                break;
                            }
                        }
                        Some(_) if len == 1 && c.is_ascii() => {
                            if !self.zistype(c, itype) {
                                break;
                            }
                        }
                        Some(wc) => {
                            let ok = match itype {
                                IWORD => iswalnum(wc) || self.wordchars_has_wide(wc),
                                ISEP => self.ifs_has_wide(wc),
                                _ => iswalnum(wc),
                            };
                            if !ok {
                                return i;
                            }
                        }
                    }
                }
                i += len;
                if once {
                    break;
                }
            }
        } else {
            loop {
                let c = at(s, i);
                let chr = if c == META { at(s, i + 1) ^ 32 } else { c };
                if c == 0 || !self.zistype(chr, itype) {
                    break;
                }
                i += if c == META { 2 } else { 1 };
                if once {
                    break;
                }
            }
        }
        i
    }
}

/// `iswalnum` for Unicode characters beyond ASCII.
pub(crate) fn iswalnum(c: u32) -> bool {
    char::from_u32(c).is_some_and(char::is_alphanumeric)
}

/// Combining characters (zero width, attached to the previous character).
pub(crate) fn is_combining(c: u32) -> bool {
    wcwidth(c) == 0 && c >= 0x300
}

/// `iswprint`.
pub(crate) fn iswprint(c: u32) -> bool {
    if c < 0x20 || (0x7f..0xa0).contains(&c) {
        return false;
    }
    match char::from_u32(c) {
        None => false,
        Some(ch) => !ch.is_control() && !(0xfff0..=0xfff8).contains(&c) && c != 0xffff,
    }
}

/// `wcwidth`: 0 for combining and format characters, 2 for wide East Asian
/// ones, 1 otherwise, -1 for unprintable.
pub(crate) fn wcwidth(c: u32) -> i32 {
    if c == 0 {
        return 0;
    }
    if !iswprint(c) {
        return -1;
    }
    const ZERO: &[(u32, u32)] = &[
        (0x0300, 0x036f),
        (0x0483, 0x0489),
        (0x0591, 0x05bd),
        (0x05bf, 0x05bf),
        (0x05c1, 0x05c2),
        (0x05c4, 0x05c5),
        (0x05c7, 0x05c7),
        (0x0610, 0x061a),
        (0x064b, 0x065f),
        (0x0670, 0x0670),
        (0x06d6, 0x06dc),
        (0x06df, 0x06e4),
        (0x06e7, 0x06e8),
        (0x06ea, 0x06ed),
        (0x0711, 0x0711),
        (0x0730, 0x074a),
        (0x07a6, 0x07b0),
        (0x0901, 0x0902),
        (0x093c, 0x093c),
        (0x0941, 0x0948),
        (0x094d, 0x094d),
        (0x0951, 0x0954),
        (0x0962, 0x0963),
        (0x0e31, 0x0e31),
        (0x0e34, 0x0e3a),
        (0x0e47, 0x0e4e),
        (0x1ab0, 0x1aff),
        (0x1dc0, 0x1dff),
        (0x200b, 0x200f),
        (0x202a, 0x202e),
        (0x2060, 0x2064),
        (0x20d0, 0x20ff),
        (0xfe00, 0xfe0f),
        (0xfe20, 0xfe2f),
        (0xfeff, 0xfeff),
        (0x1d167, 0x1d169),
        (0x1d17b, 0x1d182),
        (0xe0001, 0xe007f),
        (0xe0100, 0xe01ef),
    ];
    if ZERO.iter().any(|&(a, b)| (a..=b).contains(&c)) {
        return 0;
    }
    const WIDE: &[(u32, u32)] = &[
        (0x1100, 0x115f),
        (0x231a, 0x231b),
        (0x2329, 0x232a),
        (0x23e9, 0x23ec),
        (0x23f0, 0x23f0),
        (0x23f3, 0x23f3),
        (0x25fd, 0x25fe),
        (0x2614, 0x2615),
        (0x2648, 0x2653),
        (0x267f, 0x267f),
        (0x2693, 0x2693),
        (0x26a1, 0x26a1),
        (0x26aa, 0x26ab),
        (0x26bd, 0x26be),
        (0x26c4, 0x26c5),
        (0x26ce, 0x26ce),
        (0x26d4, 0x26d4),
        (0x26ea, 0x26ea),
        (0x26f2, 0x26f3),
        (0x26f5, 0x26f5),
        (0x26fa, 0x26fa),
        (0x26fd, 0x26fd),
        (0x2705, 0x2705),
        (0x270a, 0x270b),
        (0x2728, 0x2728),
        (0x274c, 0x274c),
        (0x274e, 0x274e),
        (0x2753, 0x2755),
        (0x2757, 0x2757),
        (0x2795, 0x2797),
        (0x27b0, 0x27b0),
        (0x27bf, 0x27bf),
        (0x2b1b, 0x2b1c),
        (0x2b50, 0x2b50),
        (0x2b55, 0x2b55),
        (0x2e80, 0x303e),
        (0x3041, 0x33ff),
        (0x3400, 0x4dbf),
        (0x4e00, 0x9fff),
        (0xa000, 0xa4cf),
        (0xa960, 0xa97f),
        (0xac00, 0xd7a3),
        (0xf900, 0xfaff),
        (0xfe10, 0xfe19),
        (0xfe30, 0xfe6f),
        (0xff00, 0xff60),
        (0xffe0, 0xffe6),
        (0x16fe0, 0x16fe4),
        (0x17000, 0x18cff),
        (0x1b000, 0x1b2ff),
        (0x1f004, 0x1f004),
        (0x1f0cf, 0x1f0cf),
        (0x1f18e, 0x1f18e),
        (0x1f191, 0x1f19a),
        (0x1f200, 0x1f251),
        (0x1f300, 0x1f64f),
        (0x1f680, 0x1f6ff),
        (0x1f7e0, 0x1f7eb),
        (0x1f90c, 0x1f9ff),
        (0x1fa70, 0x1faff),
        (0x20000, 0x2fffd),
        (0x30000, 0x3fffd),
    ];
    if WIDE.iter().any(|&(a, b)| (a..=b).contains(&c)) {
        2
    } else {
        1
    }
}

/// Decode one UTF-8 character from unmetafied bytes: `(len, char)`, the
/// character `None` when the bytes are not a valid sequence (length 1 then).
pub(crate) fn utf8_char(s: &[u8]) -> (usize, Option<u32>) {
    let Some(&b) = s.first() else {
        return (0, None);
    };
    if b < 0x80 {
        return (1, Some(u32::from(b)));
    }
    let len = match b {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return (1, None),
    };
    match s.get(..len).and_then(|p| std::str::from_utf8(p).ok()) {
        Some(st) => (len, st.chars().next().map(u32::from)),
        None => (1, None),
    }
}

/// zsh's `mb_metacharlenconv`: the length in the metafied string `s` of its
/// first character, and the character (`None` for `WEOF`).
pub(crate) fn mb_metacharlenconv(sh: &Shell, s: &[u8]) -> (usize, Option<u32>) {
    let c = at(s, 0);
    if !sh.isset(MULTIBYTE) || c <= 0x7f {
        let wc = if c == META { at(s, 1) ^ 32 } else { c };
        return (1 + usize::from(c == META), Some(u32::from(wc)));
    }
    if tok::is_tok(c) {
        return (1, None);
    }
    metachar_utf8(s)
}

/// Decode one UTF-8 character from a metafied string, stopping at a token.
pub(crate) fn metachar_utf8(s: &[u8]) -> (usize, Option<u32>) {
    let mut bytes = [0u8; 4];
    let mut n = 0;
    let mut i = 0;
    while at(s, i) != 0 || i < s.len() {
        let c = at(s, i);
        if i >= s.len() {
            break;
        }
        let b = if c == META {
            i += 1;
            at(s, i) ^ 32
        } else if tok::is_meta(c) {
            break;
        } else {
            c
        };
        i += 1;
        if let Some(slot) = bytes.get_mut(n) {
            *slot = b;
        }
        n += 1;
        let need = match bytes[0] {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 0,
        };
        if need == 0 || n > 4 {
            break;
        }
        if n == need {
            if let Some(ch) = std::str::from_utf8(bytes.get(..n).unwrap_or(&[]))
                .ok()
                .and_then(|t| t.chars().next())
            {
                return (i, Some(u32::from(ch)));
            }
            break;
        }
    }
    if s.is_empty() {
        (0, None)
    } else {
        (1 + usize::from(at(s, 0) == META), None)
    }
}

/// `MB_METACHARLEN`.
pub(crate) fn mb_metacharlen(sh: &Shell, s: &[u8]) -> usize {
    mb_metacharlenconv(sh, s).0
}

/// zsh's `mb_metastrlenend(ptr, width, eptr)` over the whole of `s`.
pub(crate) fn mb_metastrlen(sh: &Shell, s: &[u8], width: i32) -> usize {
    if !sh.isset(MULTIBYTE) {
        return ztrlen(s);
    }
    let raw = tok::unmetafy(s);
    let mut n = 0usize;
    let mut i = 0;
    while i < raw.len() {
        let (len, wc) = utf8_char(from(&raw, i));
        let len = len.max(1);
        match wc {
            Some(c) if c >= 0x80 && width != 0 => {
                let w = wcwidth(c);
                if w > 0 {
                    n += if width == 1 {
                        usize::try_from(w).unwrap_or(0)
                    } else {
                        1
                    };
                }
            }
            _ => n += 1,
        }
        i += len;
    }
    n
}

/// `MB_METASTRLEN`.
pub(crate) fn mb_metastrlen0(sh: &Shell, s: &[u8]) -> usize {
    mb_metastrlen(sh, s, 0)
}

/// `MB_METASTRWIDTH`.
pub(crate) fn mb_metastrwidth(sh: &Shell, s: &[u8]) -> usize {
    mb_metastrlen(sh, s, 1)
}

/// zsh's `ztrlen`: the unmetafied length.
pub(crate) fn ztrlen(s: &[u8]) -> usize {
    let mut l = 0;
    let mut i = 0;
    while i < s.len() {
        if at(s, i) == META {
            i += 1;
        }
        i += 1;
        l += 1;
    }
    l
}

/// zsh's `ztrsub`: characters between two offsets in a metafied string.
pub(crate) fn ztrsub(s: &[u8], a: usize, b: usize) -> usize {
    ztrlen(sub(s, a, b))
}

/// zsh's `ztrcmp`.
pub(crate) fn ztrcmp(s1: &[u8], s2: &[u8]) -> std::cmp::Ordering {
    let mut i = 0;
    while i < s1.len() && at(s1, i) == at(s2, i) {
        i += 1;
    }
    let decode = |s: &[u8], i: usize| -> i32 {
        match s.get(i) {
            None => -1,
            Some(&META) => i32::from(at(s, i + 1) ^ 32),
            Some(&c) => i32::from(c),
        }
    };
    decode(s1, i).cmp(&decode(s2, i))
}

/// zsh's `metafy`.
pub(crate) fn metafy(raw: &[u8]) -> Vec<u8> {
    tok::metafy(raw)
}

/// zsh's `unmetafy`.
pub(crate) fn unmetafy(s: &[u8]) -> Vec<u8> {
    tok::unmetafy(s)
}

/// Lossy text for messages.
pub(crate) fn lossy(s: &[u8]) -> String {
    String::from_utf8_lossy(&tok::unmetafy(s)).into_owned()
}

/// zsh's `zjoin`.
pub(crate) fn zjoin(arr: &[Vec<u8>], delim: u8) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, s) in arr.iter().enumerate() {
        if k > 0 {
            if tok::is_meta(delim) {
                out.push(META);
                out.push(delim ^ 32);
            } else {
                out.push(delim);
            }
        }
        out.extend_from_slice(s);
    }
    out
}

/// zsh's `colonsplit`.
pub(crate) fn colonsplit(s: &[u8], uniq: bool) -> Vec<Vec<u8>> {
    let mut ret: Vec<Vec<u8>> = Vec::new();
    for part in s.split(|&c| c == b':') {
        if uniq && ret.iter().any(|p| p == part) {
            continue;
        }
        ret.push(part.to_vec());
    }
    ret
}

impl Shell {
    fn skipwsep(&self, s: &[u8], i: &mut usize) -> usize {
        let mut n = 0;
        loop {
            let c = at(s, *i);
            if c == 0 && *i >= s.len() {
                break;
            }
            let chr = if c == META { at(s, *i + 1) ^ 32 } else { c };
            if !self.iwsep(chr) {
                break;
            }
            if c == META {
                *i += 1;
            }
            *i += 1;
            n += 1;
        }
        n
    }

    /// zsh's `findsep`. `sep` `None` uses IFS; `Some(b"")` splits into
    /// characters. With `quote`, `\sep` is not a separator and the backslash
    /// is removed from `s`, so `s` is modified.
    fn findsep(&self, s: &mut Vec<u8>, i: &mut usize, sep: Option<&[u8]>, quote: bool) -> i32 {
        match sep {
            None => {
                let start = *i;
                let mut t = *i;
                while t < s.len() {
                    let ilen;
                    if quote && at(s, t) == b'\\' {
                        if at(s, t + 1) == b'\\' {
                            s.remove(t);
                            ilen = 1;
                        } else {
                            let (l, wc) = mb_metacharlenconv(self, from(s, t + 1));
                            if wc.is_some_and(|c| self.wcsitype(c, ISEP)) {
                                s.remove(t);
                                ilen = l;
                            } else {
                                if self.isep(at(s, t)) {
                                    break;
                                }
                                ilen = 1;
                            }
                        }
                    } else {
                        let (l, wc) = mb_metacharlenconv(self, from(s, t));
                        let c = wc.unwrap_or_else(|| u32::from(at(s, t)));
                        if self.wcsitype(c, ISEP) {
                            break;
                        }
                        ilen = l.max(1);
                    }
                    t += ilen;
                }
                *i = t;
                i32::from(t > start)
            }
            Some([]) => {
                if *i < s.len() {
                    *i += mb_metacharlen(self, from(s, *i)).max(1);
                    1
                } else {
                    -1
                }
            }
            Some(sep) => {
                let mut n = 0;
                while *i < s.len() {
                    if from(s, *i).starts_with(sep) {
                        return i32::from(n > 0);
                    }
                    *i += mb_metacharlen(self, from(s, *i)).max(1);
                    n += 1;
                }
                -1
            }
        }
    }

    /// zsh's `findword`: the start of the next word from `*i`, which is left
    /// at its end; `None` at the end of the string.
    pub(crate) fn findword(&self, s: &[u8], i: &mut usize, sep: Option<&[u8]>) -> Option<usize> {
        if *i >= s.len() {
            return None;
        }
        let mut buf = s.to_vec();
        if let Some(sep) = sep {
            let sl = sep.len();
            let mut r = *i;
            while self.findsep(&mut buf, i, Some(sep), false) == 0 {
                *i += sl;
                r = *i;
            }
            return Some(r);
        }
        let mut t = *i;
        while t < s.len() {
            let (l, wc) = mb_metacharlenconv(self, from(s, t));
            let c = wc.unwrap_or_else(|| u32::from(at(s, t)));
            if !self.wcsitype(c, ISEP) {
                break;
            }
            t += l.max(1);
        }
        *i = t;
        let _ = self.findsep(&mut buf, i, None, false);
        Some(t)
    }

    /// zsh's `wordcount`.
    pub(crate) fn wordcount(&self, s: &[u8], sep: Option<&[u8]>, mul: i32) -> usize {
        let mut buf = s.to_vec();
        let mut i = 0;
        if let Some(sep) = sep {
            let mut r = 1;
            let sl = sep.len();
            loop {
                let c = self.findsep(&mut buf, &mut i, Some(sep), false);
                if c < 0 {
                    break;
                }
                if (c != 0 || mul != 0) && (sl != 0 || at(&buf, i + sl) != 0) {
                    r += 1;
                }
                i += sl;
            }
            r
        } else {
            let mut r = 0;
            let t0 = i;
            if mul <= 0 {
                self.skipwsep(&buf, &mut i);
            }
            if (i < buf.len() && self.itype_end(&buf, i, ISEP, true) != i) || (mul < 0 && t0 != i) {
                r += 1;
            }
            let mut t = i;
            while i < buf.len() {
                let ie = self.itype_end(&buf, i, ISEP, true);
                if ie != i {
                    i = ie;
                    if mul <= 0 {
                        self.skipwsep(&buf, &mut i);
                    }
                }
                let _ = self.findsep(&mut buf, &mut i, None, false);
                t = i;
                if mul <= 0 {
                    self.skipwsep(&buf, &mut i);
                }
                r += 1;
            }
            if mul < 0 && t != i {
                r += 1;
            }
            r
        }
    }

    /// zsh's `spacesplit`.
    pub(crate) fn spacesplit(&self, s: &[u8], allownull: bool, quote: bool) -> Vec<Vec<u8>> {
        let nulstring = vec![NULARG];
        let mut ret = Vec::new();
        let mut buf = s.to_vec();
        let mut i = 0;
        let t0 = i;
        self.skipwsep(&buf, &mut i);
        if i < buf.len() && self.itype_end(&buf, i, ISEP, true) != i {
            ret.push(if allownull {
                Vec::new()
            } else {
                nulstring.clone()
            });
        } else if !allownull && t0 != i {
            ret.push(Vec::new());
        }
        let mut t = i;
        while i < buf.len() {
            let iend = self.itype_end(&buf, i, ISEP, true);
            if iend != i {
                i = iend;
                self.skipwsep(&buf, &mut i);
            } else if quote && at(&buf, i) == b'\\' {
                i += 1;
                self.skipwsep(&buf, &mut i);
            }
            t = i;
            let _ = self.findsep(&mut buf, &mut i, None, quote);
            if i > t || allownull {
                ret.push(sub(&buf, t, i).to_vec());
            } else {
                ret.push(nulstring.clone());
            }
            t = i;
            self.skipwsep(&buf, &mut i);
        }
        if !allownull && t != i {
            ret.push(Vec::new());
        }
        ret
    }

    /// zsh's `sepjoin`: `sep` `None` joins with the first character of IFS.
    pub(crate) fn sepjoin(&self, arr: &[Vec<u8>], sep: Option<&[u8]>) -> Vec<u8> {
        let sep: Vec<u8> = match sep {
            Some(s) => s.to_vec(),
            None => match &self.ifs {
                Some(ifs) if at(ifs, 0) != b' ' => {
                    let l = mb_metacharlen(self, ifs);
                    sub(ifs, 0, l).to_vec()
                }
                _ => b" ".to_vec(),
            },
        };
        arr.join(sep.as_slice())
    }

    /// zsh's `sepsplit`.
    pub(crate) fn sepsplit(&self, s: &[u8], sep: Option<&[u8]>, allownull: bool) -> Vec<Vec<u8>> {
        let s = if s == [NULARG] { from(s, 1) } else { s };
        let Some(sep) = sep else {
            return self.spacesplit(s, allownull, false);
        };
        let sl = sep.len();
        let n = self.wordcount(s, Some(sep), 1);
        let mut buf = s.to_vec();
        let mut out = Vec::with_capacity(n);
        let mut t = 0;
        for _ in 0..n {
            let tt = t;
            let _ = self.findsep(&mut buf, &mut t, Some(sep), false);
            out.push(sub(&buf, tt, t).to_vec());
            t += sl;
        }
        out
    }
}

/// zsh's `strpfx`.
pub(crate) fn strpfx(s: &[u8], t: &[u8]) -> bool {
    t.starts_with(s)
}

/// zsh's `strsfx`.
pub(crate) fn strsfx(s: &[u8], t: &[u8]) -> bool {
    t.ends_with(s)
}

/// Quote styles for [`Shell::quotestring`] (zsh's `QT_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Qt {
    None,
    Backslash,
    Single,
    Double,
    Dollars,
    Backtick,
    SingleOptional,
    BackslashPattern,
    BackslashShownull,
}

fn addunprintable(v: &mut Vec<u8>, u: &[u8]) {
    let mut i = 0;
    while i < u.len() {
        let c = if at(u, i) == META {
            i += 1;
            at(u, i) ^ 32
        } else {
            at(u, i)
        };
        match c {
            0 => {
                v.extend_from_slice(b"\\0");
                if (b'0'..=b'7').contains(&at(u, i + 1)) {
                    v.extend_from_slice(b"00");
                }
            }
            7 => v.extend_from_slice(b"\\a"),
            8 => v.extend_from_slice(b"\\b"),
            12 => v.extend_from_slice(b"\\f"),
            b'\n' => v.extend_from_slice(b"\\n"),
            b'\r' => v.extend_from_slice(b"\\r"),
            b'\t' => v.extend_from_slice(b"\\t"),
            11 => v.extend_from_slice(b"\\v"),
            _ => {
                v.push(b'\\');
                v.push(b'0' + ((c >> 6) & 7));
                v.push(b'0' + ((c >> 3) & 7));
                v.push(b'0' + (c & 7));
            }
        }
        i += 1;
    }
}

impl Shell {
    /// zsh's `quotestring`.
    #[expect(clippy::too_many_lines, reason = "one procedure in zsh")]
    pub(crate) fn quotestring(&self, s: &[u8], instring: Qt) -> Vec<u8> {
        let mut instring = instring;
        let mut shownull = false;
        let mut quotesub = 0;
        match instring {
            Qt::BackslashShownull => {
                shownull = true;
                instring = Qt::Backslash;
            }
            Qt::SingleOptional => {
                quotesub = 1;
                shownull = true;
            }
            _ => {}
        }
        let mut v: Vec<u8> = Vec::with_capacity(s.len() * 2 + 2);
        let mut quotestart = 0usize;
        let mut u = 0usize;
        let bang = self.bangchar;
        if instring == Qt::Dollars {
            if s.first().is_some_and(|&c| tok::is_null(c)) {
                u += 1;
            }
            while u < s.len() {
                let (l, cc) = mb_metacharlenconv(self, from(s, u));
                let uend = u + l.max(1);
                match cc {
                    Some(c) if iswprint(c) => {
                        if c == u32::from(b'\\')
                            || c == u32::from(b'\'')
                            || (self.isset(BANGHIST) && c == u32::from(bang))
                        {
                            v.push(b'\\');
                        }
                        v.extend_from_slice(sub(s, u, uend));
                    }
                    _ => addunprintable(&mut v, sub(s, u, uend)),
                }
                u = uend;
            }
        } else if instring == Qt::BackslashPattern {
            while u < s.len() {
                if self.ipattern(at(s, u)) {
                    v.push(b'\\');
                }
                v.push(at(s, u));
                u += 1;
            }
        } else {
            if shownull && s.is_empty() {
                v.extend_from_slice(b"''");
            }
            while u < s.len() {
                let mut dobackslash = false;
                let c = at(s, u);
                if c == TICK || c == QTICK {
                    v.push(c);
                    u += 1;
                    while u < s.len() && at(s, u) != c {
                        v.push(at(s, u));
                        u += 1;
                    }
                    v.push(c);
                    if u < s.len() {
                        u += 1;
                    }
                    continue;
                } else if (c == QSTRING || c == b'$')
                    && at(s, u + 1) == b'\''
                    && instring == Qt::Double
                {
                    v.push(c);
                    u += 1;
                } else if (c == STRING || c == QSTRING)
                    && matches!(at(s, u + 1), INPAR | INBRACK | INBRACE)
                {
                    let close = match at(s, u + 1) {
                        INPAR => OUTPAR,
                        INBRACE => OUTBRACE,
                        _ => OUTBRACK,
                    };
                    let beg = c;
                    let mut level = 0;
                    v.push(at(s, u));
                    v.push(at(s, u + 1));
                    u += 2;
                    while u < s.len() && (at(s, u) != close || level != 0) {
                        if at(s, u) == beg {
                            level += 1;
                        } else if at(s, u) == close {
                            level -= 1;
                        }
                        v.push(at(s, u));
                        u += 1;
                    }
                    if u < s.len() {
                        v.push(at(s, u));
                        u += 1;
                    }
                    continue;
                } else if self.ispecial(c)
                    && ((c != b'=' && c != b'~')
                        || u == 0
                        || (self.isset(MAGICEQUALSUBST)
                            && (at(s, u.wrapping_sub(1)) == b'='
                                || at(s, u.wrapping_sub(1)) == b':'))
                        || (c == b'~' && self.isset(EXTENDEDGLOB)))
                    && (instring == Qt::Backslash
                        || instring == Qt::SingleOptional
                        || (self.isset(BANGHIST) && c == bang && instring != Qt::Single)
                        || (instring == Qt::Double && matches!(c, b'$' | b'`' | b'"' | b'\\'))
                        || (instring == Qt::Single && c == b'\''))
                {
                    if instring == Qt::SingleOptional {
                        if quotesub == 1 {
                            if c == b'\'' {
                                v.push(b'\\');
                            } else {
                                v.insert(quotestart, b'\'');
                                quotesub = 2;
                            }
                            v.push(at(s, u));
                            u += 1;
                            quotestart = v.len();
                        } else if c == b'\'' {
                            if !self.isset(RCQUOTES) {
                                v.extend_from_slice(b"'\\'");
                                quotesub = 1;
                                quotestart = v.len();
                            } else {
                                v.extend_from_slice(b"''");
                            }
                            u += 1;
                        } else {
                            v.push(at(s, u));
                            u += 1;
                        }
                        continue;
                    } else if c == b'\n' || (instring == Qt::Single && c == b'\'') {
                        if c == b'\n' {
                            v.extend_from_slice(b"$'\\n'");
                        } else if !self.isset(RCQUOTES) {
                            v.push(b'\'');
                            v.push(b'\\');
                            v.push(c);
                            v.push(b'\'');
                        } else {
                            v.extend_from_slice(b"''");
                        }
                        u += 1;
                        continue;
                    } else {
                        dobackslash = true;
                    }
                }
                if tok::is_tok(at(s, u)) || instring != Qt::Backslash {
                    if dobackslash {
                        v.push(b'\\');
                    }
                    if at(s, u) == INPARMATH {
                        let mut inmath = 1;
                        v.push(at(s, u));
                        u += 1;
                        loop {
                            let uc = at(s, u);
                            if u >= s.len() {
                                break;
                            }
                            v.push(uc);
                            u += 1;
                            if uc == OUTPARMATH {
                                inmath -= 1;
                                if inmath == 0 {
                                    break;
                                }
                            } else if uc == INPARMATH {
                                inmath += 1;
                            }
                        }
                    } else {
                        v.push(at(s, u));
                        u += 1;
                    }
                    continue;
                }
                let (l, cc) = mb_metacharlenconv(self, from(s, u));
                let uend = u + l.max(1);
                match cc {
                    Some(ch) if iswprint(ch) => {
                        if dobackslash {
                            v.push(b'\\');
                        }
                        v.extend_from_slice(sub(s, u, uend));
                    }
                    _ => {
                        v.extend_from_slice(b"$'");
                        addunprintable(&mut v, sub(s, u, uend));
                        v.push(b'\'');
                    }
                }
                u = uend;
            }
        }
        if quotesub == 2 {
            v.push(b'\'');
        }
        v
    }

    fn hasspecial(&self, s: &[u8]) -> bool {
        let mut i = 0;
        while i < s.len() {
            let mut c = at(s, i);
            if c == META {
                i += 1;
                c = at(s, i) ^ 32;
            }
            if self.ispecial(c) {
                return true;
            }
            i += 1;
        }
        false
    }

    /// zsh's `quotedzputs` returning the metafied text.
    pub(crate) fn quotedzputs(&self, s: &[u8]) -> Vec<u8> {
        if s.is_empty() {
            return b"''".to_vec();
        }
        if self.is_mb_niceformat(s) {
            let mut out = b"$'".to_vec();
            out.extend(self.mb_niceformat(s, true).0);
            out.push(b'\'');
            return out;
        }
        if !self.hasspecial(s) {
            return s.to_vec();
        }
        let mut out = Vec::with_capacity(s.len() + 4);
        let push = |out: &mut Vec<u8>, c: u8| {
            if tok::is_meta(c) {
                out.push(META);
                out.push(c ^ 32);
            } else {
                out.push(c);
            }
        };
        let mut i = 0;
        let next = |i: &mut usize| -> u8 {
            let c = at(s, *i);
            let r = if c == DASH {
                b'-'
            } else if c == META {
                *i += 1;
                at(s, *i) ^ 32
            } else {
                c
            };
            *i += 1;
            r
        };
        if self.isset(RCQUOTES) {
            out.push(b'\'');
            while i < s.len() {
                let c = next(&mut i);
                if c == b'\'' {
                    out.push(b'\'');
                } else if c == b'\n' && self.isset(CSHJUNKIEQUOTES) {
                    out.push(b'\\');
                }
                push(&mut out, c);
            }
            out.push(b'\'');
        } else {
            let mut inquote = false;
            while i < s.len() {
                let c = next(&mut i);
                if c == b'\'' {
                    if inquote {
                        out.push(b'\'');
                        inquote = false;
                    }
                    out.extend_from_slice(b"\\'");
                } else {
                    if !inquote {
                        out.push(b'\'');
                        inquote = true;
                    }
                    if c == b'\n' && self.isset(CSHJUNKIEQUOTES) {
                        out.push(b'\\');
                    }
                    push(&mut out, c);
                }
            }
            if inquote {
                out.push(b'\'');
            }
        }
        out
    }

    /// zsh's `nicechar_sel`: the printable form of byte `c`, metafied.
    pub(crate) fn nicechar_sel(&self, c: u8, quotable: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut c = c;
        let printable = |c: u8| (0x20..0x7f).contains(&c);
        if !printable(c) {
            let mut done = false;
            if c & 0x80 != 0 {
                if self.isset(PRINTEIGHTBIT) {
                    done = true;
                } else {
                    out.extend_from_slice(b"\\M-");
                    c &= 0x7f;
                    if printable(c) {
                        done = true;
                    }
                }
            }
            if !done {
                if c == 0x7f {
                    out.extend_from_slice(if quotable { b"\\C-" } else { b"^" });
                    c = b'?';
                } else if c == b'\n' {
                    out.push(b'\\');
                    c = b'n';
                } else if c == b'\t' {
                    out.push(b'\\');
                    c = b't';
                } else if c < 0x20 {
                    out.extend_from_slice(if quotable { b"\\C-" } else { b"^" });
                    c += 0x40;
                }
            }
        }
        if tok::is_meta(c) {
            out.push(META);
            out.push(c ^ 32);
        } else {
            out.push(c);
        }
        out
    }

    pub(crate) fn nicechar(&self, c: u8) -> Vec<u8> {
        self.nicechar_sel(c, false)
    }

    fn is_nicechar(&self, c: u8) -> bool {
        if (0x20..0x7f).contains(&c) {
            return false;
        }
        if c & 0x80 != 0 {
            return !self.isset(PRINTEIGHTBIT);
        }
        c == 0x7f || c == b'\n' || c == b'\t' || c < 0x20
    }

    /// zsh's `wcs_nicechar_sel`: `(text, width)`.
    pub(crate) fn wcs_nicechar_sel(&self, c: u32, quotable: bool) -> (Vec<u8>, usize) {
        let mut buf = Vec::new();
        let mut c = c;
        let mut convert = true;
        if !iswprint(c) && (c < 0x80 || !self.isset(PRINTEIGHTBIT)) {
            if c == 0x7f {
                buf.extend_from_slice(if quotable { b"\\C-" } else { b"^" });
                c = u32::from(b'?');
            } else if c == u32::from(b'\n') {
                buf.push(b'\\');
                c = u32::from(b'n');
            } else if c == u32::from(b'\t') {
                buf.push(b'\\');
                c = u32::from(b't');
            } else if c < 0x20 {
                buf.extend_from_slice(if quotable { b"\\C-" } else { b"^" });
                c += 0x40;
            } else if c >= 0x80 {
                convert = false;
            }
        }
        if !convert {
            if c >= 0x10000 {
                return (format!("\\U{c:08x}").into_bytes(), 10);
            } else if c >= 0x100 {
                return (format!("\\u{c:04x}").into_bytes(), 6);
            }
            let t = self.nicechar_sel(u8::try_from(c).unwrap_or(b'?'), quotable);
            let w = ztrlen(&t);
            return (t, w);
        }
        let prefix = buf.len();
        let w = wcwidth(c);
        let width = prefix
            + if w >= 0 {
                usize::try_from(w).unwrap_or(1)
            } else {
                1
            };
        let mut tmp = [0u8; 4];
        let enc = char::from_u32(c).map_or(&b"?"[..], |ch| ch.encode_utf8(&mut tmp).as_bytes());
        buf.extend(tok::metafy(enc));
        (buf, width)
    }

    pub(crate) fn wcs_nicechar(&self, c: u32) -> (Vec<u8>, usize) {
        self.wcs_nicechar_sel(c, false)
    }

    fn is_wcs_nicechar(&self, c: u32) -> bool {
        if !iswprint(c) && (c < 0x80 || !self.isset(PRINTEIGHTBIT)) {
            if c == 0x7f || c == u32::from(b'\n') || c == u32::from(b'\t') || c < 0x20 {
                return true;
            }
            if c >= 0x80 {
                return c >= 0x100 || self.is_nicechar(u8::try_from(c).unwrap_or(0));
            }
        }
        false
    }

    /// zsh's `mb_niceformat`: the printable text of `s` and its width.
    pub(crate) fn mb_niceformat(&self, s: &[u8], quote: bool) -> (Vec<u8>, usize) {
        let mut ums = s.to_vec();
        tok::untokenize(&mut ums);
        let raw = tok::unmetafy(&ums);
        let mut out = Vec::new();
        let mut l = 0;
        let mut i = 0;
        while i < raw.len() {
            let (len, wc) = utf8_char(from(&raw, i));
            match wc {
                None => {
                    let t = self.nicechar_sel(at(&raw, i), quote);
                    l += t.len();
                    out.extend(t);
                    i += 1;
                }
                Some(c) => {
                    if c == u32::from(b'\'') && quote {
                        out.extend_from_slice(b"\\'");
                        l += 2;
                    } else if c == u32::from(b'\\') && quote {
                        out.extend_from_slice(b"\\\\");
                        l += 2;
                    } else {
                        let (t, w) = self.wcs_nicechar_sel(c, quote);
                        l += w;
                        out.extend(t);
                    }
                    i += len.max(1);
                }
            }
        }
        (out, l)
    }

    pub(crate) fn is_mb_niceformat(&self, s: &[u8]) -> bool {
        let mut ums = s.to_vec();
        tok::untokenize(&mut ums);
        let raw = tok::unmetafy(&ums);
        let mut i = 0;
        while i < raw.len() {
            let (len, wc) = utf8_char(from(&raw, i));
            match wc {
                None => {
                    if self.is_nicechar(at(&raw, i)) {
                        return true;
                    }
                    i += 1;
                }
                Some(c) => {
                    if self.is_wcs_nicechar(c) {
                        return true;
                    }
                    i += len.max(1);
                }
            }
        }
        false
    }

    /// zsh's `nicedup`.
    pub(crate) fn nicedup(&self, s: &[u8]) -> Vec<u8> {
        self.mb_niceformat(s, false).0
    }

    /// zsh's `niceztrlen` (width of the printable form).
    pub(crate) fn niceztrlen(&self, s: &[u8]) -> usize {
        self.mb_niceformat(s, false).1
    }
}

/// zsh's `zputs`: unmetafy, dropping tokens.
pub(crate) fn zputs(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let c = at(s, i);
        if c == META {
            i += 1;
            out.push(at(s, i) ^ 32);
        } else if !tok::is_tok(c) {
            out.push(c);
        }
        i += 1;
    }
    out
}

pub(crate) const GETKEY_OCTAL_ESC: u32 = 1 << 0;
pub(crate) const GETKEY_EMACS: u32 = 1 << 1;
pub(crate) const GETKEY_CTRL: u32 = 1 << 2;
pub(crate) const GETKEY_BACKSLASH_C: u32 = 1 << 3;
pub(crate) const GETKEY_DOLLAR_QUOTE: u32 = 1 << 4;
pub(crate) const GETKEY_BACKSLASH_MINUS: u32 = 1 << 5;
pub(crate) const GETKEY_SINGLE_CHAR: u32 = 1 << 6;
pub(crate) const GETKEY_UPDATE_OFFSET: u32 = 1 << 7;
pub(crate) const GETKEY_PRINTF_PERCENT: u32 = 1 << 8;
pub(crate) const GETKEYS_ECHO: u32 = GETKEY_BACKSLASH_C;
pub(crate) const GETKEYS_PRINTF_FMT: u32 =
    GETKEY_OCTAL_ESC | GETKEY_BACKSLASH_C | GETKEY_PRINTF_PERCENT;
pub(crate) const GETKEYS_PRINTF_ARG: u32 = GETKEY_BACKSLASH_C;
pub(crate) const GETKEYS_PRINT: u32 = GETKEY_OCTAL_ESC | GETKEY_BACKSLASH_C | GETKEY_EMACS;
pub(crate) const GETKEYS_BINDKEY: u32 = GETKEY_OCTAL_ESC | GETKEY_EMACS | GETKEY_CTRL;
pub(crate) const GETKEYS_DOLLARS_QUOTE: u32 = GETKEY_OCTAL_ESC | GETKEY_EMACS | GETKEY_DOLLAR_QUOTE;
pub(crate) const GETKEYS_MATH: u32 =
    GETKEY_OCTAL_ESC | GETKEY_EMACS | GETKEY_CTRL | GETKEY_SINGLE_CHAR;
pub(crate) const GETKEYS_SEP: u32 = GETKEY_OCTAL_ESC | GETKEY_EMACS;
pub(crate) const GETKEYS_SUFFIX: u32 =
    GETKEY_OCTAL_ESC | GETKEY_EMACS | GETKEY_CTRL | GETKEY_BACKSLASH_MINUS;

/// What [`Shell::getkeystring`] found besides the text.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct KeyMisc {
    /// `\c` (GETKEY_BACKSLASH_C) or `\-` (GETKEY_BACKSLASH_MINUS) was seen.
    pub(crate) flag: bool,
    /// The character code for GETKEY_SINGLE_CHAR.
    pub(crate) chr: u32,
    /// For GETKEY_DOLLAR_QUOTE: the input length consumed up to and
    /// including the closing quote.
    pub(crate) used: usize,
    /// For GETKEY_SINGLE_CHAR: where parsing stopped in the input.
    pub(crate) next: Option<usize>,
}

impl Shell {
    /// zsh's `getkeystring`. The result is unmetafied unless
    /// GETKEY_DOLLAR_QUOTE is in `how`.
    #[expect(clippy::too_many_lines, reason = "one procedure in zsh")]
    pub(crate) fn getkeystring(&self, s: &[u8], how: u32) -> (Vec<u8>, KeyMisc) {
        let mut misc = KeyMisc::default();
        let dq = how & GETKEY_DOLLAR_QUOTE != 0;
        let single = how & GETKEY_SINGLE_CHAR != 0;
        let mut buf: Vec<u8> = Vec::with_capacity(s.len());
        // The escape currently being converted; flushed into buf (metafied
        // when dq) after each character.
        let mut t: Vec<u8> = Vec::new();
        let (mut meta, mut control, mut ignoring) = (0u8, false, false);
        let mut i = 0usize;
        macro_rules! flush {
            () => {
                if dq {
                    for &b in &t {
                        if self.isset(POSIXSTRINGS) {
                            if b == 0 {
                                ignoring = true;
                            }
                            if ignoring {
                                break;
                            }
                        }
                        if tok::is_meta(b) {
                            buf.push(META);
                            buf.push(b ^ 32);
                        } else {
                            buf.push(b);
                        }
                    }
                } else {
                    buf.extend_from_slice(&t);
                }
                t.clear();
            };
        }
        while i < s.len() {
            let c = at(s, i);
            if c == b'\\' && i + 1 < s.len() {
                i += 1;
                let e = at(s, i);
                match e {
                    b'a' => t.push(7),
                    b'n' => t.push(b'\n'),
                    b'b' => t.push(8),
                    b't' => t.push(b'\t'),
                    b'v' => t.push(11),
                    b'f' => t.push(12),
                    b'r' => t.push(b'\r'),
                    b'E' if how & GETKEY_EMACS == 0 => {
                        t.push(b'\\');
                        flush!();
                        continue;
                    }
                    b'E' | b'e' => t.push(0x1b),
                    b'M' => {
                        if how & GETKEY_EMACS != 0 {
                            if at(s, i + 1) == b'-' {
                                i += 1;
                            }
                            meta = 1 + u8::from(control);
                        } else {
                            t.push(b'\\');
                            flush!();
                            continue;
                        }
                        i += 1;
                        continue;
                    }
                    b'C' => {
                        if how & GETKEY_EMACS != 0 {
                            if at(s, i + 1) == b'-' {
                                i += 1;
                            }
                            control = true;
                        } else {
                            t.push(b'\\');
                            flush!();
                            continue;
                        }
                        i += 1;
                        continue;
                    }
                    META => {
                        t.push(b'\\');
                        flush!();
                        continue;
                    }
                    b'-' if how & GETKEY_BACKSLASH_MINUS != 0 => {
                        misc.flag = true;
                        i += 1;
                        continue;
                    }
                    b'c' if how & GETKEY_BACKSLASH_C != 0 => {
                        misc.flag = true;
                        flush!();
                        return (buf, misc);
                    }
                    b'u' | b'U' => {
                        let mut wval: u32 = 0;
                        let n = if e == b'u' { 4 } else { 8 };
                        for _ in 0..n {
                            let d = at(s, i + 1);
                            if let Some(v) = char::from(d).to_digit(16).filter(|_| d != 0) {
                                wval = wval.wrapping_mul(16).wrapping_add(v);
                                i += 1;
                            } else {
                                break;
                            }
                        }
                        if single {
                            misc.chr = wval;
                            misc.next = Some(i + 1);
                            return (buf, misc);
                        }
                        match char::from_u32(wval) {
                            Some(ch) => {
                                let mut tmp = [0u8; 4];
                                t.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                            }
                            None => {
                                self.zerr("character not in range");
                                flush!();
                                return (buf, misc);
                            }
                        }
                        flush!();
                        i += 1;
                        continue;
                    }
                    b'\'' | b'\\' if dq => t.push(e),
                    _ => {
                        if (e.is_ascii_digit() && e < b'8') || e == b'x' {
                            let mut j = i;
                            if how & GETKEY_OCTAL_ESC == 0 {
                                if e == b'0' {
                                    j += 1;
                                } else if e != b'x' {
                                    t.push(b'\\');
                                    flush!();
                                    continue;
                                }
                            }
                            let hex = at(s, j) == b'x';
                            let start = if hex { j + 1 } else { j };
                            // At most three characters from where zsh
                            // truncated the string (j + 3).
                            let limit =
                                if at(s, j + 1) != 0 && at(s, j + 2) != 0 && at(s, j + 3) != 0 {
                                    j + 3
                                } else {
                                    s.len()
                                };
                            let radix = if hex { 16 } else { 8 };
                            let mut k = start;
                            let mut val: u32 = 0;
                            while k < limit.min(s.len()) {
                                match char::from(at(s, k)).to_digit(radix) {
                                    Some(d) => {
                                        val = val.wrapping_mul(radix).wrapping_add(d);
                                        k += 1;
                                    }
                                    None => break,
                                }
                            }
                            let b = u8::try_from(val & 0xff).unwrap_or(0);
                            t.push(b);
                            if how & GETKEY_PRINTF_PERCENT != 0 && b == b'%' {
                                t.push(b'%');
                            }
                            i = k;
                            // zsh leaves s on the last digit and the loop
                            // increments; here i is already past it.
                            if meta == 2 {
                                if let Some(l) = t.last_mut() {
                                    *l |= 0x80;
                                }
                                meta = 0;
                            }
                            if control {
                                if let Some(l) = t.last_mut() {
                                    *l = if *l == b'?' { 0x7f } else { *l & 0x9f };
                                }
                                control = false;
                            }
                            if meta != 0 {
                                if let Some(l) = t.last_mut() {
                                    *l |= 0x80;
                                }
                                meta = 0;
                            }
                            if single {
                                misc.chr = u32::from(at(&t, 0));
                                misc.next = Some(i);
                                return (buf, misc);
                            }
                            flush!();
                            continue;
                        }
                        if how & GETKEY_EMACS == 0 && e != b'\\' {
                            t.push(b'\\');
                        }
                        t.push(e);
                    }
                }
            } else if dq && c == SNULL {
                flush!();
                misc.used = i + 1;
                return (buf, misc);
            } else if c == b'^' && !control && how & GETKEY_CTRL != 0 && i + 1 < s.len() {
                control = true;
                i += 1;
                continue;
            } else if single && self.isset(MULTIBYTE) && c > 127 {
                let (len, wc) = mb_metacharlenconv(self, from(s, i));
                if let Some(wc) = wc {
                    misc.chr = wc;
                    misc.next = Some(i + len);
                    return (buf, misc);
                }
                t.push(c);
            } else if c == META {
                i += 1;
                t.push(at(s, i) ^ 32);
            } else if tok::is_tok(c) {
                if meta != 0 || control {
                    if dq && c == BNULL {
                        i += 1;
                        t.push(at(s, i));
                    } else {
                        t.push(tok::detok(c));
                    }
                } else if dq {
                    flush!();
                    buf.push(c);
                    if c == BNULL {
                        i += 1;
                        buf.push(at(s, i));
                    }
                    i += 1;
                    continue;
                } else {
                    t.push(c);
                }
            } else {
                t.push(c);
            }
            if meta == 2 {
                if let Some(l) = t.last_mut() {
                    *l |= 0x80;
                }
                meta = 0;
            }
            if control {
                if let Some(l) = t.last_mut() {
                    *l = if *l == b'?' { 0x7f } else { *l & 0x9f };
                }
                control = false;
            }
            if meta != 0 {
                if let Some(l) = t.last_mut() {
                    *l |= 0x80;
                }
                meta = 0;
            }
            if single && !t.is_empty() {
                misc.chr = u32::from(at(&t, 0));
                misc.next = Some(i + 1);
                return (buf, misc);
            }
            flush!();
            i += 1;
        }
        if single {
            misc.chr = 0;
            misc.next = None;
            return (buf, misc);
        }
        flush!();
        misc.used = s.len();
        (buf, misc)
    }
}

/// zsh's `zstrtol_underscore`: `(value, end index)`. Base 0 reads `0x`,
/// `0b` and a leading `0` as octal.
pub(crate) fn zstrtol_underscore(s: &[u8], mut base: u32, underscore: bool) -> (i64, usize) {
    let mut i = 0;
    while matches!(at(s, i), b' ' | b'\t') {
        i += 1;
    }
    let neg = at(s, i) == b'-';
    if neg || at(s, i) == b'+' {
        i += 1;
    }
    if base == 0 {
        if at(s, i) != b'0' {
            base = 10;
        } else if matches!(at(s, i + 1), b'x' | b'X') {
            base = 16;
            i += 2;
        } else if matches!(at(s, i + 1), b'b' | b'B') {
            base = 2;
            i += 2;
        } else {
            base = 8;
        }
    }
    let mut calc: u64 = 0;
    let mut overflow = false;
    loop {
        let c = at(s, i);
        if underscore && c == b'_' {
            i += 1;
            continue;
        }
        let Some(d) = char::from(c).to_digit(base).filter(|_| c != 0) else {
            break;
        };
        match calc
            .checked_mul(u64::from(base))
            .and_then(|v| v.checked_add(u64::from(d)))
        {
            Some(v) => calc = v,
            None => overflow = true,
        }
        i += 1;
    }
    if overflow {
        calc = u64::MAX;
    }
    #[expect(clippy::cast_possible_wrap, reason = "zsh's zlong wraps the same way")]
    let v = calc as i64;
    (if neg { v.wrapping_neg() } else { v }, i)
}

/// zsh's `zstrtol`.
pub(crate) fn zstrtol(s: &[u8], base: u32) -> (i64, usize) {
    zstrtol_underscore(s, base, false)
}

/// zsh's `dquotedztrdup`.
pub(crate) fn dquotedztrdup(sh: &Shell, s: &[u8]) -> Vec<u8> {
    let raw = tok::unmetafy(s);
    let mut p = Vec::with_capacity(raw.len() * 2 + 2);
    if sh.isset(CSHJUNKIEQUOTES) {
        let mut inquote = false;
        for &c in &raw {
            match c {
                b'"' | b'$' | b'`' => {
                    if inquote {
                        p.push(b'"');
                        inquote = false;
                    }
                    p.push(b'\\');
                    p.push(c);
                }
                _ => {
                    if !inquote {
                        p.push(b'"');
                        inquote = true;
                    }
                    if c == b'\n' {
                        p.push(b'\\');
                    }
                    p.push(c);
                }
            }
        }
        if inquote {
            p.push(b'"');
        }
    } else {
        let mut pending = false;
        p.push(b'"');
        for &c in &raw {
            match c {
                b'\\' => {
                    if pending {
                        p.push(b'\\');
                    }
                    p.push(b'\\');
                    pending = true;
                }
                b'"' | b'$' | b'`' => {
                    if pending {
                        p.push(b'\\');
                    }
                    p.push(b'\\');
                    p.push(c);
                    pending = false;
                }
                _ => {
                    p.push(c);
                    pending = false;
                }
            }
        }
        if pending {
            p.push(b'\\');
        }
        p.push(b'"');
    }
    tok::metafy(&p)
}

/// zsh's `tulower`/`tuupper` on a byte.
pub(crate) fn tulower(c: u8) -> u8 {
    c.to_ascii_lowercase()
}

pub(crate) fn tuupper(c: u8) -> u8 {
    c.to_ascii_uppercase()
}

/// zsh's `untokenize` on a copy.
pub(crate) fn untokenized(s: &[u8]) -> Vec<u8> {
    let mut v = s.to_vec();
    tok::untokenize(&mut v);
    v
}

/// zsh's `has_token`.
pub(crate) fn has_token(s: &[u8]) -> bool {
    tok::has_token(s)
}

/// zsh's `remnulargs` (glob.c): remove the quote placeholders after the
/// first one, a kept backslash becoming `\`; a word left empty becomes a
/// lone `Nularg`, the marker for an empty argument.
pub(crate) fn remnulargs(s: &mut Vec<u8>) {
    let Some(first) = s
        .iter()
        .position(|&c| tok::is_null(c) && c != tok::BNULLKEEP)
    else {
        return;
    };
    let mut out: Vec<u8> = s.get(..first).unwrap_or(&[]).to_vec();
    for &c in s.get(first..).unwrap_or(&[]) {
        if c == tok::BNULLKEEP {
            out.push(b'\\');
        } else if !tok::is_null(c) {
            out.push(c);
        }
    }
    if out.is_empty() {
        out.push(NULARG);
    }
    *s = out;
}

/// C's `atoi`: leading blanks, an optional sign, then digits.
pub(crate) fn atoi(s: &[u8]) -> i64 {
    let mut i = 0;
    while s
        .get(i)
        .is_some_and(|&c| c == b' ' || c == b'\t' || c == b'\n')
    {
        i += 1;
    }
    let neg = match s.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let mut v: i64 = 0;
    while let Some(&c) = s.get(i) {
        if !c.is_ascii_digit() {
            break;
        }
        v = v.wrapping_mul(10).wrapping_add(i64::from(c - b'0'));
        i += 1;
    }
    if neg { v.wrapping_neg() } else { v }
}

/// zsh's `%e` text for `errno`.
pub(crate) fn strerror(e: i32) -> String {
    crate::sysutil::errmsg(e)
}

/// Every user name and home directory in the password database.
pub(crate) fn all_passwd_entries() -> Vec<(Vec<u8>, Vec<u8>)> {
    let Ok(p) = std::fs::read("/etc/passwd") else {
        return Vec::new();
    };
    p.split(|&c| c == b'\n')
        .filter_map(|l| {
            let f: Vec<&[u8]> = l.split(|&c| c == b':').collect();
            let name = f.first().filter(|n| !n.is_empty())?;
            let dir = f.get(5)?;
            Some((name.to_vec(), dir.to_vec()))
        })
        .collect()
}

/// The user name of `uid` in the password database.
pub(crate) fn user_name_of(uid: u32) -> Option<Vec<u8>> {
    let p = std::fs::read("/etc/passwd").ok()?;
    p.split(|&c| c == b'\n').find_map(|l| {
        let f: Vec<&[u8]> = l.split(|&c| c == b':').collect();
        let id: u32 = std::str::from_utf8(f.get(2)?).ok()?.parse().ok()?;
        (id == uid).then(|| f.first().map(|n| n.to_vec())).flatten()
    })
}

/// The home directory of `user` in the password database.
pub(crate) fn user_home_of(user: &[u8]) -> Option<Vec<u8>> {
    all_passwd_entries()
        .into_iter()
        .find(|(n, _)| n == user)
        .map(|(_, d)| d)
}

/// The user and group ids of `user` in the password database.
pub(crate) fn passwd_ids(user: &[u8]) -> Option<(u32, u32)> {
    let p = std::fs::read("/etc/passwd").ok()?;
    p.split(|&c| c == b'\n').find_map(|l| {
        let f: Vec<&[u8]> = l.split(|&c| c == b':').collect();
        if f.first() != Some(&user) {
            return None;
        }
        let uid: u32 = std::str::from_utf8(f.get(2)?).ok()?.parse().ok()?;
        let gid: u32 = std::str::from_utf8(f.get(3)?).ok()?.parse().ok()?;
        Some((uid, gid))
    })
}
