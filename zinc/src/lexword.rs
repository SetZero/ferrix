//! `gettok` and `gettokstr`: splitting input into tokens and tokenizing words.

use crate::dquote::CmdOrMath;
use crate::lex::{Lexer, Tok, is_blank, is_inblank};
use crate::tok::{self, BANG, BNULL, COMMA, DASH, DNULL, EQUALS, HAT, INANG, INBRACE, INBRACK};
use crate::tok::{INPAR, META, OUTANG, OUTANGPROC, OUTBRACE, OUTBRACK, OUTPAR, OUTPARMATH};
use crate::tok::{POUND, QUEST, SNULL, STAR, STRING, TICK, TILDE};

/// zsh's `lextok2`: the token a character becomes inside a word by default.
fn lextok2(c: u8) -> u8 {
    match c {
        b'*' => STAR,
        b'?' => QUEST,
        b'{' => INBRACE,
        b'[' => INBRACK,
        b'$' => STRING,
        b'~' => TILDE,
        b'#' => POUND,
        b'^' => HAT,
        _ => c,
    }
}

impl Lexer {
    /// Read one token (zsh's `gettok`).
    pub(crate) fn gettok(&mut self) -> Tok {
        self.tokstr = None;
        let mut peekfd: i32 = -1;
        let mut c;
        loop {
            let mut got = self.input.get();
            while got.is_some_and(is_blank) {
                got = self.input.get();
            }
            self.toklineno = self.input.lineno;
            let Some(ch) = got else {
                return if self.error.is_some() {
                    Tok::Lexerr
                } else {
                    Tok::Endinput
                };
            };
            c = ch;
            if self.dbparens {
                return self.lex_dbparens(c);
            }
            if c.is_ascii_digit() {
                let d = self.input.get();
                match d {
                    Some(b'&') => {
                        let e = self.input.get();
                        if e == Some(b'>') {
                            peekfd = i32::from(c - b'0');
                            self.input.unget(b'>');
                            c = b'&';
                        } else {
                            self.input.unget_opt(e);
                            self.input.unget(b'&');
                        }
                    }
                    Some(x @ (b'>' | b'<')) => {
                        peekfd = i32::from(c - b'0');
                        c = x;
                    }
                    other => self.input.unget_opt(other),
                }
            }
            if c == b'#' && self.opts.comments {
                while let Some(n) = self.input.get() {
                    if n == b'\n' {
                        break;
                    }
                }
                return Tok::Newlin;
            }
            if c == b'\\' {
                match self.input.get() {
                    Some(b'\n') => continue,
                    other => self.input.unget_opt(other),
                }
            }
            break;
        }
        match c {
            b'\n' => return Tok::Newlin,
            b';' => {
                return match self.input.get() {
                    Some(b';') => Tok::Dsemi,
                    Some(b'&') => Tok::Semiamp,
                    Some(b'|') => Tok::Semibar,
                    other => {
                        self.input.unget_opt(other);
                        Tok::Semi
                    }
                };
            }
            b'&' => return self.lex_amper(peekfd),
            b'|' => {
                return match self.input.get() {
                    Some(b'|') if self.incasepat == 0 => Tok::Dbar,
                    Some(b'&') => Tok::Baramp,
                    other => {
                        self.input.unget_opt(other);
                        Tok::Bar
                    }
                };
            }
            b'(' => {
                if let Some(t) = self.lex_inpar() {
                    return t;
                }
            }
            b')' => return Tok::Outpar,
            b'<' => match self.lex_inang(peekfd) {
                Ok(t) => return t,
                Err(word_start) => c = word_start,
            },
            b'>' => match self.lex_outang(peekfd) {
                Ok(t) => return t,
                Err(word_start) => c = word_start,
            },
            _ => {}
        }
        self.gettokstr(c, false)
    }

    /// Inside `(( ))` of a `for ((;;))` or after `((`: the expression text.
    fn lex_dbparens(&mut self, c: u8) -> Tok {
        let mut buf = Vec::new();
        self.input.unget(c);
        let end = if self.infor > 0 { b';' } else { b')' };
        let r = self.dquote_parse(&mut buf, end, false);
        self.tokstr = Some(buf);
        if r == 0 && self.infor > 0 {
            self.infor -= 1;
            return Tok::Dinpar;
        }
        let next = if r == 0 { self.input.get() } else { None };
        if r != 0 || next != Some(b')') {
            self.input.unget_opt(next);
            return Tok::Lexerr;
        }
        self.dbparens = false;
        Tok::Doutpar
    }

    fn lex_amper(&mut self, peekfd: i32) -> Tok {
        match self.input.get() {
            Some(b'&') => Tok::Damper,
            Some(b'!' | b'|') => Tok::Amperbang,
            Some(b'>') => {
                self.tokfd = peekfd;
                match self.input.get() {
                    Some(b'!' | b'|') => Tok::Outangampbang,
                    Some(b'>') => match self.input.get() {
                        Some(b'!' | b'|') => Tok::Doutangampbang,
                        other => {
                            self.input.unget_opt(other);
                            Tok::Doutangamp
                        }
                    },
                    other => {
                        self.input.unget_opt(other);
                        Tok::Ampoutang
                    }
                }
            }
            other => {
                self.input.unget_opt(other);
                Tok::Amper
            }
        }
    }

    /// `(` at the start of a token; `None` means it starts a word.
    fn lex_inpar(&mut self) -> Option<Tok> {
        let d = self.input.get();
        if d == Some(b'(') {
            if self.infor > 0 {
                self.dbparens = true;
                return Some(Tok::Dinpar);
            }
            if self.incmdpos || (self.opts.shglob && !self.opts.kshglob) {
                let mut buf = Vec::new();
                return Some(match self.cmd_or_math(&mut buf) {
                    CmdOrMath::Math => {
                        self.tokstr = Some(buf);
                        Tok::Dinpar
                    }
                    CmdOrMath::Cmd => Tok::Inpar,
                    CmdOrMath::Err => Tok::Lexerr,
                });
            }
        } else if d == Some(b')') {
            return Some(Tok::Inoutpar);
        }
        self.input.unget_opt(d);
        if !(self.opts.shglob || self.incond == 1 || self.incmdpos) {
            return None;
        }
        Some(Tok::Inpar)
    }

    /// Put a redirection's fd digit back in front of a word starting `<(`.
    fn unpeekfd(&mut self, c: u8, peekfd: i32) -> u8 {
        if peekfd == -1 {
            return c;
        }
        self.input.unget(c);
        b'0' + u8::try_from(peekfd).unwrap_or(0)
    }

    /// `<`: a redirection, or `Err(first char)` if it starts a word.
    fn lex_inang(&mut self, peekfd: i32) -> Result<Tok, u8> {
        let d = self.input.get();
        let peek = match d {
            Some(b'(') => {
                self.input.unget(b'(');
                return Err(self.unpeekfd(b'<', peekfd));
            }
            Some(b'>') => Tok::Inoutang,
            Some(b'<') => match self.input.get() {
                Some(b'(') => {
                    self.input.unget(b'(');
                    self.input.unget(b'<');
                    Tok::Inang
                }
                Some(b'<') => Tok::Trinang,
                Some(b'-') => Tok::Dinangdash,
                other => {
                    self.input.unget_opt(other);
                    Tok::Dinang
                }
            },
            Some(b'&') => Tok::Inangamp,
            other => {
                self.input.unget_opt(other);
                if self.isnumglob() {
                    return Err(self.unpeekfd(b'<', peekfd));
                }
                Tok::Inang
            }
        };
        self.tokfd = peekfd;
        Ok(peek)
    }

    /// `>`: a redirection, or `Err(first char)` if it starts a word.
    fn lex_outang(&mut self, peekfd: i32) -> Result<Tok, u8> {
        let peek = match self.input.get() {
            Some(b'(') => {
                self.input.unget(b'(');
                return Err(self.unpeekfd(b'>', peekfd));
            }
            Some(b'&') => match self.input.get() {
                Some(b'!' | b'|') => Tok::Outangampbang,
                other => {
                    self.input.unget_opt(other);
                    Tok::Outangamp
                }
            },
            Some(b'!' | b'|') => Tok::Outangbang,
            Some(b'>') => match self.input.get() {
                Some(b'&') => match self.input.get() {
                    Some(b'!' | b'|') => Tok::Doutangampbang,
                    other => {
                        self.input.unget_opt(other);
                        Tok::Doutangamp
                    }
                },
                Some(b'!' | b'|') => Tok::Doutangbang,
                Some(b'(') => {
                    self.input.unget(b'(');
                    self.input.unget(b'>');
                    Tok::Outang
                }
                other => {
                    self.input.unget_opt(other);
                    Tok::Doutang
                }
            },
            other => {
                self.input.unget_opt(other);
                Tok::Outang
            }
        };
        self.tokfd = peekfd;
        Ok(peek)
    }
}

/// Mutable state of one `gettokstr` scan.
#[derive(Debug, Default)]
struct WordScan {
    bct: i32,
    pct: i32,
    brct: i32,
    seen_brct: bool,
    fdpar: bool,
    intpos: i32,
    in_brace_param: i32,
    unmatched: u8,
}

/// What one character did to the scan.
enum Step {
    /// Append this byte and read on.
    Add(u8),
    /// The word ends before the current character.
    Break,
    /// Read on without appending anything.
    Skip(Option<u8>),
    /// Return this token now, with the buffer as its text.
    Return(Tok),
    /// A lexical error; the word ends.
    Error,
}

impl Lexer {
    /// Tokenize the rest of a word starting with `c` (zsh's `gettokstr`).
    /// With `sub`, the whole input is one word (a substitution's operand).
    pub(crate) fn gettokstr(&mut self, first: u8, sub: bool) -> Tok {
        let mut buf: Vec<u8> = Vec::new();
        let mut st = WordScan {
            intpos: 1,
            ..WordScan::default()
        };
        let mut peek = Tok::String;
        let mut cur = Some(first);
        while let Some(c) = cur {
            let inbl = is_inblank(c);
            if st.fdpar && !inbl && c != b')' {
                st.fdpar = false;
            }
            let step = if inbl && st.in_brace_param == 0 && st.pct == 0 {
                if st.in_brace_param == 0 && !sub {
                    Step::Break
                } else {
                    Step::Add(c)
                }
            } else {
                self.word_char(c, sub, &mut st, &mut buf, &mut peek)
            };
            match step {
                Step::Add(b) => buf.push(b),
                Step::Break => break,
                Step::Skip(next) => {
                    cur = next;
                    continue;
                }
                Step::Return(t) => {
                    self.tokstr = Some(buf);
                    return t;
                }
                Step::Error => {
                    peek = Tok::Lexerr;
                    cur = None;
                    break;
                }
            }
            cur = self.input.get();
            if st.intpos > 0 {
                st.intpos -= 1;
            }
        }
        self.input.unget_opt(cur);
        if st.unmatched != 0 {
            self.err(format!("unmatched {}", char::from(st.unmatched)));
            peek = Tok::Lexerr;
        }
        if st.in_brace_param > 0 {
            self.err("closing brace expected");
            peek = Tok::Lexerr;
        } else if !self.opts.ignorebraces
            && !sub
            && buf.len() > 1
            && peek == Tok::String
            && buf.last() == Some(&b'}')
            && buf.get(buf.len() - 2) != Some(&BNULL)
        {
            // The `{foo}` command syntax: a trailing `}` is its own word.
            let _brace = buf.pop();
            self.input.unget(b'}');
        }
        self.tokstr = Some(buf);
        peek
    }

    /// Handle one non-blank character of a word.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per character class, as in zsh's switch"
    )]
    fn word_char(
        &mut self,
        c: u8,
        sub: bool,
        st: &mut WordScan,
        buf: &mut Vec<u8>,
        peek: &mut Tok,
    ) -> Step {
        let ibp = st.in_brace_param > 0;
        match c {
            b';' | b'&' => {
                if !ibp && !sub {
                    Step::Break
                } else {
                    Step::Add(c)
                }
            }
            b'\n' | b' ' | b'\t' => Step::Add(c),
            META => {
                buf.push(META);
                self.input.get().map_or(Step::Error, Step::Add)
            }
            b')' => {
                if st.fdpar {
                    buf.push(c);
                    return Step::Return(Tok::Inoutpar);
                }
                if (sub || ibp) && self.opts.shglob {
                    return Step::Add(c);
                }
                if !ibp {
                    let was = st.pct;
                    st.pct -= 1;
                    if was == 0 {
                        if sub {
                            st.pct = 0;
                            return Step::Add(c);
                        }
                        return Step::Break;
                    }
                }
                Step::Add(OUTPAR)
            }
            b'|' => {
                if st.pct == 0 && !ibp {
                    return if sub { Step::Add(c) } else { Step::Break };
                }
                if !self.opts.shglob || (!sub && !ibp) {
                    Step::Add(tok::BAR)
                } else {
                    Step::Add(c)
                }
            }
            b'$' => self.word_dollar(st, buf),
            b'[' => {
                if !ibp {
                    st.brct += 1;
                    st.seen_brct = true;
                }
                Step::Add(INBRACK)
            }
            b']' => {
                if !ibp {
                    st.brct -= 1;
                }
                if st.brct < 0 {
                    st.brct = 0;
                }
                Step::Add(OUTBRACK)
            }
            b'(' => self.word_inpar(sub, st, buf),
            b'{' => {
                if self.opts.ignorebraces || sub {
                    return Step::Add(b'{');
                }
                if buf.is_empty() && self.incmdpos {
                    buf.push(b'{');
                    return Step::Return(Tok::String);
                }
                st.bct += 1;
                Step::Add(INBRACE)
            }
            b'}' => {
                if (self.opts.ignorebraces || sub) && !ibp {
                    return Step::Add(c);
                }
                if st.bct == 0 {
                    return Step::Add(c);
                }
                if st.bct == st.in_brace_param {
                    st.in_brace_param = 0;
                }
                st.bct -= 1;
                Step::Add(OUTBRACE)
            }
            b',' => {
                if !self.opts.ignorebraces && !sub && st.bct > st.in_brace_param {
                    Step::Add(COMMA)
                } else {
                    Step::Add(c)
                }
            }
            b'>' => {
                if ibp || sub {
                    return Step::Add(c);
                }
                let e = self.input.get();
                if e != Some(b'(') {
                    self.input.unget_opt(e);
                    return Step::Break;
                }
                buf.push(OUTANGPROC);
                if self.skipcomm(buf) {
                    Step::Error
                } else {
                    Step::Add(OUTPAR)
                }
            }
            b'<' => self.word_inang(sub, st, buf),
            b'=' => self.word_equals(sub, st, buf, peek),
            b'\\' => {
                let n = self.input.get();
                if n == Some(b'\n') {
                    return Step::Skip(self.input.get());
                }
                match n {
                    None => Step::Break,
                    Some(META) => {
                        buf.push(BNULL);
                        buf.push(META);
                        self.input.get().map_or(Step::Error, Step::Add)
                    }
                    Some(x) => {
                        buf.push(BNULL);
                        Step::Add(x)
                    }
                }
            }
            b'\'' => self.word_squote(sub, st, buf),
            b'"' => {
                buf.push(DNULL);
                if self.dquote_parse(buf, b'"', sub) != 0 {
                    st.unmatched = b'"';
                    return Step::Error;
                }
                Step::Add(DNULL)
            }
            b'`' => self.word_bquote(sub, st, buf),
            b'-' => Step::Add(DASH),
            b'!' => Step::Add(if st.seen_brct { BANG } else { b'!' }),
            _ => Step::Add(lextok2(c)),
        }
    }

    fn word_dollar(&mut self, st: &mut WordScan, buf: &mut Vec<u8>) -> Step {
        let mut e = self.input.get();
        if e == Some(b'\\') {
            let n = self.input.get();
            if n != Some(b'\n') {
                self.input.unget_opt(n);
                self.input.unget(b'\\');
                return Step::Add(STRING);
            }
            e = self.input.get();
        }
        match e {
            Some(b'[') => {
                buf.push(STRING);
                buf.push(INBRACK);
                if self.dquote_parse(buf, b']', false) != 0 {
                    return Step::Error;
                }
                Step::Add(OUTBRACK)
            }
            Some(b'(') => {
                buf.push(STRING);
                match self.cmd_or_math_sub(buf) {
                    CmdOrMath::Cmd => Step::Add(OUTPAR),
                    CmdOrMath::Math => Step::Add(OUTPARMATH),
                    CmdOrMath::Err => Step::Error,
                }
            }
            Some(b'{') => {
                buf.push(STRING);
                st.bct += 1;
                if st.in_brace_param == 0 {
                    st.in_brace_param = st.bct;
                    st.seen_brct = false;
                }
                Step::Add(INBRACE)
            }
            other => {
                self.input.unget_opt(other);
                Step::Add(STRING)
            }
        }
    }

    fn word_inpar(&mut self, sub: bool, st: &mut WordScan, buf: &mut [u8]) -> Step {
        let ibp = st.in_brace_param > 0;
        if self.opts.shglob {
            if sub || ibp {
                return Step::Add(b'(');
            }
            if self.incasepat > 0 && buf.is_empty() {
                return Step::Return(Tok::Inpar);
            }
            if !self.opts.kshglob && !buf.is_empty() {
                return Step::Break;
            }
        }
        if !ibp {
            if !sub {
                let e = self.input.peek();
                let ksh_funcdef = self.opts.shglob
                    && e.is_some_and(is_inblank)
                    && st.bct == 0
                    && st.brct == 0
                    && st.intpos == 0
                    && self.incmdpos;
                if e == Some(b')') || ksh_funcdef {
                    return Step::Break;
                }
            }
            if st.pct == 0 && self.opts.shglob && st.intpos > 0 && st.bct == 0 && st.brct == 0 {
                st.fdpar = true;
            }
            st.pct += 1;
        }
        Step::Add(INPAR)
    }

    fn word_inang(&mut self, sub: bool, st: &mut WordScan, buf: &mut Vec<u8>) -> Step {
        let ibp = st.in_brace_param > 0;
        if self.opts.shglob && sub {
            return Step::Add(b'<');
        }
        let e = self.input.get();
        if !(ibp || sub) && e == Some(b'(') {
            buf.push(INANG);
            return if self.skipcomm(buf) {
                Step::Error
            } else {
                Step::Add(OUTPAR)
            };
        }
        self.input.unget_opt(e);
        if self.isnumglob() {
            buf.push(INANG);
            while let Some(n) = self.input.get() {
                if n == b'>' {
                    break;
                }
                buf.push(n);
            }
            return Step::Add(OUTANG);
        }
        if ibp || sub {
            Step::Add(b'<')
        } else {
            Step::Break
        }
    }

    fn word_equals(
        &mut self,
        sub: bool,
        st: &mut WordScan,
        buf: &mut Vec<u8>,
        peek: &mut Tok,
    ) -> Step {
        if sub {
            return Step::Add(b'=');
        }
        if st.intpos > 0 {
            let e = self.input.get();
            if e != Some(b'(') {
                self.input.unget_opt(e);
                return Step::Add(EQUALS);
            }
            buf.push(EQUALS);
            return if self.skipcomm(buf) {
                Step::Error
            } else {
                Step::Add(OUTPAR)
            };
        }
        if *peek != Tok::Envstring
            && (self.incmdpos || self.intypeset)
            && st.bct == 0
            && st.brct == 0
        {
            let t = assignment_name_end(buf);
            if t == buf.len() {
                let e = self.input.get();
                if e == Some(b'(') {
                    return Step::Return(Tok::Envarray);
                }
                self.input.unget_opt(e);
                *peek = Tok::Envstring;
                st.intpos = 2;
                return Step::Add(b'=');
            }
        }
        Step::Add(EQUALS)
    }

    fn word_squote(&mut self, sub: bool, st: &mut WordScan, buf: &mut Vec<u8>) -> Step {
        let strquote = buf.last() == Some(&STRING);
        buf.push(SNULL);
        loop {
            let mut closed = false;
            while let Some(mut ch) = self.input.get() {
                if ch == b'\'' {
                    closed = true;
                    break;
                }
                if strquote && ch == b'\\' {
                    let Some(n) = self.input.get() else { break };
                    ch = n;
                    buf.push(if ch == b'\\' || ch == b'\'' {
                        BNULL
                    } else {
                        b'\\'
                    });
                } else if !sub && self.opts.cshjunkiequotes && ch == b'\n' {
                    if buf.last() == Some(&b'\\') {
                        let _bs = buf.pop();
                    } else {
                        break;
                    }
                }
                buf.push(ch);
            }
            if !closed {
                st.unmatched = b'\'';
                return Step::Error;
            }
            let e = self.input.get();
            if e != Some(b'\'') || !self.opts.rcquotes || strquote {
                self.input.unget_opt(e);
                return Step::Add(SNULL);
            }
            buf.push(b'\'');
        }
    }

    fn word_bquote(&mut self, sub: bool, st: &mut WordScan, buf: &mut Vec<u8>) -> Step {
        buf.push(TICK);
        loop {
            let Some(ch) = self.input.get() else {
                st.unmatched = b'`';
                return Step::Error;
            };
            match ch {
                b'`' => return Step::Add(TICK),
                b'\\' => match self.input.get() {
                    Some(b'\n') => {
                        if !sub && self.opts.cshjunkiequotes {
                            buf.push(b'\n');
                        }
                    }
                    Some(n) => {
                        buf.push(if matches!(n, b'`' | b'\\' | b'$') {
                            BNULL
                        } else {
                            b'\\'
                        });
                        buf.push(n);
                    }
                    None => {
                        st.unmatched = b'`';
                        return Step::Error;
                    }
                },
                b'\n' if !sub && self.opts.cshjunkiequotes => {
                    st.unmatched = b'`';
                    return Step::Error;
                }
                _ => buf.push(ch),
            }
        }
    }
}

/// Where the name of a `name=`, `name[sub]=` or `name+=` assignment ends in
/// `buf`; equal to `buf.len()` when all of `buf` is such a name.
fn assignment_name_end(buf: &[u8]) -> usize {
    let mut t = 0;
    if buf.first().is_some_and(u8::is_ascii_digit) {
        while buf.get(t).is_some_and(u8::is_ascii_digit) {
            t += 1;
        }
    } else {
        while buf
            .get(t)
            .is_some_and(|&b| b.is_ascii_alphanumeric() || b == b'_')
        {
            t += 1;
        }
        if t < buf.len() && t > 0 && buf.get(t) == Some(&INBRACK) {
            let mut depth = 0;
            while let Some(&b) = buf.get(t) {
                t += 1;
                if b == INBRACK {
                    depth += 1;
                } else if b == OUTBRACK {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
        }
    }
    if buf.get(t) == Some(&b'+') {
        t += 1;
    }
    t
}
