//! Command execution, part two (zsh's `exec.c`): lists, sublists and
//! pipelines — `execlist`, `execsimple`, `execpline`, `execpline2` — and
//! running a string or a parsed program.

use std::rc::Rc;

use crate::ast::{
    AndOr, AssignValue, CmdKind, Command, List, ListMode, Pipeline, Sublist, Sublist2,
};
use crate::exec::*;
use crate::jobs::*;
use crate::options::*;
use crate::shell::{ERRFLAG_ERROR, ERRFLAG_INT, Shell};
use crate::signals::{
    NOERREXIT_EXIT, NOERREXIT_RETURN, NOERREXIT_UNTIL_EXEC, child_block, child_unblock, errno,
};
use crate::signames::{SIGDEBUG, SIGEXIT, SIGZERR};
use crate::tables::Eprog;
use crate::tok::{EQUALS, INANG, INPAR, OUTANGPROC};

/// Whether a list is "complex" in zsh's parser's sense: it may need a job,
/// a fork or redirection, so it cannot run through `execsimple`.
pub(crate) fn list_complex(l: &List) -> bool {
    l.items
        .iter()
        .any(|i| i.mode != ListMode::Sync || sublist_complex(&i.sublist))
}

fn sublist_complex(s: &Sublist) -> bool {
    sublist2_complex(&s.first) || s.rest.iter().any(|(_, p)| sublist2_complex(p))
}

/// The complexity `par_sublist2` records for one part of a sublist.
pub(crate) fn sublist2_complex(s: &Sublist2) -> bool {
    if s.not || s.coproc {
        return true;
    }
    match &s.pipeline {
        None => false,
        Some(p) => p.cmds.len() > 1 || p.cmds.first().is_some_and(cmd_complex),
    }
}

fn has_procsubst(w: &[u8]) -> bool {
    w.windows(2)
        .any(|p| matches!(p, [c, INPAR] if *c == EQUALS || *c == INANG || *c == OUTANGPROC))
}

/// The complexity `par_cmd` records for a command.
pub(crate) fn cmd_complex(c: &Command) -> bool {
    if !c.redirs.is_empty() {
        return true;
    }
    match &c.kind {
        CmdKind::Simple { assigns, words } => {
            !words.is_empty()
                || assigns.iter().any(|a| match &a.value {
                    AssignValue::Array(_) => true,
                    AssignValue::Scalar(v) => has_procsubst(v),
                    AssignValue::None => false,
                })
        }
        CmdKind::Typeset { .. } | CmdKind::Subsh(_) | CmdKind::Select { .. } | CmdKind::Time(_) => {
            true
        }
        CmdKind::Cursh(l) => list_complex(l),
        CmdKind::Try { body, always } => list_complex(body) || list_complex(always),
        CmdKind::For { body, .. }
        | CmdKind::ForArith { body, .. }
        | CmdKind::Repeat { body, .. } => list_complex(body),
        CmdKind::Case { arms, .. } => arms.iter().any(|a| list_complex(&a.body)),
        CmdKind::If {
            branches,
            otherwise,
        } => {
            branches
                .iter()
                .any(|(c, b)| list_complex(c) || list_complex(b))
                || otherwise.as_ref().is_some_and(list_complex)
        }
        CmdKind::While { cond, body, .. } => list_complex(cond) || list_complex(body),
        CmdKind::FuncDef { names, args, .. } => names.is_empty() && !args.is_empty(),
        CmdKind::Cond(_) | CmdKind::Arith(_) => false,
    }
}

/// The word code type of a command, for the comparisons zsh makes.
pub(crate) fn wc_type(c: &Command) -> i32 {
    match &c.kind {
        CmdKind::Simple { .. } => WC_SIMPLE,
        CmdKind::Typeset { .. } => WC_TYPESET,
        CmdKind::Subsh(_) => WC_SUBSH,
        CmdKind::Cursh(_) => WC_CURSH,
        CmdKind::Time(_) => WC_TIMED,
        CmdKind::FuncDef { .. } => WC_FUNCDEF,
        CmdKind::For { .. } | CmdKind::ForArith { .. } => WC_FOR,
        CmdKind::Select { .. } => WC_SELECT,
        CmdKind::While { .. } => WC_WHILE,
        CmdKind::Repeat { .. } => WC_REPEAT,
        CmdKind::Case { .. } => WC_CASE,
        CmdKind::If { .. } => WC_IF,
        CmdKind::Cond(_) => WC_COND,
        CmdKind::Arith(_) => WC_ARITH,
        CmdKind::Try { .. } => WC_TRY,
    }
}

impl Shell {
    /// zsh's `parse_string`.
    pub(crate) fn parse_string(&mut self, s: &[u8], reset_lineno: bool) -> Option<Eprog> {
        let mut lx = crate::lex::Lexer::new(s.to_vec(), self.lex_opts());
        if !reset_lineno {
            lx.input.lineno = u64::try_from(self.lineno.max(1)).unwrap_or(1);
        }
        let result = {
            let mut p = crate::parse::Parser::new(&mut lx, &*self);
            p.parse_all()
        };
        match result {
            Ok(list) => Some(Eprog::new(list)),
            Err(e) => {
                if self.lastval == 0 {
                    self.lastval = 1;
                }
                if self.noerrs < 2 {
                    self.lineno = i64::try_from(e.lineno).unwrap_or(0);
                    self.zerr(&e.msg);
                }
                None
            }
        }
    }

    /// zsh's `execstring`.
    pub(crate) fn execstring(
        &mut self,
        s: &[u8],
        dont_change_job: bool,
        exiting: bool,
        context: &str,
    ) {
        if self.isset(VERBOSE) {
            let mut v = crate::tok::unmetafy(s);
            v.push(b'\n');
            crate::shell::write_fd(2, &v);
        }
        if let Some(prog) = self.parse_string(s, false) {
            self.execode(&prog, dont_change_job, exiting, context);
        }
    }

    /// `execstring` for a glob qualifier or sort key: true on success.
    pub(crate) fn execstring_ctx(&mut self, s: &[u8], context: &str) -> bool {
        self.execstring(s, true, false, context);
        self.lastval == 0 && !self.errflag()
    }

    /// zsh's `execode`.
    pub(crate) fn execode(
        &mut self,
        p: &Eprog,
        dont_change_job: bool,
        exiting: bool,
        context: &str,
    ) {
        self.zsh_eval_context.push(context.as_bytes().to_vec());
        let list = Rc::clone(&p.list);
        self.execlist(&list, dont_change_job, exiting);
        let _ = self.zsh_eval_context.pop();
    }

    /// zsh's `execsimple` on a command that needs no job.
    fn execsimple(&mut self, cmd: &Command, lineno: u64) -> i32 {
        if self.errflag() {
            self.lastval = 1;
            return 1;
        }
        if !self.isset(EXECOPT) {
            self.lastval = 0;
            return 0;
        }
        if self.intrap == 0 && self.ineval == 0 && lineno != 0 {
            self.lineno = i64::try_from(lineno).unwrap_or(0);
        }
        let otj = self.thisjob;
        self.thisjob = -1;
        let lv = match &cmd.kind {
            CmdKind::Simple { assigns, words } if words.is_empty() => {
                self.cmdoutval = 0;
                self.addvars(assigns, 0);
                self.setunderscore(b"");
                if self.isset(XTRACE) {
                    crate::shell::write_fd(self.xtrerr_fd(), b"\n");
                }
                if self.errflag() {
                    self.errflag.get()
                } else {
                    self.cmdoutval
                }
            }
            _ => {
                let q = self.queue_signal_level();
                self.dont_queue_signals();
                let lv = if self.errflag() {
                    self.errflag.get()
                } else if matches!(cmd.kind, CmdKind::FuncDef { .. }) {
                    self.execfuncdef(cmd, None)
                } else {
                    self.execconstruct(cmd, false)
                };
                self.restore_queue_signals(q);
                lv
            }
        };
        self.thisjob = otj;
        self.lastval = lv;
        lv
    }

    /// The first line number of a sublist, for `lineno`.
    fn sublist_lineno(s: &Sublist) -> u64 {
        s.first.pipeline.as_ref().map_or(0, |p| p.lineno)
    }

    /// zsh's `execlist`.
    #[expect(clippy::too_many_lines, reason = "zsh's execlist")]
    pub(crate) fn execlist(&mut self, list: &List, dont_change_job: bool, exiting: bool) {
        let oldnoerrexit = self.noerrexit;
        let mut exiting = exiting;
        self.queue_signals();
        let cj = self.thisjob;
        let old_pline_level = self.pline_level;
        let old_list_pipe = self.list_pipe;
        let old_list_pipe_job = self.list_pipe_job;
        let old_list_pipe_text = self.list_pipe_text.clone();
        let oldlineno = self.lineno;
        if self.sourcelevel != 0 && self.unset_opt(SHINSTDIN) {
            self.pline_level = 0;
            self.list_pipe = false;
            self.list_pipe_job = 0;
            self.list_pipe_text.clear();
        }
        if list.items.is_empty() {
            self.lastval = 0;
        }
        let n = list.items.len();
        for (idx, item) in list.items.iter().enumerate() {
            if self.breaks != 0 || self.retflag || self.errflag() {
                break;
            }
            self.check_signals();
            let is_end = idx + 1 == n;
            let mut this_donetrap = false;
            self.this_noerrexit = false;
            let sublist = &item.sublist;
            let simple =
                item.mode == ListMode::Sync && sublist.rest.is_empty() && !sublist_complex(sublist);
            if self.intrap == 0 && self.ineval == 0 {
                let l = Self::sublist_lineno(sublist);
                if l != 0 {
                    self.lineno = i64::try_from(l).unwrap_or(0);
                }
            }
            let mut donedebug = 0;
            if self.sigtrapped.get(SIGDEBUG).copied().unwrap_or(0) != 0
                && self.isset(DEBUGBEFORECMD)
                && self.intrap == 0
            {
                let oerrexit_opt = self.opts[ERREXIT];
                self.opts[ERREXIT] = false;
                self.noerrexit = NOERREXIT_EXIT | NOERREXIT_RETURN;
                let text = self.getpermtext_sublist(sublist);
                let _ = self.assignsparam(b"ZSH_DEBUG_CMD", text, 0);
                exiting = self.donetrap;
                let ret = self.lastval;
                self.dotrap(SIGDEBUG);
                if !self.retflag {
                    self.lastval = ret;
                }
                self.donetrap = exiting;
                self.noerrexit = oldnoerrexit;
                donedebug = if self.isset(ERREXIT) { 2 } else { 1 };
                self.opts[ERREXIT] = oerrexit_opt;
                self.unsetparam(b"ZSH_DEBUG_CMD");
            } else if self.intrap != 0 {
                donedebug = 1;
            }
            self.donetrap = false;
            let ltype = match item.mode {
                ListMode::Sync => Z_SYNC,
                ListMode::Async => Z_ASYNC,
                ListMode::Disown => Z_ASYNC | Z_DISOWN,
            };
            if simple {
                if donedebug != 2
                    && let Some(cmd) = sublist.first.pipeline.as_ref().and_then(|p| p.cmds.first())
                {
                    let l = Self::sublist_lineno(sublist);
                    let _ = self.execsimple(cmd, l);
                }
            } else if donedebug == 2 {
                self.donetrap = true;
            } else {
                // Loop through the parts joined by && and ||.
                let parts: Vec<(Option<AndOr>, &Sublist2)> =
                    std::iter::once((None, &sublist.first))
                        .chain(sublist.rest.iter().map(|(a, p)| (Some(*a), p)))
                        .collect();
                let mut k = 0usize;
                while k < parts.len() {
                    let Some((_, part)) = parts.get(k) else { break };
                    let next_conn = parts.get(k + 1).and_then(|(c, _)| *c);
                    let isend = next_conn.is_none();
                    if oldnoerrexit == 0 {
                        self.noerrexit = if isend {
                            0
                        } else {
                            NOERREXIT_EXIT | NOERREXIT_RETURN
                        };
                    }
                    if part.not {
                        if isend {
                            self.this_noerrexit = true;
                        }
                        self.noerrexit = NOERREXIT_EXIT | NOERREXIT_RETURN;
                    }
                    let part_simple = !sublist2_complex(part);
                    match next_conn {
                        None => {
                            if part_simple {
                                if let Some(cmd) =
                                    part.pipeline.as_ref().and_then(|p| p.cmds.first())
                                {
                                    let l = part.pipeline.as_ref().map_or(0, |p| p.lineno);
                                    let _ = self.execsimple(cmd, l);
                                } else {
                                    self.lastval = i32::from(part.not);
                                }
                            } else {
                                let last1 = ltype & Z_SYNC != 0 && is_end && exiting;
                                let _ = self.execpline(part, ltype, last1);
                            }
                            break;
                        }
                        Some(conn) => {
                            let ret = if part_simple {
                                match part.pipeline.as_ref().and_then(|p| p.cmds.first()) {
                                    Some(cmd) => {
                                        let l = part.pipeline.as_ref().map_or(0, |p| p.lineno);
                                        self.execsimple(cmd, l)
                                    }
                                    None => {
                                        self.lastval = i32::from(part.not);
                                        self.lastval
                                    }
                                }
                            } else {
                                self.execpline(part, Z_SYNC, false)
                            };
                            let skip = match conn {
                                AndOr::And => ret != 0,
                                AndOr::Or => ret == 0,
                            };
                            if skip {
                                // Skip parts joined by the same connector.
                                let mut j = k + 1;
                                while let Some((Some(c), _)) = parts.get(j + 1) {
                                    if *c != conn {
                                        break;
                                    }
                                    j += 1;
                                }
                                if j + 1 >= parts.len() {
                                    this_donetrap = true;
                                    break;
                                }
                                k = j + 1;
                                continue;
                            }
                        }
                    }
                    k += 1;
                }
            }
            if oldnoerrexit & NOERREXIT_UNTIL_EXEC == 0 {
                self.noerrexit = oldnoerrexit;
            }
            if self.sigtrapped.get(SIGDEBUG).copied().unwrap_or(0) != 0
                && !self.isset(DEBUGBEFORECMD)
                && donedebug == 0
            {
                let oerrexit_opt = self.opts[ERREXIT];
                self.opts[ERREXIT] = false;
                self.noerrexit = NOERREXIT_EXIT | NOERREXIT_RETURN;
                exiting = self.donetrap;
                let ret = self.lastval;
                self.dotrap(SIGDEBUG);
                if !self.retflag {
                    self.lastval = ret;
                }
                self.donetrap = exiting;
                self.noerrexit = oldnoerrexit;
                self.opts[ERREXIT] = oerrexit_opt;
            }
            if !self.this_noerrexit && !self.donetrap && !this_donetrap {
                if self.sigtrapped.get(SIGZERR).copied().unwrap_or(0) != 0
                    && self.lastval != 0
                    && self.noerrexit & NOERREXIT_EXIT == 0
                {
                    self.dotrap(SIGZERR);
                    self.donetrap = true;
                }
                if self.lastval != 0 {
                    let errreturn = self.isset(ERRRETURN)
                        && (self.isset(INTERACTIVE)
                            || self.locallevel != 0
                            || self.sourcelevel != 0)
                        && self.noerrexit & NOERREXIT_RETURN == 0;
                    let errexit = (self.isset(ERREXIT) || (self.isset(ERRRETURN) && !errreturn))
                        && self.noerrexit & NOERREXIT_EXIT == 0;
                    if errexit {
                        if self.sigtrapped.get(SIGEXIT).copied().unwrap_or(0) != 0 {
                            self.dotrap(SIGEXIT);
                        }
                        // SAFETY: getpid has no preconditions.
                        if self.mypid != unsafe { libc::getpid() } {
                            self._realexit();
                        } else {
                            self.realexit();
                        }
                    }
                    if errreturn {
                        self.retflag = true;
                        self.breaks = self.loops;
                    }
                }
            }
            if ltype & Z_SYNC != 0 && is_end {
                break;
            }
        }
        self.pline_level = old_pline_level;
        self.list_pipe = old_list_pipe;
        self.list_pipe_job = old_list_pipe_job;
        self.list_pipe_text = old_list_pipe_text;
        self.lineno = oldlineno;
        if dont_change_job {
            self.thisjob = cj;
        }
        if exiting && self.sigtrapped.get(SIGEXIT).copied().unwrap_or(0) != 0 {
            self.dotrap(SIGEXIT);
            if let Some(t) = self.sigtrapped.get_mut(SIGEXIT) {
                *t = 0;
            }
        }
        self.unqueue_signals();
    }

    /// zsh's `execpline`.
    #[expect(clippy::too_many_lines, reason = "zsh's execpline")]
    pub(crate) fn execpline(&mut self, sl: &Sublist2, how: i32, last1: bool) -> i32 {
        let mut how = how;
        let mut last1 = last1;
        let Some(pipeline) = &sl.pipeline else {
            self.lastval = i32::from(sl.not);
            return self.lastval;
        };
        if sl.not {
            last1 = false;
        }
        let old_simple_pline = self.simple_pline;
        self.queue_signals();
        let pj = self.thisjob;
        let mut ipipe = [0i32; 2];
        let mut opipe = [0i32; 2];
        child_block();
        let newjob = self.initjob();
        self.thisjob = newjob;
        if newjob == -1 {
            child_unblock();
            self.unqueue_signals();
            return 1;
        }
        if how & Z_TIMED != 0
            && let Some(j) = self.job_mut(newjob)
        {
            j.stat |= STAT_TIMED;
        }
        let mut coproc = sl.coproc;
        if coproc {
            how = Z_ASYNC;
            if self.coprocin >= 0 {
                let (ci, co) = (self.coprocin, self.coprocout);
                let _ = self.zclose(ci);
                let _ = self.zclose(co);
            }
            if self.mpipe(&mut ipipe) < 0 {
                self.coprocin = -1;
                self.coprocout = -1;
                coproc = false;
            } else if self.mpipe(&mut opipe) < 0 {
                let _ = crate::sysutil::close_fd(ipipe[0]);
                let _ = crate::sysutil::close_fd(ipipe[1]);
                self.coprocin = -1;
                self.coprocout = -1;
                coproc = false;
            } else {
                self.coprocin = ipipe[0];
                self.coprocout = opipe[1];
                self.fdtable_mark(ipipe[0], FDT_UNUSED);
                self.fdtable_mark(opipe[1], FDT_UNUSED);
            }
        }
        if self.pline_level == 0 {
            self.list_pipe_pid = 0;
            self.nowait = false;
            self.simple_pline = pipeline.cmds.len() == 1;
            self.list_pipe_job = newjob;
        }
        self.pline_level += 1;
        self.lastwj = 0;
        self.lpforked = 0;
        self.execpline2(pipeline, 0, how, opipe[0], ipipe[1], last1);
        self.pline_level -= 1;
        if how & Z_ASYNC != 0 {
            self.clearoldjobtab();
            self.lastwj = newjob;
            if self.thisjob == self.list_pipe_job {
                self.list_pipe_job = 0;
            }
            let tj = self.thisjob;
            if let Some(j) = self.job_mut(tj) {
                j.stat |= STAT_NOSTTY;
            }
            if coproc {
                let _ = self.zclose(ipipe[1]);
                let _ = self.zclose(opipe[0]);
            }
            if how & Z_DISOWN != 0 {
                self.pipecleanfilelist_job(tj, false);
                if let Ok(t) = usize::try_from(tj) {
                    self.deletejob(t, true);
                }
                self.thisjob = -1;
            } else {
                self.spawnjob();
            }
            child_unblock();
            self.unqueue_signals();
            self.lastval = 0;
            return 0;
        }
        if newjob != self.lastwj {
            let jn = usize::try_from(newjob).unwrap_or(0);
            if newjob == self.list_pipe_job && self.list_pipe_child {
                crate::shell::exit_now(0);
            }
            self.lastwj = newjob;
            self.thisjob = newjob;
            let jstat = self.jobtab.get(jn).map_or(0, |j| j.stat);
            if (self.list_pipe
                || (self.pline_level != 0 && how & Z_TIMED == 0 && jstat & STAT_NOSTTY == 0))
                && let Some(j) = self.jobtab.get_mut(jn)
            {
                j.stat |= STAT_NOPRINT;
            }
            if self.nowait {
                if self.pline_level == 0 {
                    self.curjob = newjob;
                    let text = self.list_pipe_text.clone();
                    let lpp = self.list_pipe_pid;
                    let start = self.list_pipe_start;
                    self.addproc(lpp, Some(&text), false, start, -1, -1);
                    let single = self.jobtab.get(jn).is_some_and(|j| j.procs.len() <= 1);
                    if single || self.lpforked == 2 {
                        if let Some(j) = self.jobtab.get_mut(jn) {
                            j.gleader = lpp;
                            j.stat |= STAT_SUBLEADER;
                        }
                        for jobsub in 1..=self.maxjob {
                            if self
                                .jobtab
                                .get(jobsub)
                                .is_some_and(|s| s.stat & STAT_SUBJOB_ORPHANED != 0)
                            {
                                if let Some(j) = self.jobtab.get_mut(jn) {
                                    j.other = i32::try_from(jobsub).unwrap_or(0);
                                    j.stat |= STAT_SUPERJOB;
                                }
                                if let Some(s) = self.jobtab.get_mut(jobsub) {
                                    s.stat &= !STAT_SUBJOB_ORPHANED;
                                    s.other = lpp;
                                }
                            }
                        }
                    }
                    let other = self.jobtab.get(jn).map_or(0, |j| j.other);
                    let stopped = self
                        .job(other)
                        .and_then(|o| o.procs.iter().find(|p| libc::WIFSTOPPED(p.status)))
                        .map(|p| p.status);
                    if let Some(st) = stopped
                        && let Some(last) = self.jobtab.get_mut(jn).and_then(|j| j.procs.last_mut())
                    {
                        last.status = st;
                    }
                    if let Some(j) = self.jobtab.get_mut(jn) {
                        j.stat &= !(STAT_DONE | STAT_NOPRINT);
                        j.stat |= STAT_STOPPED | STAT_CHANGED | STAT_LOCKED | STAT_INUSE;
                    }
                    let lng = i32::from(self.isset(LONGLISTJOBS));
                    let _ = self.printjob(jn, lng, 1);
                } else if newjob != self.list_pipe_job {
                    self.deletejob(jn, false);
                } else {
                    self.lastwj = -1;
                }
            }
            self.errbrk_saved = false;
            while !self.nowait {
                if self.list_pipe_child {
                    if let Some(j) = self.jobtab.get_mut(jn) {
                        j.stat |= STAT_NOPRINT;
                    }
                    self.makerunning(jn);
                }
                let locked = self
                    .jobtab
                    .get(jn)
                    .is_some_and(|j| j.stat & STAT_LOCKED != 0);
                let updated = if locked {
                    false
                } else {
                    let u = self.hasprocs(self.thisjob);
                    self.waitjobs();
                    child_block();
                    u
                };
                let lpj = self.list_pipe_job;
                if !updated
                    && lpj != 0
                    && self.hasprocs(lpj)
                    && !self.job(lpj).is_some_and(|j| j.stat & STAT_STOPPED != 0)
                {
                    let q = self.queue_signal_level();
                    child_unblock();
                    child_block();
                    self.dont_queue_signals();
                    self.restore_queue_signals(q);
                }
                let jstat = self.jobtab.get(jn).map_or(0, |j| j.stat);
                if self.list_pipe_child && jstat & STAT_DONE != 0 && self.lastval2 & 0o200 != 0 {
                    // SAFETY: plain killpg call.
                    unsafe {
                        libc::killpg(self.mypgrp, self.lastval2 & !0o200);
                    }
                }
                let lpj_stopped = self.job(lpj).is_some_and(|j| j.stat & STAT_STOPPED != 0);
                if !self.list_pipe_child
                    && self.lpforked == 0
                    && !self.subsh
                    && self.jobbing()
                    && (self.list_pipe || last1 || self.pline_level != 0)
                    && (jstat & STAT_STOPPED != 0
                        || (lpj != 0 && self.pline_level != 0 && lpj_stopped))
                {
                    if self.suspend_pipeline_rhs(jn, newjob, last1) {
                        break;
                    }
                } else if self.subsh && jstat & STAT_STOPPED != 0 {
                    self.thisjob = newjob;
                } else {
                    break;
                }
            }
            child_unblock();
            self.unqueue_signals();
            let jstat = self.jobtab.get(jn).map_or(0, |j| j.stat);
            let mut jcur = jn;
            if self.list_pipe
                && self.lastval & 0o200 != 0
                && pj >= 0
                && (jstat & STAT_INUSE == 0 || jstat & STAT_DONE != 0)
            {
                self.deletejob(jn, false);
                jcur = usize::try_from(pj).unwrap_or(0);
                if self.jobtab.get(jcur).is_some_and(|j| j.gleader != 0) {
                    let sig = self.lastval & !0o200;
                    let _ = self.killjb(jcur, sig);
                }
            }
            let cstat = self.jobtab.get(jcur).map_or(0, |j| j.stat);
            if self.list_pipe_child
                || (cstat & STAT_DONE != 0
                    && (self.list_pipe || (self.pline_level != 0 && cstat & STAT_SUBJOB == 0)))
            {
                self.deletejob(jcur, false);
            }
            self.thisjob = pj;
        } else {
            self.unqueue_signals();
        }
        if sl.not && !self.errflag() {
            self.lastval = i32::from(self.lastval == 0);
        }
        if self.pline_level == 0 {
            self.simple_pline = old_simple_pline;
        }
        self.lastval
    }

    /// The part of `execpline` that forks to let a stopped pipeline's
    /// shell-side right hand continue. True to leave the wait loop.
    fn suspend_pipeline_rhs(&mut self, jn: usize, newjob: i32, last1: bool) -> bool {
        let mut synch = [0i32; 2];
        let mut bgtime = (0i64, 0i64);
        // SAFETY: synch is a valid two-element array.
        let piped = unsafe { libc::pipe(synch.as_mut_ptr()) } >= 0;
        let pid = if piped {
            self.zfork(Some(&mut bgtime))
        } else {
            0
        };
        if !piped || pid == -1 {
            if pid < 0 {
                let _ = crate::sysutil::close_fd(synch[0]);
                let _ = crate::sysutil::close_fd(synch[1]);
            } else {
                self.zerr(&format!("pipe failed: {}", crate::sysutil::errmsg(errno())));
            }
            self.zleentry_trash();
            crate::shell::write_fd(2, b"zsh: job can't be suspended\n");
            self.makerunning(jn);
            let _ = self.killjb(jn, libc::SIGCONT);
            self.thisjob = newjob;
            return false;
        }
        if pid != 0 {
            let lpj = self.list_pipe_job;
            let lpj_gl = self.job(lpj).map_or(0, |j| j.gleader);
            self.lpforked = if crate::signals::killpg(lpj_gl, 0) == -1 {
                2
            } else {
                1
            };
            self.list_pipe_pid = pid;
            self.list_pipe_start = bgtime;
            self.nowait = true;
            self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
            self.breaks = self.loops;
            let mut dummy = [0u8; 1];
            // SAFETY: closing our end and reading one byte into dummy.
            unsafe {
                libc::close(synch[1]);
            }
            let _ = self.read_loop(synch[0], &mut dummy);
            // SAFETY: closing the read end.
            unsafe {
                libc::close(synch[0]);
            }
            if self.jobtab.get(jn).is_some_and(|j| j.stat & STAT_DONE == 0) {
                if let Some(l) = self.job_mut(lpj) {
                    l.other = newjob;
                    l.stat |= STAT_SUPERJOB;
                }
                let has = self.hasprocs(lpj);
                if let Some(j) = self.jobtab.get_mut(jn) {
                    j.stat |= STAT_SUBJOB | STAT_NOPRINT;
                    j.other = pid;
                    if has {
                        j.gleader = lpj_gl;
                    }
                }
            }
            if (self.list_pipe || last1) && self.hasprocs(lpj) {
                // SAFETY: plain killpg call.
                unsafe {
                    libc::killpg(lpj_gl, libc::SIGSTOP);
                }
            }
            return true;
        }
        // SAFETY: closing the read end in the child.
        unsafe {
            libc::close(synch[0]);
        }
        let _ = self.entersubsh(ESUB_ASYNC);
        self.mypgrp = crate::sysutil::getpid();
        let _ = crate::sysutil::setpgid(0, self.mypgrp);
        let _ = crate::sysutil::close_fd(synch[1]);
        let _ = crate::signals::kill(crate::sysutil::getpid(), libc::SIGSTOP);
        self.list_pipe = false;
        self.list_pipe_child = true;
        self.opts[INTERACTIVE] = false;
        if self.errbrk_saved {
            self.errflag
                .set(self.prev_errflag | (self.errflag.get() & ERRFLAG_INT));
            self.breaks = self.prev_breaks;
        }
        true
    }

    /// zsh's `execpline2`: run `pipeline.cmds[from..]`.
    fn execpline2(
        &mut self,
        pipeline: &Pipeline,
        from: usize,
        how: i32,
        input: i32,
        output: i32,
        last1: bool,
    ) {
        if self.breaks != 0 || self.retflag {
            return;
        }
        if self.intrap == 0 && self.ineval == 0 && pipeline.lineno != 0 {
            self.lineno = i64::try_from(pipeline.lineno).unwrap_or(0);
        }
        let Some(cmd) = pipeline.cmds.get(from) else {
            return;
        };
        if self.pline_level == 1 {
            if how & Z_ASYNC != 0 || self.sfcontext == 0 {
                self.list_pipe_text =
                    self.getjobtext_cmds(pipeline.cmds.get(from..).unwrap_or(&[]));
            } else {
                self.list_pipe_text.clear();
            }
        }
        if from + 1 == pipeline.cmds.len() {
            self.execcmd_exec(cmd, input, output, how, if last1 { 1 } else { 2 }, -1);
        } else {
            let mut pipes = [0i32; 2];
            let old_list_pipe = self.list_pipe;
            let _ = self.mpipe(&mut pipes);
            self.addfilelist(None, pipes[0]);
            self.execcmd_exec(cmd, input, pipes[1], how, 0, pipes[0]);
            let _ = self.zclose(pipes[1]);
            self.list_pipe = true;
            self.execpline2(pipeline, from + 1, how, pipes[0], output, last1);
            self.list_pipe = old_list_pipe;
        }
    }

    /// zsh's `mpipe`: a pipe with both ends above 9.
    pub(crate) fn mpipe(&mut self, pp: &mut [i32; 2]) -> i32 {
        // SAFETY: pp is a valid two-element array.
        if unsafe { libc::pipe(pp.as_mut_ptr()) } < 0 {
            self.zerr(&format!("pipe failed: {}", crate::sysutil::errmsg(errno())));
            return -1;
        }
        pp[0] = self.movefd(pp[0]);
        pp[1] = self.movefd(pp[1]);
        0
    }

    /// Set a descriptor's `fdtable` entry.
    pub(crate) fn fdtable_mark(&mut self, fd: i32, v: u8) {
        if let Ok(f) = usize::try_from(fd)
            && let Some(slot) = self.fdtable.get_mut(f)
        {
            *slot = v;
        }
    }
}
