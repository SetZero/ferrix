//! Compound commands (zsh's `loop.c`, and `execcursh`, `exectime` and
//! `execarith` from `exec.c`).

use crate::ast::{CaseTerm, CmdKind, Command, List};
use crate::exec::*;
use crate::math::MNumber;
use crate::options::*;
use crate::shell::{ERRFLAG_ERROR, ERRFLAG_INT, Shell, write_fd};
use crate::signals::{NOERREXIT_EXIT, NOERREXIT_RETURN, NOERREXIT_UNTIL_EXEC};
use crate::subst::WordList;
use crate::tok;
use crate::utils::has_token;

impl Shell {
    /// Run a compound command: zsh's `execfuncs[type - WC_CURSH]`.
    pub(crate) fn execconstruct(&mut self, cmd: &Command, do_exec: bool) -> i32 {
        match &cmd.kind {
            CmdKind::Cursh(l) => self.execcursh(l, do_exec),
            CmdKind::Time(p) => self.exectime(p.as_deref()),
            CmdKind::For { vars, words, body } => {
                self.execfor(vars, words.as_deref(), body, do_exec)
            }
            CmdKind::ForArith {
                init,
                cond,
                step,
                body,
            } => self.execfor_cond(init, cond, step, body, do_exec),
            CmdKind::Select { var, words, body } => self.execselect(var, words.as_deref(), body),
            CmdKind::While { until, cond, body } => self.execwhile(*until, cond, body),
            CmdKind::Repeat { count, body } => self.execrepeat(count, body),
            CmdKind::Case { word, arms } => self.execcase(word, arms, do_exec),
            CmdKind::If {
                branches,
                otherwise,
            } => self.execif(branches, otherwise.as_ref(), do_exec),
            CmdKind::Cond(c) => self.execcond(c),
            CmdKind::Arith(e) => self.execarith(e),
            CmdKind::Try { body, always } => self.exectry(body, always, do_exec),
            CmdKind::Subsh(l) => {
                self.execlist(l, false, true);
                self.lastval
            }
            CmdKind::FuncDef { .. } => self.execfuncdef(cmd, None),
            CmdKind::Simple { .. } | CmdKind::Typeset { .. } => self.lastval,
        }
    }

    /// zsh's `execcursh`.
    fn execcursh(&mut self, l: &List, do_exec: bool) -> i32 {
        let tj = self.thisjob;
        if !self.list_pipe
            && tj != -1
            && tj != self.list_pipe_job
            && !self.hasprocs(tj)
            && let Ok(t) = usize::try_from(tj)
        {
            self.deletejob(t, false);
        }
        self.execlist(l, true, do_exec);
        self.this_noerrexit = true;
        self.lastval
    }

    /// zsh's `exectime`.
    fn exectime(&mut self, p: Option<&crate::ast::Sublist2>) -> i32 {
        let jb = self.thisjob;
        let Some(p) = p else {
            self.shelltime();
            return 0;
        };
        let _ = self.execpline(p, Z_TIMED | Z_SYNC, false);
        self.thisjob = jb;
        self.lastval
    }

    /// zsh's `execarith`.
    fn execarith(&mut self, e: &[u8]) -> i32 {
        if self.isset(XTRACE) {
            self.printprompt4();
            write_fd(self.xtrerr_fd(), b"((");
        }
        let e = if has_token(e) {
            self.singsub(e)
        } else {
            e.to_vec()
        };
        if self.isset(XTRACE) {
            let mut t = b" ".to_vec();
            t.extend(tok::unmetafy(&e));
            write_fd(self.xtrerr_fd(), &t);
        }
        let val = self.matheval(&e);
        if self.isset(XTRACE) {
            write_fd(self.xtrerr_fd(), b" ))\n");
        }
        if self.errflag() {
            self.errflag.set(self.errflag.get() & !ERRFLAG_ERROR);
            return 2;
        }
        match val {
            MNumber::Int(i) => i32::from(i == 0),
            MNumber::Float(d) => i32::from(d == 0.0),
        }
    }

    /// zsh's `execfor` for `for x in words` and `for x`.
    fn execfor(
        &mut self,
        vars: &[Vec<u8>],
        words: Option<&[Vec<u8>]>,
        body: &List,
        do_exec: bool,
    ) -> i32 {
        let old_simple_pline = self.simple_pline;
        self.simple_pline = true;
        let mut args: Vec<Vec<u8>> = match words {
            Some(w) => {
                let mut wl = WordList {
                    words: w.to_vec(),
                    flags: 0,
                };
                if w.iter().any(|x| has_token(x)) {
                    self.execsubst(&mut wl);
                    if self.errflag() {
                        self.simple_pline = old_simple_pline;
                        return 1;
                    }
                }
                wl.words
            }
            None => self.arrvar(crate::params::ArrVar::Pparams).to_vec(),
        };
        if args.is_empty() {
            self.lastval = 0;
        }
        self.loops += 1;
        args.reverse();
        let mut last = false;
        while !last {
            let mut count = 0;
            for name in vars {
                let s = match args.pop() {
                    Some(s) => s,
                    None => {
                        if count != 0 {
                            last = true;
                            Vec::new()
                        } else {
                            break;
                        }
                    }
                };
                if self.isset(XTRACE) {
                    self.printprompt4();
                    let mut t = tok::unmetafy(name);
                    t.push(b'=');
                    t.extend(tok::unmetafy(&s));
                    t.push(b'\n');
                    write_fd(self.xtrerr_fd(), &t);
                }
                let _ = self.setsparam(name, s);
                count += 1;
            }
            if count == 0 {
                break;
            }
            self.execlist(body, true, do_exec && args.is_empty());
            if self.breaks != 0 {
                self.breaks -= 1;
                if self.breaks != 0 || self.contflag == 0 {
                    break;
                }
                self.contflag = 0;
            }
            if self.retflag {
                break;
            }
            if self.errflag() {
                if self.breaks != 0 {
                    self.breaks -= 1;
                }
                self.lastval = 1;
                break;
            }
        }
        self.loops -= 1;
        self.simple_pline = old_simple_pline;
        self.this_noerrexit = true;
        self.lastval
    }

    /// zsh's `execfor` for `for ((init; cond; step))`.
    fn execfor_cond(
        &mut self,
        init: &[u8],
        cond: &[u8],
        step: &[u8],
        body: &List,
        do_exec: bool,
    ) -> i32 {
        let old_simple_pline = self.simple_pline;
        self.simple_pline = true;
        let s = self.singsub(init);
        if self.isset(XTRACE) {
            let mut s2 = s.clone();
            tok::untokenize(&mut s2);
            self.printprompt4();
            let mut t = tok::unmetafy(&s2);
            t.push(b'\n');
            write_fd(self.xtrerr_fd(), &t);
        }
        if !self.errflag() {
            let _ = self.matheval(&s);
        }
        if self.errflag() {
            self.simple_pline = old_simple_pline;
            return 1;
        }
        self.loops += 1;
        loop {
            let c = if has_token(cond) {
                self.singsub(cond)
            } else {
                cond.to_vec()
            };
            let mut val = 1;
            if !self.errflag() {
                let start = c
                    .iter()
                    .position(|&b| !(b == b' ' || b == b'\t'))
                    .unwrap_or(c.len());
                let c = c.get(start..).unwrap_or(&[]).to_vec();
                if !c.is_empty() {
                    if self.isset(XTRACE) {
                        self.printprompt4();
                        let mut t = tok::unmetafy(&c);
                        t.push(b'\n');
                        write_fd(self.xtrerr_fd(), &t);
                    }
                    val = self.mathevali(&c);
                }
            }
            if self.errflag() {
                if self.breaks != 0 {
                    self.breaks -= 1;
                }
                self.lastval = 1;
                break;
            }
            if val == 0 {
                break;
            }
            self.execlist(body, true, do_exec);
            if self.breaks != 0 {
                self.breaks -= 1;
                if self.breaks != 0 || self.contflag == 0 {
                    break;
                }
                self.contflag = 0;
            }
            if self.retflag {
                break;
            }
            if !self.errflag() {
                let a = if has_token(step) {
                    self.singsub(step)
                } else {
                    step.to_vec()
                };
                if self.isset(XTRACE) {
                    self.printprompt4();
                    let mut t = tok::unmetafy(&a);
                    t.push(b'\n');
                    write_fd(self.xtrerr_fd(), &t);
                }
                if !self.errflag() {
                    let _ = self.matheval(&a);
                }
            }
            if self.errflag() {
                if self.breaks != 0 {
                    self.breaks -= 1;
                }
                self.lastval = 1;
                break;
            }
        }
        self.loops -= 1;
        self.simple_pline = old_simple_pline;
        self.this_noerrexit = true;
        self.lastval
    }

    /// zsh's `selectlist`.
    fn selectlist(&mut self, l: &[Vec<u8>], start: usize) -> usize {
        self.zleentry_trash();
        let mut longest = l.iter().map(Vec::len).max().unwrap_or(1).max(1);
        let ct = l.len();
        let mut t0 = ct;
        longest += 1;
        while t0 != 0 {
            t0 /= 10;
            longest += 1;
        }
        let cols = usize::try_from(self.zterm_columns.max(1)).unwrap_or(80);
        let lines = usize::try_from(self.zterm_lines.max(3)).unwrap_or(24);
        let mut fct = (cols - 1) / (longest + 3);
        let fw = (cols - 1).checked_div(fct).unwrap_or(0);
        if fct == 0 {
            fct = 1;
        }
        let colsz = ct.div_ceil(fct);
        let mut out = Vec::new();
        let mut t1 = start;
        while t1 != colsz && t1 - start < lines - 2 {
            let mut ap = t1;
            while let Some(item) = l.get(ap) {
                let t3 = ap + 1;
                let mut t2 = item.len() + 2 + t3.to_string().len();
                out.extend_from_slice(format!("{t3}) ").as_bytes());
                out.extend(tok::unmetafy(item));
                while t2 < fw {
                    out.push(b' ');
                    t2 += 1;
                }
                ap += colsz;
                if ap >= ct {
                    break;
                }
            }
            out.push(b'\n');
            t1 += 1;
        }
        write_fd(2, &out);
        if t1 < colsz { t1 } else { 0 }
    }

    /// zsh's `execselect`.
    fn execselect(&mut self, name: &[u8], words: Option<&[Vec<u8>]>, body: &List) -> i32 {
        let old_simple_pline = self.simple_pline;
        self.simple_pline = true;
        let args: Vec<Vec<u8>> = match words {
            None => self.arrvar(crate::params::ArrVar::Pparams).to_vec(),
            Some(w) => {
                let mut wl = WordList {
                    words: w.to_vec(),
                    flags: 0,
                };
                if w.iter().any(|x| has_token(x)) {
                    self.execsubst(&mut wl);
                    if self.errflag() {
                        self.simple_pline = old_simple_pline;
                        return 1;
                    }
                }
                wl.words
            }
        };
        if args.is_empty() {
            self.simple_pline = old_simple_pline;
            return 0;
        }
        self.loops += 1;
        let mut more = self.selectlist(&args, 0);
        'outer: loop {
            let s = loop {
                let prompt3 = self.getsparam(b"PS3").unwrap_or_else(|| b"?# ".to_vec());
                let (p, _) = self.promptexpand(&prompt3, false, None, None);
                write_fd(2, &tok::unmetafy(&p));
                let line = self.read_select_line();
                if line.is_none() && !self.errflag() {
                    let _ = self.setsparam(b"REPLY", Vec::new());
                }
                let Some(mut str_) = line.filter(|_| !self.errflag()) else {
                    if self.breaks != 0 {
                        self.breaks -= 1;
                    }
                    write_fd(2, b"\n");
                    break 'outer;
                };
                if let Some(p) = str_.iter().position(|&c| c == b'\n') {
                    str_.truncate(p);
                }
                if !str_.is_empty() {
                    break str_;
                }
                more = self.selectlist(&args, more);
            };
            let _ = self.setsparam(b"REPLY", tok::metafy(&s));
            let i = crate::utils::atoi(&s);
            let choice = if i == 0 {
                Vec::new()
            } else {
                usize::try_from(i - 1)
                    .ok()
                    .and_then(|k| args.get(k).cloned())
                    .unwrap_or_default()
            };
            let _ = self.setsparam(name, choice);
            self.execlist(body, true, false);
            if self.breaks != 0 {
                self.breaks -= 1;
                if self.breaks != 0 || self.contflag == 0 {
                    break;
                }
                self.contflag = 0;
            }
            if self.retflag || self.errflag() {
                break;
            }
        }
        self.loops -= 1;
        self.simple_pline = old_simple_pline;
        self.this_noerrexit = true;
        self.lastval
    }

    /// One line from the terminal or standard input for `select`.
    fn read_select_line(&mut self) -> Option<Vec<u8>> {
        let fd = if self.interact() && self.shtty != -1 && self.isset(USEZLE) {
            self.shtty
        } else {
            0
        };
        let mut line = Vec::new();
        let mut b = [0u8; 1];
        loop {
            // SAFETY: b is one writable byte.
            let n = unsafe { libc::read(fd, b.as_mut_ptr().cast(), 1) };
            if n < 0 && crate::signals::errno() == libc::EINTR {
                self.check_signals();
                if self.errflag() {
                    return None;
                }
                continue;
            }
            if n <= 0 {
                return if line.is_empty() { None } else { Some(line) };
            }
            line.push(b[0]);
            if b[0] == b'\n' {
                return Some(line);
            }
        }
    }

    /// zsh's `execwhile`.
    fn execwhile(&mut self, isuntil: bool, cond: &List, body: &List) -> i32 {
        let old_simple_pline = self.simple_pline;
        let olderrexit = self.noerrexit;
        let mut oldval = 0;
        self.loops += 1;
        if cond.items.is_empty() && body.items.is_empty() {
            self.simple_pline = true;
            while self.breaks == 0 {
                self.check_signals();
                if self.errflag.get() & ERRFLAG_INT != 0 {
                    self.breaks = self.loops;
                }
            }
            self.breaks -= 1;
            self.simple_pline = old_simple_pline;
        } else {
            loop {
                self.noerrexit = NOERREXIT_EXIT | NOERREXIT_RETURN;
                self.simple_pline = true;
                self.execlist(cond, true, false);
                self.simple_pline = old_simple_pline;
                self.noerrexit = olderrexit;
                if (self.lastval == 0) == isuntil {
                    if self.breaks != 0 {
                        self.breaks -= 1;
                    }
                    if !self.retflag {
                        self.lastval = oldval;
                    }
                    break;
                }
                if self.retflag {
                    if self.breaks != 0 {
                        self.breaks -= 1;
                    }
                    break;
                }
                self.simple_pline = true;
                self.execlist(body, true, false);
                self.simple_pline = old_simple_pline;
                if self.breaks != 0 {
                    self.breaks -= 1;
                    if self.breaks != 0 || self.contflag == 0 {
                        break;
                    }
                    self.contflag = 0;
                }
                if self.errflag() {
                    self.lastval = 1;
                    break;
                }
                if self.retflag {
                    break;
                }
                oldval = self.lastval;
            }
        }
        self.loops -= 1;
        self.this_noerrexit = true;
        self.lastval
    }

    /// zsh's `execrepeat`.
    fn execrepeat(&mut self, count_word: &[u8], body: &List) -> i32 {
        let old_simple_pline = self.simple_pline;
        self.simple_pline = true;
        let mut tmp = count_word.to_vec();
        if has_token(&tmp) {
            tmp = self.singsub(&tmp);
            tok::untokenize(&mut tmp);
        }
        let mut count = self.mathevali(&tmp);
        if self.errflag() {
            return 1;
        }
        self.lastval = 0;
        self.loops += 1;
        while count > 0 {
            count -= 1;
            self.execlist(body, true, false);
            if self.breaks != 0 {
                self.breaks -= 1;
                if self.breaks != 0 || self.contflag == 0 {
                    break;
                }
                self.contflag = 0;
            }
            if self.errflag() {
                self.lastval = 1;
                break;
            }
            if self.retflag {
                break;
            }
        }
        self.loops -= 1;
        self.simple_pline = old_simple_pline;
        self.this_noerrexit = true;
        self.lastval
    }

    /// zsh's `execif`.
    fn execif(
        &mut self,
        branches: &[(List, List)],
        otherwise: Option<&List>,
        do_exec: bool,
    ) -> i32 {
        let olderrexit = self.noerrexit;
        let mut s = false;
        let mut run = 0;
        let mut chosen: Option<&List> = None;
        self.noerrexit |= NOERREXIT_EXIT | NOERREXIT_RETURN;
        for (cond, body) in branches {
            self.execlist(cond, true, false);
            if self.lastval == 0 {
                run = 1;
                chosen = Some(body);
                break;
            }
            if self.retflag {
                break;
            }
            s = true;
        }
        if run == 0
            && !self.retflag
            && let Some(o) = otherwise
        {
            run = 2;
            chosen = Some(o);
        }
        let _ = s;
        if let Some(body) = chosen {
            if olderrexit != 0 || run == 2 {
                self.noerrexit = olderrexit;
            } else if self.lastval != 0 {
                self.noerrexit |= NOERREXIT_EXIT | NOERREXIT_RETURN | NOERREXIT_UNTIL_EXEC;
            } else {
                self.noerrexit &= !(NOERREXIT_EXIT | NOERREXIT_RETURN);
            }
            self.execlist(body, true, do_exec);
        } else {
            self.noerrexit = olderrexit;
            if !self.retflag && !self.errflag() {
                self.lastval = 0;
            }
        }
        self.this_noerrexit = true;
        self.lastval
    }

    /// zsh's `execcase`.
    fn execcase(&mut self, word: &[u8], arms: &[crate::ast::CaseArm], do_exec: bool) -> i32 {
        let mut w = self.singsub(word);
        tok::untokenize(&mut w);
        let mut anypatok = false;
        let mut idx = 0usize;
        while idx < arms.len() {
            let Some(arm) = arms.get(idx) else { break };
            let mut patok = false;
            if self.isset(XTRACE) {
                self.printprompt4();
                let mut t = b"case ".to_vec();
                t.extend(tok::unmetafy(&w));
                t.extend_from_slice(b" (");
                write_fd(self.xtrerr_fd(), &t);
            }
            for (ialt, pat) in arm.patterns.iter().enumerate() {
                if patok {
                    break;
                }
                self.queue_signals();
                let p = if has_token(pat) {
                    self.singsub(pat)
                } else {
                    pat.clone()
                };
                if self.isset(XTRACE) {
                    let mut t = Vec::new();
                    if ialt > 0 {
                        t.extend_from_slice(b" | ");
                    }
                    t.extend(self.quote_tokenized_output(&p));
                    write_fd(self.xtrerr_fd(), &t);
                }
                match self.patcompile(&p, crate::pattern::PAT_STATIC, None) {
                    None => {
                        let mut shown = p.clone();
                        tok::untokenize(&mut shown);
                        self.zerr(&format!("bad pattern: {}", crate::utils::lossy(&shown)));
                    }
                    Some(prog) => {
                        if self.pattry(&prog, &w) {
                            patok = true;
                            anypatok = true;
                        }
                    }
                }
                self.unqueue_signals();
            }
            if self.isset(XTRACE) {
                write_fd(self.xtrerr_fd(), b")\n");
            }
            if patok {
                self.execlist(&arm.body, true, arm.term == CaseTerm::Break && do_exec);
                let mut term = arm.term;
                while !self.retflag && term == CaseTerm::Fallthrough && idx + 1 < arms.len() {
                    idx += 1;
                    let Some(next) = arms.get(idx) else { break };
                    self.execlist(&next.body, true, next.term == CaseTerm::Break && do_exec);
                    term = next.term;
                }
                if term != CaseTerm::TestNext {
                    break;
                }
            }
            idx += 1;
        }
        if !anypatok {
            self.lastval = 0;
        }
        self.this_noerrexit = true;
        self.lastval
    }

    /// zsh's `exectry`.
    fn exectry(&mut self, body: &List, always: &List, do_exec: bool) -> i32 {
        self.try_tryflag += 1;
        self.execlist(body, true, false);
        self.try_tryflag -= 1;
        let endval = if self.lastval != 0 {
            self.lastval
        } else {
            self.errflag.get()
        };
        let save_try_errflag = self.try_errflag;
        let save_try_interrupt = self.try_interrupt;
        self.try_errflag = i64::from(self.errflag.get() & ERRFLAG_ERROR);
        self.try_interrupt = i64::from(self.errflag.get() & ERRFLAG_INT != 0);
        self.errflag.set(0);
        let save_retflag = self.retflag;
        self.retflag = false;
        let save_breaks = self.breaks;
        self.breaks = 0;
        let save_contflag = self.contflag;
        self.contflag = 0;
        self.execlist(always, true, do_exec);
        if self.try_errflag != 0 {
            self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
        } else {
            self.errflag.set(self.errflag.get() & !ERRFLAG_ERROR);
        }
        if self.try_interrupt != 0 {
            self.errflag.set(self.errflag.get() | ERRFLAG_INT);
        } else {
            self.errflag.set(self.errflag.get() & !ERRFLAG_INT);
        }
        self.try_errflag = save_try_errflag;
        self.try_interrupt = save_try_interrupt;
        if !self.retflag {
            self.retflag = save_retflag;
        }
        if self.breaks == 0 {
            self.breaks = save_breaks;
        }
        if self.contflag == 0 {
            self.contflag = save_contflag;
        }
        endval
    }

    /// zsh's `quote_tokenized_output`, into a buffer.
    pub(crate) fn quote_tokenized_output(&self, s: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while let Some(&c) = s.get(i) {
            match c {
                tok::META => {
                    i += 1;
                    out.push(s.get(i).copied().unwrap_or(0) ^ 32);
                    i += 1;
                    continue;
                }
                tok::NULARG => {
                    i += 1;
                    continue;
                }
                b'\\' | b'<' | b'>' | b'(' | b'|' | b')' | b'^' | b'#' | b'~' | b'[' | b']'
                | b'*' | b'?' | b'$' | b' ' => out.push(b'\\'),
                b'\t' => {
                    out.extend_from_slice(b"$'\\t'");
                    i += 1;
                    continue;
                }
                b'\n' => {
                    out.extend_from_slice(b"$'\\n'");
                    i += 1;
                    continue;
                }
                b'\r' => {
                    out.extend_from_slice(b"$'\\r'");
                    i += 1;
                    continue;
                }
                b'=' if i == 0 => out.push(b'\\'),
                _ if tok::is_tok(c) => {
                    out.push(tok::detok(c));
                    i += 1;
                    continue;
                }
                _ => {}
            }
            out.push(c);
            i += 1;
        }
        out
    }
}
