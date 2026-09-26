//! Job control (zsh's `jobs.c`): the job table, waiting for processes,
//! reporting their status, and the job builtins.

use crate::options::*;
use crate::shell::{ERRFLAG_ERROR, ERRFLAG_INT, Shell, write_fd};
use crate::signals::{ZSIG_TRAPPED, child_block, child_unblock, errno};
use crate::signames::*;
use crate::utils::lossy;

pub(crate) const STAT_CHANGED: i32 = 0x0001;
pub(crate) const STAT_STOPPED: i32 = 0x0002;
pub(crate) const STAT_TIMED: i32 = 0x0004;
pub(crate) const STAT_DONE: i32 = 0x0008;
pub(crate) const STAT_LOCKED: i32 = 0x0010;
pub(crate) const STAT_NOPRINT: i32 = 0x0020;
pub(crate) const STAT_INUSE: i32 = 0x0040;
pub(crate) const STAT_SUPERJOB: i32 = 0x0080;
pub(crate) const STAT_SUBJOB: i32 = 0x0100;
pub(crate) const STAT_WASSUPER: i32 = 0x0200;
pub(crate) const STAT_CURSH: i32 = 0x0400;
pub(crate) const STAT_NOSTTY: i32 = 0x0800;
pub(crate) const STAT_ATTACH: i32 = 0x1000;
pub(crate) const STAT_SUBLEADER: i32 = 0x2000;
pub(crate) const STAT_BUILTIN: i32 = 0x4000;
pub(crate) const STAT_SUBJOB_ORPHANED: i32 = 0x8000;
pub(crate) const STAT_DISOWN: i32 = 0x10000;

/// `SP_RUNNING`: the status of a process still running.
pub(crate) const SP_RUNNING: i32 = -1;
pub(crate) const JOBTEXTSIZE: usize = 80;
pub(crate) const MAXJOBS_ALLOC: usize = 50;
const MAX_MAXJOBS: usize = 1000;

pub(crate) const BIN_FG: i32 = 0;
pub(crate) const BIN_BG: i32 = 1;
pub(crate) const BIN_JOBS: i32 = 2;
pub(crate) const BIN_WAIT: i32 = 3;
pub(crate) const BIN_DISOWN: i32 = 4;

/// A process of a job (zsh's `struct process`).
#[derive(Clone)]
pub(crate) struct Process {
    pub(crate) pid: i32,
    pub(crate) text: Vec<u8>,
    pub(crate) status: i32,
    pub(crate) ti: libc::rusage,
    pub(crate) bgtime: (i64, i64),
    pub(crate) endtime: (i64, i64),
}

impl std::fmt::Debug for Process {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Process")
            .field("pid", &self.pid)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// A file to delete or descriptor to close when a job ends.
#[derive(Debug, Clone)]
pub(crate) enum JobFile {
    Name(Vec<u8>),
    Fd(i32),
}

/// Saved terminal state (zsh's `struct ttyinfo`).
#[derive(Clone, Copy)]
pub(crate) struct TtyInfo {
    pub(crate) tio: libc::termios,
    pub(crate) winsize: libc::winsize,
}

impl Default for TtyInfo {
    fn default() -> TtyInfo {
        // SAFETY: all-zero termios and winsize are valid values.
        unsafe { std::mem::zeroed() }
    }
}

impl std::fmt::Debug for TtyInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TtyInfo")
    }
}

/// A job (zsh's `struct job`).
#[derive(Debug, Clone, Default)]
pub(crate) struct Job {
    pub(crate) gleader: i32,
    pub(crate) other: i32,
    pub(crate) stat: i32,
    pub(crate) pwd: Option<Vec<u8>>,
    pub(crate) procs: Vec<Process>,
    pub(crate) auxprocs: Vec<Process>,
    pub(crate) filelist: Option<Vec<JobFile>>,
    pub(crate) stty_in_env: bool,
    pub(crate) ty: Option<TtyInfo>,
}

/// The value a wait status gives `$?`.
pub(crate) fn status_value(status: i32) -> i32 {
    if libc::WIFSIGNALED(status) {
        0o200 | libc::WTERMSIG(status)
    } else if libc::WIFSTOPPED(status) {
        0o200 | libc::WEXITSTATUS(status)
    } else {
        libc::WEXITSTATUS(status)
    }
}

fn tv_diff(a: (i64, i64), b: (i64, i64)) -> (i64, i64) {
    let mut s = b.0 - a.0;
    let mut u = b.1 - a.1;
    if u < 0 {
        u += 1_000_000;
        s -= 1;
    }
    (s, u)
}

fn tv_secs(t: libc::timeval) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "C's double arithmetic")]
    let r = t.tv_sec as f64 + t.tv_usec as f64 / 1_000_000.0;
    r
}

impl Shell {
    pub(crate) fn job(&self, j: i32) -> Option<&Job> {
        usize::try_from(j).ok().and_then(|j| self.jobtab.get(j))
    }

    pub(crate) fn job_mut(&mut self, j: i32) -> Option<&mut Job> {
        usize::try_from(j).ok().and_then(|j| self.jobtab.get_mut(j))
    }

    /// `jobbing`: `isset(MONITOR)`.
    pub(crate) fn jobbing(&self) -> bool {
        self.isset(MONITOR)
    }

    /// `interact`: `isset(INTERACTIVE)`.
    pub(crate) fn interact(&self) -> bool {
        self.isset(INTERACTIVE)
    }

    /// zsh's `makerunning`.
    pub(crate) fn makerunning(&mut self, j: usize) {
        let other = {
            let Some(jn) = self.jobtab.get_mut(j) else {
                return;
            };
            jn.stat &= !STAT_STOPPED;
            for pn in &mut jn.procs {
                if libc::WIFSTOPPED(pn.status) {
                    pn.status = SP_RUNNING;
                }
            }
            (jn.stat & STAT_SUPERJOB != 0).then_some(jn.other)
        };
        if let Some(o) = other.and_then(|o| usize::try_from(o).ok())
            && o != j
        {
            self.makerunning(o);
        }
    }

    /// zsh's `findproc`: the job and index of the process `pid`.
    pub(crate) fn findproc(&self, pid: i32, aux: bool) -> Option<(usize, usize)> {
        let mut found = None;
        for i in 1..=self.maxjob {
            let Some(jn) = self.jobtab.get(i) else {
                continue;
            };
            if jn.stat & STAT_DONE != 0 {
                continue;
            }
            let list = if aux { &jn.auxprocs } else { &jn.procs };
            for (k, pn) in list.iter().enumerate() {
                if pn.pid == pid {
                    found = Some((i, k));
                    if pn.status == SP_RUNNING {
                        return found;
                    }
                }
            }
        }
        found
    }

    /// zsh's `hasprocs`.
    pub(crate) fn hasprocs(&self, job: i32) -> bool {
        self.job(job)
            .is_some_and(|jn| !jn.procs.is_empty() || !jn.auxprocs.is_empty())
    }

    /// zsh's `super_job`.
    fn super_job(&self, sub: usize) -> usize {
        for i in 1..=self.maxjob {
            if let Some(jn) = self.jobtab.get(i)
                && jn.stat & STAT_SUPERJOB != 0
                && usize::try_from(jn.other).ok() == Some(sub)
                && jn.gleader != 0
            {
                return i;
            }
        }
        0
    }

    /// zsh's `handle_sub`.
    fn handle_sub(&mut self, job: usize, fg: bool) -> bool {
        let Some(jn) = self.jobtab.get(job).cloned() else {
            return false;
        };
        let other = usize::try_from(jn.other).unwrap_or(0);
        let Some(sj) = self.jobtab.get(other).cloned() else {
            return false;
        };
        if sj.stat & STAT_DONE != 0 || (sj.procs.is_empty() && sj.auxprocs.is_empty()) {
            let signalled = sj.procs.iter().find(|p| libc::WIFSIGNALED(p.status));
            if let Some(p) = signalled {
                let sig = libc::WTERMSIG(p.status);
                if jn.gleader != self.mypgrp && jn.procs.len() > 1 {
                    let _ = crate::signals::killpg(jn.gleader, sig);
                } else if let Some(first) = jn.procs.first() {
                    let _ = crate::signals::kill(first.pid, sig);
                }
                let _ = crate::signals::kill(sj.other, libc::SIGCONT);
                let _ = crate::signals::kill(sj.other, sig);
            } else {
                let first_done = jn
                    .procs
                    .first()
                    .is_some_and(|p| libc::WIFEXITED(p.status) || libc::WIFSIGNALED(p.status));
                let cp = first_done
                    && crate::signals::killpg(jn.gleader, 0) == -1
                    && errno() == libc::ESRCH;
                let mut gleader = jn.gleader;
                if let Some(j) = self.jobtab.get_mut(job) {
                    j.stat &= !STAT_SUPERJOB;
                    j.stat |= STAT_WASSUPER;
                    if cp && let Some(last) = j.procs.last() {
                        j.gleader = last.pid;
                    }
                    gleader = j.gleader;
                }
                let single = jn.procs.len() <= 1;
                let first_pid = jn.procs.first().map_or(0, |p| p.pid);
                if (fg || i32::try_from(job).ok() == Some(self.thisjob))
                    && (single || cp || first_pid != gleader)
                {
                    self.attachtty(gleader);
                }
                // SAFETY: plain kill call.
                unsafe {
                    libc::kill(sj.other, libc::SIGCONT);
                }
                if jn.stat & STAT_DISOWN != 0 {
                    self.deletejob(job, true);
                }
            }
            self.curjob = i32::try_from(job).unwrap_or(-1);
        } else if sj.stat & STAT_STOPPED != 0 {
            let sub_status = sj.procs.first().map_or(0, |p| p.status);
            if let Some(j) = self.jobtab.get_mut(job) {
                j.stat |= STAT_STOPPED;
                for p in &mut j.procs {
                    if p.status == SP_RUNNING
                        || (!libc::WIFEXITED(p.status) && !libc::WIFSIGNALED(p.status))
                    {
                        p.status = sub_status;
                    }
                }
            }
            self.curjob = i32::try_from(job).unwrap_or(-1);
            let lng = i32::from(self.isset(LONGLISTJOBS));
            let _ = self.printjob(job, lng, 1);
            return true;
        }
        false
    }

    /// zsh's `get_usage`.
    pub(crate) fn get_usage(&mut self) {
        // SAFETY: the out pointer is valid.
        unsafe {
            libc::getrusage(libc::RUSAGE_CHILDREN, &mut self.child_usage);
        }
    }

    /// zsh's `check_cursh_sig`.
    pub(crate) fn check_cursh_sig(&self, sig: i32) {
        if !self.errflag() {
            return;
        }
        for i in 1..=self.maxjob {
            let Some(jn) = self.jobtab.get(i) else {
                continue;
            };
            if jn.stat & (STAT_CURSH | STAT_DONE) == STAT_CURSH {
                for pn in jn.procs.iter().chain(jn.auxprocs.iter()) {
                    if pn.status == SP_RUNNING {
                        // SAFETY: plain kill call.
                        unsafe {
                            libc::kill(pn.pid, sig);
                        }
                    }
                }
            }
        }
    }

    /// zsh's `storepipestats`.
    pub(crate) fn storepipestats(&mut self, j: usize, inforeground: bool, fixlastval: bool) {
        let Some(jn) = self.jobtab.get(j) else { return };
        let mut jpipestats = Vec::new();
        let mut pipefail = 0;
        for p in jn.procs.iter().take(crate::params::MAX_PIPESTATS) {
            let v = status_value(p.status);
            if v != 0 {
                pipefail = v;
            }
            jpipestats.push(v);
        }
        let cursh = jn.stat & STAT_CURSH != 0;
        if inforeground {
            let mut i = jpipestats.len();
            for (k, v) in jpipestats.iter().enumerate() {
                if let Some(slot) = self.pipestats.get_mut(k) {
                    *slot = *v;
                }
            }
            if cursh && i < crate::params::MAX_PIPESTATS {
                if let Some(slot) = self.pipestats.get_mut(i) {
                    *slot = self.lastval;
                }
                i += 1;
            }
            self.numpipestats = i;
        }
        if fixlastval {
            if cursh {
                if self.lastval == 0 && self.isset(PIPEFAIL) {
                    self.lastval = pipefail;
                }
            } else if self.isset(PIPEFAIL) {
                self.lastval = pipefail;
            }
        }
    }

    /// zsh's `update_job`.
    pub(crate) fn update_job(&mut self, j: usize) {
        let Some(jn) = self.jobtab.get_mut(j) else {
            return;
        };
        for pn in &mut jn.auxprocs {
            if libc::WIFCONTINUED(pn.status) {
                pn.status = SP_RUNNING;
            }
            if pn.status == SP_RUNNING {
                return;
            }
        }
        let mut somestopped = false;
        let mut val = 0;
        let mut status = 0;
        let mut signalled = false;
        let n = jn.procs.len();
        let gleader = jn.gleader;
        for (k, pn) in jn.procs.iter_mut().enumerate() {
            if libc::WIFCONTINUED(pn.status) {
                jn.stat &= !STAT_STOPPED;
                pn.status = SP_RUNNING;
            }
            if pn.status == SP_RUNNING {
                return;
            }
            if libc::WIFSTOPPED(pn.status) {
                somestopped = true;
            }
            if k + 1 == n {
                val = status_value(pn.status);
                signalled = libc::WIFSIGNALED(pn.status);
            }
            if pn.pid == gleader {
                status = pn.status;
            }
        }
        let job = j;
        let jstat = jn.stat;
        let need_ty = jn.stty_in_env && jn.ty.is_none();
        if somestopped {
            if need_ty {
                let ti = self.gettyinfo_now();
                if let Some(jn) = self.jobtab.get_mut(j) {
                    jn.ty = Some(ti);
                }
            }
            if jstat & STAT_SUBJOB != 0 {
                if let Some(jn) = self.jobtab.get_mut(j) {
                    jn.stat |= STAT_CHANGED | STAT_STOPPED;
                }
                let i = self.super_job(job);
                if i != 0 {
                    let sg = self.jobtab.get(i).map_or(0, |s| s.gleader);
                    // SAFETY: plain killpg call.
                    unsafe {
                        libc::killpg(sg, libc::SIGTSTP);
                    }
                    let mut print = false;
                    if let Some(sjn) = self.jobtab.get_mut(i) {
                        sjn.stat |= STAT_CHANGED | STAT_STOPPED;
                        print = sjn.stat & STAT_LOCKED != 0 && sjn.stat & STAT_NOPRINT == 0;
                    }
                    if self.isset(NOTIFY) && print {
                        let lng = i32::from(self.isset(LONGLISTJOBS));
                        if self.printjob(i, lng, 0) && self.zleactive {
                            self.zleentry_refresh();
                        }
                    }
                }
                return;
            }
            if jstat & STAT_STOPPED != 0 {
                return;
            }
        }
        let mut inforeground = 0;
        self.lastval2 = val;
        if jstat & STAT_CURSH != 0 {
            inforeground = 1;
        } else if i32::try_from(job).ok() == Some(self.thisjob) {
            self.lastval = val;
            inforeground = 2;
        }
        if self.shout >= 0
            && self.shout != 2
            && !self.ttyfrozen
            && !self.jobtab.get(j).is_some_and(|x| x.stty_in_env)
            && !self.zleactive
            && i32::try_from(job).ok() == Some(self.thisjob)
            && !somestopped
            && jstat & STAT_NOSTTY == 0
        {
            self.shttyinfo = self.gettyinfo_now();
        }
        if self.isset(MONITOR) {
            let pgrp = self.gettygrp();
            let jgleader = self.jobtab.get(j).map_or(0, |x| x.gleader);
            let pgrp_gone =
                pgrp > 1 && crate::signals::kill(-pgrp, 0) == -1 && errno() == libc::ESRCH;
            if self.mypgrp != pgrp && inforeground != 0 && (jgleader == pgrp || pgrp_gone) {
                if self.list_pipe {
                    if somestopped || pgrp_gone {
                        let mp = self.mypgrp;
                        self.attachtty(mp);
                        self.adjustwinsize(0);
                    } else if let Some(x) = self.jobtab.get_mut(j) {
                        x.stat |= STAT_ATTACH;
                    }
                    if signalled
                        && inforeground == 1
                        && ((val & !0o200) == libc::SIGINT || (val & !0o200) == libc::SIGQUIT)
                    {
                        if !self.errbrk_saved {
                            self.errbrk_saved = true;
                            self.prev_breaks = self.breaks;
                            self.prev_errflag = self.errflag.get();
                        }
                        self.breaks = self.loops;
                        self.errflag.set(self.errflag.get() | ERRFLAG_INT);
                        self.inerrflush();
                    }
                } else {
                    let mp = self.mypgrp;
                    self.attachtty(mp);
                    self.adjustwinsize(0);
                }
            }
        } else if self.list_pipe
            && signalled
            && inforeground == 1
            && ((val & !0o200) == libc::SIGINT || (val & !0o200) == libc::SIGQUIT)
        {
            if !self.errbrk_saved {
                self.errbrk_saved = true;
                self.prev_breaks = self.breaks;
                self.prev_errflag = self.errflag.get();
            }
            self.breaks = self.loops;
            self.errflag.set(self.errflag.get() | ERRFLAG_INT);
            self.inerrflush();
        }
        if somestopped && jstat & STAT_SUPERJOB != 0 {
            return;
        }
        let newstat = if let Some(x) = self.jobtab.get_mut(j) {
            x.stat |= if somestopped {
                STAT_CHANGED | STAT_STOPPED
            } else {
                STAT_CHANGED | STAT_DONE
            };
            x.stat
        } else {
            return;
        };
        if newstat & (STAT_DONE | STAT_STOPPED) != 0 {
            self.storepipestats(j, inforeground != 0, false);
        }
        if inforeground == 0 && newstat & (STAT_SUBJOB | STAT_DONE) == (STAT_SUBJOB | STAT_DONE) {
            let su = self.super_job(j);
            if su != 0 {
                let _ = self.handle_sub(su, false);
            }
        }
        if newstat & (STAT_DONE | STAT_STOPPED) == STAT_STOPPED {
            self.prevjob = self.curjob;
            self.curjob = i32::try_from(job).unwrap_or(-1);
        }
        let locked = self
            .jobtab
            .get(j)
            .is_some_and(|x| x.stat & STAT_LOCKED != 0);
        if (self.isset(NOTIFY) || i32::try_from(job).ok() == Some(self.thisjob)) && locked {
            let lng = i32::from(self.isset(LONGLISTJOBS));
            if self.printjob(j, lng, 0) && self.zleactive {
                self.zleentry_refresh();
            }
        }
        if self
            .sigtrapped
            .get(libc::SIGCHLD as usize)
            .copied()
            .unwrap_or(0)
            != 0
            && i32::try_from(job).ok() != Some(self.thisjob)
        {
            self.dotrap(libc::SIGCHLD as usize);
        }
        if inforeground == 2 && self.isset(MONITOR) && libc::WIFSIGNALED(status) {
            let sig = libc::WTERMSIG(status);
            if sig == libc::SIGINT || sig == libc::SIGQUIT {
                if self.sigtrapped.get(sig as usize).copied().unwrap_or(0) != 0 {
                    self.dotrap(sig as usize);
                    if self.errflag() {
                        self.breaks = self.loops;
                    }
                } else {
                    self.breaks = self.loops;
                    self.errflag.set(self.errflag.get() | ERRFLAG_INT);
                }
                self.check_cursh_sig(sig);
            }
        }
    }

    /// zsh's `setprevjob`.
    fn setprevjob(&mut self) {
        for i in (1..=self.maxjob).rev() {
            if let Some(jn) = self.jobtab.get(i) {
                let ii = i32::try_from(i).unwrap_or(-1);
                if jn.stat & STAT_INUSE != 0
                    && jn.stat & STAT_STOPPED != 0
                    && jn.stat & STAT_SUBJOB == 0
                    && ii != self.curjob
                    && ii != self.thisjob
                {
                    self.prevjob = ii;
                    return;
                }
            }
        }
        for i in (1..=self.maxjob).rev() {
            if let Some(jn) = self.jobtab.get(i) {
                let ii = i32::try_from(i).unwrap_or(-1);
                if jn.stat & STAT_INUSE != 0
                    && jn.stat & STAT_SUBJOB == 0
                    && ii != self.curjob
                    && ii != self.thisjob
                {
                    self.prevjob = ii;
                    return;
                }
            }
        }
        self.prevjob = -1;
    }

    /// zsh's `printtime`.
    fn printtime(&mut self, real: (i64, i64), ti: &libc::rusage, desc: Option<&[u8]>) {
        let desc: Vec<u8> = desc.map(crate::tok::unmetafy).unwrap_or_default();
        #[expect(clippy::cast_precision_loss, reason = "C's double arithmetic")]
        let elapsed_time = real.0 as f64 + real.1 as f64 / 1_000_000.0;
        let user_time = tv_secs(ti.ru_utime);
        let system_time = tv_secs(ti.ru_stime);
        let total_time = user_time + system_time;
        #[expect(clippy::cast_possible_truncation, reason = "C's int conversion")]
        let percent = (100.0 * total_time / elapsed_time) as i32;
        self.queue_signals();
        let fmt = self
            .getsparam(b"TIMEFMT")
            .map(|s| crate::tok::unmetafy(&s))
            .unwrap_or_else(|| b"%J  %U user %S system %P cpu %*E total".to_vec());
        let mut out = String::new();
        let hms = |secs: f64| -> String {
            #[expect(clippy::cast_possible_truncation, reason = "C's int conversion")]
            let mut mins = secs as i32 / 60;
            let hours = mins / 60;
            let secs = secs - f64::from(60 * mins);
            mins -= 60 * hours;
            if hours != 0 {
                format!("{hours}:{mins:02}:{secs:05.2}")
            } else if mins != 0 {
                format!("{mins}:{secs:05.2}")
            } else {
                format!("{secs:.3}")
            }
        };
        let total_nz = total_time != 0.0;
        #[expect(clippy::cast_possible_truncation, reason = "C's long conversion")]
        let per = |v: i64| -> i64 {
            if total_nz {
                (v as f64 / total_time) as i64
            } else {
                0
            }
        };
        let mut i = 0;
        let mut raw: Vec<u8> = Vec::new();
        let flush = |out: &mut String, raw: &mut Vec<u8>| {
            out.push_str(&String::from_utf8_lossy(raw));
            raw.clear();
        };
        while i < fmt.len() {
            let c = fmt.get(i).copied().unwrap_or(0);
            if c != b'%' {
                raw.push(c);
                i += 1;
                continue;
            }
            flush(&mut out, &mut raw);
            i += 1;
            let c2 = fmt.get(i).copied().unwrap_or(0);
            match c2 {
                b'E' => out.push_str(&format!("{elapsed_time:4.2}s")),
                b'U' => out.push_str(&format!("{user_time:4.2}s")),
                b'S' => out.push_str(&format!("{system_time:4.2}s")),
                b'm' | b'u' | b'*' => {
                    i += 1;
                    let c3 = fmt.get(i).copied().unwrap_or(0);
                    let v = match c3 {
                        b'E' => Some(elapsed_time),
                        b'U' => Some(user_time),
                        b'S' => Some(system_time),
                        _ => None,
                    };
                    match (c2, v) {
                        (b'm', Some(v)) => out.push_str(&format!("{:.0}ms", v * 1000.0)),
                        (b'u', Some(v)) => out.push_str(&format!("{:.0}us", v * 1_000_000.0)),
                        (b'*', Some(v)) => out.push_str(&hms(v)),
                        _ => {
                            out.push('%');
                            out.push(char::from(c2));
                            i -= 1;
                        }
                    }
                }
                b'P' => out.push_str(&format!("{percent}%")),
                b'W' => out.push_str(&ti.ru_nswap.to_string()),
                b'X' => out.push_str(&per(ti.ru_ixrss).to_string()),
                b'D' => out.push_str(&per(ti.ru_idrss + ti.ru_isrss).to_string()),
                b'K' => out.push_str(&per(ti.ru_ixrss + ti.ru_idrss + ti.ru_isrss).to_string()),
                b'M' => out.push_str(&(ti.ru_maxrss / 1024).to_string()),
                b'F' => out.push_str(&ti.ru_majflt.to_string()),
                b'R' => out.push_str(&ti.ru_minflt.to_string()),
                b'I' => out.push_str(&ti.ru_inblock.to_string()),
                b'O' => out.push_str(&ti.ru_oublock.to_string()),
                b'r' => out.push_str(&ti.ru_msgrcv.to_string()),
                b's' => out.push_str(&ti.ru_msgsnd.to_string()),
                b'k' => out.push_str(&ti.ru_nsignals.to_string()),
                b'w' => out.push_str(&ti.ru_nvcsw.to_string()),
                b'c' => out.push_str(&ti.ru_nivcsw.to_string()),
                b'J' => {
                    let mut b = out.into_bytes();
                    b.extend_from_slice(&desc);
                    out = String::from_utf8_lossy(&b).into_owned();
                }
                b'%' => out.push('%'),
                0 => {
                    i -= 1;
                }
                other => {
                    out.push('%');
                    out.push(char::from(other));
                }
            }
            i += 1;
        }
        flush(&mut out, &mut raw);
        self.unqueue_signals();
        out.push('\n');
        write_fd(2, out.as_bytes());
    }

    /// zsh's `dumptime`.
    fn dumptime(&mut self, j: usize) {
        let Some(jn) = self.jobtab.get(j).cloned() else {
            return;
        };
        for pn in &jn.procs {
            self.printtime(tv_diff(pn.bgtime, pn.endtime), &pn.ti, Some(&pn.text));
        }
    }

    /// zsh's `should_report_time`.
    fn should_report_time(&mut self, j: usize) -> bool {
        let Some(jn) = self.jobtab.get(j) else {
            return false;
        };
        if jn.stat & STAT_TIMED != 0 {
            return true;
        }
        let first = jn.procs.first().cloned();
        self.queue_signals();
        let save = self.errflag.get();
        self.errflag.set(0);
        let mut reporttime: i64 = -1;
        let mut reportmemory: i64 = -1;
        let s = b"REPORTTIME".to_vec();
        let mut i = 0;
        if let Some(mut v) = self.getvalue(&s, &mut i, 0) {
            reporttime = self.getintvalue(Some(&mut v));
        }
        let s = b"REPORTMEMORY".to_vec();
        let mut i = 0;
        if let Some(mut v) = self.getvalue(&s, &mut i, 0) {
            reportmemory = self.getintvalue(Some(&mut v));
        }
        self.errflag.set(save);
        self.unqueue_signals();
        if reporttime < 0 && reportmemory < 0 {
            return false;
        }
        let Some(p) = first else { return false };
        if self.zleactive {
            return false;
        }
        if reporttime >= 0 {
            reporttime -= p.ti.ru_utime.tv_sec + p.ti.ru_stime.tv_sec;
            if p.ti.ru_utime.tv_usec + p.ti.ru_stime.tv_usec >= 1_000_000 {
                reporttime -= 1;
            }
            if reporttime <= 0 {
                return true;
            }
        }
        reportmemory >= 0 && p.ti.ru_maxrss / 1024 > reportmemory
    }

    /// zsh's `printjob`: true if something was printed.
    #[expect(clippy::too_many_lines, reason = "zsh's printjob")]
    pub(crate) fn printjob(&mut self, jidx: usize, lng: i32, synch: i32) -> bool {
        let use_old = synch > 1 && self.oldjobtab.is_some();
        let mut lng = lng;
        let job = jidx;
        let mut jn = if use_old {
            self.oldjobtab
                .as_ref()
                .and_then(|t| t.get(jidx))
                .cloned()
                .unwrap_or_default()
        } else {
            self.jobtab.get(jidx).cloned().unwrap_or_default()
        };
        let fout = if synch == 2 || self.shout < 0 {
            1
        } else {
            self.shout
        };
        let mut skip_print = jn.stat & STAT_NOPRINT != 0;
        let mut conted = false;
        if lng < 0 {
            conted = true;
            lng = i32::from(self.isset(LONGLISTJOBS));
        }
        if jn.stat & STAT_SUPERJOB != 0 && jn.other != 0 {
            let sjn = self.job(jn.other).cloned().unwrap_or_default();
            if !sjn.procs.is_empty() || !sjn.auxprocs.is_empty() {
                jn = sjn;
            }
        }
        let thisjob_u = usize::try_from(self.thisjob).ok();
        let mut len: usize = 9;
        let mut sflag = false;
        let mut doputnl = false;
        let super_running = jn.stat & STAT_SUPERJOB != 0
            && jn.procs.first().is_some_and(|p| p.status == SP_RUNNING);
        let n = jn.procs.len();
        for (k, pn) in jn.procs.iter_mut().enumerate() {
            if super_running && k + 1 == n {
                pn.status = SP_RUNNING;
            }
            if pn.status != SP_RUNNING {
                if libc::WIFSIGNALED(pn.status) {
                    let sig = libc::WTERMSIG(pn.status);
                    let mut llen = sigmsg(sig).len();
                    if libc::WCOREDUMP(pn.status) {
                        llen += 14;
                    }
                    len = len.max(llen);
                    if sig != libc::SIGINT && sig != libc::SIGPIPE {
                        sflag = true;
                    }
                    if Some(job) == thisjob_u && sig == libc::SIGINT {
                        doputnl = true;
                    }
                    if self.isset(PRINTEXITVALUE) && self.isset(SHINSTDIN) {
                        sflag = true;
                        skip_print = false;
                    }
                } else if libc::WIFSTOPPED(pn.status) {
                    let sig = libc::WSTOPSIG(pn.status);
                    len = len.max(sigmsg(sig).len());
                    if Some(job) == thisjob_u && sig == libc::SIGTSTP {
                        doputnl = true;
                    }
                } else if self.isset(PRINTEXITVALUE)
                    && self.isset(SHINSTDIN)
                    && libc::WEXITSTATUS(pn.status) != 0
                {
                    sflag = true;
                    skip_print = false;
                }
            }
        }
        let jobi = i32::try_from(job).unwrap_or(-1);
        let finish_done = |sh: &mut Shell| {
            if synch <= 1 {
                sh.storepipestats(job, Some(job) == thisjob_u, Some(job) == thisjob_u);
            }
            if sh.should_report_time(job) {
                sh.dumptime(job);
            }
            sh.deletejob(job, false);
            if jobi == sh.curjob {
                sh.curjob = sh.prevjob;
                sh.prevjob = jobi;
            }
            if jobi == sh.prevjob {
                sh.setprevjob();
            }
        };
        if skip_print {
            if jn.stat & STAT_DONE != 0 {
                finish_done(self);
            }
            return false;
        }
        let mut doneprint = false;
        let mut outb: Vec<u8> = Vec::new();
        if synch == 2
            || ((self.interact() || synch != 0)
                && self.jobbing()
                && (jn.stat & STAT_STOPPED != 0 || sflag || Some(job) != thisjob_u))
        {
            let plainfmt = synch == 3 && self.isset(POSIXJOBS);
            let thisfmt = Some(job) == thisjob_u && synch != 2;
            let lineleng = usize::try_from(self.zterm_columns).unwrap_or(80);
            if synch == 0 {
                self.zleentry_trash();
            }
            if doputnl && synch == 0 {
                doneprint = true;
                outb.push(b'\n');
            }
            let mut skip = 0usize;
            let mut fline = true;
            let mut pi = 0usize;
            while pi < jn.procs.len() {
                let Some(pn) = jn.procs.get(pi) else { break };
                let mut len2 = (if thisfmt { 5 } else { 10 }) + len;
                let mut qi = pi + 1;
                if lng & 3 == 0 {
                    while let Some(qn) = jn.procs.get(qi) {
                        if qn.status != pn.status {
                            break;
                        }
                        if qn.text.len() + len2 + if qi + 1 < jn.procs.len() { 3 } else { 0 }
                            > lineleng
                        {
                            break;
                        }
                        len2 += qn.text.len() + 2;
                        qi += 1;
                    }
                }
                doneprint = true;
                if !plainfmt {
                    if !thisfmt || lng != 0 {
                        if fline {
                            let mark = if jobi == self.curjob {
                                '+'
                            } else if jobi == self.prevjob {
                                '-'
                            } else {
                                ' '
                            };
                            outb.extend_from_slice(format!("[{job}]  {mark} ").as_bytes());
                        } else {
                            outb.extend_from_slice(if job > 9 { b"        " } else { b"       " });
                        }
                    } else {
                        outb.extend_from_slice(b"zsh: ");
                    }
                    if lng & 1 != 0 {
                        outb.extend_from_slice(format!("{} ", pn.pid).as_bytes());
                    } else if lng & 2 != 0 {
                        let x = jn.gleader;
                        outb.extend_from_slice(format!("{x} ").as_bytes());
                        let mut xx = x;
                        loop {
                            skip += 1;
                            xx /= 10;
                            if xx == 0 {
                                break;
                            }
                        }
                        skip += 1;
                        lng &= !3;
                    } else {
                        outb.extend(std::iter::repeat_n(b' ', skip));
                    }
                    let st = pn.status;
                    if st == SP_RUNNING {
                        if conted {
                            outb.extend_from_slice(
                                format!("continued{:w$}", "", w = (len + 2).saturating_sub(9))
                                    .as_bytes(),
                            );
                        } else {
                            outb.extend_from_slice(
                                format!("running{:w$}", "", w = (len + 2).saturating_sub(7))
                                    .as_bytes(),
                            );
                        }
                    } else if libc::WIFEXITED(st) {
                        if libc::WEXITSTATUS(st) != 0 {
                            outb.extend_from_slice(
                                format!(
                                    "exit {:<4}{:w$}",
                                    libc::WEXITSTATUS(st),
                                    "",
                                    w = (len + 2).saturating_sub(9)
                                )
                                .as_bytes(),
                            );
                        } else {
                            outb.extend_from_slice(
                                format!("done{:w$}", "", w = (len + 2).saturating_sub(4))
                                    .as_bytes(),
                            );
                        }
                    } else if libc::WIFSTOPPED(st) {
                        outb.extend_from_slice(
                            format!("{:<w$}", sigmsg(libc::WSTOPSIG(st)), w = len + 2).as_bytes(),
                        );
                    } else if libc::WCOREDUMP(st) {
                        let m = sigmsg(libc::WTERMSIG(st));
                        outb.extend_from_slice(
                            format!(
                                "{m} (core dumped){:w$}",
                                "",
                                w = (len + 2).saturating_sub(14 + m.len())
                            )
                            .as_bytes(),
                        );
                    } else {
                        outb.extend_from_slice(
                            format!("{:<w$}", sigmsg(libc::WTERMSIG(st)), w = len + 2).as_bytes(),
                        );
                    }
                }
                for k in pi..qi {
                    if let Some(p) = jn.procs.get(k) {
                        outb.extend(crate::tok::unmetafy(&p.text));
                    }
                    if k + 1 < jn.procs.len() {
                        outb.extend_from_slice(b" | ");
                    }
                }
                outb.push(b'\n');
                fline = false;
                pi = qi;
            }
        } else if doputnl && self.interact() && synch == 0 {
            doneprint = true;
            outb.push(b'\n');
        }
        if lng & 4 != 0
            || (self.interact()
                && Some(job) == thisjob_u
                && jn.pwd.as_ref().is_some_and(|p| *p != self.pwd))
        {
            doneprint = true;
            outb.extend_from_slice(if lng & 4 != 0 {
                b"(pwd : "
            } else {
                b"(pwd now: "
            });
            let d = if lng & 4 != 0 {
                jn.pwd.clone().unwrap_or_else(|| self.pwd.clone())
            } else {
                self.pwd.clone()
            };
            outb.extend(self.fprintdir(&d));
            outb.extend_from_slice(b")\n");
        }
        if !outb.is_empty() {
            write_fd(fout, &outb);
        }
        if jn.stat & STAT_DONE != 0 {
            finish_done(self);
        } else if !use_old && let Some(x) = self.jobtab.get_mut(jidx) {
            x.stat &= !STAT_CHANGED;
        }
        doneprint
    }

    /// zsh's `addfilelist`.
    pub(crate) fn addfilelist(&mut self, name: Option<&[u8]>, fd: i32) {
        let tj = self.thisjob;
        let Some(jn) = self.job_mut(tj) else { return };
        let ll = jn.filelist.get_or_insert_with(Vec::new);
        ll.push(match name {
            Some(n) => JobFile::Name(n.to_vec()),
            None => JobFile::Fd(fd),
        });
    }

    /// zsh's `pipecleanfilelist` on job `j`'s list.
    pub(crate) fn pipecleanfilelist_job(&mut self, j: i32, proc_subst_only: bool) {
        let Some(list) = self.job_mut(j).and_then(|jn| jn.filelist.take()) else {
            return;
        };
        let list = self.pipecleanfilelist(list, proc_subst_only);
        if let Some(jn) = self.job_mut(j) {
            jn.filelist = Some(list);
        }
    }

    /// zsh's `pipecleanfilelist`: returns what is left.
    pub(crate) fn pipecleanfilelist(
        &mut self,
        filelist: Vec<JobFile>,
        proc_subst_only: bool,
    ) -> Vec<JobFile> {
        let mut kept = Vec::new();
        for jf in filelist {
            match jf {
                JobFile::Fd(fd)
                    if !proc_subst_only || self.fdtable_get(fd) == crate::exec::FDT_PROC_SUBST =>
                {
                    let _ = self.zclose(fd);
                }
                other => kept.push(other),
            }
        }
        kept
    }

    /// zsh's `deletefilelist`.
    pub(crate) fn deletefilelist(&mut self, file_list: Option<Vec<JobFile>>, disowning: bool) {
        let Some(list) = file_list else { return };
        for jf in list {
            match jf {
                JobFile::Fd(fd) => {
                    if !disowning {
                        let _ = self.zclose(fd);
                    }
                }
                JobFile::Name(n) => {
                    if !disowning {
                        let _ = std::fs::remove_file(std::ffi::OsStr::from_bytes_compat(
                            &crate::tok::unmetafy(&n),
                        ));
                    }
                }
            }
        }
    }

    /// zsh's `cleanfilelists`.
    pub(crate) fn cleanfilelists(&mut self) {
        for i in 1..=self.maxjob {
            let l = self.jobtab.get_mut(i).and_then(|j| j.filelist.take());
            self.deletefilelist(l, false);
        }
    }

    /// zsh's `freejob`.
    pub(crate) fn freejob(&mut self, j: usize, deleting: bool) {
        let (wassuper, other) = {
            let Some(jn) = self.jobtab.get_mut(j) else {
                return;
            };
            jn.procs.clear();
            jn.auxprocs.clear();
            jn.ty = None;
            jn.pwd = None;
            (jn.stat & STAT_WASSUPER != 0, jn.other)
        };
        if wassuper && let Ok(o) = usize::try_from(other) {
            if deleting {
                self.deletejob(o, false);
            } else {
                self.freejob(o, false);
            }
        }
        if let Some(jn) = self.jobtab.get_mut(j) {
            jn.gleader = 0;
            jn.other = 0;
            jn.stat = 0;
            jn.stty_in_env = false;
            jn.filelist = None;
            jn.ty = None;
        }
        if self.maxjob == j {
            while self.maxjob > 0
                && self
                    .jobtab
                    .get(self.maxjob)
                    .is_none_or(|jb| jb.stat & STAT_INUSE == 0)
            {
                self.maxjob -= 1;
            }
        }
    }

    /// zsh's `deletejob`.
    pub(crate) fn deletejob(&mut self, j: usize, disowning: bool) {
        let fl = self.jobtab.get_mut(j).and_then(|jn| jn.filelist.take());
        self.deletefilelist(fl, disowning);
        let (stat, other) = self.jobtab.get(j).map_or((0, 0), |jn| (jn.stat, jn.other));
        if stat & STAT_ATTACH != 0 {
            let mp = self.mypgrp;
            self.attachtty(mp);
            self.adjustwinsize(0);
        }
        if stat & STAT_SUPERJOB != 0
            && let Some(jno) = self.job_mut(other)
            && jno.stat & STAT_SUBJOB != 0
        {
            jno.stat |= STAT_SUBJOB_ORPHANED;
        }
        self.freejob(j, true);
    }

    /// zsh's `addproc`.
    pub(crate) fn addproc(
        &mut self,
        pid: i32,
        text: Option<&[u8]>,
        aux: bool,
        bgtime: (i64, i64),
        gleader: i32,
        list_pipe_job_used: i32,
    ) {
        let tj = self.thisjob;
        let mut text_v = text.map(<[u8]>::to_vec).unwrap_or_default();
        text_v.truncate(JOBTEXTSIZE - 1);
        let pn = Process {
            pid,
            text: text_v,
            status: SP_RUNNING,
            // SAFETY: all-zero rusage is valid.
            ti: unsafe { std::mem::zeroed() },
            bgtime: if aux { (0, 0) } else { bgtime },
            endtime: (0, 0),
        };
        if !aux {
            if gleader != -1 {
                if let Some(jn) = self.job_mut(tj) {
                    jn.gleader = if jn.stat & STAT_CURSH != 0 {
                        gleader
                    } else {
                        pid
                    };
                }
                if list_pipe_job_used != -1
                    && let Some(lj) = self.job_mut(list_pipe_job_used)
                {
                    lj.gleader = gleader;
                }
                self.last_attached_pgrp = gleader;
            } else if let Some(jn) = self.job_mut(tj)
                && jn.gleader == 0
            {
                jn.gleader = pid;
            }
            if let Some(jn) = self.job_mut(tj) {
                jn.procs.push(pn);
            }
        } else if let Some(jn) = self.job_mut(tj) {
            jn.auxprocs.push(pn);
        }
        if let Some(jn) = self.job_mut(tj) {
            jn.stat &= !STAT_DONE;
        }
    }

    /// zsh's `havefiles`.
    pub(crate) fn havefiles(&self) -> bool {
        (1..=self.maxjob).any(|i| {
            self.jobtab
                .get(i)
                .is_some_and(|j| j.stat != 0 && j.filelist.is_some())
        })
    }

    /// zsh's `waitforpid`.
    pub(crate) fn waitforpid(&mut self, pid: i32, wait_cmd: bool) -> i32 {
        let q = self.queue_signal_level();
        let mut first = true;
        self.dont_queue_signals();
        child_block();
        self.queue_traps(wait_cmd);
        while !self.errflag() && (crate::signals::kill(pid, 0) >= 0 || errno() != libc::ESRCH) {
            if first {
                first = false;
            } else if !wait_cmd {
                // SAFETY: plain kill call.
                unsafe {
                    libc::kill(pid, libc::SIGCONT);
                }
            }
            crate::signals::LAST_SIGNAL.store(-1, std::sync::atomic::Ordering::SeqCst);
            let _ = self.signal_suspend(wait_cmd);
            let ls = crate::signals::LAST_SIGNAL.load(std::sync::atomic::Ordering::SeqCst);
            if ls != libc::SIGCHLD
                && wait_cmd
                && ls >= 0
                && self
                    .sigtrapped
                    .get(usize::try_from(ls).unwrap_or(0))
                    .copied()
                    .unwrap_or(0)
                    & ZSIG_TRAPPED
                    != 0
            {
                self.restore_queue_signals(q);
                return 128 + ls;
            }
            child_block();
        }
        self.unqueue_traps();
        child_unblock();
        self.restore_queue_signals(q);
        0
    }

    /// zsh's `zwaitjob`.
    fn zwaitjob(&mut self, job: usize, wait_cmd: bool) -> i32 {
        let q = self.queue_signal_level();
        child_block();
        self.queue_traps(wait_cmd);
        self.dont_queue_signals();
        let has = self
            .jobtab
            .get(job)
            .is_some_and(|jn| !jn.procs.is_empty() || !jn.auxprocs.is_empty());
        if has {
            let changed = if let Some(jn) = self.jobtab.get_mut(job) {
                jn.stat |= STAT_LOCKED;
                jn.stat & STAT_CHANGED != 0
            } else {
                false
            };
            if changed {
                let lng = i32::from(self.isset(LONGLISTJOBS));
                let _ = self.printjob(job, lng, 1);
            }
            let fl = self.jobtab.get_mut(job).and_then(|jn| jn.filelist.take());
            if let Some(fl) = fl {
                let rest = self.pipecleanfilelist(fl, false);
                if let Some(jn) = self.jobtab.get_mut(job) {
                    jn.filelist = Some(rest);
                }
            }
            loop {
                let st = self.jobtab.get(job).map_or(0, |jn| jn.stat);
                if self.errflag.get() & ERRFLAG_ERROR != 0
                    || st == 0
                    || st & STAT_DONE != 0
                    || (self.interact() && st & STAT_STOPPED != 0)
                {
                    break;
                }
                let _ = self.signal_suspend(wait_cmd);
                let ls = crate::signals::LAST_SIGNAL.load(std::sync::atomic::Ordering::SeqCst);
                if ls != libc::SIGCHLD
                    && wait_cmd
                    && ls >= 0
                    && self
                        .sigtrapped
                        .get(usize::try_from(ls).unwrap_or(0))
                        .copied()
                        .unwrap_or(0)
                        & ZSIG_TRAPPED
                        != 0
                {
                    self.restore_queue_signals(q);
                    return 128 + ls;
                }
                if self.subsh {
                    let _ = self.killjb(job, libc::SIGCONT);
                }
                if self
                    .jobtab
                    .get(job)
                    .is_some_and(|jn| jn.stat & STAT_SUPERJOB != 0)
                    && self.handle_sub(job, true)
                {
                    break;
                }
                child_block();
            }
        } else {
            self.deletejob(job, false);
            if let Some(p) = self.pipestats.first_mut() {
                *p = self.lastval;
            }
            self.numpipestats = 1;
        }
        self.restore_queue_signals(q);
        self.unqueue_traps();
        child_unblock();
        0
    }

    fn waitonejob(&mut self, j: usize) {
        if self
            .jobtab
            .get(j)
            .is_some_and(|jn| !jn.procs.is_empty() || !jn.auxprocs.is_empty())
        {
            let _ = self.zwaitjob(j, false);
        } else {
            self.deletejob(j, false);
            if let Some(p) = self.pipestats.first_mut() {
                *p = self.lastval;
            }
            self.numpipestats = 1;
        }
    }

    /// zsh's `waitjobs`.
    pub(crate) fn waitjobs(&mut self) {
        let Ok(tj) = usize::try_from(self.thisjob) else {
            return;
        };
        let (stat, other) = self.jobtab.get(tj).map_or((0, 0), |j| (j.stat, j.other));
        if stat & STAT_SUPERJOB != 0
            && let Ok(o) = usize::try_from(other)
        {
            self.waitonejob(o);
        }
        self.waitonejob(tj);
        self.thisjob = -1;
    }

    /// zsh's `clearjobtab`.
    pub(crate) fn clearjobtab(&mut self, monitor: bool) {
        if self.isset(POSIXJOBS) {
            self.oldmaxjob = 0;
        }
        for i in 1..=self.maxjob {
            let stat = self.jobtab.get(i).map_or(0, |j| j.stat);
            if monitor && !self.isset(POSIXJOBS) && stat != 0 {
                self.oldmaxjob = i + 1;
            } else if stat & STAT_INUSE != 0 {
                self.freejob(i, false);
            }
        }
        if monitor && self.oldmaxjob != 0 {
            let mut old: Vec<Job> = self.jobtab.iter().take(self.oldmaxjob).cloned().collect();
            if let Ok(tj) = usize::try_from(self.thisjob)
                && let Some(slot) = old.get_mut(tj)
            {
                *slot = Job::default();
            }
            self.oldjobtab = Some(old);
            self.oldmaxjob -= 1;
        }
        for j in &mut self.jobtab {
            *j = Job::default();
        }
        self.maxjob = 0;
        self.thisjob = self.initjob();
    }

    /// zsh's `clearoldjobtab`.
    pub(crate) fn clearoldjobtab(&mut self) {
        self.oldjobtab = None;
        self.oldmaxjob = 0;
    }

    fn initnewjob(&mut self, i: usize) -> i32 {
        if let Some(j) = self.jobtab.get_mut(i) {
            j.stat = STAT_INUSE;
            j.pwd = None;
            j.gleader = 0;
        }
        if i > self.maxjob {
            self.maxjob = i;
        }
        i32::try_from(i).unwrap_or(-1)
    }

    /// zsh's `initjob`.
    pub(crate) fn initjob(&mut self) -> i32 {
        for i in 1..=self.maxjob {
            if self.jobtab.get(i).is_some_and(|j| j.stat == 0) {
                return self.initnewjob(i);
            }
        }
        if self.maxjob + 1 < self.jobtab.len() {
            return self.initnewjob(self.maxjob + 1);
        }
        if self.expandjobtab() {
            return self.initnewjob(self.maxjob + 1);
        }
        self.zerr("job table full or recursion limit exceeded");
        -1
    }

    /// zsh's `setjobpwd`.
    pub(crate) fn setjobpwd(&mut self) {
        let pwd = self.pwd.clone();
        for i in 1..=self.maxjob {
            if let Some(j) = self.jobtab.get_mut(i)
                && j.stat != 0
                && j.pwd.is_none()
            {
                j.pwd = Some(pwd.clone());
            }
        }
    }

    /// zsh's `spawnjob`.
    pub(crate) fn spawnjob(&mut self) {
        let tj = self.thisjob;
        if !self.subsh {
            let cur_stopped = self
                .job(self.curjob)
                .is_some_and(|j| j.stat & STAT_STOPPED != 0);
            if self.curjob == -1 || !cur_stopped {
                self.curjob = tj;
                self.setprevjob();
            } else if self.prevjob == -1
                || !self
                    .job(self.prevjob)
                    .is_some_and(|j| j.stat & STAT_STOPPED != 0)
            {
                self.prevjob = tj;
            }
            if self.jobbing() && self.job(tj).is_some_and(|j| !j.procs.is_empty()) {
                let fout = if self.shout >= 0 { self.shout } else { 1 };
                let mut s = format!("[{tj}]");
                if let Some(j) = self.job(tj) {
                    for pn in &j.procs {
                        s.push_str(&format!(" {}", pn.pid));
                    }
                }
                s.push('\n');
                write_fd(fout, s.as_bytes());
            }
        }
        if !self.hasprocs(tj) {
            if let Ok(t) = usize::try_from(tj) {
                self.deletejob(t, false);
            }
        } else {
            if let Some(j) = self.job_mut(tj) {
                j.stat |= STAT_LOCKED;
            }
            self.pipecleanfilelist_job(tj, false);
        }
        self.thisjob = -1;
    }

    /// zsh's `shelltime`.
    pub(crate) fn shelltime(&mut self) {
        let now = crate::params::now_tv();
        // SAFETY: all-zero rusage is valid; getrusage fills it.
        let mut ti: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: the out pointer is valid.
        unsafe {
            libc::getrusage(libc::RUSAGE_SELF, &mut ti);
        }
        let d = tv_diff(self.shtimer, now);
        self.printtime(d, &ti, Some(b"shell"));
        // SAFETY: the out pointer is valid.
        unsafe {
            libc::getrusage(libc::RUSAGE_CHILDREN, &mut ti);
        }
        self.printtime(d, &ti, Some(b"children"));
    }

    /// zsh's `scanjobs`.
    pub(crate) fn scanjobs(&mut self) {
        for i in 1..=self.maxjob {
            if self
                .jobtab
                .get(i)
                .is_some_and(|j| j.stat & STAT_CHANGED != 0)
            {
                let lng = i32::from(self.isset(LONGLISTJOBS));
                let _ = self.printjob(i, lng, 1);
            }
        }
    }

    /// zsh's `setcurjob`.
    fn setcurjob(&mut self) {
        let inuse = |sh: &Shell, j: i32| sh.job(j).is_some_and(|x| x.stat & STAT_INUSE != 0);
        if self.curjob == self.thisjob || (self.curjob != -1 && !inuse(self, self.curjob)) {
            self.curjob = self.prevjob;
            self.setprevjob();
            if self.curjob == self.thisjob
                || (self.curjob != -1 && !(inuse(self, self.curjob) && self.curjob != self.thisjob))
            {
                self.curjob = self.prevjob;
                self.setprevjob();
            }
        }
    }

    /// zsh's `getjob`.
    pub(crate) fn getjob(&mut self, s: &[u8], prog: Option<&str>) -> i32 {
        let use_old = self.oldjobtab.is_some();
        let mymaxjob = if use_old { self.oldmaxjob } else { self.maxjob };
        let posix = self.isset(POSIXBUILTINS);
        let tab: Vec<Job> = if use_old {
            self.oldjobtab.clone().unwrap_or_default()
        } else {
            self.jobtab.clone()
        };
        if s.first() == Some(&b'%') {
            let s = s.get(1..).unwrap_or(&[]);
            if s.first().is_none_or(|&c| c == b'%' || c == b'+') {
                if self.curjob == -1 {
                    if let Some(p) = prog
                        && !posix
                    {
                        self.zwarnnam(p, "no current job");
                    }
                    return -1;
                }
                return self.curjob;
            }
            if s.first() == Some(&b'-') {
                if self.prevjob == -1 {
                    if let Some(p) = prog
                        && !posix
                    {
                        self.zwarnnam(p, "no previous job");
                    }
                    return -1;
                }
                return self.prevjob;
            }
            if s.first().is_some_and(u8::is_ascii_digit) {
                let jobnum = crate::utils::atoi(s);
                if let Ok(ju) = usize::try_from(jobnum)
                    && jobnum > 0
                    && ju <= mymaxjob
                    && tab
                        .get(ju)
                        .is_some_and(|j| j.stat != 0 && j.stat & STAT_SUBJOB == 0)
                    && (use_old || i32::try_from(jobnum).ok() != Some(self.thisjob))
                {
                    return i32::try_from(jobnum).unwrap_or(-1);
                }
                if let Some(p) = prog
                    && !posix
                {
                    self.zwarnnam(p, &format!("%{}: no such job", lossy(s)));
                }
                return -1;
            }
            if s.first() == Some(&b'?') {
                let needle = s.get(1..).unwrap_or(&[]);
                for jobnum in (0..=mymaxjob).rev() {
                    let Some(j) = tab.get(jobnum) else { continue };
                    if j.stat != 0
                        && j.stat & STAT_SUBJOB == 0
                        && i32::try_from(jobnum).ok() != Some(self.thisjob)
                    {
                        for pn in &j.procs {
                            if pn.text.windows(needle.len().max(1)).any(|w| w == needle)
                                || needle.is_empty()
                            {
                                return i32::try_from(jobnum).unwrap_or(-1);
                            }
                        }
                    }
                }
                if let Some(p) = prog
                    && !posix
                {
                    self.zwarnnam(p, &format!("job not found: {}", lossy(s)));
                }
                return -1;
            }
            let jobnum = self.findjobnam(s);
            if jobnum != -1 {
                return jobnum;
            }
            if !posix {
                self.zwarnnam(prog.unwrap_or(""), &format!("job not found: {}", lossy(s)));
            }
            return -1;
        }
        let jobnum = self.findjobnam(s);
        if jobnum != -1 {
            return jobnum;
        }
        if !posix {
            self.zwarnnam(prog.unwrap_or(""), &format!("job not found: {}", lossy(s)));
        }
        -1
    }

    /// zsh's `init_jobs`.
    pub(crate) fn init_jobs(&mut self) {
        self.jobtab = vec![Job::default(); MAXJOBS_ALLOC];
    }

    /// zsh's `expandjobtab`.
    pub(crate) fn expandjobtab(&mut self) -> bool {
        let newsize = self.jobtab.len() + MAXJOBS_ALLOC;
        if newsize > MAX_MAXJOBS {
            return false;
        }
        self.jobtab.resize(newsize, Job::default());
        true
    }

    /// zsh's `maybeshrinkjobtab`.
    pub(crate) fn maybeshrinkjobtab(&mut self) {
        self.queue_signals();
        let jobbound = self.maxjob + MAXJOBS_ALLOC - (self.maxjob % MAXJOBS_ALLOC);
        if jobbound < self.jobtab.len() && jobbound > self.maxjob + 20 {
            self.jobtab.truncate(jobbound);
        }
        self.unqueue_signals();
    }

    /// zsh's `addbgstatus`.
    pub(crate) fn addbgstatus(&mut self, pid: i32, status: i32) {
        // SAFETY: sysconf has no preconditions.
        let child_max = match unsafe { libc::sysconf(libc::_SC_CHILD_MAX) } {
            n if n > 0 => usize::try_from(n).unwrap_or(1024),
            _ => 1024,
        };
        if self.bgstatus.len() == child_max {
            let _ = self.bgstatus.pop_front();
        }
        self.bgstatus.push_back((pid, status));
    }

    fn getbgstatus(&mut self, pid: i32) -> i32 {
        if let Some(pos) = self.bgstatus.iter().position(|&(p, _)| p == pid) {
            return self.bgstatus.remove(pos).map_or(-1, |(_, s)| s);
        }
        -1
    }

    /// zsh's `bin_fg` (fg, bg, jobs, wait, disown).
    #[expect(clippy::too_many_lines, reason = "zsh's bin_fg")]
    pub(crate) fn bin_fg(
        &mut self,
        name: &str,
        argv: &[Vec<u8>],
        ops: &crate::builtin::Options,
        func: i32,
    ) -> i32 {
        let ofunc = func;
        let mut retval = 0;
        if ops.isset(b'Z') {
            if self.isset(RESTRICTED) {
                self.zwarnnam(name, "-Z is restricted");
                return 1;
            }
            if argv.len() != 1 {
                self.zwarnnam(name, "-Z requires one argument");
                return 1;
            }
            let title = crate::tok::unmetafy(argv.first().map_or(&[][..], Vec::as_slice));
            let mut t = title.clone();
            t.push(0);
            // SAFETY: t is NUL-terminated.
            unsafe {
                libc::prctl(libc::PR_SET_NAME, t.as_ptr());
            }
            return 0;
        }
        let mut lng = if func == BIN_JOBS {
            let mut l = if ops.isset(b'l') {
                1
            } else if ops.isset(b'p') {
                2
            } else {
                0
            };
            if ops.isset(b'd') {
                l |= 4;
            }
            l
        } else {
            i32::from(self.isset(LONGLISTJOBS))
        };
        if (func == BIN_FG || func == BIN_BG) && !self.jobbing() {
            self.zwarnnam(name, "no job control in this shell.");
            return 1;
        }
        self.queue_signals();
        self.wait_for_processes();
        if self.unset_opt(NOTIFY) {
            self.scanjobs();
        }
        if func != BIN_JOBS || self.isset(MONITOR) || self.oldmaxjob == 0 {
            self.setcurjob();
        }
        if func == BIN_JOBS {
            self.stopmsg = 2;
        }
        let mut firstjob = -1;
        if argv.is_empty() {
            if func == BIN_FG || func == BIN_BG || func == BIN_DISOWN {
                if self.curjob == -1
                    || self
                        .job(self.curjob)
                        .is_some_and(|j| j.stat & STAT_NOPRINT != 0)
                {
                    self.zwarnnam(name, "no current job");
                    self.unqueue_signals();
                    return 1;
                }
                firstjob = self.curjob;
            } else if func == BIN_JOBS {
                let (use_old, curmaxjob, ignorejob) =
                    if self.unset_opt(MONITOR) && self.oldmaxjob != 0 {
                        (true, self.oldmaxjob.saturating_sub(1), -1)
                    } else {
                        (false, self.maxjob, self.thisjob)
                    };
                for job in 0..=curmaxjob {
                    let stat = if use_old {
                        self.oldjobtab
                            .as_ref()
                            .and_then(|t| t.get(job))
                            .map_or(0, |j| j.stat)
                    } else {
                        self.jobtab.get(job).map_or(0, |j| j.stat)
                    };
                    if i32::try_from(job).ok() != Some(ignorejob) && stat != 0 {
                        let r = ops.isset(b'r');
                        let s = ops.isset(b's');
                        if (!r && !s)
                            || (r && s)
                            || (r && stat & STAT_STOPPED == 0)
                            || (s && stat & STAT_STOPPED != 0)
                        {
                            if use_old {
                                let saved = self.oldjobtab.take();
                                let mut tmp = saved.clone();
                                std::mem::swap(&mut self.jobtab, tmp.get_or_insert_with(Vec::new));
                                let _ = self.printjob(job, lng, 2);
                                std::mem::swap(&mut self.jobtab, tmp.get_or_insert_with(Vec::new));
                                self.oldjobtab = saved;
                            } else {
                                let _ = self.printjob(job, lng, 2);
                            }
                        }
                    }
                }
                self.unqueue_signals();
                return 0;
            } else {
                for job in 0..=self.maxjob {
                    if i32::try_from(job).ok() != Some(self.thisjob)
                        && self
                            .jobtab
                            .get(job)
                            .is_some_and(|j| j.stat != 0 && j.stat & STAT_NOPRINT == 0)
                    {
                        retval = self.zwaitjob(job, true);
                    }
                }
                self.unqueue_signals();
                return retval;
            }
        }
        let mut ai = 0usize;
        while firstjob != -1 || ai < argv.len() {
            let ocj = self.thisjob;
            let mut func = ofunc;
            let arg = argv.get(ai).cloned();
            if func == BIN_WAIT && arg.as_ref().is_some_and(|a| isanum(a)) {
                let pid = crate::utils::atoi(arg.as_deref().unwrap_or(&[]));
                let pid = i32::try_from(pid).unwrap_or(0);
                if let Some((j, _)) = self.findproc(pid, false) {
                    if self
                        .jobtab
                        .get(j)
                        .is_some_and(|x| x.stat & STAT_STOPPED != 0)
                    {
                        retval = i32::from(self.killjb(j, libc::SIGCONT) != 0);
                    }
                    if retval == 0 {
                        retval = self.waitforpid(pid, true);
                    }
                    if retval == 0 {
                        retval = self.getbgstatus(pid);
                        if retval < 0 {
                            retval = self.lastval2;
                        }
                    }
                } else {
                    retval = self.getbgstatus(pid);
                    if retval < 0 {
                        if !self.isset(POSIXBUILTINS) {
                            self.zwarnnam(name, &format!("pid {pid} is not a child of this shell"));
                        }
                        retval = 127;
                    }
                }
                self.thisjob = ocj;
                ai += 1;
                continue;
            }
            if func != BIN_JOBS && self.oldjobtab.is_some() {
                self.zwarnnam(name, "can't manipulate jobs in subshell");
                self.unqueue_signals();
                return 1;
            }
            let job = match &arg {
                Some(a) => self.getjob(a, Some(name)),
                None => firstjob,
            };
            firstjob = -1;
            if job == -1 {
                retval = 127;
                break;
            }
            let ju = usize::try_from(job).unwrap_or(0);
            let jstat = if let Some(old) = &self.oldjobtab {
                old.get(ju).map_or(0, |j| j.stat)
            } else {
                self.jobtab.get(ju).map_or(0, |j| j.stat)
            };
            if jstat & STAT_INUSE == 0 || jstat & STAT_NOPRINT != 0 {
                if !self.isset(POSIXBUILTINS) {
                    self.zwarnnam(
                        name,
                        &format!("{}: no such job", lossy(arg.as_deref().unwrap_or(&[]))),
                    );
                }
                self.unqueue_signals();
                return 127;
            }
            if self.isset(AUTOCONTINUE) && func == BIN_DISOWN && jstat & STAT_STOPPED != 0 {
                func = BIN_BG;
            }
            match func {
                BIN_FG | BIN_BG | BIN_WAIT => {
                    if func == BIN_BG {
                        self.clearoldjobtab();
                        if let Some(j) = self.jobtab.get_mut(ju) {
                            j.stat |= STAT_NOSTTY;
                            j.stat &= !STAT_CURSH;
                        }
                    }
                    let stopped = self
                        .jobtab
                        .get(ju)
                        .is_some_and(|j| j.stat & STAT_STOPPED != 0);
                    if stopped {
                        self.makerunning(ju);
                        if func == BIN_BG
                            && let Some(last) = self.jobtab.get(ju).and_then(|j| j.procs.last())
                        {
                            self.lastpid = i64::from(last.pid);
                        }
                    } else if func == BIN_BG {
                        self.zwarnnam(name, "job already in background");
                        self.thisjob = ocj;
                        self.unqueue_signals();
                        return 1;
                    }
                    if self.curjob == job {
                        self.curjob = self.prevjob;
                        self.prevjob = if func == BIN_BG { -1 } else { job };
                    }
                    if self.prevjob == job || self.prevjob == -1 {
                        self.setprevjob();
                    }
                    if self.curjob == -1 {
                        self.curjob = self.prevjob;
                        self.setprevjob();
                    }
                    if func != BIN_WAIT {
                        let _ = self.printjob(ju, if stopped { -1 } else { lng }, 3);
                    }
                    if func != BIN_BG {
                        if let Some(jpwd) = self.jobtab.get(ju).and_then(|j| j.pwd.clone())
                            && jpwd != self.pwd
                        {
                            let fout = if self.shout < 0 { 1 } else { self.shout };
                            let mut b = b"(pwd : ".to_vec();
                            b.extend(self.fprintdir(&jpwd));
                            b.extend_from_slice(b")\n");
                            write_fd(fout, &b);
                        }
                        if func != BIN_WAIT {
                            self.thisjob = job;
                            let (jst, jother, jgl, nprocs) =
                                self.jobtab.get(ju).map_or((0, 0, 0, 0), |j| {
                                    (j.stat, j.other, j.gleader, j.procs.len())
                                });
                            let other_gl = self.job(jother).map_or(0, |o| o.gleader);
                            if jst & STAT_SUPERJOB != 0
                                && (nprocs <= 1
                                    || jst & STAT_SUBLEADER != 0
                                    || (crate::signals::killpg(jgl, 0) == -1
                                        && errno() == libc::ESRCH))
                                && other_gl != 0
                            {
                                self.attachtty(other_gl);
                            } else {
                                self.attachtty(jgl);
                            }
                        }
                    }
                    if stopped {
                        if func != BIN_BG
                            && let Some(ty) = self.jobtab.get(ju).and_then(|j| j.ty)
                        {
                            self.settyinfo(&ty);
                        }
                        let _ = self.killjb(ju, libc::SIGCONT);
                    }
                    if func == BIN_WAIT {
                        retval = self.zwaitjob(ju, true);
                        if retval == 0 {
                            retval = self.lastval2;
                        }
                    } else if func != BIN_BG {
                        self.waitjobs();
                        retval = self.lastval2;
                    } else if ofunc == BIN_DISOWN {
                        self.deletejob(ju, true);
                    }
                }
                BIN_JOBS => {
                    let _ = self.printjob(ju, lng, 2);
                }
                BIN_DISOWN => {
                    let (jst, jother, jgl) = self
                        .jobtab
                        .get(ju)
                        .map_or((0, 0, 0), |j| (j.stat, j.other, j.gleader));
                    if jst & STAT_SUPERJOB != 0 {
                        if let Some(j) = self.jobtab.get_mut(ju) {
                            j.stat |= STAT_DISOWN;
                        }
                        ai += 1;
                        continue;
                    }
                    if jst & STAT_STOPPED != 0 {
                        let _ = jother;
                        let pids = format!(" -{jgl}");
                        self.zwarnnam(
                            name,
                            &format!("warning: job is suspended, use `kill -CONT{pids}' to resume"),
                        );
                    }
                    self.deletejob(ju, true);
                }
                _ => {}
            }
            self.thisjob = ocj;
            ai += 1;
        }
        lng &= 7;
        let _ = lng;
        self.unqueue_signals();
        retval
    }

    /// zsh's `bin_kill`.
    #[expect(clippy::too_many_lines, reason = "zsh's bin_kill")]
    pub(crate) fn bin_kill(&mut self, nam: &str, argv: &[Vec<u8>]) -> i32 {
        let mut sig = libc::SIGTERM;
        let mut returnval = 0;
        let mut ai = 0usize;
        let arg = |i: usize| argv.get(i).map_or(&[][..], Vec::as_slice);
        let sigcount = i32::try_from(SIGCOUNT).unwrap_or(31);
        if ai < argv.len() && arg(ai).first() == Some(&b'-') {
            let a = arg(ai);
            if a.get(1).is_some_and(u8::is_ascii_digit) {
                let (v, used) = crate::utils::zstrtol(a.get(1..).unwrap_or(&[]), 10);
                if used + 1 != a.len() {
                    self.zwarnnam(nam, &format!("invalid signal number: {}", lossy(a)));
                    return 1;
                }
                sig = i32::try_from(v).unwrap_or(0);
            } else if a.get(1) != Some(&b'-') || a.len() > 2 {
                if a.get(1) == Some(&b'l') && a.len() == 2 {
                    if ai + 1 < argv.len() {
                        let mut out = Vec::new();
                        ai += 1;
                        while ai < argv.len() {
                            let a = arg(ai);
                            let (v, used) = crate::utils::zstrtol(a, 10);
                            if used == 0 {
                                let signame = a.strip_prefix(b"SIG").unwrap_or(a);
                                let mut found = None;
                                for s in 1..=SIGCOUNT {
                                    if SIGS
                                        .get(s)
                                        .is_some_and(|n| n.as_bytes().eq_ignore_ascii_case(signame))
                                    {
                                        found = Some(s);
                                        break;
                                    }
                                }
                                if found.is_none() {
                                    for (n, num) in ALT_SIGS {
                                        if n.as_bytes().eq_ignore_ascii_case(signame) {
                                            found = Some(num);
                                            break;
                                        }
                                    }
                                }
                                match found {
                                    Some(s) if s <= SIGCOUNT => {
                                        out.extend_from_slice(format!("{s}\n").as_bytes())
                                    }
                                    _ => {
                                        self.zwarnnam(
                                            nam,
                                            &format!("unknown signal: SIG{}", lossy(signame)),
                                        );
                                        returnval += 1;
                                    }
                                }
                            } else if used < a.len() {
                                self.zwarnnam(
                                    nam,
                                    &format!(
                                        "unknown signal: SIG{}",
                                        lossy(a.get(used..).unwrap_or(&[]))
                                    ),
                                );
                                returnval += 1;
                            } else {
                                let mut s = i32::try_from(v).unwrap_or(0);
                                if libc::WIFSIGNALED(s) {
                                    s = libc::WTERMSIG(s);
                                } else if libc::WIFSTOPPED(s) {
                                    s = libc::WSTOPSIG(s);
                                }
                                if (1..=sigcount).contains(&s) {
                                    out.extend_from_slice(
                                        format!(
                                            "{}\n",
                                            SIGS.get(usize::try_from(s).unwrap_or(0))
                                                .copied()
                                                .unwrap_or("")
                                        )
                                        .as_bytes(),
                                    );
                                } else {
                                    out.extend_from_slice(format!("{s}\n").as_bytes());
                                }
                            }
                            ai += 1;
                        }
                        self.write_stdout(&out);
                        return returnval;
                    }
                    let list: Vec<&str> = SIGS.iter().skip(1).take(SIGCOUNT).copied().collect();
                    let mut s = list.join(" ");
                    s.push('\n');
                    self.write_stdout(s.as_bytes());
                    return 0;
                }
                if a.get(1) == Some(&b'n') && a.len() == 2 {
                    ai += 1;
                    if ai >= argv.len() {
                        self.zwarnnam(nam, "-n: argument expected");
                        return 1;
                    }
                    let a = arg(ai);
                    let (v, used) = crate::utils::zstrtol(a, 10);
                    if used != a.len() {
                        self.zwarnnam(nam, &format!("invalid signal number: {}", lossy(a)));
                        return 1;
                    }
                    sig = i32::try_from(v).unwrap_or(0);
                } else {
                    let signame: Vec<u8> = if !(a.get(1) == Some(&b's') && a.len() == 2) {
                        a.get(1..).unwrap_or(&[]).to_vec()
                    } else {
                        ai += 1;
                        if ai >= argv.len() {
                            self.zwarnnam(nam, "-s: argument expected");
                            return 1;
                        }
                        arg(ai).to_vec()
                    };
                    if signame.is_empty() {
                        self.zwarnnam(nam, "-: signal name expected");
                        return 1;
                    }
                    let up = self.casemodify(&signame, crate::hist::CASMOD_UPPER);
                    let signame = up.strip_prefix(b"SIG").unwrap_or(&up).to_vec();
                    let mut s = SIGCOUNT + 1;
                    for k in 1..=SIGCOUNT {
                        if SIGS
                            .get(k)
                            .is_some_and(|n| n.as_bytes() == signame.as_slice())
                        {
                            s = k;
                            break;
                        }
                    }
                    if signame == b"0" {
                        s = 0;
                    }
                    if s > SIGCOUNT {
                        for (n, num) in ALT_SIGS {
                            if n.as_bytes() == signame.as_slice() {
                                s = num;
                                break;
                            }
                        }
                    }
                    if s > SIGCOUNT {
                        self.zwarnnam(nam, &format!("unknown signal: SIG{}", lossy(&signame)));
                        self.zwarnnam(nam, "type kill -l for a list of signals");
                        return 1;
                    }
                    sig = i32::try_from(s).unwrap_or(0);
                }
            }
            ai += 1;
        }
        if ai < argv.len()
            && arg(ai).first() == Some(&b'-')
            && (arg(ai).len() == 1 || arg(ai).get(1) == Some(&b'-'))
        {
            ai += 1;
        }
        if ai >= argv.len() {
            self.zwarnnam(nam, "not enough arguments");
            return 1;
        }
        self.queue_signals();
        self.setcurjob();
        while ai < argv.len() {
            let a = arg(ai).to_vec();
            if a.first() == Some(&b'%') {
                let p = self.getjob(&a, Some(nam));
                if p == -1 {
                    returnval += 1;
                    ai += 1;
                    continue;
                }
                let pu = usize::try_from(p).unwrap_or(0);
                if self.killjb(pu, sig) == -1 {
                    self.zwarnnam(
                        "kill",
                        &format!(
                            "kill {} failed: {}",
                            lossy(&a),
                            crate::utils::strerror(errno())
                        ),
                    );
                    returnval += 1;
                    ai += 1;
                    continue;
                }
                if self
                    .jobtab
                    .get(pu)
                    .is_some_and(|j| j.stat & STAT_STOPPED != 0)
                    && ![
                        libc::SIGKILL,
                        libc::SIGCONT,
                        libc::SIGTSTP,
                        libc::SIGTTOU,
                        libc::SIGTTIN,
                        libc::SIGSTOP,
                    ]
                    .contains(&sig)
                {
                    let _ = self.killjb(pu, libc::SIGCONT);
                }
            } else if !isanum(&a) {
                self.zwarnnam("kill", &format!("illegal pid: {}", lossy(&a)));
                returnval += 1;
            } else {
                let pid = i32::try_from(crate::utils::atoi(&a)).unwrap_or(0);
                if crate::signals::kill(pid, sig) == -1 {
                    self.zwarnnam(
                        "kill",
                        &format!(
                            "kill {} failed: {}",
                            lossy(&a),
                            crate::utils::strerror(errno())
                        ),
                    );
                    returnval += 1;
                }
            }
            ai += 1;
        }
        self.unqueue_signals();
        if returnval < 126 { returnval } else { 1 }
    }

    /// zsh's `bin_suspend`.
    pub(crate) fn bin_suspend(&mut self, name: &str, force: bool) -> i32 {
        if self.islogin && !force {
            self.zwarnnam(name, "can't suspend login shell");
            return 1;
        }
        if self.jobbing() {
            crate::signals::signal_default(libc::SIGTTIN);
            crate::signals::signal_default(libc::SIGTSTP);
            crate::signals::signal_default(libc::SIGTTOU);
            self.release_pgrp();
        }
        // SAFETY: plain killpg call.
        unsafe {
            libc::killpg(self.origpgrp, libc::SIGTSTP);
        }
        if self.jobbing() {
            self.acquire_pgrp();
            crate::signals::signal_ignore(libc::SIGTTOU);
            crate::signals::signal_ignore(libc::SIGTSTP);
            crate::signals::signal_ignore(libc::SIGTTIN);
        }
        0
    }

    /// zsh's `findjobnam`.
    pub(crate) fn findjobnam(&self, s: &[u8]) -> i32 {
        for jobnum in (0..=self.maxjob).rev() {
            let Some(j) = self.jobtab.get(jobnum) else {
                continue;
            };
            if j.stat & (STAT_SUBJOB | STAT_NOPRINT) == 0
                && j.stat != 0
                && i32::try_from(jobnum).ok() != Some(self.thisjob)
                && let Some(p) = j.procs.first()
                && !p.text.is_empty()
                && p.text.starts_with(s)
            {
                return i32::try_from(jobnum).unwrap_or(-1);
            }
        }
        -1
    }

    /// zsh's `acquire_pgrp`.
    pub(crate) fn acquire_pgrp(&mut self) {
        // SAFETY: getpgrp has no preconditions.
        self.mypgrp = unsafe { libc::getpgrp() };
        if self.mypgrp >= 0 {
            let mut lastpgrp = self.mypgrp;
            let blockset =
                crate::signals::sigset_of(&[libc::SIGTTIN, libc::SIGTTOU, libc::SIGTSTP]);
            let oldset = crate::signals::signal_block(&blockset);
            let mut loop_count = 0;
            loop {
                let ttpgrp = self.gettygrp();
                if ttpgrp == -1 || ttpgrp == self.mypgrp {
                    break;
                }
                // SAFETY: getpgrp has no preconditions.
                self.mypgrp = unsafe { libc::getpgrp() };
                if self.mypgrp == self.mypid {
                    if !self.interact() {
                        break;
                    }
                    let _ = crate::signals::signal_setmask(&oldset);
                    let mp = self.mypgrp;
                    self.attachtty(mp);
                    let _ = crate::signals::signal_block(&blockset);
                }
                if self.mypgrp == self.gettygrp() {
                    break;
                }
                let _ = crate::signals::signal_setmask(&oldset);
                // SAFETY: a zero-length read may raise SIGTTIN, which is the point.
                unsafe {
                    libc::read(0, std::ptr::null_mut(), 0);
                }
                let _ = crate::signals::signal_block(&blockset);
                // SAFETY: getpgrp has no preconditions.
                self.mypgrp = unsafe { libc::getpgrp() };
                if self.mypgrp == lastpgrp {
                    if !self.interact() {
                        break;
                    }
                    loop_count += 1;
                    if loop_count == 100 {
                        break;
                    }
                }
                lastpgrp = self.mypgrp;
            }
            if self.mypgrp != self.mypid {
                // SAFETY: setpgid on ourselves.
                if unsafe { libc::setpgid(0, 0) } == 0 {
                    self.mypgrp = self.mypid;
                    let mp = self.mypgrp;
                    self.attachtty(mp);
                } else {
                    self.opts[MONITOR] = false;
                }
            }
            let _ = crate::signals::signal_setmask(&oldset);
        } else {
            self.opts[MONITOR] = false;
        }
    }

    /// zsh's `release_pgrp`.
    pub(crate) fn release_pgrp(&mut self) {
        if self.origpgrp != self.mypgrp {
            if self.origpgrp != 0 {
                let op = self.origpgrp;
                self.attachtty(op);
                // SAFETY: setpgid on ourselves.
                unsafe {
                    libc::setpgid(0, op);
                }
            }
            self.mypgrp = self.origpgrp;
        }
    }
}

/// zsh's `isanum`.
fn isanum(s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|&c| c == b'-' || c.is_ascii_digit())
}

/// Bytes as an `OsStr`.
trait OsStrCompat {
    fn from_bytes_compat(b: &[u8]) -> &std::ffi::OsStr;
}

impl OsStrCompat for std::ffi::OsStr {
    fn from_bytes_compat(b: &[u8]) -> &std::ffi::OsStr {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(b)
    }
}
