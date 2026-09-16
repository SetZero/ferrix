//! Prompt expansion (zsh's `prompt.c`): `%` escapes, `!` history numbers,
//! `%(x.a.b)` conditionals, `%<<`/`%>>`/`%[]` truncation, text attributes
//! and colours, plus `ztrftime` from `utils.c` and `printprompt4`.
//!
//! zsh reads terminal capabilities through termcap; zinc links no curses, so
//! `termcaps` carries the terminfo entries of the terminal families zinc
//! meets. An unlisted `$TERM` behaves as zsh does for an unknown terminal:
//! attribute escapes print nothing and colours fall back to ANSI sequences.

use crate::exec::{FS_EVAL, FS_SOURCE};
use crate::options::*;
use crate::params::{ArrVar, StrVar};
use crate::shell::{ERRFLAG_INT, Shell, write_fd};
use crate::tok::{self, INPAR, META, NULARG, OUTPAR};
use crate::utils::{utf8_char, wcwidth, zstrtol};

pub(crate) const TXTBOLDFACE: u64 = 0x0001;
pub(crate) const TXTSTANDOUT: u64 = 0x0002;
pub(crate) const TXTUNDERLINE: u64 = 0x0004;
pub(crate) const TXTFGCOLOUR: u64 = 0x0008;
pub(crate) const TXTBGCOLOUR: u64 = 0x0010;
pub(crate) const TXT_ATTR_ON_MASK: u64 = 0x001F;
pub(crate) const TXTNOBOLDFACE: u64 = 0x0020;
pub(crate) const TXTNOSTANDOUT: u64 = 0x0040;
pub(crate) const TXTNOUNDERLINE: u64 = 0x0080;
pub(crate) const TXTNOFGCOLOUR: u64 = 0x0100;
pub(crate) const TXTNOBGCOLOUR: u64 = 0x0200;
pub(crate) const TXT_ERROR: u64 = 0x0800;
pub(crate) const TXT_ATTR_FG_COL_MASK: u64 = 0x0000_00FF_FFFF_0000;
pub(crate) const TXT_ATTR_FG_COL_SHIFT: u32 = 16;
pub(crate) const TXT_ATTR_BG_COL_MASK: u64 = 0xFFFF_FF00_0000_0000;
pub(crate) const TXT_ATTR_BG_COL_SHIFT: u32 = 40;
pub(crate) const TXT_ATTR_FG_24BIT: u64 = 0x4000;
pub(crate) const TXT_ATTR_BG_24BIT: u64 = 0x8000;
pub(crate) const TXT_ATTR_FG_ON_MASK: u64 = TXTFGCOLOUR | TXT_ATTR_FG_COL_MASK | TXT_ATTR_FG_24BIT;
pub(crate) const TXT_ATTR_BG_ON_MASK: u64 = TXTBGCOLOUR | TXT_ATTR_BG_COL_MASK | TXT_ATTR_BG_24BIT;

pub(crate) const COL_SEQ_FG: usize = 0;
pub(crate) const COL_SEQ_BG: usize = 1;

pub(crate) const TSC_RAW: i32 = 0x0001;
pub(crate) const TSC_PROMPT: i32 = 0x0002;
pub(crate) const TSC_OUTPUT_MASK: i32 = 0x0003;
pub(crate) const TSC_DIRTY: i32 = 0x0004;

pub(crate) const TCCLEAREOL: usize = 14;
pub(crate) const TCBOLDFACEBEG: usize = 18;
pub(crate) const TCSTANDOUTBEG: usize = 19;
pub(crate) const TCUNDERLINEBEG: usize = 20;
pub(crate) const TCALLATTRSOFF: usize = 21;
pub(crate) const TCSTANDOUTEND: usize = 22;
pub(crate) const TCUNDERLINEEND: usize = 23;
pub(crate) const TCFGCOLOUR: usize = 32;
pub(crate) const TCBGCOLOUR: usize = 33;

/// `CMDSTACKSZ`.
pub(crate) const CMDSTACKSZ: usize = 256;

/// The parser states `%_` names (zsh's `cmdnames`).
const CMDNAMES: [&[u8]; 32] = [
    b"for",
    b"while",
    b"repeat",
    b"select",
    b"until",
    b"if",
    b"then",
    b"else",
    b"elif",
    b"math",
    b"cond",
    b"cmdor",
    b"cmdand",
    b"pipe",
    b"errpipe",
    b"foreach",
    b"case",
    b"function",
    b"subsh",
    b"cursh",
    b"array",
    b"quote",
    b"dquote",
    b"bquote",
    b"cmdsubst",
    b"mathsubst",
    b"elif-then",
    b"heredoc",
    b"heredocd",
    b"brace",
    b"braceparam",
    b"always",
];

const ANSI_COLOURS: [&[u8]; 9] = [
    b"black", b"red", b"green", b"yellow", b"blue", b"magenta", b"cyan", b"white", b"default",
];

/// zsh's `highlights`: name, bits set, bits cleared.
const HIGHLIGHTS: [(&[u8], u64, u64); 4] = [
    (b"none", 0, TXT_ATTR_ON_MASK),
    (b"bold", TXTBOLDFACE, 0),
    (b"standout", TXTSTANDOUT, 0),
    (b"underline", TXTUNDERLINE, 0),
];

const TC_COL_FG_START: &[u8] = b"\x1b[3";
const TC_COL_BG_START: &[u8] = b"\x1b[4";
const TC_COL_END: &[u8] = b"m";
const TC_COL_DEFAULT: &[u8] = b"9";

/// One entry of zsh's `fg_bg_sequences`.
#[derive(Debug, Clone)]
pub(crate) struct ColourSeq {
    pub(crate) start: Vec<u8>,
    pub(crate) end: Vec<u8>,
    pub(crate) def: Vec<u8>,
}

/// zsh's `set_default_colour_sequences`.
pub(crate) fn default_colour_sequences() -> [ColourSeq; 2] {
    [
        ColourSeq {
            start: TC_COL_FG_START.to_vec(),
            end: TC_COL_END.to_vec(),
            def: TC_COL_DEFAULT.to_vec(),
        },
        ColourSeq {
            start: TC_COL_BG_START.to_vec(),
            end: TC_COL_END.to_vec(),
            def: TC_COL_DEFAULT.to_vec(),
        },
    ]
}

/// The capabilities prompts use, from the terminfo entry of a terminal.
#[derive(Debug)]
struct TermCaps {
    bold: &'static [u8],
    smso: &'static [u8],
    rmso: &'static [u8],
    smul: &'static [u8],
    rmul: &'static [u8],
    sgr0: &'static [u8],
    el: &'static [u8],
    /// `colors`; 0 when the entry has no `setaf`/`setab`.
    colours: i32,
}

fn termcaps(term: &[u8]) -> Option<TermCaps> {
    let has = |p: &[u8]| term.starts_with(p);
    let wide = term.windows(8).any(|w| w == b"256color") || term.ends_with(b"direct");
    if has(b"screen") {
        return Some(TermCaps {
            bold: b"\x1b[1m",
            smso: b"\x1b[3m",
            rmso: b"\x1b[23m",
            smul: b"\x1b[4m",
            rmul: b"\x1b[24m",
            sgr0: b"\x1b[m\x0f",
            el: b"\x1b[K",
            colours: if wide { 256 } else { 8 },
        });
    }
    if has(b"linux") {
        return Some(TermCaps {
            bold: b"\x1b[1m",
            smso: b"\x1b[7m",
            rmso: b"\x1b[27m",
            smul: b"\x1b[4m",
            rmul: b"\x1b[24m",
            sgr0: b"\x1b[m\x0f",
            el: b"\x1b[K",
            colours: 8,
        });
    }
    if has(b"vt1") || has(b"vt2") {
        return Some(TermCaps {
            bold: b"\x1b[1m",
            smso: b"\x1b[7m",
            rmso: b"\x1b[m",
            smul: b"\x1b[4m",
            rmul: b"\x1b[m",
            sgr0: b"\x1b[m\x0f",
            el: b"\x1b[K",
            colours: 0,
        });
    }
    let xterm_like = [
        &b"xterm"[..],
        b"tmux",
        b"rxvt",
        b"alacritty",
        b"foot",
        b"kitty",
        b"wezterm",
        b"ansi",
        b"konsole",
        b"gnome",
        b"st-",
    ];
    if xterm_like.iter().any(|p| has(p)) {
        let truecolour = has(b"alacritty") || has(b"foot") || has(b"kitty") || has(b"wezterm");
        return Some(TermCaps {
            bold: b"\x1b[1m",
            smso: b"\x1b[7m",
            rmso: b"\x1b[27m",
            smul: b"\x1b[4m",
            rmul: b"\x1b[24m",
            sgr0: b"\x1b(B\x1b[m",
            el: b"\x1b[K",
            colours: if wide || truecolour { 256 } else { 8 },
        });
    }
    None
}

/// A byte-at-a-time UTF-8 `mbrtowc`.
#[derive(Debug, Default)]
struct MbState {
    pending: Vec<u8>,
}

#[derive(Debug)]
enum Mb {
    Incomplete,
    Invalid,
    Nul,
    Char(u32),
}

impl MbState {
    fn feed(&mut self, b: u8) -> Mb {
        self.pending.push(b);
        let first = self.pending.first().copied().unwrap_or(0);
        let need = match first {
            0 => {
                self.pending.clear();
                return Mb::Nul;
            }
            0x01..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => {
                self.pending.clear();
                return Mb::Invalid;
            }
        };
        if self.pending.len() > 1 && b & 0xc0 != 0x80 {
            self.pending.clear();
            return Mb::Invalid;
        }
        if self.pending.len() < need {
            return Mb::Incomplete;
        }
        let (_, c) = utf8_char(&self.pending);
        self.pending.clear();
        c.map_or(Mb::Invalid, Mb::Char)
    }
}

/// The width glibc's `wcwidth` gives, which is -1 for a control character.
fn wc_width(c: u32) -> i32 {
    if c < 0x80 {
        return if c < 0x20 || c == 0x7f { -1 } else { 1 };
    }
    wcwidth(c)
}

/// zsh's `struct buf_vars`.
#[derive(Debug)]
pub(crate) struct BufVars {
    buf: Vec<u8>,
    bufline: usize,
    fm: Vec<u8>,
    fp: isize,
    truncwidth: i64,
    dontcount: i32,
    trunccount: i32,
    rstring: Option<Vec<u8>>,
    rstring2: Option<Vec<u8>>,
    txtchange: Option<u64>,
}

impl BufVars {
    /// The format byte `k` places from the cursor; NUL past either end.
    fn c(&self, k: isize) -> u8 {
        usize::try_from(self.fp + k)
            .ok()
            .and_then(|i| self.fm.get(i))
            .copied()
            .unwrap_or(0)
    }

    fn pputc(&mut self, c: u8) {
        if tok::is_meta(c) {
            self.buf.push(META);
            self.buf.push(c ^ 32);
        } else {
            self.buf.push(c);
        }
        if c == b'\n' && self.dontcount == 0 {
            self.bufline = self.buf.len();
        }
    }

    fn push_str(&mut self, s: &[u8]) {
        self.buf.extend_from_slice(s);
    }

    fn txtchangeset(&mut self, on: u64, off: u64) {
        if let Some(t) = self.txtchange.as_mut() {
            *t &= !off;
            *t |= on;
        }
    }
}

fn localtime(secs: i64) -> libc::tm {
    // SAFETY: an all-zero tm is a valid value for localtime_r to fill.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = secs;
    // SAFETY: both pointers are valid for the call.
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    tm
}

/// zsh's `zgettime`: the wall clock as seconds and nanoseconds.
pub(crate) fn zgettime() -> (i64, i64) {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (
        i64::try_from(d.as_secs()).unwrap_or(0),
        i64::from(d.subsec_nanos()),
    )
}

fn digit(n: i32) -> u8 {
    b'0' + u8::try_from(n.rem_euclid(10)).unwrap_or(0)
}

/// zsh's `ztrftime`: `fmt` is metafied, the result is not.
pub(crate) fn ztrftime(fmt: &[u8], tm: &libc::tm, nsec: i64) -> Vec<u8> {
    let at = |i: usize| fmt.get(i).copied().unwrap_or(0);
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < fmt.len() {
        let ch = at(i);
        if ch == META {
            out.push(at(i + 1) ^ 32);
            i += 2;
            continue;
        }
        if ch != b'%' {
            out.push(ch);
            i += 1;
            continue;
        }
        let fmtstart = i;
        i += 1;
        let mut strip = false;
        let mut digs: i64 = 3;
        if at(i) == b'-' {
            strip = true;
            i += 1;
        }
        if at(i).is_ascii_digit() {
            let mut dend = i + 1;
            while at(dend).is_ascii_digit() {
                dend += 1;
            }
            if at(dend) == b'.' {
                digs = crate::utils::atoi(fmt.get(i..dend).unwrap_or(&[]));
                i = dend;
            }
        }
        // GNU padding and modifiers go to strftime.
        let plain = i - fmtstart == 1 || (i - fmtstart == 2 && strip) || at(i) == b'.';
        let mut use_strftime = false;
        if !plain {
            while at(i) != 0 && b"OE^#_-0123456789".contains(&at(i)) {
                i += 1;
            }
            if at(i) != 0 {
                i += 1;
                use_strftime = true;
            }
        }
        if !use_strftime {
            let conv = at(i);
            i += 1;
            match conv {
                b'.' => {
                    let digs = if (0..=9).contains(&digs) { digs } else { 9 };
                    let mut fnsec = nsec;
                    if digs < 9 {
                        let mut max: i64 = 100_000_000;
                        for _ in 0..(8 - digs) {
                            max /= 10;
                            fnsec /= 10;
                        }
                        max -= 1;
                        fnsec = (fnsec + 5) / 10;
                        fnsec = fnsec.min(max);
                    }
                    let w = usize::try_from(digs).unwrap_or(9);
                    out.extend_from_slice(format!("{fnsec:0w$}").as_bytes());
                }
                0 => {
                    out.push(b'%');
                    i -= 1;
                }
                b'f' | b'e' => {
                    let strip = strip || conv == b'f';
                    if tm.tm_mday > 9 {
                        out.push(digit(tm.tm_mday / 10));
                    } else if !strip {
                        out.push(b' ');
                    }
                    out.push(digit(tm.tm_mday));
                }
                b'K' | b'H' | b'k' => {
                    let strip = strip || conv == b'K';
                    if tm.tm_hour > 9 {
                        out.push(digit(tm.tm_hour / 10));
                    } else if !strip {
                        out.push(if conv == b'H' { b'0' } else { b' ' });
                    }
                    out.push(digit(tm.tm_hour));
                }
                b'L' | b'l' => {
                    let strip = strip || conv == b'L';
                    let hr12 = if tm.tm_hour % 12 == 0 {
                        12
                    } else {
                        tm.tm_hour % 12
                    };
                    if hr12 > 9 {
                        out.push(b'1');
                    } else if !strip {
                        out.push(b' ');
                    }
                    out.push(digit(hr12));
                }
                b'd' => {
                    if tm.tm_mday > 9 || !strip {
                        out.push(digit(tm.tm_mday / 10));
                    }
                    out.push(digit(tm.tm_mday));
                }
                b'm' => {
                    if tm.tm_mon > 8 || !strip {
                        out.push(digit((tm.tm_mon + 1) / 10));
                    }
                    out.push(digit(tm.tm_mon + 1));
                }
                b'M' => {
                    if tm.tm_min > 9 || !strip {
                        out.push(digit(tm.tm_min / 10));
                    }
                    out.push(digit(tm.tm_min));
                }
                b'N' => out.extend_from_slice(format!("{nsec:09}").as_bytes()),
                b'S' => {
                    if tm.tm_sec > 9 || !strip {
                        out.push(digit(tm.tm_sec / 10));
                    }
                    out.push(digit(tm.tm_sec));
                }
                b'y' => {
                    if tm.tm_year > 9 || !strip {
                        out.push(digit(tm.tm_year / 10));
                    }
                    out.push(digit(tm.tm_year));
                }
                b'E' | b'O' | b'^' | b'#' | b'_' | b'-' | b'0'..=b'9' => {
                    // zsh's `goto morefmt` with the plain test now false.
                    while at(i) != 0 && b"OE^#_-0123456789".contains(&at(i)) {
                        i += 1;
                    }
                    if at(i) != 0 {
                        i += 1;
                    }
                    use_strftime = true;
                }
                _ => use_strftime = true,
            }
        }
        if use_strftime {
            let mut spec: Vec<u8> = fmt.get(fmtstart..i).unwrap_or(&[]).to_vec();
            let origchar = spec.last().copied().unwrap_or(0);
            if origchar == META {
                if let Some(last) = spec.last_mut() {
                    *last = at(i) ^ 32;
                }
                i += 1;
            }
            spec.push(0);
            let mut buf = vec![0u8; 256 + spec.len() * 64];
            // SAFETY: buf is writable for its length, spec is NUL-terminated
            // and tm is a valid broken-down time.
            let n = unsafe {
                libc::strftime(buf.as_mut_ptr().cast(), buf.len(), spec.as_ptr().cast(), tm)
            };
            if n == 0 && origchar != b'p' && origchar != b'P' {
                return out;
            }
            out.extend_from_slice(buf.get(..n).unwrap_or(&[]));
        }
    }
    out
}

impl Shell {
    /// `IN_EVAL_TRAP()`.
    fn in_eval_trap(&self) -> bool {
        self.intrap != 0 && !self.trapisfunc && self.traplocallevel == self.locallevel
    }

    /// zsh's `promptexpand`: `(expanded, attribute changes)`.
    pub(crate) fn promptexpand(
        &mut self,
        s: &[u8],
        ns: bool,
        rs: Option<&[u8]>,
        rs2: Option<&[u8]>,
    ) -> (Vec<u8>, u64) {
        self.promptexpand_txt(s, ns, rs, rs2, None)
    }

    /// `promptexpand` with a `txtchangep`.
    pub(crate) fn promptexpand_txt(
        &mut self,
        s: &[u8],
        ns: bool,
        rs: Option<&[u8]>,
        rs2: Option<&[u8]>,
        txtchange: Option<u64>,
    ) -> (Vec<u8>, u64) {
        let mut s = s.to_vec();
        if self.isset(PROMPTSUBST) {
            let olderr = self.errflag.get();
            let oldval = self.lastval;
            if let Ok(t) = self.parsestr(&s) {
                s = self.singsub(&t);
            }
            if s.as_slice() == [NULARG] {
                s.clear();
            }
            self.errflag
                .set(olderr | (self.errflag.get() & ERRFLAG_INT));
            self.lastval = oldval;
        }
        let mut bv = BufVars {
            buf: Vec::with_capacity(256),
            bufline: 0,
            fm: s,
            fp: 0,
            truncwidth: 0,
            dontcount: 0,
            trunccount: 0,
            rstring: rs.map(<[u8]>::to_vec),
            rstring2: rs2.map(<[u8]>::to_vec),
            txtchange,
        };
        let _ = self.putpromptchar(&mut bv, true, 0);
        if bv.dontcount != 0 {
            bv.buf.push(OUTPAR);
        }
        if !ns {
            let mut out = Vec::with_capacity(bv.buf.len());
            let mut i = 0;
            while let Some(&c) = bv.buf.get(i) {
                if c == META {
                    out.push(c);
                    out.push(bv.buf.get(i + 1).copied().unwrap_or(0));
                    i += 2;
                } else {
                    if c != INPAR && c != OUTPAR && c != NULARG {
                        out.push(c);
                    }
                    i += 1;
                }
            }
            bv.buf = out;
        }
        (bv.buf, bv.txtchange.unwrap_or(0))
    }

    /// zsh's `promptpath`.
    fn promptpath(&mut self, bv: &mut BufVars, p: &[u8], mut npath: i64, tilde: bool) {
        let mut modp = p.to_vec();
        if tilde && let Some(nd) = self.finddir(p) {
            modp = b"~".to_vec();
            modp.extend_from_slice(&nd.name);
            modp.extend_from_slice(p.get(nd.dir.len()..).unwrap_or(&[]));
        }
        let at = |i: usize| modp.get(i).copied().unwrap_or(0);
        if npath > 0 {
            let mut sptr = modp.len();
            while sptr > 0 {
                if at(sptr) == b'/' {
                    npath -= 1;
                    if npath == 0 {
                        sptr += 1;
                        break;
                    }
                }
                sptr -= 1;
            }
            if at(sptr) == b'/' && at(sptr + 1) != 0 && sptr != 0 {
                sptr += 1;
            }
            let tail = modp.get(sptr..).unwrap_or(&[]).to_vec();
            self.stradd(bv, &tail);
        } else if npath < 0 {
            let mut sptr = 1;
            while sptr < modp.len() {
                if at(sptr) == b'/' {
                    npath += 1;
                    if npath == 0 {
                        break;
                    }
                }
                sptr += 1;
            }
            let head = modp.get(..sptr.min(modp.len())).unwrap_or(&[]).to_vec();
            self.stradd(bv, &head);
        } else {
            self.stradd(bv, &modp);
        }
    }

    /// zsh's `stradd`: add the metafied `d` in its visible form.
    fn stradd(&mut self, bv: &mut BufVars, d: &[u8]) {
        let raw = tok::unmetafy(d);
        let mut i = 0;
        while i < raw.len() {
            let rest = raw.get(i..).unwrap_or(&[]);
            match utf8_char(rest) {
                (len, Some(c)) if len > 0 => {
                    let (pc, _) = self.wcs_nicechar(c);
                    bv.push_str(&pc);
                    i += len;
                }
                _ => {
                    let pc = self.nicechar(rest.first().copied().unwrap_or(0));
                    bv.push_str(&pc);
                    i += 1;
                }
            }
        }
    }

    /// The terminal's value for a capability, when it has one.
    fn tcstr(&self, cap: usize) -> Option<&'static [u8]> {
        let t = termcaps(&self.term)?;
        let s = match cap {
            TCCLEAREOL => t.el,
            TCBOLDFACEBEG => t.bold,
            TCSTANDOUTBEG => t.smso,
            TCUNDERLINEBEG => t.smul,
            TCALLATTRSOFF => t.sgr0,
            TCSTANDOUTEND => t.rmso,
            TCUNDERLINEEND => t.rmul,
            TCFGCOLOUR | TCBGCOLOUR if t.colours > 0 => b"",
            _ => return None,
        };
        Some(s)
    }

    /// `tccan`.
    fn tccan(&self, cap: usize) -> bool {
        self.tcstr(cap).is_some()
    }

    /// `tccolours`: -1 when the terminal gives no count.
    fn tccolours(&self) -> i32 {
        termcaps(&self.term).map_or(-1, |t| if t.colours > 0 { t.colours } else { -1 })
    }

    /// `tgoto(tcstr[TCFGCOLOUR or TCBGCOLOUR], colour, colour)`.
    fn colour_cap(&self, cap: usize, colour: i32) -> Vec<u8> {
        let (base, bright, ext) = if cap == TCFGCOLOUR {
            (3, 9, 38)
        } else {
            (4, 10, 48)
        };
        let wide = self.tccolours() > 8;
        if colour < 8 || !wide {
            format!("\x1b[{base}{colour}m").into_bytes()
        } else if colour < 16 {
            format!("\x1b[{bright}{}m", colour - 8).into_bytes()
        } else {
            format!("\x1b[{ext};5;{colour}m").into_bytes()
        }
    }

    /// Where `tputs` output goes for `flags` outside a prompt.
    fn tputs_out(&self, flags: i32, s: &[u8]) {
        if flags & TSC_RAW != 0 {
            write_fd(1, s);
        } else if self.shout >= 0 {
            write_fd(self.shout, s);
        }
    }

    /// zsh's `tsetcap`.
    pub(crate) fn tsetcap(&mut self, mut bv: Option<&mut BufVars>, cap: usize, mut flags: i32) {
        let Some(s) = self.tcstr(cap) else { return };
        if self.isset(SINGLELINEZLE) {
            return;
        }
        match flags & TSC_OUTPUT_MASK {
            TSC_PROMPT => {
                if let Some(bv) = bv.as_deref_mut() {
                    if bv.dontcount == 0 {
                        bv.buf.push(INPAR);
                    }
                    for &c in s {
                        bv.pputc(c);
                    }
                    if bv.dontcount == 0 {
                        bv.buf.push(OUTPAR);
                    }
                }
            }
            _ => self.tputs_out(flags, s),
        }
        if flags & TSC_DIRTY != 0 {
            flags &= !TSC_DIRTY;
            let mask = self.txtattrmask;
            if mask & TXTBOLDFACE != 0 && cap != TCBOLDFACEBEG {
                self.tsetcap(bv.as_deref_mut(), TCBOLDFACEBEG, flags);
            }
            if mask & TXTSTANDOUT != 0 {
                self.tsetcap(bv.as_deref_mut(), TCSTANDOUTBEG, flags);
            }
            if mask & TXTUNDERLINE != 0 {
                self.tsetcap(bv.as_deref_mut(), TCUNDERLINEBEG, flags);
            }
            if mask & TXTFGCOLOUR != 0 {
                self.set_colour_attribute(bv.as_deref_mut(), mask, COL_SEQ_FG, flags);
            }
            if mask & TXTBGCOLOUR != 0 {
                self.set_colour_attribute(bv, mask, COL_SEQ_BG, flags);
            }
        }
    }

    /// zsh's `parsecolorchar`.
    fn parsecolorchar(&mut self, bv: &mut BufVars, arg: i64, is_fg: bool) -> u64 {
        if bv.c(1) != b'{' {
            return self.match_colour(None, &mut 0, is_fg, arg);
        }
        bv.fp += 2;
        let start = usize::try_from(bv.fp).unwrap_or(0);
        match bv
            .fm
            .get(start..)
            .and_then(|r| r.iter().position(|&c| c == b'}'))
        {
            Some(off) => {
                let inner = bv.fm.get(start..start + off).unwrap_or(&[]).to_vec();
                let (ops, opb, opp) = (
                    self.opts[PROMPTSUBST],
                    self.opts[PROMPTBANG],
                    self.opts[PROMPTPERCENT],
                );
                self.opts[PROMPTPERCENT] = true;
                self.opts[PROMPTSUBST] = false;
                self.opts[PROMPTBANG] = false;
                let (col, _) = self.promptexpand(&inner, false, None, None);
                let atr = self.match_colour(Some(&col), &mut 0, is_fg, 0);
                bv.fp = isize::try_from(start + off).unwrap_or(bv.fp);
                self.opts[PROMPTSUBST] = ops;
                self.opts[PROMPTBANG] = opb;
                self.opts[PROMPTPERCENT] = opp;
                atr
            }
            None => {
                let rest = bv.fm.get(start..).unwrap_or(&[]).to_vec();
                let mut pos = 0;
                let atr = self.match_colour(Some(&rest), &mut pos, is_fg, 0);
                bv.fp += isize::try_from(pos).unwrap_or(0);
                if bv.c(0) != b'}' {
                    bv.fp -= 1;
                }
                atr
            }
        }
    }

    /// zsh's `putpromptchar`: returns the byte it stopped at, NUL at the end.
    #[expect(clippy::too_many_lines, reason = "one procedure in zsh")]
    fn putpromptchar(&mut self, bv: &mut BufVars, doprint: bool, endchar: u8) -> u8 {
        while bv.c(0) != 0 && bv.c(0) != endchar {
            let mut arg: i64 = 0;
            if bv.c(0) == b'%' && self.isset(PROMPTPERCENT) {
                let mut minus = false;
                bv.fp += 1;
                if bv.c(0) == b'-' {
                    minus = true;
                    bv.fp += 1;
                }
                if self.idigit(bv.c(0)) {
                    arg = self.prompt_number(bv);
                    if minus {
                        arg = -arg;
                    }
                } else if minus {
                    arg = -1;
                }
                if bv.c(0) == b'(' {
                    bv.fp += 1;
                    if self.idigit(bv.c(0)) {
                        arg = self.prompt_number(bv);
                    } else if arg < 0 {
                        arg = -arg;
                    }
                    let test = self.prompt_test(bv, bv.c(0), arg, minus);
                    if bv.c(0) == 0 {
                        return 0;
                    }
                    bv.fp += 1;
                    let sep = bv.c(0);
                    if sep == 0 {
                        return 0;
                    }
                    bv.fp += 1;
                    let otruncwidth = bv.truncwidth;
                    bv.truncwidth = 0;
                    let ok = self.putpromptchar(bv, test == 1 && doprint, sep) != 0
                        && {
                            bv.fp += 1;
                            bv.c(0) != 0
                        }
                        && self.putpromptchar(bv, test == 0 && doprint, b')') != 0;
                    bv.truncwidth = otruncwidth;
                    if !ok {
                        return 0;
                    }
                    bv.fp += 1;
                    continue;
                }
                if !doprint {
                    match bv.c(0) {
                        b'[' => {
                            bv.fp += 1;
                            while self.idigit(bv.c(0)) {
                                bv.fp += 1;
                            }
                            loop {
                                bv.fp += 1;
                                if bv.c(0) == 0 || bv.c(0) == b']' {
                                    break;
                                }
                            }
                        }
                        c @ (b'<' | b'>') => {
                            bv.fp += 1;
                            while bv.c(0) != 0 && bv.c(0) != c {
                                bv.fp += 1;
                            }
                        }
                        b'D' if bv.c(1) == b'{' => {
                            bv.fp += 1;
                            while bv.c(0) != 0 && bv.c(0) != b'}' {
                                bv.fp += 1;
                            }
                        }
                        _ => {}
                    }
                    bv.fp += 1;
                    continue;
                }
                match bv.c(0) {
                    b'~' => {
                        let pwd = self.pwd.clone();
                        self.promptpath(bv, &pwd, arg, true);
                    }
                    b'd' | b'/' => {
                        let pwd = self.pwd.clone();
                        self.promptpath(bv, &pwd, arg, false);
                    }
                    b'c' | b'.' => {
                        let pwd = self.pwd.clone();
                        self.promptpath(bv, &pwd, if arg != 0 { arg } else { 1 }, true);
                    }
                    b'C' => {
                        let pwd = self.pwd.clone();
                        self.promptpath(bv, &pwd, if arg != 0 { arg } else { 1 }, false);
                    }
                    b'N' => {
                        let p = self
                            .scriptname
                            .clone()
                            .unwrap_or_else(|| self.argzero.clone());
                        self.promptpath(bv, &p, arg, false);
                    }
                    b'h' | b'!' => {
                        let n = self.convbase(self.curhist, 10);
                        bv.push_str(&n);
                    }
                    b'j' => {
                        let n = self.prompt_numjobs();
                        bv.push_str(n.to_string().as_bytes());
                    }
                    b'M' => {
                        self.queue_signals();
                        if let Some(h) = self.getsparam(b"HOST") {
                            self.stradd(bv, &h);
                        }
                        self.unqueue_signals();
                    }
                    b'm' => {
                        if arg == 0 {
                            arg = 1;
                        }
                        self.queue_signals();
                        if let Some(h) = self.getsparam(b"HOST") {
                            if arg < 0 {
                                let mut ss = h.len();
                                while ss > 0 {
                                    if h.get(ss - 1) == Some(&b'.') {
                                        arg += 1;
                                        if arg == 0 {
                                            break;
                                        }
                                    }
                                    ss -= 1;
                                }
                                let tail = h.get(ss..).unwrap_or(&[]).to_vec();
                                self.stradd(bv, &tail);
                            } else {
                                let mut ss = 0;
                                while ss < h.len() {
                                    if h.get(ss) == Some(&b'.') {
                                        arg -= 1;
                                        if arg == 0 {
                                            break;
                                        }
                                    }
                                    ss += 1;
                                }
                                let part = h.get(..ss).unwrap_or(&[]).to_vec();
                                self.stradd(bv, &part);
                            }
                        }
                        self.unqueue_signals();
                    }
                    b'S' => {
                        bv.txtchangeset(TXTSTANDOUT, TXTNOSTANDOUT);
                        self.txtattrmask |= TXTSTANDOUT;
                        self.tsetcap(Some(&mut *bv), TCSTANDOUTBEG, TSC_PROMPT);
                    }
                    b's' => {
                        bv.txtchangeset(TXTNOSTANDOUT, TXTSTANDOUT);
                        self.txtattrmask &= !TXTSTANDOUT;
                        self.tsetcap(Some(&mut *bv), TCSTANDOUTEND, TSC_PROMPT | TSC_DIRTY);
                    }
                    b'B' => {
                        bv.txtchangeset(TXTBOLDFACE, TXTNOBOLDFACE);
                        self.txtattrmask |= TXTBOLDFACE;
                        self.tsetcap(Some(&mut *bv), TCBOLDFACEBEG, TSC_PROMPT | TSC_DIRTY);
                    }
                    b'b' => {
                        bv.txtchangeset(TXTNOBOLDFACE, TXTBOLDFACE);
                        self.txtattrmask &= !TXTBOLDFACE;
                        self.tsetcap(Some(&mut *bv), TCALLATTRSOFF, TSC_PROMPT | TSC_DIRTY);
                    }
                    b'U' => {
                        bv.txtchangeset(TXTUNDERLINE, TXTNOUNDERLINE);
                        self.txtattrmask |= TXTUNDERLINE;
                        self.tsetcap(Some(&mut *bv), TCUNDERLINEBEG, TSC_PROMPT);
                    }
                    b'u' => {
                        bv.txtchangeset(TXTNOUNDERLINE, TXTUNDERLINE);
                        self.txtattrmask &= !TXTUNDERLINE;
                        self.tsetcap(Some(&mut *bv), TCUNDERLINEEND, TSC_PROMPT | TSC_DIRTY);
                    }
                    c @ (b'F' | b'f' | b'K' | b'k') => {
                        let fg = c == b'F' || c == b'f';
                        let (seq, off, on_mask, col_mask) = if fg {
                            (
                                COL_SEQ_FG,
                                TXTNOFGCOLOUR,
                                TXT_ATTR_FG_ON_MASK,
                                TXT_ATTR_FG_COL_MASK,
                            )
                        } else {
                            (
                                COL_SEQ_BG,
                                TXTNOBGCOLOUR,
                                TXT_ATTR_BG_ON_MASK,
                                TXT_ATTR_BG_COL_MASK,
                            )
                        };
                        let mut set = false;
                        if c == b'F' || c == b'K' {
                            let atr = self.parsecolorchar(bv, arg, fg);
                            if atr & (TXT_ERROR | off) == 0 {
                                bv.txtchangeset(atr & on_mask, off | col_mask);
                                self.txtattrmask &= !col_mask;
                                self.txtattrmask |= atr & on_mask;
                                self.set_colour_attribute(Some(&mut *bv), atr, seq, TSC_PROMPT);
                                set = true;
                            }
                        }
                        if !set {
                            bv.txtchangeset(off, on_mask);
                            self.txtattrmask &= !on_mask;
                            self.set_colour_attribute(Some(&mut *bv), off, seq, TSC_PROMPT);
                        }
                    }
                    b'[' => {
                        bv.fp += 1;
                        if self.idigit(bv.c(0)) {
                            arg = self.prompt_number(bv);
                        }
                        if !self.prompttrunc(bv, arg, b']', doprint, endchar) {
                            return bv.c(0);
                        }
                    }
                    c @ (b'<' | b'>') => {
                        if minus {
                            let (t0, _) =
                                self.countprompt(bv.buf.get(bv.bufline..).unwrap_or(&[]), 0);
                            arg += self.zterm_columns - i64::from(t0);
                            if arg <= 0 {
                                arg = 1;
                            }
                        }
                        if !self.prompttrunc(bv, arg, c, doprint, endchar) {
                            return bv.c(0);
                        }
                    }
                    c @ (b'{' | b'G') => {
                        if c == b'{' {
                            if bv.dontcount == 0 {
                                bv.buf.push(INPAR);
                            }
                            bv.dontcount += 1;
                        }
                        if c == b'G' || arg > 0 {
                            let n = if arg > 0 {
                                usize::try_from(arg).unwrap_or(0)
                            } else {
                                1
                            };
                            bv.buf.extend(std::iter::repeat_n(NULARG, n));
                        }
                    }
                    b'}' => {
                        if bv.trunccount != 0 && bv.trunccount >= bv.dontcount {
                            return bv.c(0);
                        }
                        if bv.dontcount != 0 {
                            bv.dontcount -= 1;
                            if bv.dontcount == 0 {
                                bv.buf.push(OUTPAR);
                            }
                        }
                    }
                    c @ (b't' | b'@' | b'T' | b'*' | b'w' | b'W' | b'D') => {
                        let tmfmt: Vec<u8> = match c {
                            b'T' => b"%K:%M".to_vec(),
                            b'*' => b"%K:%M:%S".to_vec(),
                            b'w' => b"%a %f".to_vec(),
                            b'W' => b"%m/%d/%y".to_vec(),
                            b'D' if bv.c(1) == b'{' => {
                                let mut f = Vec::new();
                                let mut k = 2;
                                while bv.c(k) != 0 && bv.c(k) != b'}' {
                                    if bv.c(k) == b'\\' && bv.c(k + 1) != 0 {
                                        k += 1;
                                    }
                                    f.push(bv.c(k));
                                    k += 1;
                                }
                                bv.fp += k - isize::from(bv.c(k) == 0);
                                if f.is_empty() {
                                    bv.fp += 1;
                                    continue;
                                }
                                f
                            }
                            b'D' => b"%y-%m-%d".to_vec(),
                            _ => b"%l:%M%p".to_vec(),
                        };
                        let (secs, nsec) = zgettime();
                        let tm = localtime(secs);
                        let out = ztrftime(&tmfmt, &tm, nsec);
                        bv.push_str(&tok::metafy(&out));
                    }
                    b'n' => {
                        let u = self.get_username();
                        self.stradd(bv, &u);
                    }
                    c @ (b'l' | b'y') => {
                        let t = self.ttystrname.clone();
                        if t.is_empty() {
                            self.stradd(bv, b"()");
                        } else {
                            let skip = if c == b'l' {
                                if t.starts_with(b"/dev/tty") { 8 } else { 5 }
                            } else if t.starts_with(b"/dev/") {
                                5
                            } else {
                                0
                            };
                            let tail = t.get(skip..).unwrap_or(&[]).to_vec();
                            self.stradd(bv, &tail);
                        }
                    }
                    b'L' => bv.push_str(self.shlvl.to_string().as_bytes()),
                    b'?' => bv.push_str(self.lastval.to_string().as_bytes()),
                    c @ (b'%' | b')') => bv.buf.push(c),
                    b'#' => bv.buf.push(if privasserted() { b'#' } else { b'%' }),
                    b'v' => {
                        let psvar = self.arrvar(ArrVar::Psvar).to_vec();
                        let len = i64::try_from(psvar.len()).unwrap_or(0);
                        if arg == 0 {
                            arg = 1;
                        } else if arg < 0 {
                            arg += len + 1;
                        }
                        if arg > 0 && len >= arg {
                            let v = psvar
                                .get(usize::try_from(arg - 1).unwrap_or(0))
                                .cloned()
                                .unwrap_or_default();
                            self.stradd(bv, &v);
                        }
                    }
                    b'E' => self.tsetcap(Some(&mut *bv), TCCLEAREOL, TSC_PROMPT),
                    c @ (b'^' | b'_') => {
                        let cmdsp = i64::try_from(self.cmdstack.len()).unwrap_or(0);
                        if cmdsp != 0 {
                            // The newest entries for a count, the oldest for a negative one.
                            let (lo, hi) = if arg >= 0 {
                                (
                                    cmdsp - if arg > cmdsp || arg == 0 { cmdsp } else { arg },
                                    cmdsp,
                                )
                            } else {
                                (0, (-arg).min(cmdsp))
                            };
                            let mut names: Vec<usize> =
                                (lo..hi).filter_map(|k| usize::try_from(k).ok()).collect();
                            if c == b'^' {
                                names.reverse();
                            }
                            for (k, idx) in names.iter().enumerate() {
                                let st = self.cmdstack.get(*idx).copied().unwrap_or(0);
                                let name = CMDNAMES.get(usize::from(st)).copied().unwrap_or(b"");
                                self.stradd(bv, name);
                                if k + 1 < names.len() {
                                    bv.buf.push(b' ');
                                }
                            }
                        }
                    }
                    b'r' => {
                        if let Some(r) = bv.rstring.clone() {
                            self.stradd(bv, &r);
                        }
                    }
                    b'R' => {
                        if let Some(r) = bv.rstring2.clone() {
                            self.stradd(bv, &r);
                        }
                    }
                    b'e' => bv.push_str(self.funcstack.len().to_string().as_bytes()),
                    c @ (b'I' | b'i') => {
                        let top = self.funcstack.last().map(|f| (f.tp, f.flineno));
                        match top {
                            Some((tp, flineno))
                                if c == b'I' && tp != FS_SOURCE && !self.in_eval_trap() =>
                            {
                                let n = self.lineno + flineno;
                                if tp == FS_EVAL {
                                    self.lineno -= 1;
                                }
                                bv.push_str(n.to_string().as_bytes());
                            }
                            _ => bv.push_str(self.lineno.to_string().as_bytes()),
                        }
                    }
                    b'x' => {
                        let p = match self.funcstack.last() {
                            Some(f) if f.tp != FS_SOURCE && !self.in_eval_trap() => {
                                f.filename.clone().unwrap_or_default()
                            }
                            _ => self
                                .scriptfilename
                                .clone()
                                .unwrap_or_else(|| self.argzero.clone()),
                        };
                        self.promptpath(bv, &p, arg, false);
                    }
                    0 => return 0,
                    META => bv.fp += 1,
                    _ => {}
                }
            } else if bv.c(0) == b'!' && self.isset(PROMPTBANG) {
                if doprint {
                    if bv.c(1) == b'!' {
                        bv.fp += 1;
                        bv.pputc(b'!');
                    } else {
                        let n = self.convbase(self.curhist, 10);
                        bv.push_str(&n);
                    }
                }
            } else {
                let c = if bv.c(0) == META {
                    bv.fp += 1;
                    bv.c(0) ^ 32
                } else {
                    bv.c(0)
                };
                if doprint {
                    bv.pputc(c);
                }
            }
            bv.fp += 1;
        }
        bv.c(0)
    }

    /// `zstrtol(bv->fm, &bv->fm, 10)`.
    fn prompt_number(&self, bv: &mut BufVars) -> i64 {
        let start = usize::try_from(bv.fp).unwrap_or(0);
        let (v, used) = zstrtol(bv.fm.get(start..).unwrap_or(&[]), 10);
        bv.fp += isize::try_from(used).unwrap_or(0);
        // zsh keeps the value in an int.
        i64::from(v as i32)
    }

    fn prompt_numjobs(&self) -> usize {
        (1..=self.maxjob)
            .filter(|&j| {
                self.jobtab.get(j).is_some_and(|jt| {
                    jt.stat != 0 && !jt.procs.is_empty() && jt.stat & crate::jobs::STAT_NOPRINT == 0
                })
            })
            .count()
    }

    /// The test of a `%(x.true.false)` conditional: 1, 0, or -1 for an
    /// unknown `x`.
    fn prompt_test(&mut self, bv: &BufVars, tc: u8, mut arg: i64, minus: bool) -> i32 {
        let test = match tc {
            b'c' | b'.' | b'~' | b'/' | b'C' => {
                let pwd = self.pwd.clone();
                let mut ss = 0usize;
                if matches!(tc, b'c' | b'.' | b'~')
                    && let Some(nd) = self.finddir(&pwd)
                {
                    arg -= 1;
                    ss = nd.dir.len();
                }
                if ss < pwd.len() {
                    let first = pwd.get(ss).copied();
                    ss += 1;
                    if first == Some(b'/') && ss < pwd.len() {
                        arg -= 1;
                    }
                }
                arg -= i64::try_from(
                    pwd.get(ss..)
                        .unwrap_or(&[])
                        .iter()
                        .filter(|&&c| c == b'/')
                        .count(),
                )
                .unwrap_or(0);
                arg <= 0
            }
            b't' | b'T' | b'd' | b'D' | b'w' => {
                let tm = localtime(zgettime().0);
                let v = match tc {
                    b't' => tm.tm_min,
                    b'T' => tm.tm_hour,
                    b'd' => tm.tm_mday,
                    b'D' => tm.tm_mon,
                    _ => tm.tm_wday,
                };
                arg == i64::from(v)
            }
            b'?' => i64::from(self.lastval) == arg,
            // SAFETY: geteuid and getegid have no preconditions.
            b'#' => i64::from(unsafe { libc::geteuid() }) == arg,
            // SAFETY: as above.
            b'g' => i64::from(unsafe { libc::getegid() }) == arg,
            b'j' => i64::try_from(self.prompt_numjobs()).unwrap_or(0) >= arg,
            b'l' => {
                let (mut t0, _) = self.countprompt(bv.buf.get(bv.bufline..).unwrap_or(&[]), 0);
                if minus {
                    t0 = i32::try_from(self.zterm_columns).unwrap_or(80) - t0;
                }
                i64::from(t0) >= arg
            }
            b'e' => {
                let mut t = arg;
                let mut n = self.funcstack.len();
                while n > 0 && t > 0 {
                    t -= 1;
                    n -= 1;
                }
                t == 0
            }
            b'L' => self.shlvl >= arg,
            b'S' => zgettime().0 - self.shtimer.0 >= arg,
            b'v' => i64::try_from(self.arrvar(ArrVar::Psvar).len()).unwrap_or(0) >= arg,
            b'V' => {
                let psvar = self.arrvar(ArrVar::Psvar);
                let idx = usize::try_from(if arg != 0 { arg } else { 1 } - 1).unwrap_or(usize::MAX);
                !psvar.is_empty()
                    && i64::try_from(psvar.len()).unwrap_or(0) >= arg
                    && psvar.get(idx).is_some_and(|v| !v.is_empty())
            }
            b'_' => i64::try_from(self.cmdstack.len()).unwrap_or(0) >= arg,
            b'!' => privasserted(),
            _ => return -1,
        };
        i32::from(test)
    }

    /// zsh's `countprompt`: the width of the last line and the height.
    pub(crate) fn countprompt(&self, s: &[u8], overf: i32) -> (i32, i32) {
        let cols = i32::try_from(self.zterm_columns).unwrap_or(80);
        let (mut w, mut h) = (0i32, 1i32);
        let mut multi = false;
        let mut wcw = 0i32;
        let mut visible = true;
        let mut mbs = MbState::default();
        let mut i = 0usize;
        while let Some(&c) = s.get(i) {
            while w > cols && overf >= 0 && !multi {
                h += 1;
                if wcw != 0 {
                    w = wcw;
                    break;
                }
                w -= cols;
            }
            wcw = 0;
            if c == INPAR {
                visible = false;
            } else if c == OUTPAR {
                visible = true;
            } else if c == NULARG {
                w += 1;
            } else if visible {
                let inchar = if c == META {
                    i += 1;
                    s.get(i).copied().unwrap_or(0) ^ 32
                } else {
                    if !multi {
                        if c == b'\t' {
                            w = (w | 7) + 1;
                            i += 1;
                            continue;
                        } else if c == b'\n' {
                            w = 0;
                            h += 1;
                            i += 1;
                            continue;
                        }
                    }
                    c
                };
                match mbs.feed(inchar) {
                    Mb::Incomplete => multi = true,
                    Mb::Invalid => {
                        multi = false;
                        w += 1;
                    }
                    Mb::Nul => multi = false,
                    Mb::Char(wc) => {
                        wcw = wc_width(wc);
                        w += if wcw >= 0 { wcw } else { 1 };
                        multi = false;
                    }
                }
            }
            i += 1;
        }
        while w > cols && overf >= 0 {
            h += 1;
            if wcw != 0 {
                w = wcw;
                break;
            }
            w -= cols;
        }
        if w == cols && overf == 0 {
            w = 0;
            h += 1;
        }
        (w, h)
    }

    /// The width of one character read from a metafied string at `*i`.
    fn trunc_char_width(s: &[u8], i: &mut usize, mbs: &mut MbState) -> i64 {
        let c = s.get(*i).copied().unwrap_or(0);
        let inchar = if c == META {
            *i += 1;
            s.get(*i).copied().unwrap_or(0) ^ 32
        } else {
            c
        };
        *i += 1;
        match mbs.feed(inchar) {
            Mb::Incomplete => 0,
            Mb::Invalid | Mb::Nul => 1,
            Mb::Char(wc) => {
                let w = wc_width(wc);
                if w >= 0 { i64::from(w) } else { 1 }
            }
        }
    }

    /// zsh's `prompttrunc`.
    fn prompttrunc(
        &mut self,
        bv: &mut BufVars,
        arg: i64,
        truncchar: u8,
        doprint: bool,
        endchar: u8,
    ) -> bool {
        if arg <= 0 {
            if bv.c(0) != endchar {
                bv.fp += 1;
            }
            while bv.c(0) != 0 && bv.c(0) != truncchar {
                if bv.c(0) == b'\\' && bv.c(1) != 0 {
                    bv.fp += 1;
                }
                bv.fp += 1;
            }
            return !(bv.truncwidth != 0 || bv.c(0) == 0);
        }
        let truncatleft = bv.c(0) == b'<';
        let w = bv.buf.len();
        if bv.truncwidth != 0 {
            loop {
                bv.fp -= 1;
                if bv.c(0) == b'%' || bv.fp < 0 {
                    break;
                }
            }
            bv.fp -= 1;
            return false;
        }
        bv.truncwidth = arg;
        if bv.c(0) != b']' {
            bv.fp += 1;
        }
        while bv.c(0) != 0 && bv.c(0) != truncchar {
            if bv.c(0) == b'\\' && bv.c(1) != 0 {
                bv.fp += 1;
            }
            bv.buf.push(bv.c(0));
            bv.fp += 1;
        }
        if bv.c(0) == 0 {
            return false;
        }
        if bv.buf.len() == w && truncchar == b']' {
            bv.buf.push(b'<');
        }
        let truncstr = bv.buf.split_off(w);
        bv.fp += 1;
        bv.trunccount = bv.dontcount;
        let _ = self.putpromptchar(bv, doprint, endchar);
        bv.trunccount = 0;
        let (fullw, _) = self.countprompt(bv.buf.get(w..).unwrap_or(&[]), -1);
        let fullw = i64::from(fullw);
        if fullw > bv.truncwidth {
            let twidth = i64::try_from(crate::utils::mb_metastrwidth(self, &truncstr)).unwrap_or(0);
            if twidth < bv.truncwidth {
                let mut maxwidth = bv.truncwidth - twidth;
                let mut mbs = MbState::default();
                if truncatleft {
                    let fulltext = bv.buf.split_off(w);
                    bv.buf.extend_from_slice(&truncstr);
                    let mut remw = fullw;
                    let mut i = 0usize;
                    while remw > maxwidth && i < fulltext.len() {
                        if fulltext.get(i) == Some(&INPAR) {
                            while let Some(&ch) = fulltext.get(i) {
                                bv.buf.push(ch);
                                i += 1;
                                if ch == OUTPAR {
                                    break;
                                }
                                if ch == NULARG {
                                    remw -= 1;
                                }
                            }
                        } else {
                            remw -= Self::trunc_char_width(&fulltext, &mut i, &mut mbs);
                        }
                    }
                    bv.buf.extend_from_slice(fulltext.get(i..).unwrap_or(&[]));
                } else {
                    let mut i = w;
                    while maxwidth > 0 && i < bv.buf.len() {
                        if bv.buf.get(i) == Some(&INPAR) {
                            while let Some(&ch) = bv.buf.get(i) {
                                i += 1;
                                if ch == OUTPAR {
                                    break;
                                }
                                if ch == NULARG {
                                    maxwidth -= 1;
                                }
                            }
                        } else {
                            maxwidth -= Self::trunc_char_width(&bv.buf, &mut i, &mut mbs);
                        }
                    }
                    let rest = bv.buf.split_off(i.min(bv.buf.len()));
                    bv.buf.extend_from_slice(&truncstr);
                    let mut j = 0;
                    while let Some(&ch) = rest.get(j) {
                        if ch == INPAR {
                            while let Some(&g) = rest.get(j) {
                                bv.buf.push(g);
                                if g == OUTPAR {
                                    break;
                                }
                                j += 1;
                            }
                        }
                        j += 1;
                    }
                }
            } else {
                bv.buf.truncate(w);
                bv.buf.extend_from_slice(&truncstr);
            }
        }
        bv.truncwidth = 0;
        if bv.c(0) == 0 {
            return false;
        }
        if bv.c(0) != endchar {
            bv.fp += 1;
            if self.putpromptchar(bv, doprint, endchar) == 0 {
                return false;
            }
        }
        bv.fp -= 1;
        true
    }

    /// zsh's `cmdpush`.
    pub(crate) fn cmdpush(&mut self, cmdtok: u8) {
        if self.cmdstack.len() < CMDSTACKSZ {
            self.cmdstack.push(cmdtok);
        }
    }

    /// zsh's `cmdpop`.
    pub(crate) fn cmdpop(&mut self) {
        let _ = self.cmdstack.pop();
    }

    /// zsh's `match_named_colour`.
    fn match_named_colour(&self, s: &[u8], pos: &mut usize) -> i32 {
        let start = *pos;
        let mut end = start;
        while end < s.len() && self.ialpha(s.get(end).copied().unwrap_or(0)) {
            end += 1;
        }
        *pos = end;
        let word = s.get(start..end).unwrap_or(&[]);
        ANSI_COLOURS
            .iter()
            .position(|name| {
                name.get(..word.len().min(name.len())) == Some(word) && word.len() <= name.len()
            })
            .and_then(|p| i32::try_from(p).ok())
            .unwrap_or(-1)
    }

    /// zsh's `match_colour`: `s` from `*pos`, or the number `colour` when
    /// `s` is `None`.
    pub(crate) fn match_colour(
        &mut self,
        s: Option<&[u8]>,
        pos: &mut usize,
        is_fg: bool,
        mut colour: i64,
    ) -> u64 {
        let (shft, on, tc) = if is_fg {
            (TXT_ATTR_FG_COL_SHIFT, TXTFGCOLOUR, TCFGCOLOUR)
        } else {
            (TXT_ATTR_BG_COL_SHIFT, TXTBGCOLOUR, TCBGCOLOUR)
        };
        if let Some(s) = s {
            let at = |i: usize| s.get(i).copied().unwrap_or(0);
            if at(*pos) == b'#' && at(*pos + 1).is_ascii_hexdigit() {
                let (col, used) = zstrtol(s.get(*pos + 1..).unwrap_or(&[]), 16);
                let (red, green, blue) = match used + 1 {
                    4 => {
                        let r = (col >> 8) | ((col >> 8) << 4);
                        let g = (col & 0xf0) >> 4;
                        let b = col & 0xf;
                        (r, g | (g << 4), b | (b << 4))
                    }
                    7 => (col >> 16, (col & 0xff00) >> 8, col & 0xff),
                    _ => return TXT_ERROR,
                };
                *pos += used + 1;
                // No module adds a GETCOLORATTR hook, so true colour it is.
                let c = u64::try_from((((red << 8) + green) << 8) + blue).unwrap_or(0);
                return on
                    | if is_fg {
                        TXT_ATTR_FG_24BIT
                    } else {
                        TXT_ATTR_BG_24BIT
                    }
                    | c << shft;
            } else if self.ialpha(at(*pos)) {
                let c = self.match_named_colour(s, pos);
                if c == 8 {
                    return if is_fg { TXTNOFGCOLOUR } else { TXTNOBGCOLOUR };
                }
                if c < 0 {
                    return TXT_ERROR;
                }
                colour = i64::from(c);
            } else {
                let (v, used) = zstrtol(s.get(*pos..).unwrap_or(&[]), 10);
                *pos += used;
                colour = i64::from(v as i32);
                if !(0..256).contains(&colour) {
                    return TXT_ERROR;
                }
            }
        }
        if self.tccan(tc) && colour > 7 && colour >= i64::from(self.tccolours()) {
            return TXT_ERROR;
        }
        on | u64::try_from(colour).unwrap_or(0) << shft
    }

    /// zsh's `match_highlight`: the attributes and where parsing stopped.
    pub(crate) fn match_highlight(&mut self, s: &[u8]) -> (u64, usize) {
        let mut on: u64 = 0;
        let mut pos = 0usize;
        let mut found = true;
        while found && pos < s.len() {
            found = false;
            let rest = s.get(pos..).unwrap_or(&[]);
            if rest.starts_with(b"fg=") || rest.starts_with(b"bg=") {
                let is_fg = rest.first() == Some(&b'f');
                pos += 3;
                let atr = self.match_colour(Some(s), &mut pos, is_fg, 0);
                match s.get(pos) {
                    Some(b',') => pos += 1,
                    Some(b' ') | None => {}
                    Some(_) => break,
                }
                found = true;
                if atr != TXT_ERROR {
                    on |= atr;
                }
            } else {
                for (name, mask_on, mask_off) in HIGHLIGHTS {
                    let rest = s.get(pos..).unwrap_or(&[]);
                    if rest.starts_with(name) {
                        let mut val = pos + name.len();
                        match s.get(val) {
                            Some(b',') => val += 1,
                            Some(b' ') | None => {}
                            Some(_) => break,
                        }
                        on |= mask_on;
                        on &= !mask_off;
                        pos = val;
                        found = true;
                    }
                }
            }
        }
        (on, pos)
    }

    /// zsh's `output_highlight`.
    pub(crate) fn output_highlight(atr: u64) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let colour = |out: &mut Vec<u8>, fg: bool, col: u64, truecol: bool| {
            out.extend_from_slice(if fg { b"fg=" } else { b"bg=" });
            if truecol {
                out.extend_from_slice(
                    format!(
                        "#{:02x}{:02x}{:02x}",
                        col >> 16,
                        (col >> 8) & 0xff,
                        col & 0xff
                    )
                    .as_bytes(),
                );
            } else if col > 7 {
                out.extend_from_slice(col.to_string().as_bytes());
            } else {
                out.extend_from_slice(
                    ANSI_COLOURS
                        .get(usize::try_from(col).unwrap_or(0))
                        .copied()
                        .unwrap_or(b""),
                );
            }
        };
        if atr & TXTFGCOLOUR != 0 {
            colour(
                &mut out,
                true,
                (atr & TXT_ATTR_FG_COL_MASK) >> TXT_ATTR_FG_COL_SHIFT,
                atr & TXT_ATTR_FG_24BIT != 0,
            );
        }
        if atr & TXTBGCOLOUR != 0 {
            if !out.is_empty() {
                out.push(b',');
            }
            colour(
                &mut out,
                false,
                (atr & TXT_ATTR_BG_COL_MASK) >> TXT_ATTR_BG_COL_SHIFT,
                atr & TXT_ATTR_BG_24BIT != 0,
            );
        }
        for (name, mask_on, _) in HIGHLIGHTS {
            if mask_on & atr != 0 {
                if !out.is_empty() {
                    out.push(b',');
                }
                out.extend_from_slice(name);
            }
        }
        if out.is_empty() {
            out.extend_from_slice(b"none");
        }
        out
    }

    /// The colour-code half of zsh's `allocate_colour_buffer`: take any
    /// codes `$zle_highlight` sets.
    fn load_colour_codes(&mut self) {
        let Some(atrs) = self.getaparam(b"zle_highlight") else {
            return;
        };
        for a in atrs {
            let keys: [(&[u8], usize, u8); 6] = [
                (b"fg_start_code:", COL_SEQ_FG, b's'),
                (b"fg_default_code:", COL_SEQ_FG, b'd'),
                (b"fg_end_code:", COL_SEQ_FG, b'e'),
                (b"bg_start_code:", COL_SEQ_BG, b's'),
                (b"bg_default_code:", COL_SEQ_BG, b'd'),
                (b"bg_end_code:", COL_SEQ_BG, b'e'),
            ];
            for (prefix, seq, which) in keys {
                if let Some(code) = a.strip_prefix(prefix) {
                    let (raw, _) = self.getkeystring(code, crate::utils::GETKEYS_BINDKEY);
                    if let Some(entry) = self.fg_bg_sequences.get_mut(seq) {
                        match which {
                            b's' => entry.start = raw,
                            b'd' => entry.def = raw,
                            _ => entry.end = raw,
                        }
                    }
                    break;
                }
            }
        }
    }

    /// zsh's `set_colour_attribute`.
    pub(crate) fn set_colour_attribute(
        &mut self,
        bv: Option<&mut BufVars>,
        atr: u64,
        fg_bg: usize,
        flags: i32,
    ) {
        let is_prompt = flags & TSC_PROMPT != 0;
        let (colour, tc, def, use_truecolor) = if fg_bg == COL_SEQ_FG {
            (
                (atr & TXT_ATTR_FG_COL_MASK) >> TXT_ATTR_FG_COL_SHIFT,
                TCFGCOLOUR,
                atr & TXTNOFGCOLOUR != 0,
                atr & TXT_ATTR_FG_24BIT != 0,
            )
        } else {
            (
                (atr & TXT_ATTR_BG_COL_MASK) >> TXT_ATTR_BG_COL_SHIFT,
                TCBGCOLOUR,
                atr & TXTNOBGCOLOUR != 0,
                atr & TXT_ATTR_BG_24BIT != 0,
            )
        };
        let colour = i32::try_from(colour).unwrap_or(i32::MAX);
        let std_start = if fg_bg == COL_SEQ_FG {
            TC_COL_FG_START
        } else {
            TC_COL_BG_START
        };
        let is_default = self.fg_bg_sequences.get(fg_bg).is_some_and(|s| {
            s.start == std_start && s.def == TC_COL_DEFAULT && s.end == TC_COL_END
        });
        let emit = |sh: &Shell, bv: Option<&mut BufVars>, seq: &[u8]| {
            if is_prompt {
                if let Some(bv) = bv {
                    if bv.dontcount == 0 {
                        bv.buf.push(INPAR);
                    }
                    for &c in seq {
                        bv.pputc(c);
                    }
                    if bv.dontcount == 0 {
                        bv.buf.push(OUTPAR);
                    }
                }
            } else {
                sh.tputs_out(flags, seq);
            }
        };
        if !def && !use_truecolor && is_default {
            let tccolours = self.tccolours();
            if self.tccan(tc) && (tccolours < 0 || colour < tccolours) {
                let seq = self.colour_cap(tc, colour);
                emit(self, bv, &seq);
                return;
            }
            if colour > 255 {
                return;
            }
        }
        self.load_colour_codes();
        let seqs = self
            .fg_bg_sequences
            .get(fg_bg)
            .cloned()
            .unwrap_or_else(|| default_colour_sequences()[0].clone());
        let mut buf: Vec<u8> = if use_truecolor {
            std_start.to_vec()
        } else {
            seqs.start.clone()
        };
        if def {
            buf.extend_from_slice(if use_truecolor {
                TC_COL_DEFAULT
            } else {
                &seqs.def
            });
        } else if use_truecolor {
            buf.extend_from_slice(
                format!(
                    "8;2;{};{};{}",
                    colour >> 16,
                    (colour >> 8) & 0xff,
                    colour & 0xff
                )
                .as_bytes(),
            );
        } else if colour > 7 && colour <= 255 {
            buf.extend_from_slice(colour.to_string().as_bytes());
        } else {
            buf.push(b'0'.wrapping_add(u8::try_from(colour & 0xff).unwrap_or(0)));
        }
        buf.extend_from_slice(if use_truecolor { TC_COL_END } else { &seqs.end });
        emit(self, bv, &buf);
    }

    /// zsh's `printprompt4`: `$PS4` for an xtrace line.
    pub(crate) fn printprompt4(&mut self) {
        let Some(p4) = self
            .strvars
            .get(StrVar::Prompt4 as usize)
            .cloned()
            .flatten()
        else {
            return;
        };
        let t = self.opts[XTRACE];
        self.opts[XTRACE] = false;
        let (s, _) = self.promptexpand(&p4, false, None, None);
        self.opts[XTRACE] = t;
        write_fd(self.xtrerr_fd(), &tok::unmetafy(&s));
    }
}

/// zsh's `privasserted`: the effective user is root. (zinc has no POSIX.1e
/// capability sets to consult.)
pub(crate) fn privasserted() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}
