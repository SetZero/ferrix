//! Compound commands and `[[ ]]`: the rest of zsh's `parse.c`.

use std::rc::Rc;

use crate::ast::{CaseArm, CaseTerm, CmdKind, Cond, CondOp, List, Redir, Word};
use crate::lex::{Tok, is_blank};
use crate::parse::{PResult, Parser};
use crate::tok::{BAR, DASH, EQUALS, INBRACE, INPAR, OUTPAR, TILDE};

/// True if `s` is an identifier (zsh's `isident`, without subscripts).
fn is_ident(s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'_')
}

/// A dash, tokenized or not.
fn is_dash(c: Option<&u8>) -> bool {
    matches!(c, Some(&b'-') | Some(&DASH))
}

impl Parser<'_> {
    /// The body of a loop: `do list done`, `{ list }`, `list end` or a short
    /// form. `short_ok` says whether the short form is allowed here.
    fn par_loop_body(&mut self, csh: bool, short_ok: bool) -> PResult<List> {
        self.lx.incmdpos = true;
        self.skip_seps();
        match self.tok() {
            Tok::Doloop => {
                self.next();
                let body = self.par_list()?;
                if self.tok() != Tok::Done {
                    return self.error();
                }
                self.lx.incmdpos = false;
                self.next();
                Ok(body)
            }
            Tok::Inbrace => {
                self.next();
                let body = self.par_list()?;
                if self.tok() != Tok::Outbrace {
                    return self.error();
                }
                self.lx.incmdpos = false;
                self.next();
                Ok(body)
            }
            _ if csh || self.lx.opts.cshjunkieloops => {
                let body = self.par_list()?;
                if self.tok() != Tok::Zend {
                    return self.error();
                }
                self.lx.incmdpos = false;
                self.next();
                Ok(body)
            }
            _ if !short_ok => self.error(),
            _ => self.par_list1(),
        }
    }

    /// `for`, `foreach` and `select`.
    pub(crate) fn par_for(&mut self) -> PResult<CmdKind> {
        let csh = self.tok() == Tok::Foreach;
        let sel = self.tok() == Tok::Select;
        self.lx.incmdpos = false;
        self.lx.infor = if self.tok() == Tok::For { 2 } else { 0 };
        self.next();
        if self.tok() == Tok::Dinpar {
            self.next();
            if self.tok() != Tok::Dinpar {
                return self.error();
            }
            let init = self.tokstr();
            self.next();
            if self.tok() != Tok::Dinpar {
                return self.error();
            }
            let cond = self.tokstr();
            self.next();
            if self.tok() != Tok::Doutpar {
                return self.error();
            }
            let step = self.tokstr();
            self.lx.infor = 0;
            self.lx.incmdpos = true;
            self.next();
            let body = self.par_loop_body(false, self.lx.opts.shortloops)?;
            return Ok(CmdKind::ForArith {
                init,
                cond,
                step,
                body,
            });
        }
        self.lx.infor = 0;
        if self.tok() != Tok::String || !is_ident(&self.tokstr()) {
            return self.error();
        }
        let mut vars = Vec::new();
        self.lx.incmdpos = true;
        let saved_noaliases = self.lx.noaliases;
        self.lx.noaliases = true;
        loop {
            vars.push(self.tokstr());
            self.next();
            if self.tok() != Tok::String || self.tokstr() == b"in" || sel {
                break;
            }
            if !is_ident(&self.tokstr()) || self.lx.error.is_some() {
                self.lx.noaliases = saved_noaliases;
                return self.error();
            }
        }
        self.lx.noaliases = saved_noaliases;
        let posix_in = self.lx.isnewlin != 0;
        while self.lx.isnewlin != 0 {
            self.next();
        }
        let words = if self.tok() == Tok::String && self.tokstr() == b"in" {
            self.lx.incmdpos = false;
            self.next();
            let w = self.par_wordlist();
            if self.tok() != Tok::Seper {
                return self.error();
            }
            Some(w)
        } else if !posix_in && self.tok() == Tok::Inpar {
            self.lx.incmdpos = false;
            self.next();
            let w = self.par_nl_wordlist();
            if self.tok() != Tok::Outpar {
                return self.error();
            }
            self.lx.incmdpos = true;
            self.next();
            Some(w)
        } else {
            None
        };
        let body = self.par_loop_body(csh, self.lx.opts.shortloops)?;
        if sel {
            let var = vars.into_iter().next().unwrap_or_default();
            Ok(CmdKind::Select { var, words, body })
        } else {
            Ok(CmdKind::For { vars, words, body })
        }
    }

    /// `case word in pattern) list ;; ... esac`.
    pub(crate) fn par_case(&mut self) -> PResult<CmdKind> {
        self.lx.incmdpos = false;
        self.next();
        if self.tok() != Tok::String {
            return self.error();
        }
        let word = self.tokstr();
        self.lx.incmdpos = true;
        let saved_noaliases = self.lx.noaliases;
        self.lx.noaliases = true;
        self.next();
        self.skip_seps();
        if !((self.tok() == Tok::String && self.tokstr() == b"in") || self.tok() == Tok::Inbrace) {
            self.lx.noaliases = saved_noaliases;
            return self.error();
        }
        let brflag = self.tok() == Tok::Inbrace;
        self.lx.incasepat = 1;
        self.lx.incmdpos = false;
        self.lx.noaliases = saved_noaliases;
        self.next();
        let mut arms = Vec::new();
        loop {
            self.skip_seps();
            if self.tok() == Tok::Outbrace {
                break;
            }
            if self.tok() == Tok::Inpar {
                self.next();
            }
            let (mut s, skip_lex) = if self.tok() == Tok::Bar {
                (Vec::new(), true)
            } else {
                if self.tok() != Tok::String {
                    return self.error();
                }
                if self.tokstr() == b"esac" {
                    break;
                }
                (self.tokstr(), false)
            };
            let mut patterns: Vec<Word> = Vec::new();
            self.lx.incasepat = -1;
            self.lx.incmdpos = true;
            if !skip_lex {
                self.next();
            }
            loop {
                if self.tok() == Tok::Outpar {
                    patterns.push(std::mem::take(&mut s));
                    self.lx.incasepat = 0;
                    self.lx.incmdpos = true;
                    self.next();
                    break;
                } else if self.tok() == Tok::Bar {
                    patterns.push(std::mem::take(&mut s));
                    self.lx.incasepat = 1;
                    self.lx.incmdpos = false;
                } else {
                    if patterns.is_empty() && s.first() == Some(&INPAR) {
                        match strip_case_parens(&s) {
                            Some(p) => {
                                patterns.push(p);
                                break;
                            }
                            None => return self.error(),
                        }
                    }
                    return self.error();
                }
                self.next();
                match self.tok() {
                    Tok::String => {
                        s = self.tokstr();
                        self.next();
                    }
                    Tok::Outpar | Tok::Bar => s = Vec::new(),
                    _ => return self.error(),
                }
            }
            self.lx.incasepat = 0;
            let body = self.par_list()?;
            let term = match self.tok() {
                Tok::Semiamp => CaseTerm::Fallthrough,
                Tok::Semibar => CaseTerm::TestNext,
                _ => CaseTerm::Break,
            };
            arms.push(CaseArm {
                patterns,
                body,
                term,
            });
            if (self.tok() == Tok::Esac && !brflag) || (self.tok() == Tok::Outbrace && brflag) {
                break;
            }
            if !matches!(self.tok(), Tok::Dsemi | Tok::Semiamp | Tok::Semibar) {
                return self.error();
            }
            self.lx.incasepat = 1;
            self.lx.incmdpos = false;
            self.next();
        }
        self.lx.incmdpos = true;
        self.lx.incasepat = 0;
        self.next();
        Ok(CmdKind::Case { word, arms })
    }

    /// `if list then list [elif list then list]... [else list] fi`, and the
    /// brace and short forms.
    pub(crate) fn par_if(&mut self) -> PResult<CmdKind> {
        let mut branches = Vec::new();
        let mut otherwise = None;
        let mut usebrace = false;
        let mut xtok;
        loop {
            xtok = self.tok();
            if xtok == Tok::Fi {
                self.lx.incmdpos = false;
                self.next();
                break;
            }
            self.next();
            if xtok == Tok::Else {
                break;
            }
            self.skip_seps();
            if !matches!(xtok, Tok::If | Tok::Elif) {
                return self.error();
            }
            let cond = self.par_list()?;
            self.lx.incmdpos = true;
            if self.tok() == Tok::Endinput {
                return self.error();
            }
            self.skip_seps();
            xtok = Tok::Fi;
            match self.tok() {
                Tok::Then => {
                    usebrace = false;
                    self.next();
                    let body = self.par_list()?;
                    branches.push((cond, body));
                    self.lx.incmdpos = true;
                }
                Tok::Inbrace => {
                    usebrace = true;
                    self.next();
                    let body = self.par_list()?;
                    if self.tok() != Tok::Outbrace {
                        return self.error();
                    }
                    branches.push((cond, body));
                    self.next();
                    self.lx.incmdpos = true;
                    if self.tok() == Tok::Seper {
                        break;
                    }
                }
                _ if !self.lx.opts.shortloops => return self.error(),
                _ => {
                    let body = self.par_list1()?;
                    branches.push((cond, body));
                    self.lx.incmdpos = true;
                    break;
                }
            }
        }
        if xtok == Tok::Else || self.tok() == Tok::Else {
            self.skip_seps();
            let body = if self.tok() == Tok::Inbrace && usebrace {
                self.next();
                let l = self.par_list()?;
                if self.tok() != Tok::Outbrace {
                    return self.error();
                }
                l
            } else {
                let l = self.par_list()?;
                if self.tok() != Tok::Fi {
                    return self.error();
                }
                l
            };
            self.lx.incmdpos = false;
            self.next();
            otherwise = Some(body);
        }
        Ok(CmdKind::If {
            branches,
            otherwise,
        })
    }

    /// `while`/`until list do list done` and the other body forms.
    pub(crate) fn par_while(&mut self) -> PResult<CmdKind> {
        let until = self.tok() == Tok::Until;
        self.next();
        let cond = self.par_list()?;
        let body = self.par_loop_body(false, self.lx.opts.shortloops)?;
        Ok(CmdKind::While { until, cond, body })
    }

    /// `repeat count body`.
    pub(crate) fn par_repeat(&mut self) -> PResult<CmdKind> {
        self.lx.incmdpos = false;
        self.next();
        if self.tok() != Tok::String {
            return self.error();
        }
        let count = self.tokstr();
        self.lx.incmdpos = true;
        self.next();
        let short = self.lx.opts.shortloops || self.lx.opts.shortrepeat;
        let body = self.par_loop_body(false, short)?;
        Ok(CmdKind::Repeat { count, body })
    }

    /// `( list )`, `{ list }` and `{ list } always { list }`.
    pub(crate) fn par_subsh(&mut self, zsh_construct: bool) -> PResult<CmdKind> {
        let otok = self.tok();
        self.next();
        let body = self.par_list()?;
        let close = if otok == Tok::Inpar {
            Tok::Outpar
        } else {
            Tok::Outbrace
        };
        if self.tok() != close {
            return self.error();
        }
        self.lx.incmdpos = !zsh_construct;
        self.next();
        if otok == Tok::Inbrace && self.tok() == Tok::String && self.tokstr() == b"always" {
            self.lx.incmdpos = true;
            self.next();
            self.skip_seps();
            if self.tok() != Tok::Inbrace {
                return self.error();
            }
            self.next();
            let always = self.par_list()?;
            self.skip_seps();
            self.lx.incmdpos = true;
            if self.tok() != Tok::Outbrace {
                return self.error();
            }
            self.next();
            return Ok(CmdKind::Try { body, always });
        }
        Ok(if otok == Tok::Inpar {
            CmdKind::Subsh(body)
        } else {
            CmdKind::Cursh(body)
        })
    }

    /// `function name... [()] body`, and `function { body } args`.
    pub(crate) fn par_funcdef(&mut self, redirs: &mut Vec<Redir>) -> PResult<CmdKind> {
        let _ = redirs;
        self.lx.incmdpos = false;
        self.next();
        let mut tracing = false;
        if self.tok() == Tok::String && self.tokstr().first() == Some(&DASH) {
            if self.tokstr() == [DASH, b'T'] {
                tracing = true;
                self.next();
            }
            if self.tok() == Tok::String && self.tokstr() == [DASH, DASH] {
                self.next();
            }
        }
        let mut names = Vec::new();
        while self.tok() == Tok::String {
            let s = self.tokstr();
            if s == [INBRACE] || s == b"{" {
                self.lx.tok = Tok::Inbrace;
                break;
            }
            names.push(s);
            self.next();
        }
        self.lx.incmdpos = true;
        if self.tok() == Tok::Inoutpar {
            self.next();
        }
        self.skip_seps();
        let anonymous = names.is_empty();
        let body = if self.tok() == Tok::Inbrace {
            self.next();
            let l = self.par_list()?;
            if self.tok() != Tok::Outbrace {
                return self.error();
            }
            if anonymous {
                self.lx.incmdpos = false;
            }
            self.next();
            l
        } else if !self.lx.opts.shortloops {
            return self.error();
        } else {
            self.par_list1()?
        };
        let mut args = Vec::new();
        if anonymous {
            while self.tok() == Tok::String {
                args.push(self.tokstr());
                self.next();
            }
        }
        Ok(CmdKind::FuncDef {
            names,
            body: Rc::new(body),
            tracing,
            args,
        })
    }

    /// `time [pipeline]`.
    pub(crate) fn par_time(&mut self) -> PResult<CmdKind> {
        self.next();
        Ok(CmdKind::Time(self.par_sublist2()?.map(Box::new)))
    }

    /// `[[ cond ]]`.
    pub(crate) fn par_dinbrack(&mut self) -> PResult<CmdKind> {
        self.lx.incond = 1;
        self.lx.incmdpos = false;
        self.next();
        let cond = self.par_cond()?;
        if self.tok() != Tok::Doutbrack {
            return self.error();
        }
        self.lx.incond = 0;
        self.lx.incmdpos = true;
        self.next();
        Ok(CmdKind::Cond(cond))
    }

    /// Separators inside `[[ ]]` are newlines only: a `;` is an error.
    fn cond_sep(&self) -> bool {
        self.tok() == Tok::Seper && !self.lx.sep_was_semi
    }

    fn skip_cond_seps(&mut self) {
        while self.cond_sep() {
            self.next();
        }
    }

    /// `cond : cond_1 { SEPER } [ DBAR { SEPER } cond ]`
    fn par_cond(&mut self) -> PResult<Cond> {
        let left = self.par_cond_1()?;
        self.skip_cond_seps();
        if self.tok() == Tok::Dbar {
            self.next();
            self.skip_cond_seps();
            let right = self.par_cond()?;
            return Ok(Cond::Or(Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    /// `cond_1 : cond_2 { SEPER } [ DAMPER { SEPER } cond_1 ]`
    fn par_cond_1(&mut self) -> PResult<Cond> {
        let left = self.par_cond_2()?;
        self.skip_cond_seps();
        if self.tok() == Tok::Damper {
            self.next();
            self.skip_cond_seps();
            let right = self.par_cond_1()?;
            return Ok(Cond::And(Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    /// A primary: `! cond`, `( cond )`, or one to many words.
    fn par_cond_2(&mut self) -> PResult<Cond> {
        self.skip_cond_seps();
        if self.tok() == Tok::Bang {
            self.next();
            return Ok(Cond::Not(Box::new(self.par_cond_2()?)));
        }
        if self.tok() == Tok::Inpar {
            self.next();
            self.skip_cond_seps();
            let c = self.par_cond()?;
            self.skip_cond_seps();
            if self.tok() != Tok::Outpar {
                return self.error();
            }
            self.next();
            return Ok(c);
        }
        let s1 = self.lx.tokstr.clone();
        let dble = s1
            .as_deref()
            .is_some_and(|s| is_dash(s.first()) && s.len() == 2);
        if self.tok() != Tok::String {
            if let Some(s1) = s1
                && self.tok() != Tok::Lexerr
                && !dble
            {
                loop {
                    self.next();
                    if !self.cond_sep() {
                        break;
                    }
                }
                return self.cond_double(b"-".iter().copied().chain(*b"n").collect(), s1);
            }
            return self.error();
        }
        let s1 = s1.unwrap_or_default();
        self.next();
        self.skip_cond_seps();
        if matches!(self.tok(), Tok::Inang | Tok::Outang) {
            let op = if self.tok() == Tok::Inang {
                CondOp::StrLt
            } else {
                CondOp::StrGt
            };
            loop {
                self.next();
                if !self.cond_sep() {
                    break;
                }
            }
            if self.tok() != Tok::String {
                return self.error();
            }
            let s3 = self.tokstr();
            loop {
                self.next();
                if !self.cond_sep() {
                    break;
                }
            }
            return Ok(Cond::Binary(op, s1, s3));
        }
        if self.tok() != Tok::String {
            if self.tok() != Tok::Lexerr {
                if !dble {
                    return self.cond_double(b"-n".to_vec(), s1);
                }
                return self.cond_multi(s1, Vec::new());
            }
            return self.error();
        }
        let s2 = self.tokstr();
        let dble2 = is_dash(s2.first()) && s2.len() == 2;
        self.lx.incond += 1;
        loop {
            self.next();
            if !self.cond_sep() {
                break;
            }
        }
        self.lx.incond -= 1;
        if self.tok() == Tok::String && !dble2 {
            let s3 = self.tokstr();
            loop {
                self.next();
                if !self.cond_sep() {
                    break;
                }
            }
            if self.tok() == Tok::String {
                let mut rest = vec![s2, s3];
                while self.tok() == Tok::String {
                    rest.push(self.tokstr());
                    loop {
                        self.next();
                        if !self.cond_sep() {
                            break;
                        }
                    }
                }
                return self.cond_multi(s1, rest);
            }
            return self.cond_triple(s1, s2, s3);
        }
        self.cond_double(s1, s2)
    }

    fn cond_double(&mut self, a: Word, b: Word) -> PResult<Cond> {
        if !is_dash(a.first()) || a.len() < 2 {
            return self.error_msg(format!("parse error: condition expected: {}", shown(&a)));
        }
        if let [_, letter] = a.as_slice()
            && b"abcdefgknoprstuvwxzhLONGS".contains(letter)
        {
            return Ok(Cond::Unary(*letter, b));
        }
        Ok(Cond::Module {
            name: a,
            args: vec![b],
            infix: false,
        })
    }

    fn cond_triple(&mut self, a: Word, b: Word, c: Word) -> PResult<Cond> {
        let eq = |x: u8| x == b'=' || x == EQUALS;
        let op = match b.as_slice() {
            [x] if eq(*x) => Some(CondOp::StrEq),
            [x, y] if eq(*x) && eq(*y) => Some(CondOp::StrDeq),
            [b'!', y] if eq(*y) => Some(CondOp::StrNeq),
            [x, y] if eq(*x) && (*y == b'~' || *y == TILDE) => Some(CondOp::Regex),
            _ => None,
        };
        if let Some(op) = op {
            return Ok(Cond::Binary(op, a, c));
        }
        if is_dash(b.first()) {
            let num = match b.get(1..).unwrap_or(&[]) {
                b"nt" => Some(CondOp::Nt),
                b"ot" => Some(CondOp::Ot),
                b"ef" => Some(CondOp::Ef),
                b"eq" => Some(CondOp::Eq),
                b"ne" => Some(CondOp::Ne),
                b"lt" => Some(CondOp::Lt),
                b"gt" => Some(CondOp::Gt),
                b"le" => Some(CondOp::Le),
                b"ge" => Some(CondOp::Ge),
                _ => None,
            };
            return Ok(match num {
                Some(op) => Cond::Binary(op, a, c),
                None => Cond::Module {
                    name: b,
                    args: vec![a, c],
                    infix: true,
                },
            });
        }
        if is_dash(a.first()) && a.len() > 1 {
            return Ok(Cond::Module {
                name: a,
                args: vec![b, c],
                infix: false,
            });
        }
        self.error_msg(format!("condition expected: {}", shown(&b)))
    }

    fn cond_multi(&mut self, a: Word, args: Vec<Word>) -> PResult<Cond> {
        if !is_dash(a.first()) || a.len() < 2 {
            return self.error_msg(format!("condition expected: {}", shown(&a)));
        }
        Ok(Cond::Module {
            name: a,
            args,
            infix: false,
        })
    }
}

/// A word as zsh shows it in a message.
fn shown(w: &[u8]) -> String {
    let mut t = w.to_vec();
    crate::tok::untokenize(&mut t);
    String::from_utf8_lossy(&crate::tok::unmetafy(&t)).into_owned()
}

/// A case pattern written as one `(a|b)` word: strip the outer parentheses
/// and the blanks beside `|` and the parentheses, as zsh does.
fn strip_case_parens(s: &[u8]) -> Option<Word> {
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let mut pct = 0;
    let mut i = 0;
    while let Some(&c) = s.get(i) {
        if c == INPAR {
            pct += 1;
        }
        if pct == 0 {
            break;
        }
        if pct == 1 && (c == BAR || c == OUTPAR) {
            while out.last().is_some_and(|&b| is_blank(b)) {
                let _blank = out.pop();
            }
        }
        out.push(c);
        if pct == 1 && (c == BAR || c == INPAR) {
            while s.get(i + 1).is_some_and(|&b| is_blank(b)) {
                i += 1;
            }
        }
        if c == OUTPAR {
            pct -= 1;
        }
        i += 1;
    }
    if i < s.len() || pct != 0 || out.len() < 2 {
        return None;
    }
    out.get(1..out.len() - 1).map(<[u8]>::to_vec)
}
