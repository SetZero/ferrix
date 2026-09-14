//! The lexer's nested scanners: double quotes, `$(...)`, `$((...))`.
//!
//! Ported from zsh's `dquote_parse`, `cmd_or_math`, `cmd_or_math_sub` and
//! `isnumglob`. `skipcomm` differs: zsh runs the whole parser over a command
//! substitution to find its closing parenthesis; zinc scans for it with the
//! quoting rules and `case ... esac` counted, and keeps the raw text either
//! way, which is all the result zsh keeps too.

use crate::lex::{LexOpts, Lexer};
use crate::tok::{self, BNULL, DNULL, INBRACE, INBRACK, INPAR, INPARMATH, OUTBRACE, OUTBRACK};
use crate::tok::{OUTPAR, OUTPARMATH, QSTRING, QTICK, STRING};

/// What `((` or `$((` turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CmdOrMath {
    Cmd,
    Math,
    Err,
}

impl Lexer {
    /// Scan as inside double quotes up to `endchar` (0: to the end of input),
    /// appending tokenized text to `buf`. Returns 0, or non-zero on error:
    /// the character that could not be accepted, or 1.
    pub(crate) fn dquote_parse(&mut self, buf: &mut Vec<u8>, endchar: u8, sub: bool) -> i32 {
        let (mut pct, mut brct, mut bct, mut intick) = (0i32, 0i32, 0i32, 0i32);
        let mut err: i32 = 0;
        let math = endchar == b')' || endchar == b']' || self.infor > 0;
        let mut last: u8 = 0;
        'outer: loop {
            let Some(mut c) = self.input.get() else { break };
            if !(c != endchar || bct > 0 || (math && (pct > 0 || brct > 0)) || intick > 0) {
                break;
            }
            loop {
                match c {
                    b'\\' => {
                        let Some(n) = self.input.get() else {
                            break 'outer;
                        };
                        c = n;
                        if c != b'\n' {
                            let escapes = c == b'$'
                                || c == b'\\'
                                || (c == b'}' && intick == 0 && bct > 0)
                                || c == endchar
                                || c == b'`'
                                || (endchar == b']'
                                    && (b"[](){}".contains(&c) || (c == b'"' && sub)));
                            if escapes {
                                buf.push(BNULL);
                            } else {
                                buf.push(b'\\');
                                continue;
                            }
                        } else if sub || !self.opts.cshjunkiequotes || endchar != b'"' {
                            continue 'outer;
                        }
                    }
                    b'\n' => {
                        err = i32::from(!sub && self.opts.cshjunkiequotes && endchar == b'"');
                    }
                    b'$' if intick == 0 => match self.input.get() {
                        Some(b'(') => {
                            buf.push(QSTRING);
                            match self.cmd_or_math_sub(buf) {
                                CmdOrMath::Cmd => c = OUTPAR,
                                CmdOrMath::Math => c = OUTPARMATH,
                                CmdOrMath::Err => err = 1,
                            }
                        }
                        Some(b'[') => {
                            buf.push(STRING);
                            buf.push(INBRACK);
                            err = self.dquote_parse(buf, b']', sub);
                            c = OUTBRACK;
                        }
                        Some(b'{') => {
                            buf.push(QSTRING);
                            c = INBRACE;
                            bct += 1;
                        }
                        Some(b'$') => buf.push(QSTRING),
                        other => {
                            self.input.unget_opt(other);
                            c = QSTRING;
                        }
                    },
                    b'}' if intick == 0 && bct > 0 => {
                        c = OUTBRACE;
                        bct -= 1;
                    }
                    b'`' => {
                        c = QTICK;
                        intick = i32::from(intick == 0);
                    }
                    b'\'' if intick > 0 => intick = if intick == 1 { 2 } else { 1 },
                    b'(' if !math || bct == 0 => pct += 1,
                    b')' if !math || bct == 0 => {
                        err = i32::from(pct == 0 && math);
                        pct -= 1;
                    }
                    b'[' if !math || bct == 0 => brct += 1,
                    b']' if !math || bct == 0 => {
                        err = i32::from(brct == 0 && math);
                        brct -= 1;
                    }
                    b'"' if !(intick > 0 || (endchar != b'"' && bct == 0)) => {
                        if bct > 0 {
                            buf.push(DNULL);
                            err = self.dquote_parse(buf, b'"', sub);
                            c = DNULL;
                        } else {
                            err = 1;
                        }
                    }
                    _ => {}
                }
                break;
            }
            last = c;
            if err != 0 || self.input.stop {
                break;
            }
            buf.push(c);
        }
        if self.input.stop {
            i32::from(intick > 0 || endchar != 0 || err != 0)
        } else if err == 1 {
            i32::from(last)
        } else {
            err
        }
    }

    /// After `((` in command position: arithmetic if the parentheses close
    /// as `))`, otherwise a subshell whose text is pushed back.
    pub(crate) fn cmd_or_math(&mut self, buf: &mut Vec<u8>) -> CmdOrMath {
        let oldlen = buf.len();
        let r = self.dquote_parse(buf, b')', false);
        let back = if r == 0 {
            match self.input.get() {
                Some(b')') => return CmdOrMath::Math,
                other => {
                    self.input.unget_opt(other);
                    Some(b')')
                }
            }
        } else if self.input.stop {
            return CmdOrMath::Err;
        } else {
            u8::try_from(r).ok()
        };
        self.input.unget_opt(back);
        while buf.len() > oldlen {
            if let Some(ch) = buf.pop() {
                self.input.unget(tok::detok(ch));
            }
        }
        self.input.unget(b'(');
        CmdOrMath::Cmd
    }

    /// After `$(`: `$((...))` or `$(...)`.
    pub(crate) fn cmd_or_math_sub(&mut self, buf: &mut Vec<u8>) -> CmdOrMath {
        let mut c = self.input.get();
        if c == Some(b'\\') {
            let n = self.input.get();
            if n != Some(b'\n') {
                self.input.unget_opt(n);
                self.input.unget(b'\\');
                return if self.skipcomm(buf) {
                    CmdOrMath::Err
                } else {
                    CmdOrMath::Cmd
                };
            }
            c = self.input.get();
        }
        if c == Some(b'(') {
            let lexpos = buf.len();
            buf.push(INPAR);
            buf.push(b'(');
            match self.cmd_or_math(buf) {
                CmdOrMath::Math => {
                    if let Some(slot) = buf.get_mut(lexpos) {
                        *slot = INPARMATH;
                    }
                    buf.push(b')');
                    return CmdOrMath::Math;
                }
                CmdOrMath::Err => return CmdOrMath::Err,
                CmdOrMath::Cmd => buf.truncate(lexpos),
            }
        } else {
            self.input.unget_opt(c);
        }
        if self.skipcomm(buf) {
            CmdOrMath::Err
        } else {
            CmdOrMath::Cmd
        }
    }

    /// Copy a command substitution's text up to its closing parenthesis,
    /// which is consumed but not copied. Returns true if the input ended
    /// first.
    pub(crate) fn skipcomm(&mut self, buf: &mut Vec<u8>) -> bool {
        buf.push(INPAR);
        let saved_infor = self.infor;
        self.infor = 0;
        let mut depth = 1u32;
        let mut cases = 0u32;
        // At the start of a word, for comments and keywords.
        let mut word_start = true;
        let mut word: Vec<u8> = Vec::new();
        let ended = loop {
            let Some(c) = self.input.get() else {
                break true;
            };
            let was_start = word_start;
            word_start = false;
            match c {
                b'(' => depth += 1,
                b')' if cases > 0 => {}
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break false;
                    }
                }
                b'\\' => {
                    buf.push(c);
                    if let Some(n) = self.input.get() {
                        buf.push(n);
                    }
                    continue;
                }
                b'\'' => {
                    let dollar = buf.last() == Some(&b'$');
                    buf.push(c);
                    while let Some(n) = self.input.get() {
                        buf.push(n);
                        if n == b'\\' && dollar {
                            if let Some(m) = self.input.get() {
                                buf.push(m);
                            }
                        } else if n == b'\'' {
                            break;
                        }
                    }
                    continue;
                }
                b'"' | b'`' => {
                    buf.push(c);
                    while let Some(n) = self.input.get() {
                        buf.push(n);
                        if n == b'\\' {
                            if let Some(m) = self.input.get() {
                                buf.push(m);
                            }
                        } else if n == c {
                            break;
                        }
                    }
                    continue;
                }
                b'#' if was_start && self.opts.comments => {
                    buf.push(c);
                    while let Some(n) = self.input.get() {
                        if n == b'\n' {
                            self.input.unget(n);
                            break;
                        }
                        buf.push(n);
                    }
                    continue;
                }
                _ => {}
            }
            buf.push(c);
            if c.is_ascii_alphanumeric() || c == b'_' {
                word.push(c);
            } else {
                match word.as_slice() {
                    b"case" => cases += 1,
                    b"esac" => cases = cases.saturating_sub(1),
                    _ => {}
                }
                word.clear();
                word_start = matches!(c, b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(');
            }
        };
        self.infor = saved_infor;
        ended
    }

    /// After `<`: is this `<digits-digits>`? The input is left unchanged.
    pub(crate) fn isnumglob(&mut self) -> bool {
        let mut read: Vec<u8> = Vec::new();
        let mut expect = b'-';
        let mut ok = false;
        while let Some(c) = self.input.get() {
            read.push(c);
            if !c.is_ascii_digit() {
                if c != expect {
                    break;
                }
                if expect == b'>' {
                    ok = true;
                    break;
                }
                expect = b'>';
            }
        }
        while let Some(c) = read.pop() {
            self.input.unget(c);
        }
        ok
    }
}

/// Tokenize `text` as if it were inside double quotes (zsh's `parsestr`).
pub(crate) fn parse_dquote_string(text: &[u8], opts: LexOpts) -> Result<Vec<u8>, i32> {
    let mut lx = Lexer::new(text.to_vec(), opts);
    let mut buf = Vec::with_capacity(text.len());
    match lx.dquote_parse(&mut buf, 0, true) {
        0 => Ok(buf),
        e => Err(e),
    }
}
