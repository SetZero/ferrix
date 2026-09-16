//! Turning parsed code back into text (zsh's `text.c`): what `functions`
//! and `whence -c` print, `$functions`, and the text `jobs` shows.
//!
//! zsh walks its word code with an explicit stack; this walks the tree,
//! emitting the same text in the same order.

use crate::ast::{
    AndOr, Assign, AssignValue, CaseTerm, CmdKind, Command, Cond, CondOp, List, ListMode, Pipeline,
    Redir, Sublist, Sublist2,
};
use crate::exec::*;
use crate::exec_redir::*;
use crate::jobs::JOBTEXTSIZE;
use crate::shell::Shell;
use crate::tok;
use crate::utils::{Qt, has_token};

/// The text being built (zsh's `tbuf`, `tindent`, `tnewlins`, `tjob`).
struct Text<'a> {
    sh: &'a Shell,
    buf: Vec<u8>,
    indent: i32,
    newlins: bool,
    job: bool,
    pending: Option<Vec<u8>>,
    expand_tabs: i32,
}

impl Text<'_> {
    fn full(&self) -> bool {
        self.job && self.buf.len() >= JOBTEXTSIZE - 1
    }

    fn addchr(&mut self, c: u8) {
        if !self.full() {
            self.buf.push(c);
        }
    }

    fn addstr(&mut self, s: &[u8]) {
        for &c in s {
            if self.full() {
                return;
            }
            self.buf
                .push(if !self.newlins && c == b'\n' { b' ' } else { c });
        }
    }

    fn addlist(&mut self, words: &[Vec<u8>]) {
        for (i, w) in words.iter().enumerate() {
            if i > 0 {
                self.addchr(b' ');
            }
            self.addstr(w);
        }
    }

    fn dec_indent(&mut self) {
        if self.indent > 0 {
            self.indent -= 1;
        }
    }

    fn dopending(&mut self) {
        if let Some(p) = self.pending.take() {
            self.addchr(b'\n');
            self.addstr(&p);
        }
    }

    fn addpending(&mut self, a: &[u8], b: &[u8]) {
        let mut s = a.to_vec();
        s.extend_from_slice(b);
        match &mut self.pending {
            Some(p) => {
                p.push(b'\n');
                p.extend(s);
            }
            None => self.pending = Some(s),
        }
    }

    fn nl(&mut self, no_semicolon: bool) {
        if self.newlins {
            self.dopending();
            self.addchr(b'\n');
            for _ in 0..self.indent {
                if self.expand_tabs > 0 {
                    for _ in 0..self.expand_tabs {
                        self.addchr(b' ');
                    }
                } else if self.expand_tabs == 0 {
                    self.addchr(b'\t');
                }
            }
        } else if no_semicolon {
            self.addstr(b" ");
        } else {
            self.addstr(b"; ");
        }
    }

    fn assign(&mut self, a: &Assign, typeset: bool) {
        self.addstr(&a.name);
        if matches!(a.value, AssignValue::None) && typeset {
            self.addchr(b' ');
            return;
        }
        if a.append {
            self.addchr(b'+');
        }
        self.addchr(b'=');
        match &a.value {
            AssignValue::Array(ws) => {
                self.addchr(b'(');
                self.addlist(ws);
                self.addstr(b") ");
            }
            AssignValue::Scalar(v) => {
                self.addstr(v);
                self.addchr(b' ');
            }
            AssignValue::None => self.addchr(b' '),
        }
    }

    fn list(&mut self, l: &List) {
        let n = l.items.len();
        for (i, item) in l.items.iter().enumerate() {
            self.sublist(&item.sublist);
            if item.mode != ListMode::Sync {
                self.addstr(b" &");
                if item.mode == ListMode::Disown {
                    self.addstr(b"|");
                }
            }
            if i + 1 < n {
                if self.newlins {
                    self.nl(false);
                } else {
                    self.addstr(if item.mode == ListMode::Sync {
                        b"; "
                    } else {
                        b" "
                    });
                }
            }
        }
    }

    fn sublist2_prefix(&mut self, s: &Sublist2) {
        let bare = s.pipeline.is_none();
        if s.not {
            self.addstr(if bare { b"!" } else { b"! " });
        }
        if s.coproc {
            self.addstr(if bare { b"coproc" } else { b"coproc " });
        }
    }

    fn sublist(&mut self, s: &Sublist) {
        self.sublist2_prefix(&s.first);
        if let Some(p) = &s.first.pipeline {
            self.pipeline(p);
        }
        for (conn, part) in &s.rest {
            self.addstr(if *conn == AndOr::Or { b" || " } else { b" && " });
            self.sublist2_prefix(part);
            if let Some(p) = &part.pipeline {
                self.pipeline(p);
            }
        }
    }

    fn pipeline(&mut self, p: &Pipeline) {
        for (i, c) in p.cmds.iter().enumerate() {
            if i > 0 {
                self.addstr(b" | ");
            }
            self.command(c);
        }
    }

    fn command(&mut self, c: &Command) {
        self.kind(&c.kind);
        if !c.redirs.is_empty() {
            self.redirs(&c.redirs);
        }
    }

    #[expect(clippy::too_many_lines, reason = "zsh's gettext2")]
    fn kind(&mut self, k: &CmdKind) {
        match k {
            CmdKind::Simple { assigns, words } => {
                for a in assigns {
                    self.assign(a, false);
                }
                self.addlist(words);
            }
            CmdKind::Typeset {
                assigns,
                words,
                args,
            } => {
                for a in assigns {
                    self.assign(a, false);
                }
                self.addlist(words);
                if !args.is_empty() {
                    self.addchr(b' ');
                }
                for a in args {
                    self.assign(a, true);
                }
            }
            CmdKind::Subsh(l) => {
                self.addstr(b"(");
                self.indent += 1;
                self.nl(true);
                self.list(l);
                self.dec_indent();
                self.nl(false);
                self.addstr(b")");
            }
            CmdKind::Cursh(l) => {
                self.addstr(b"{");
                self.indent += 1;
                self.nl(true);
                self.list(l);
                self.dec_indent();
                self.nl(false);
                self.addstr(b"}");
            }
            CmdKind::Time(p) => {
                self.addstr(b"time");
                if let Some(p) = p {
                    self.addchr(b' ');
                    self.indent += 1;
                    self.sublist2_prefix(p);
                    if let Some(pl) = &p.pipeline {
                        self.pipeline(pl);
                    }
                    self.dec_indent();
                }
            }
            CmdKind::FuncDef {
                names, body, args, ..
            } => {
                self.addlist(names);
                if !names.is_empty() {
                    self.addstr(b" ");
                }
                if self.job {
                    self.addstr(b"() { ... }");
                    return;
                }
                self.addstr(b"() {");
                self.indent += 1;
                self.nl(true);
                self.list(body);
                self.dec_indent();
                self.nl(false);
                self.addstr(b"}");
                if names.is_empty() && !args.is_empty() {
                    self.addstr(b" ");
                    self.addlist(args);
                }
            }
            CmdKind::For { vars, words, body } => {
                self.addstr(b"for ");
                self.addlist(vars);
                if let Some(w) = words {
                    self.addstr(b" in ");
                    self.addlist(w);
                }
                self.nl(false);
                self.addstr(b"do");
                self.indent += 1;
                self.nl(false);
                self.list(body);
                self.dec_indent();
                self.nl(false);
                self.addstr(b"done");
            }
            CmdKind::ForArith {
                init,
                cond,
                step,
                body,
            } => {
                self.addstr(b"for ((");
                self.addstr(init);
                self.addstr(b"; ");
                self.addstr(cond);
                self.addstr(b"; ");
                self.addstr(step);
                self.addstr(b")) do");
                self.indent += 1;
                self.nl(false);
                self.list(body);
                self.dec_indent();
                self.nl(false);
                self.addstr(b"done");
            }
            CmdKind::Select { var, words, body } => {
                self.addstr(b"select ");
                self.addstr(var);
                if let Some(w) = words {
                    self.addstr(b" in ");
                    self.addlist(w);
                }
                self.nl(false);
                self.addstr(b"do");
                self.nl(false);
                self.indent += 1;
                self.list(body);
                self.dec_indent();
                self.nl(false);
                self.addstr(b"done");
            }
            CmdKind::While { until, cond, body } => {
                self.addstr(if *until { b"until " } else { b"while " });
                self.indent += 1;
                self.list(cond);
                self.dec_indent();
                self.nl(false);
                self.addstr(b"do");
                self.indent += 1;
                self.nl(false);
                self.list(body);
                self.dec_indent();
                self.nl(false);
                self.addstr(b"done");
            }
            CmdKind::Repeat { count, body } => {
                self.addstr(b"repeat ");
                self.addstr(count);
                self.nl(false);
                self.addstr(b"do");
                self.indent += 1;
                self.nl(false);
                self.list(body);
                self.dec_indent();
                self.nl(false);
                self.addstr(b"done");
            }
            CmdKind::Case { word, arms } => {
                self.addstr(b"case ");
                self.addstr(word);
                self.addstr(b" in");
                if arms.is_empty() {
                    if self.newlins {
                        self.nl(false);
                    } else {
                        self.addchr(b' ');
                    }
                    self.addstr(b"esac");
                    return;
                }
                self.indent += 1;
                for arm in arms {
                    if self.newlins {
                        self.nl(false);
                    } else {
                        self.addchr(b' ');
                    }
                    self.addstr(b"(");
                    for (i, p) in arm.patterns.iter().enumerate() {
                        if i > 0 {
                            self.addstr(b" | ");
                        }
                        self.addstr(p);
                    }
                    self.addstr(b") ");
                    self.indent += 1;
                    self.list(&arm.body);
                    self.dec_indent();
                    self.addstr(match arm.term {
                        CaseTerm::Break => b" ;;",
                        CaseTerm::Fallthrough => b" ;&",
                        CaseTerm::TestNext => b" ;|",
                    });
                }
                self.dec_indent();
                if self.newlins {
                    self.nl(false);
                } else {
                    self.addchr(b' ');
                }
                self.addstr(b"esac");
            }
            CmdKind::If {
                branches,
                otherwise,
            } => {
                for (i, (cond, body)) in branches.iter().enumerate() {
                    if i == 0 {
                        self.addstr(b"if ");
                    } else {
                        self.dec_indent();
                        self.nl(false);
                        self.addstr(b"elif ");
                    }
                    self.indent += 1;
                    self.list(cond);
                    self.dec_indent();
                    self.nl(false);
                    self.addstr(b"then");
                    self.indent += 1;
                    self.nl(false);
                    self.list(body);
                }
                if let Some(o) = otherwise {
                    self.dec_indent();
                    self.nl(false);
                    self.addstr(b"else");
                    self.indent += 1;
                    self.nl(false);
                    self.list(o);
                }
                self.dec_indent();
                self.nl(false);
                self.addstr(b"fi");
            }
            CmdKind::Cond(c) => {
                self.addstr(b"[[ ");
                self.cond(c, false);
                self.addstr(b" ]]");
            }
            CmdKind::Arith(e) => {
                self.addstr(b"((");
                self.addstr(e);
                self.addstr(b"))");
            }
            CmdKind::Try { body, always } => {
                self.addstr(b"{");
                self.indent += 1;
                self.nl(false);
                self.list(body);
                self.dec_indent();
                self.nl(false);
                self.addstr(b"} always {");
                self.indent += 1;
                self.nl(false);
                self.list(always);
                self.dec_indent();
                self.nl(false);
                self.addstr(b"}");
            }
        }
    }

    fn cond(&mut self, c: &Cond, _nested: bool) {
        match c {
            Cond::Not(inner) => {
                self.addstr(b"! ");
                let paren = matches!(**inner, Cond::Not(_) | Cond::And(..) | Cond::Or(..));
                if paren {
                    self.addstr(b"( ");
                }
                self.cond(inner, true);
                if paren {
                    self.addstr(b" )");
                }
            }
            Cond::And(a, b) => {
                self.cond_side(a, true);
                self.addstr(b" && ");
                self.cond_side(b, true);
            }
            Cond::Or(a, b) => {
                self.cond_side(a, false);
                self.addstr(b" || ");
                self.cond_side(b, false);
            }
            Cond::Unary(op, w) => {
                self.addstr(&[b'-', *op, b' ']);
                self.addstr(w);
            }
            Cond::Binary(op, l, r) => {
                self.addstr(l);
                self.addstr(b" ");
                self.addstr(cond_op_text(*op));
                self.addstr(b" ");
                self.addstr(r);
            }
            Cond::Module { name, args, infix } => {
                if *infix {
                    self.addstr(args.first().map_or(&[][..], Vec::as_slice));
                    self.addchr(b' ');
                    self.addstr(name);
                    self.addchr(b' ');
                    self.addstr(args.get(1).map_or(&[][..], Vec::as_slice));
                } else {
                    self.addstr(name);
                    self.addchr(b' ');
                    self.addlist(args);
                }
            }
        }
    }

    /// One side of `&&` or `||`, parenthesised when it is the other one.
    fn cond_side(&mut self, c: &Cond, in_and: bool) {
        let paren = if in_and {
            matches!(c, Cond::Or(..))
        } else {
            matches!(c, Cond::And(..))
        };
        if paren {
            self.addstr(b"( ");
        }
        self.cond(c, true);
        if paren {
            self.addstr(b" )");
        }
    }

    fn redirs(&mut self, rs: &[Redir]) {
        const FSTR: [&[u8]; 18] = [
            b">", b">|", b">>", b">>|", b"&>", b"&>|", b"&>>", b"&>>|", b"<>", b"<", b"<<", b"<<-",
            b"<<<", b"<&", b">&", b"", b"<", b">",
        ];
        self.addchr(b' ');
        let xs = xredirs(rs);
        for (x, r) in xs.iter().zip(rs.iter()) {
            if x.typ == REDIR_CLOSE {
                continue;
            }
            if let Some(v) = &x.varid {
                self.addchr(b'{');
                self.addstr(v);
                self.addchr(b'}');
            } else {
                let readfd = matches!(
                    x.typ,
                    REDIR_READWRITE
                        | REDIR_READ
                        | REDIR_HEREDOC
                        | REDIR_HEREDOCDASH
                        | REDIR_HERESTR
                        | REDIR_MERGEIN
                        | REDIR_INPIPE
                );
                if x.fd1 != i32::from(!readfd) {
                    self.addchr(b'0' + u8::try_from(x.fd1).unwrap_or(0));
                }
            }
            if x.typ == REDIR_HERESTR && x.flags & REDIRF_FROM_HEREDOC != 0 {
                let term = r
                    .heredoc
                    .as_ref()
                    .map(|h| h.borrow().term.clone())
                    .unwrap_or_default();
                if self.newlins {
                    self.addstr(FSTR.get(10).copied().unwrap_or(b""));
                    self.addstr(&term);
                    let munged = tok::remove_nulls(&term);
                    let name = x.name.clone();
                    self.addpending(&name, &munged);
                } else {
                    self.addstr(FSTR.get(12).copied().unwrap_or(b""));
                    let mut name = x.name.clone();
                    if name.last() == Some(&b'\n') {
                        let _ = name.pop();
                    }
                    let (q, qt) = if has_token(&name) {
                        (b'"', Qt::Double)
                    } else {
                        (b'\'', Qt::Single)
                    };
                    self.addchr(q);
                    let quoted = self.sh.quotestring(&name, qt);
                    self.addstr(&quoted);
                    self.addchr(q);
                }
            } else {
                self.addstr(
                    FSTR.get(usize::try_from(x.typ).unwrap_or(0))
                        .copied()
                        .unwrap_or(b""),
                );
                if x.typ != REDIR_MERGEIN && x.typ != REDIR_MERGEOUT {
                    self.addchr(b' ');
                }
                self.addstr(&x.name);
            }
            self.addchr(b' ');
        }
        let _ = self.buf.pop();
    }
}

fn cond_op_text(op: CondOp) -> &'static [u8] {
    match op {
        CondOp::StrEq => b"=",
        CondOp::StrDeq => b"==",
        CondOp::StrNeq => b"!=",
        CondOp::StrLt => b"<",
        CondOp::StrGt => b">",
        CondOp::Nt => b"-nt",
        CondOp::Ot => b"-ot",
        CondOp::Ef => b"-ef",
        CondOp::Eq => b"-eq",
        CondOp::Ne => b"-ne",
        CondOp::Lt => b"-lt",
        CondOp::Gt => b"-gt",
        CondOp::Le => b"-le",
        CondOp::Ge => b"-ge",
        CondOp::Regex => b"=~",
    }
}

impl Shell {
    fn text(&self, newlins: bool, job: bool, indent: i32) -> Text<'_> {
        Text {
            sh: self,
            buf: Vec::new(),
            indent,
            newlins,
            job,
            pending: None,
            expand_tabs: self.text_expand_tabs,
        }
    }

    /// zsh's `getpermtext` for a whole program.
    pub(crate) fn getpermtext(&self, l: &List, start_indent: bool) -> Vec<u8> {
        let mut t = self.text(true, false, i32::from(start_indent));
        t.list(l);
        t.dopending();
        let mut b = t.buf;
        tok::untokenize(&mut b);
        b
    }

    /// `getpermtext` from a sublist, for `$ZSH_DEBUG_CMD`.
    pub(crate) fn getpermtext_sublist(&self, s: &Sublist) -> Vec<u8> {
        let mut t = self.text(true, false, 0);
        t.sublist(s);
        t.dopending();
        let mut b = t.buf;
        tok::untokenize(&mut b);
        b
    }

    /// `getpermtext` of a function's stored redirections.
    pub(crate) fn getredirtext(&self, rs: &[Redir]) -> Vec<u8> {
        let mut t = self.text(true, false, 1);
        t.redirs(rs);
        t.dopending();
        let mut b = t.buf;
        tok::untokenize(&mut b);
        b
    }

    /// zsh's `getjobtext` for one command.
    pub(crate) fn getjobtext_cmd(&self, c: &Command) -> Vec<u8> {
        let mut t = self.text(false, true, 0);
        t.command(c);
        let mut b = t.buf;
        if b.last() == Some(&tok::META) {
            let _ = b.pop();
        }
        tok::untokenize(&mut b);
        b
    }

    /// zsh's `getjobtext` from a command to the end of its pipeline.
    pub(crate) fn getjobtext_cmds(&self, cmds: &[Command]) -> Vec<u8> {
        let mut t = self.text(false, true, 0);
        for (i, c) in cmds.iter().enumerate() {
            if i > 0 {
                t.addstr(b" | ");
            }
            t.command(c);
        }
        let mut b = t.buf;
        if b.last() == Some(&tok::META) {
            let _ = b.pop();
        }
        tok::untokenize(&mut b);
        b
    }
}

/// A job's text for a command running `WC_AUTOFN`.
pub(crate) const AUTOFN_TEXT: &[u8] = b"builtin autoload -X";

/// Keep `FS_FUNC` referenced from here for the text of function frames.
pub(crate) const _TEXT_FS: i32 = FS_FUNC;

impl Shell {
    /// zsh's `getjobtext` for a whole program, as `preexec` receives it.
    pub(crate) fn getjobtext_list(&self, l: &List) -> Vec<u8> {
        let mut t = self.text(false, true, 0);
        t.list(l);
        let mut b = t.buf;
        if b.last() == Some(&tok::META) {
            let _ = b.pop();
        }
        tok::untokenize(&mut b);
        b
    }
}
