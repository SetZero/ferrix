//! The parser: zsh's `parse.c`, lists down to simple commands and
//! redirections. Compound commands are in [`crate::parsectl`].
//!
//! The lexer's context flags (`incmdpos` and the rest) are set here exactly
//! where zsh sets them, because they decide how the next token is read.

use std::cell::RefCell;
use std::rc::Rc;

use crate::ast::{AndOr, Assign, AssignValue, CmdKind, Command, List, ListItem, ListMode};
use crate::ast::{Pipeline, Redir, RedirKind, Sublist, Sublist2, Word};
use crate::lex::{HereDoc, LexEnv, Lexer, Tok};
use crate::tok::{self, INANG, INBRACE, INBRACK, INPAR, OUTANGPROC, OUTBRACE, OUTBRACK};

/// A parse error, with zsh's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseError {
    pub(crate) msg: String,
    pub(crate) lineno: u64,
}

/// Result of a parsing step.
pub(crate) type PResult<T> = Result<T, ParseError>;

/// The parser over a lexer, with the shell's aliases and options.
pub(crate) struct Parser<'a> {
    pub(crate) lx: &'a mut Lexer,
    pub(crate) env: &'a dyn LexEnv,
    /// Inside `time`: a nested `time` is an ordinary word.
    pub(crate) inpartime: bool,
}

impl std::fmt::Debug for Parser<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Parser").field("tok", &self.lx.tok).finish_non_exhaustive()
    }
}

impl<'a> Parser<'a> {
    /// A parser reading from `lx`.
    pub(crate) fn new(lx: &'a mut Lexer, env: &'a dyn LexEnv) -> Parser<'a> {
        Parser { lx, env, inpartime: false }
    }

    /// Read the next token.
    pub(crate) fn next(&mut self) {
        self.lx.ctxtlex(self.env);
    }

    pub(crate) fn tok(&self) -> Tok {
        self.lx.tok
    }

    /// The current token's text, tokens still in.
    pub(crate) fn tokstr(&self) -> Word {
        self.lx.tokstr.clone().unwrap_or_default()
    }

    /// zsh's `yyerror`: "parse error near `text'", or the lexer's own error.
    pub(crate) fn error<T>(&mut self) -> PResult<T> {
        let lineno = self.lx.toklineno;
        if let Some(msg) = self.lx.error.take() {
            self.lx.tok = Tok::Lexerr;
            return Err(ParseError { msg, lineno });
        }
        let text: Vec<u8> = match &self.lx.tokstr {
            Some(s) if self.lx.tok != Tok::Newlin => {
                let mut t = s.clone();
                tok::untokenize(&mut t);
                tok::unmetafy(&t)
            }
            _ => self.lx.tok.text().as_bytes().to_vec(),
        };
        let shown: Vec<u8> = text.iter().copied().take_while(|&c| c != b'\n').take(20).collect();
        self.lx.tok = Tok::Lexerr;
        let msg = if shown.is_empty() {
            "parse error".to_owned()
        } else if shown.len() == 20 {
            format!("parse error near `{}...'", String::from_utf8_lossy(&shown))
        } else {
            format!("parse error near `{}'", String::from_utf8_lossy(&shown))
        };
        Err(ParseError { msg, lineno })
    }

    /// An error with a message of its own (zsh's `COND_ERROR` and friends).
    pub(crate) fn error_msg<T>(&mut self, msg: String) -> PResult<T> {
        self.lx.tok = Tok::Lexerr;
        Err(ParseError { msg, lineno: self.lx.toklineno })
    }

    /// Skip separators.
    pub(crate) fn skip_seps(&mut self) {
        while self.tok() == Tok::Seper {
            self.next();
        }
    }

    /// Parse one event: the commands up to the end of the current input
    /// line (zsh's `parse_event`). `None` at the end of input.
    pub(crate) fn parse_event(&mut self) -> PResult<Option<List>> {
        self.lx.tok = Tok::Endinput;
        self.lx.incmdpos = true;
        self.next();
        let mut list = List::default();
        loop {
            while self.tok() == Tok::Seper {
                if self.lx.isnewlin > 0 {
                    return Ok(Some(list));
                }
                self.next();
            }
            if matches!(self.tok(), Tok::Endinput) {
                return Ok((!list.items.is_empty()).then_some(list));
            }
            let Some(sublist) = self.par_sublist()? else { return self.error() };
            let mode = match self.tok() {
                Tok::Endinput => ListMode::Sync,
                Tok::Seper => ListMode::Sync,
                Tok::Amper => ListMode::Async,
                Tok::Amperbang => ListMode::Disown,
                _ => return self.error(),
            };
            list.items.push(ListItem { sublist, mode });
            match self.tok() {
                Tok::Endinput => return Ok(Some(list)),
                Tok::Seper if self.lx.isnewlin > 0 => return Ok(Some(list)),
                _ => self.next(),
            }
        }
    }

    /// Parse all remaining input as one list (zsh's `parse_list`, used by
    /// `eval` and `-c`).
    pub(crate) fn parse_all(&mut self) -> PResult<List> {
        self.lx.tok = Tok::Endinput;
        self.lx.incmdpos = true;
        self.next();
        let list = self.par_list()?;
        if self.tok() != Tok::Endinput {
            return self.error();
        }
        Ok(list)
    }

    /// `list : { SEPER } [ sublist [ { SEPER | AMPER | AMPERBANG } list ] ]`
    pub(crate) fn par_list(&mut self) -> PResult<List> {
        let mut list = List::default();
        loop {
            self.skip_seps();
            let Some(sublist) = self.par_sublist()? else { break };
            let mode = match self.tok() {
                Tok::Seper => ListMode::Sync,
                Tok::Amper => ListMode::Async,
                Tok::Amperbang => ListMode::Disown,
                _ => {
                    list.items.push(ListItem { sublist, mode: ListMode::Sync });
                    break;
                }
            };
            list.items.push(ListItem { sublist, mode });
            self.lx.incmdpos = true;
            self.next();
            self.skip_seps();
        }
        Ok(list)
    }

    /// One sublist as a list (zsh's `par_list1`, the body of short forms).
    pub(crate) fn par_list1(&mut self) -> PResult<List> {
        let mut list = List::default();
        if let Some(sublist) = self.par_sublist()? {
            list.items.push(ListItem { sublist, mode: ListMode::Sync });
        }
        Ok(list)
    }

    /// `sublist : sublist2 [ ( DBAR | DAMPER ) { SEPER } sublist ]`
    fn par_sublist(&mut self) -> PResult<Option<Sublist>> {
        let Some(first) = self.par_sublist2()? else { return Ok(None) };
        let mut rest = Vec::new();
        while matches!(self.tok(), Tok::Dbar | Tok::Damper) {
            let op = if self.tok() == Tok::Dbar { AndOr::Or } else { AndOr::And };
            self.next();
            self.skip_seps();
            match self.par_sublist2()? {
                Some(s) => rest.push((op, s)),
                None => break,
            }
        }
        Ok(Some(Sublist { first, rest }))
    }

    /// `sublist2 : [ COPROC | BANG ] pline`
    pub(crate) fn par_sublist2(&mut self) -> PResult<Option<Sublist2>> {
        let (mut not, mut coproc) = (false, false);
        if self.tok() == Tok::Coproc {
            coproc = true;
            self.next();
        } else if self.tok() == Tok::Bang {
            not = true;
            self.next();
        }
        let pipeline = self.par_pline()?;
        if pipeline.is_none() && !not && !coproc {
            return Ok(None);
        }
        Ok(Some(Sublist2 { not, coproc, pipeline }))
    }

    /// `pline : cmd [ ( BAR | BARAMP ) { SEPER } pline ]`
    fn par_pline(&mut self) -> PResult<Option<Pipeline>> {
        let lineno = self.lx.toklineno;
        let mut cmds = Vec::new();
        loop {
            let Some(mut cmd) = self.par_cmd(false)? else {
                if cmds.is_empty() {
                    return Ok(None);
                }
                return self.error();
            };
            match self.tok() {
                Tok::Bar => {
                    cmds.push(cmd);
                    self.next();
                    self.skip_seps();
                }
                Tok::Baramp => {
                    cmd.redirs.push(Redir {
                        kind: RedirKind::MergeOut,
                        fd: 2,
                        target: b"1".to_vec(),
                        varid: None,
                        heredoc: None,
                    });
                    cmds.push(cmd);
                    self.next();
                    self.skip_seps();
                }
                _ => {
                    cmds.push(cmd);
                    return Ok(Some(Pipeline { cmds, lineno }));
                }
            }
        }
    }

    /// `cmd : { redir } ( compound | simple ) { redir }`
    pub(crate) fn par_cmd(&mut self, zsh_construct: bool) -> PResult<Option<Command>> {
        let lineno = self.lx.toklineno;
        let mut redirs = Vec::new();
        while self.tok().is_redir() {
            self.par_redir(&mut redirs, None)?;
        }
        let kind = match self.tok() {
            Tok::For | Tok::Foreach | Tok::Select => self.par_for()?,
            Tok::Case => self.par_case()?,
            Tok::If => self.par_if()?,
            Tok::While | Tok::Until => self.par_while()?,
            Tok::Repeat => self.par_repeat()?,
            Tok::Inpar | Tok::Inbrace => self.par_subsh(zsh_construct)?,
            Tok::Func => self.par_funcdef(&mut redirs)?,
            Tok::Dinbrack => self.par_dinbrack()?,
            Tok::Dinpar => {
                let expr = self.tokstr();
                self.next();
                CmdKind::Arith(expr)
            }
            Tok::Time if !self.inpartime => {
                self.inpartime = true;
                let r = self.par_time();
                self.inpartime = false;
                r?
            }
            t => {
                if t == Tok::Time {
                    self.lx.tok = Tok::String;
                    self.lx.tokstr = Some(b"time".to_vec());
                }
                match self.par_simple(&mut redirs)? {
                    Some(k) => k,
                    None if redirs.is_empty() => return Ok(None),
                    None => CmdKind::Simple { assigns: Vec::new(), words: Vec::new() },
                }
            }
        };
        while self.tok().is_redir() {
            self.par_redir(&mut redirs, None)?;
        }
        self.lx.incmdpos = true;
        self.lx.incasepat = 0;
        self.lx.incond = 0;
        self.lx.intypeset = false;
        Ok(Some(Command { kind, redirs, lineno }))
    }

    /// `simple : { COMMAND | EXEC | NOGLOB | NOCORRECT | DASH }
    ///           { STRING | ENVSTRING | ENVARRAY wordlist OUTPAR | redir }
    ///           [ INOUTPAR { SEPER } ( list1 | INBRACE list OUTBRACE ) ]`
    #[expect(clippy::too_many_lines, reason = "follows par_simple's single loop")]
    fn par_simple(&mut self, redirs: &mut Vec<Redir>) -> PResult<Option<CmdKind>> {
        let mut assigns = Vec::new();
        loop {
            match self.tok() {
                Tok::Nocorrect => {}
                Tok::Envstring => assigns.push(split_envstring(&self.tokstr())),
                Tok::Envarray => {
                    let old = self.lx.incmdpos;
                    self.lx.incmdpos = false;
                    let (name, append) = strip_plus(self.tokstr());
                    self.next();
                    let words = self.par_nl_wordlist();
                    if self.tok() != Tok::Outpar {
                        return self.error();
                    }
                    self.lx.incmdpos = old;
                    assigns.push(Assign { name, value: AssignValue::Array(words), append });
                }
                t if t.is_redir() => {
                    self.par_redir(redirs, None)?;
                    continue;
                }
                _ => break,
            }
            self.next();
        }
        if matches!(self.tok(), Tok::Amper | Tok::Amperbang) && !assigns.is_empty() {
            return self.error();
        }
        let mut words: Vec<Word> = Vec::new();
        let mut args: Vec<Assign> = Vec::new();
        let mut is_typeset = false;
        let mut postassigns = false;
        loop {
            match self.tok() {
                Tok::String | Tok::Typeset => {
                    self.lx.incmdpos = false;
                    if self.tok() == Tok::Typeset {
                        self.lx.intypeset = true;
                        is_typeset = true;
                    }
                    let word = self.tokstr();
                    if !self.lx.opts.ignorebraces && is_varid_word(&word) {
                        let id: Word = word.get(1..word.len() - 1).unwrap_or(&[]).to_vec();
                        self.next();
                        if self.tok().is_redir() && self.lx.tokfd == -1 {
                            self.par_redir(redirs, Some(id))?;
                        } else if postassigns {
                            args.push(Assign { name: word, value: AssignValue::None, append: false });
                        } else {
                            words.push(word);
                        }
                        continue;
                    }
                    if postassigns {
                        args.push(Assign { name: word, value: AssignValue::None, append: false });
                    } else {
                        words.push(word);
                    }
                    self.next();
                }
                t if t.is_redir() => self.par_redir(redirs, None)?,
                Tok::Envstring => {
                    postassigns = true;
                    let mut a = split_envstring(&self.tokstr());
                    a.append = false;
                    args.push(a);
                    self.next();
                }
                Tok::Envarray => {
                    postassigns = true;
                    let name = self.tokstr();
                    self.lx.intypeset = false;
                    self.next();
                    let list = self.par_nl_wordlist();
                    self.lx.intypeset = true;
                    if self.tok() != Tok::Outpar {
                        return self.error();
                    }
                    args.push(Assign { name, value: AssignValue::Array(list), append: false });
                    self.next();
                }
                Tok::Inoutpar => return self.par_simple_funcdef(words, &assigns, postassigns, redirs),
                _ => break,
            }
        }
        if assigns.is_empty() && words.is_empty() && args.is_empty() && redirs.is_empty() {
            return Ok(None);
        }
        self.lx.incmdpos = true;
        self.lx.intypeset = false;
        Ok(Some(if is_typeset {
            CmdKind::Typeset { assigns, words, args }
        } else {
            CmdKind::Simple { assigns, words }
        }))
    }

    /// `name () body`, `name1 name2 () body` and `() { body } args`.
    fn par_simple_funcdef(
        &mut self,
        names: Vec<Word>,
        assigns: &[Assign],
        postassigns: bool,
        redirs: &mut Vec<Redir>,
    ) -> PResult<Option<CmdKind>> {
        if !self.lx.opts.multifuncdef && names.len() > 1 {
            return self.error();
        }
        if !assigns.is_empty() || postassigns {
            return self.error();
        }
        self.lx.incmdpos = true;
        self.next();
        self.skip_seps();
        let anonymous = names.is_empty();
        let body = if self.tok() == Tok::Inbrace {
            self.next();
            let list = self.par_list()?;
            if self.tok() != Tok::Outbrace {
                return self.error();
            }
            if anonymous {
                self.lx.incmdpos = false;
            }
            self.next();
            list
        } else {
            let Some(cmd) = self.par_cmd(anonymous)? else { return self.error() };
            if anonymous {
                self.lx.incmdpos = false;
            }
            single_command_list(cmd)
        };
        let mut args = Vec::new();
        if anonymous {
            loop {
                if self.tok() == Tok::String {
                    args.push(self.tokstr());
                    self.next();
                } else if self.tok().is_redir() {
                    self.par_redir(redirs, None)?;
                } else {
                    break;
                }
            }
        }
        Ok(Some(CmdKind::FuncDef { names, body: Rc::new(body), tracing: false, args }))
    }

    /// `redir : ( OUTANG | ... | TRINANG ) STRING`
    pub(crate) fn par_redir(&mut self, redirs: &mut Vec<Redir>, varid: Option<Word>) -> PResult<()> {
        let oldcmdpos = self.lx.incmdpos;
        self.lx.incmdpos = false;
        let mut kind = match self.tok() {
            Tok::Outang => RedirKind::Write,
            Tok::Outangbang => RedirKind::WriteNow,
            Tok::Doutang => RedirKind::App,
            Tok::Doutangbang => RedirKind::AppNow,
            Tok::Inang => RedirKind::Read,
            Tok::Inoutang => RedirKind::ReadWrite,
            Tok::Dinang => RedirKind::HereDoc,
            Tok::Dinangdash => RedirKind::HereDocDash,
            Tok::Inangamp => RedirKind::MergeIn,
            Tok::Outangamp => RedirKind::MergeOut,
            Tok::Ampoutang => RedirKind::ErrWrite,
            Tok::Outangampbang => RedirKind::ErrWriteNow,
            Tok::Doutangamp => RedirKind::ErrApp,
            Tok::Doutangampbang => RedirKind::ErrAppNow,
            _ => RedirKind::HereStr,
        };
        let fd1 = self.lx.tokfd;
        self.lx.tokfd = -1;
        self.next();
        if !matches!(self.tok(), Tok::String | Tok::Envstring) {
            return self.error();
        }
        self.lx.incmdpos = oldcmdpos;
        let fd = if fd1 == -1 { i32::from(!kind.reads()) } else { fd1 };
        let target = self.tokstr();
        if matches!(kind, RedirKind::HereDoc | RedirKind::HereDocDash) {
            let slot = Rc::new(RefCell::new(HereDoc {
                term: target.clone(),
                strip_tabs: kind == RedirKind::HereDocDash,
                ..HereDoc::default()
            }));
            self.lx.add_heredoc(Rc::clone(&slot));
            redirs.push(Redir { kind, fd, target, varid, heredoc: Some(slot) });
            self.next();
            return Ok(());
        }
        let procsub = |t: &Word, first: u8| t.first() == Some(&first) && t.get(1) == Some(&INPAR);
        match kind {
            RedirKind::Write | RedirKind::WriteNow => {
                if procsub(&target, OUTANGPROC) {
                    kind = RedirKind::OutPipe;
                } else if procsub(&target, INANG) {
                    return self.error();
                }
            }
            RedirKind::Read => {
                if procsub(&target, INANG) {
                    kind = RedirKind::InPipe;
                } else if procsub(&target, OUTANGPROC) {
                    return self.error();
                }
            }
            RedirKind::ReadWrite => {
                if procsub(&target, INANG) {
                    kind = RedirKind::InPipe;
                } else if procsub(&target, OUTANGPROC) {
                    kind = RedirKind::OutPipe;
                }
            }
            _ => {}
        }
        self.next();
        redirs.push(Redir { kind, fd, target, varid, heredoc: None });
        Ok(())
    }

    /// `wordlist : { STRING }`
    pub(crate) fn par_wordlist(&mut self) -> Vec<Word> {
        let mut words = Vec::new();
        while self.tok() == Tok::String {
            words.push(self.tokstr());
            self.next();
        }
        words
    }

    /// `nl_wordlist : { STRING | SEPER }`
    pub(crate) fn par_nl_wordlist(&mut self) -> Vec<Word> {
        let mut words = Vec::new();
        while matches!(self.tok(), Tok::String | Tok::Seper) {
            if self.tok() == Tok::String {
                words.push(self.tokstr());
            }
            self.next();
        }
        words
    }
}

/// A list holding one synchronous command.
pub(crate) fn single_command_list(cmd: Command) -> List {
    let lineno = cmd.lineno;
    List {
        items: vec![ListItem {
            sublist: Sublist {
                first: Sublist2 {
                    not: false,
                    coproc: false,
                    pipeline: Some(Pipeline { cmds: vec![cmd], lineno }),
                },
                rest: Vec::new(),
            },
            mode: ListMode::Sync,
        }],
    }
}

/// `{name}`, the word before a redirection that names its descriptor.
fn is_varid_word(w: &[u8]) -> bool {
    w.len() > 2
        && w.first() == Some(&INBRACE)
        && w.last() == Some(&OUTBRACE)
        && w.get(1..w.len() - 1)
            .is_some_and(|id| id.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'_'))
}

/// Split an `ENVSTRING` token into name and value.
fn split_envstring(s: &[u8]) -> Assign {
    let mut i = 0;
    while let Some(&c) = s.get(i) {
        if c == INBRACK || c == b'=' || c == b'+' {
            break;
        }
        i += 1;
    }
    if s.get(i) == Some(&INBRACK) {
        let mut depth = 0;
        while let Some(&c) = s.get(i) {
            i += 1;
            if c == INBRACK {
                depth += 1;
            } else if c == OUTBRACK {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
        }
    }
    let name = s.get(..i).unwrap_or(&[]).to_vec();
    let mut append = false;
    if s.get(i) == Some(&b'+') {
        append = true;
        i += 1;
    }
    if s.get(i) == Some(&b'=') {
        i += 1;
    }
    let value = s.get(i..).unwrap_or(&[]).to_vec();
    Assign { name, value: AssignValue::Scalar(value), append }
}

/// `name+` of an `ENVARRAY` token: the name and whether it appends.
fn strip_plus(mut name: Word) -> (Word, bool) {
    if name.len() > 1 && name.last() == Some(&b'+') {
        let _plus = name.pop();
        (name, true)
    } else {
        (name, false)
    }
}
