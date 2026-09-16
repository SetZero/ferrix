//! Signals and traps (zsh's `signals.c`).
//!
//! zsh runs most of its signal handling inside the handler, guarded by a
//! queue that the shell holds while it changes shared state. Here the
//! handler only records the signal; the shell runs zsh's handler body at
//! the points where zsh would have let it run: when the queue is released,
//! while suspended waiting for children, and at the check points the
//! executor passes between commands.

use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

use crate::options::*;
use crate::shell::{ERRFLAG_ERROR, ERRFLAG_INT, Shell};
use crate::signames::*;
use crate::tables::{Eprog, Shfunc};

pub(crate) const ZSIG_TRAPPED: i32 = 1 << 0;
pub(crate) const ZSIG_IGNORED: i32 = 1 << 1;
pub(crate) const ZSIG_FUNC: i32 = 1 << 2;
pub(crate) const ZSIG_MASK: i32 = ZSIG_TRAPPED | ZSIG_IGNORED | ZSIG_FUNC;
pub(crate) const ZSIG_ALIAS: i32 = 1 << 3;
pub(crate) const ZSIG_SHIFT: i32 = 4;

pub(crate) const TRAP_STATE_INACTIVE: i32 = 0;
pub(crate) const TRAP_STATE_PRIMED: i32 = 1;
pub(crate) const TRAP_STATE_FORCE_RETURN: i32 = 2;

pub(crate) const NOERREXIT_EXIT: i32 = 1;
pub(crate) const NOERREXIT_RETURN: i32 = 2;
pub(crate) const NOERREXIT_UNTIL_EXEC: i32 = 4;
pub(crate) const NOERREXIT_SIGNAL: i32 = 8;

pub(crate) const ZEXIT_NORMAL: i32 = 0;
pub(crate) const ZEXIT_SIGNAL: i32 = 1;
pub(crate) const ZEXIT_DEFERRED: i32 = 2;

/// Signals delivered and not yet handled, one bit each.
static PENDING: AtomicU64 = AtomicU64::new(0);
/// zsh's `last_signal`.
pub(crate) static LAST_SIGNAL: AtomicI32 = AtomicI32::new(-1);

extern "C" fn zhandler(sig: libc::c_int) {
    LAST_SIGNAL.store(sig, Ordering::SeqCst);
    if let Ok(s) = u32::try_from(sig)
        && s < 64
    {
        PENDING.fetch_or(1u64 << s, Ordering::SeqCst);
    }
}

/// A saved trap (zsh's `struct savetrap`).
#[derive(Debug, Clone)]
pub(crate) struct SaveTrap {
    pub(crate) sig: usize,
    pub(crate) flags: i32,
    pub(crate) local: i32,
    pub(crate) posix: bool,
    pub(crate) list: SavedTrapList,
}

#[derive(Debug, Clone)]
pub(crate) enum SavedTrapList {
    None,
    Func(Vec<u8>, Box<Shfunc>),
    List(Eprog),
}

/// An empty signal set.
pub(crate) fn empty_sigset() -> libc::sigset_t {
    // SAFETY: an all-zero sigset_t is a valid value.
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: set is a valid, writable set.
    unsafe {
        libc::sigemptyset(&mut set);
    }
    set
}

pub(crate) fn sigset_of(sigs: &[i32]) -> libc::sigset_t {
    let mut set = empty_sigset();
    for &s in sigs {
        if s != 0 {
            // SAFETY: set is a valid, writable set.
            unsafe {
                libc::sigaddset(&mut set, s);
            }
        }
    }
    set
}

/// zsh's `install_handler`.
pub(crate) fn install_handler(sig: i32) {
    // SAFETY: an all-zero sigaction is a valid value.
    let mut act: libc::sigaction = unsafe { std::mem::zeroed() };
    act.sa_sigaction = zhandler as extern "C" fn(libc::c_int) as usize;
    act.sa_mask = empty_sigset();
    act.sa_flags = 0;
    // SAFETY: act is fully initialised; zhandler is async-signal-safe (it
    // only touches atomics).
    unsafe {
        libc::sigaction(sig, &act, std::ptr::null_mut());
    }
}

/// zsh's `signal_ignore`.
pub(crate) fn signal_ignore(sig: i32) {
    // SAFETY: SIG_IGN is a valid disposition.
    unsafe {
        libc::signal(sig, libc::SIG_IGN);
    }
}

/// zsh's `signal_default`.
pub(crate) fn signal_default(sig: i32) {
    // SAFETY: SIG_DFL is a valid disposition.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
    }
}

/// `sigprocmask(how, set)`: returns the old mask.
fn sigprocmask(how: i32, set: &libc::sigset_t) -> libc::sigset_t {
    let mut old = empty_sigset();
    // SAFETY: both sets are valid.
    unsafe {
        libc::sigprocmask(how, set, &mut old);
    }
    old
}

/// zsh's `signal_block`: returns the old mask.
pub(crate) fn signal_block(set: &libc::sigset_t) -> libc::sigset_t {
    sigprocmask(libc::SIG_BLOCK, set)
}

/// zsh's `signal_unblock`.
pub(crate) fn signal_unblock(set: &libc::sigset_t) -> libc::sigset_t {
    sigprocmask(libc::SIG_UNBLOCK, set)
}

/// zsh's `signal_setmask`.
pub(crate) fn signal_setmask(set: &libc::sigset_t) -> libc::sigset_t {
    sigprocmask(libc::SIG_SETMASK, set)
}

/// `killpg`.
pub(crate) fn killpg(pgrp: i32, sig: i32) -> i32 {
    // SAFETY: killpg has no memory preconditions.
    unsafe { libc::killpg(pgrp, sig) }
}

/// `kill`.
pub(crate) fn kill(pid: i32, sig: i32) -> i32 {
    // SAFETY: kill has no memory preconditions.
    unsafe { libc::kill(pid, sig) }
}

/// zsh's `signal_mask(sig)`.
pub(crate) fn signal_mask(sig: i32) -> libc::sigset_t {
    sigset_of(&[sig])
}

/// zsh's `child_block`.
pub(crate) fn child_block() {
    let _ = signal_block(&signal_mask(libc::SIGCHLD));
}

/// zsh's `child_unblock`.
pub(crate) fn child_unblock() {
    let _ = signal_unblock(&signal_mask(libc::SIGCHLD));
}

/// zsh's `winch_block`.
pub(crate) fn winch_block() {
    let _ = signal_block(&signal_mask(libc::SIGWINCH));
}

/// zsh's `winch_unblock`.
pub(crate) fn winch_unblock() {
    let _ = signal_unblock(&signal_mask(libc::SIGWINCH));
}

impl Shell {
    /// zsh's `queue_signals`.
    pub(crate) fn queue_signals(&mut self) {
        self.queueing_enabled += 1;
    }

    /// zsh's `unqueue_signals`.
    pub(crate) fn unqueue_signals(&mut self) {
        self.queueing_enabled -= 1;
        if self.queueing_enabled <= 0 {
            self.queueing_enabled = 0;
            self.run_queued_signals();
        }
    }

    /// zsh's `queue_signal_level`.
    pub(crate) fn queue_signal_level(&self) -> i32 {
        self.queueing_enabled
    }

    /// zsh's `dont_queue_signals`.
    pub(crate) fn dont_queue_signals(&mut self) {
        self.queueing_enabled = 0;
        self.run_queued_signals();
    }

    /// zsh's `restore_queue_signals`.
    pub(crate) fn restore_queue_signals(&mut self, q: i32) {
        self.queueing_enabled = q;
    }

    /// Run any signals delivered while queueing, as zsh's
    /// `run_queued_signals` does.
    pub(crate) fn run_queued_signals(&mut self) {
        loop {
            let pending = PENDING.swap(0, Ordering::SeqCst);
            if pending == 0 {
                break;
            }
            for sig in 1..64i32 {
                if pending & (1u64 << sig) != 0 {
                    self.zhandler_body(sig);
                }
            }
        }
    }

    /// A check point: handle signals now unless they are queued.
    pub(crate) fn check_signals(&mut self) {
        if self.queueing_enabled == 0 {
            self.run_queued_signals();
        }
    }

    /// zsh's `intr`.
    pub(crate) fn intr(&self) {
        if self.interact() {
            install_handler(libc::SIGINT);
        }
    }

    /// zsh's `holdintr`.
    pub(crate) fn holdintr(&self) {
        if self.interact() {
            let _ = signal_block(&signal_mask(libc::SIGINT));
        }
    }

    /// zsh's `noholdintr`.
    pub(crate) fn noholdintr(&self) {
        if self.interact() {
            let _ = signal_unblock(&signal_mask(libc::SIGINT));
        }
    }

    /// zsh's `signal_suspend`.
    pub(crate) fn signal_suspend(&mut self, wait_cmd: bool) -> i32 {
        let mut sigs = Vec::new();
        if !(wait_cmd
            || self.isset(TRAPSASYNC)
            || (self
                .sigtrapped
                .get(libc::SIGINT as usize)
                .copied()
                .unwrap_or(0)
                & !ZSIG_IGNORED)
                != 0)
        {
            sigs.push(libc::SIGINT);
        }
        let set = sigset_of(&sigs);
        // SAFETY: the set is valid.
        let ret = unsafe { libc::sigsuspend(&set) };
        // The handler ran inside sigsuspend; run its body now.
        let q = self.queueing_enabled;
        self.queueing_enabled = 0;
        self.run_queued_signals();
        self.queueing_enabled = q;
        ret
    }

    /// zsh's `wait_for_processes`.
    pub(crate) fn wait_for_processes(&mut self) {
        loop {
            let mut status: libc::c_int = 0;
            // SAFETY: an all-zero rusage is valid.
            let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
            // SAFETY: the out pointers are valid.
            let pid = unsafe {
                libc::wait4(
                    -1,
                    &mut status,
                    libc::WNOHANG | libc::WUNTRACED | libc::WCONTINUED,
                    &mut ru,
                )
            };
            if pid == 0 {
                break;
            }
            let old_errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if pid > 0 {
                // Process substitution children.
                let mut cont = false;
                if pid == self.cmdoutpid {
                    self.cmdoutpid = 0;
                    self.cmdoutval = if libc::WIFSIGNALED(status) {
                        0o200 | libc::WTERMSIG(status)
                    } else {
                        libc::WEXITSTATUS(status)
                    };
                    self.use_cmdoutval = true;
                    self.get_usage();
                    cont = true;
                } else {
                    for es in self.exstack.iter_mut().rev() {
                        if pid == es.cmdoutpid {
                            es.cmdoutpid = 0;
                            es.cmdoutval = if libc::WIFSIGNALED(status) {
                                0o200 | libc::WTERMSIG(status)
                            } else {
                                libc::WEXITSTATUS(status)
                            };
                            self.use_cmdoutval = true;
                            cont = true;
                            break;
                        }
                    }
                    if cont {
                        self.get_usage();
                    }
                }
                if cont {
                    continue;
                }
            }
            if pid == -1 {
                if old_errno != libc::ECHILD {
                    self.zerr(&format!(
                        "wait failed: {}",
                        crate::utils::strerror(old_errno)
                    ));
                }
                break;
            }
            self.queue_signals();
            let mut found = None;
            if let Some((j, p)) = self.findproc(pid, false) {
                found = Some(j);
                let builtin_stop = (self
                    .jobtab
                    .get(j)
                    .is_some_and(|jb| jb.stat & crate::jobs::STAT_BUILTIN != 0)
                    || (self.list_pipe
                        && (self.thisjob == -1
                            || self
                                .job(self.thisjob)
                                .is_some_and(|jb| jb.stat & crate::jobs::STAT_BUILTIN != 0))))
                    && libc::WIFSTOPPED(status)
                    && libc::WSTOPSIG(status) == libc::SIGTSTP;
                if builtin_stop {
                    let _ = self.killjb(j, libc::SIGCONT);
                    self.zwarn("job can't be suspended");
                } else {
                    let now = crate::params::now_tv();
                    let mypgrp = self.mypgrp;
                    let last_attached = self.last_attached_pgrp;
                    let mut reclaim = false;
                    if let Some(jb) = self.jobtab.get_mut(j)
                        && let Some(pn) = jb.procs.get_mut(p)
                    {
                        pn.endtime = now;
                        pn.status = if libc::WIFCONTINUED(status) {
                            crate::jobs::SP_RUNNING
                        } else {
                            status
                        };
                        pn.ti = ru;
                        if libc::WIFEXITED(status)
                            && pn.pid == jb.gleader
                            && killpg(pn.pid, 0) == -1
                            && errno() == libc::ESRCH
                        {
                            if last_attached == jb.gleader
                                && jb.stat & crate::jobs::STAT_NOSTTY == 0
                            {
                                reclaim = true;
                            }
                            jb.gleader = 0;
                        }
                    }
                    if reclaim {
                        self.attachtty(mypgrp);
                        self.adjustwinsize(0);
                    }
                }
                self.update_job(j);
            } else if let Some((j, p)) = self.findproc(pid, true) {
                found = Some(j);
                if let Some(pn) = self.jobtab.get_mut(j).and_then(|jb| jb.auxprocs.get_mut(p)) {
                    pn.status = status;
                }
                self.update_job(j);
            } else {
                self.get_usage();
            }
            if let Some(j) = found
                && self.jobtab.get(j).is_some_and(|jb| {
                    jb.stat & (crate::jobs::STAT_CURSH | crate::jobs::STAT_BUILTIN) == 0
                })
                && i32::try_from(j).ok() != Some(self.thisjob)
            {
                let val = crate::jobs::status_value(status);
                self.addbgstatus(pid, val);
            }
            self.unqueue_signals();
        }
    }

    /// The body of zsh's `zhandler` for `sig`.
    fn zhandler_body(&mut self, sig: i32) {
        match sig {
            libc::SIGCHLD => self.wait_for_processes(),
            libc::SIGPIPE => {
                if !self.handletrap(sig) {
                    if !self.interact() {
                        crate::shell::exit_now(libc::SIGPIPE);
                    // SAFETY: isatty has no preconditions.
                    } else if unsafe { libc::isatty(self.shtty) } == 0 {
                        self.stopmsg = 1;
                        self.zexit(libc::SIGPIPE, ZEXIT_SIGNAL);
                    }
                }
            }
            libc::SIGHUP => {
                if !self.handletrap(sig) {
                    self.stopmsg = 1;
                    self.zexit(libc::SIGHUP, ZEXIT_SIGNAL);
                }
            }
            libc::SIGINT => {
                if !self.handletrap(sig) {
                    if (self.isset(PRIVILEGED) || self.isset(RESTRICTED))
                        && self.isset(INTERACTIVE)
                        && self.noerrexit & NOERREXIT_SIGNAL != 0
                    {
                        self.zexit(libc::SIGINT, ZEXIT_SIGNAL);
                    }
                    self.errflag.set(self.errflag.get() | ERRFLAG_INT);
                    if self.list_pipe || self.chline_active || self.simple_pline {
                        self.breaks = self.loops;
                        self.inerrflush();
                        self.check_cursh_sig(libc::SIGINT);
                    }
                    self.lastval = 128 + libc::SIGINT;
                }
            }
            libc::SIGWINCH => {
                self.adjustwinsize(1);
                let _ = self.handletrap(sig);
            }
            libc::SIGALRM => {
                if !self.handletrap(sig) {
                    let idle = self.ttyidle();
                    let tmout = self.getiparam(b"TMOUT");
                    if idle >= 0 && idle < tmout {
                        // SAFETY: alarm has no preconditions.
                        unsafe {
                            libc::alarm(u32::try_from(tmout - idle).unwrap_or(0));
                        }
                    } else {
                        self.errflag.set(0);
                        self.noerrs = 0;
                        self.zwarn("timeout");
                        self.stopmsg = 1;
                        self.zexit(libc::SIGALRM, ZEXIT_SIGNAL);
                    }
                }
            }
            _ => {
                let _ = self.handletrap(sig);
            }
        }
    }

    /// zsh's `killrunjobs`.
    pub(crate) fn killrunjobs(&mut self, from_signal: bool) {
        if self.unset_opt(HUP) {
            return;
        }
        let mut killed = 0;
        // SAFETY: getpid has no preconditions.
        let me = unsafe { libc::getpid() };
        for i in 1..=self.maxjob {
            let Some(jb) = self.jobtab.get(i) else {
                continue;
            };
            if (from_signal || i32::try_from(i).ok() != Some(self.thisjob))
                && jb.stat & crate::jobs::STAT_LOCKED != 0
                && jb.stat & crate::jobs::STAT_NOPRINT == 0
                && jb.stat & crate::jobs::STAT_STOPPED == 0
                && jb.gleader != me
                && killpg(jb.gleader, libc::SIGHUP) != -1
            {
                killed += 1;
            }
        }
        if killed != 0 {
            self.zwarn(&format!("warning: {killed} jobs SIGHUPed"));
        }
    }

    /// zsh's `killjb`.
    pub(crate) fn killjb(&mut self, j: usize, sig: i32) -> i32 {
        let Some(jn) = self.jobtab.get(j).cloned() else {
            return -1;
        };
        let mut err = 0;
        if self.jobbing() {
            if jn.stat & crate::jobs::STAT_SUPERJOB != 0 {
                let other = usize::try_from(jn.other).unwrap_or(0);
                let sub = self.jobtab.get(other).cloned().unwrap_or_default();
                if sig == libc::SIGCONT {
                    for pn in &sub.procs {
                        if killpg(pn.pid, sig) == -1
                            && kill(pn.pid, sig) == -1
                            && errno() != libc::ESRCH
                        {
                            err = -1;
                        }
                    }
                    let n = jn.procs.len();
                    for pn in jn.procs.iter().take(n.saturating_sub(1)) {
                        if kill(pn.pid, sig) == -1 && errno() != libc::ESRCH {
                            err = -1;
                        }
                    }
                    if sub.procs.is_empty()
                        && let Some(pn) = jn.procs.last()
                        && kill(pn.pid, sig) == -1
                        && errno() != libc::ESRCH
                    {
                        err = -1;
                    }
                    if err != -1 {
                        self.makerunning(j);
                    }
                    return err;
                }
                if killpg(sub.gleader, sig) == -1 && errno() != libc::ESRCH {
                    err = -1;
                }
                if killpg(jn.gleader, sig) == -1 && errno() != libc::ESRCH {
                    err = -1;
                }
                return err;
            }
            err = killpg(jn.gleader, sig);
            if sig == libc::SIGCONT && err != -1 {
                self.makerunning(j);
            }
            return err;
        }
        for pn in &jn.procs {
            if pn.status == crate::jobs::SP_RUNNING || libc::WIFSTOPPED(pn.status) {
                err = kill(pn.pid, sig);
                if err == -1 && errno() != libc::ESRCH && sig != 0 {
                    return -1;
                }
            }
        }
        err
    }

    /// zsh's `dosavetrap`.
    fn dosavetrap(&mut self, sig: usize, level: i32) {
        let flags = self.sigtrapped.get(sig).copied().unwrap_or(0);
        let list = if flags & ZSIG_FUNC != 0 {
            match self.gettrapnode(sig, true) {
                Some((name, shf)) => SavedTrapList::Func(name, Box::new(shf)),
                None => SavedTrapList::None,
            }
        } else if flags != 0 {
            match self.siglists.get(sig).cloned().flatten() {
                Some(l) => SavedTrapList::List(l),
                None => SavedTrapList::None,
            }
        } else {
            SavedTrapList::None
        };
        let st = SaveTrap {
            sig,
            flags,
            local: level,
            posix: if sig == SIGEXIT {
                self.exit_trap_posix
            } else {
                false
            },
            list,
        };
        self.savetraps.insert(0, st);
    }

    /// zsh's `settrap`.
    pub(crate) fn settrap(&mut self, sig: i32, l: Option<Eprog>, flags: i32) -> i32 {
        let Ok(s) = usize::try_from(sig) else {
            return 1;
        };
        if self.jobbing() && (sig == libc::SIGTTOU || sig == libc::SIGTSTP || sig == libc::SIGTTIN)
        {
            self.zerr(&format!(
                "can't trap SIG{} in interactive shells",
                SIGS.get(s).copied().unwrap_or("")
            ));
            return 1;
        }
        self.queue_signals();
        self.unsettrap(sig);
        let empty = l.as_ref().is_none_or(|p| p.list.items.is_empty());
        if let Some(slot) = self.siglists.get_mut(s) {
            *slot = l;
        }
        let real = s != 0 && s <= SIGCOUNT && sig != libc::SIGWINCH && sig != libc::SIGCHLD;
        if flags & ZSIG_FUNC == 0 && empty {
            if let Some(t) = self.sigtrapped.get_mut(s) {
                *t = ZSIG_IGNORED;
            }
            if real {
                signal_ignore(sig);
            }
        } else {
            self.nsigtrapped += 1;
            if let Some(t) = self.sigtrapped.get_mut(s) {
                *t = ZSIG_TRAPPED;
            }
            if real {
                install_handler(sig);
            }
        }
        let locallevel = self.locallevel;
        let posixtraps = self.isset(POSIXTRAPS);
        if let Some(t) = self.sigtrapped.get_mut(s) {
            *t |= flags;
            if s == SIGEXIT {
                if !posixtraps {
                    *t |= locallevel << ZSIG_SHIFT;
                }
            } else {
                *t |= locallevel << ZSIG_SHIFT;
            }
        }
        if s == SIGEXIT {
            self.exit_trap_posix = posixtraps;
        }
        self.unqueue_signals();
        0
    }

    /// zsh's `unsettrap`.
    pub(crate) fn unsettrap(&mut self, sig: i32) {
        self.queue_signals();
        let _ = self.removetrap(sig);
        self.unqueue_signals();
    }

    /// zsh's `removetrap`: the trap function removed, if any.
    pub(crate) fn removetrap(&mut self, sig: i32) -> Option<(Vec<u8>, Shfunc)> {
        let s = usize::try_from(sig).ok()?;
        if self.jobbing() && (sig == libc::SIGTTOU || sig == libc::SIGTSTP || sig == libc::SIGTTIN)
        {
            return None;
        }
        self.queue_signals();
        let trapped = self.sigtrapped.get(s).copied().unwrap_or(0);
        let should_save = if s == SIGEXIT {
            !self.isset(POSIXTRAPS)
        } else {
            self.isset(LOCALTRAPS)
        };
        if self.dontsavetrap == 0
            && should_save
            && self.locallevel != 0
            && (trapped == 0 || self.locallevel > (trapped >> ZSIG_SHIFT))
        {
            let ll = self.locallevel;
            self.dosavetrap(s, ll);
        }
        if trapped & ZSIG_TRAPPED != 0 {
            self.nsigtrapped -= 1;
        }
        if let Some(t) = self.sigtrapped.get_mut(s) {
            *t = 0;
        }
        if sig == libc::SIGINT && self.interact() {
            self.intr();
            self.noholdintr();
        } else if sig == libc::SIGHUP
            || (sig == libc::SIGPIPE && self.interact() && self.forklevel == 0)
        {
            install_handler(sig);
        } else if s != 0 && s <= SIGCOUNT && sig != libc::SIGWINCH && sig != libc::SIGCHLD {
            signal_default(sig);
        }
        if s == SIGEXIT {
            self.exit_trap_posix = false;
        }
        if trapped & ZSIG_FUNC != 0 {
            let node = self.gettrapnode(s, true);
            if let Some((name, _)) = &node {
                let _ = self.shfunctab.remove(name);
            }
            self.unqueue_signals();
            return node;
        } else if let Some(slot) = self.siglists.get_mut(s) {
            *slot = None;
        }
        self.unqueue_signals();
        None
    }

    /// zsh's `starttrapscope`.
    pub(crate) fn starttrapscope(&mut self) {
        if self.intrap != 0 {
            return;
        }
        if self.sigtrapped.get(SIGEXIT).copied().unwrap_or(0) != 0 && !self.exit_trap_posix {
            self.locallevel += 1;
            self.unsettrap(0);
            self.locallevel -= 1;
        }
    }

    /// zsh's `endtrapscope`.
    pub(crate) fn endtrapscope(&mut self) {
        let mut exittr = 0;
        let mut exitfn: Option<SavedTrapList> = None;
        if self.intrap == 0 && !self.exit_trap_posix {
            exittr = self.sigtrapped.get(SIGEXIT).copied().unwrap_or(0);
            if exittr != 0 {
                if exittr & ZSIG_FUNC != 0 {
                    exitfn = self
                        .shfunctab
                        .remove(b"TRAPEXIT")
                        .map(|f| SavedTrapList::Func(b"TRAPEXIT".to_vec(), Box::new(f)));
                } else {
                    exitfn = self
                        .siglists
                        .get_mut(SIGEXIT)
                        .and_then(Option::take)
                        .map(SavedTrapList::List);
                }
                if exittr & ZSIG_TRAPPED != 0 {
                    self.nsigtrapped -= 1;
                }
                if let Some(t) = self.sigtrapped.get_mut(SIGEXIT) {
                    *t = 0;
                }
            }
        }
        while let Some(st) = self.savetraps.first().cloned() {
            if st.local <= self.locallevel {
                break;
            }
            let _ = self.savetraps.remove(0);
            let sig = st.sig;
            let has_list = !matches!(st.list, SavedTrapList::None);
            if st.flags != 0 && has_list {
                self.dontsavetrap += 1;
                let sig_i = i32::try_from(sig).unwrap_or(-1);
                match &st.list {
                    SavedTrapList::Func(..) => {
                        let _ = self.settrap(sig_i, None, ZSIG_FUNC);
                    }
                    SavedTrapList::List(l) => {
                        let _ = self.settrap(sig_i, Some(l.clone()), 0);
                    }
                    SavedTrapList::None => {}
                }
                if sig == SIGEXIT {
                    self.exit_trap_posix = st.posix;
                }
                self.dontsavetrap -= 1;
                if let Some(t) = self.sigtrapped.get_mut(sig) {
                    *t = st.flags;
                }
                if st.flags & ZSIG_FUNC != 0
                    && let SavedTrapList::Func(name, f) = st.list
                {
                    let _ = self.shfunctab.insert(name, *f);
                }
            } else if self.sigtrapped.get(sig).copied().unwrap_or(0) != 0
                && (sig != SIGEXIT || !self.exit_trap_posix)
            {
                self.unsettrap(i32::try_from(sig).unwrap_or(-1));
            }
        }
        if exittr != 0 {
            let mut tr = exittr;
            if let Some(f) = exitfn {
                self.dotrapargs(SIGEXIT, &mut tr, Some(f));
            }
        }
    }

    /// zsh's `handletrap`.
    fn handletrap(&mut self, sig: i32) -> bool {
        let Ok(s) = usize::try_from(sig) else {
            return false;
        };
        if self.sigtrapped.get(s).copied().unwrap_or(0) == 0 {
            return false;
        }
        if self.trap_queueing_enabled {
            self.trap_queue.push_back(s);
            return true;
        }
        self.dotrap(s);
        if sig == libc::SIGALRM {
            let tmout = self.getiparam(b"TMOUT");
            if tmout != 0 {
                // SAFETY: alarm has no preconditions.
                unsafe {
                    libc::alarm(u32::try_from(tmout).unwrap_or(0));
                }
            }
        }
        true
    }

    /// zsh's `queue_traps`.
    pub(crate) fn queue_traps(&mut self, wait_cmd: bool) {
        if !self.isset(TRAPSASYNC) && !wait_cmd {
            self.trap_queueing_enabled = true;
        }
    }

    /// zsh's `unqueue_traps`.
    pub(crate) fn unqueue_traps(&mut self) {
        self.trap_queueing_enabled = false;
        while let Some(s) = self.trap_queue.pop_front() {
            let _ = self.handletrap(i32::try_from(s).unwrap_or(0));
        }
    }

    /// zsh's `dotrapargs`.
    fn dotrapargs(&mut self, sig: usize, sigtr: &mut i32, sigfn: Option<SavedTrapList>) {
        let Some(sigfn) = sigfn else { return };
        if *sigtr & ZSIG_IGNORED != 0 || matches!(sigfn, SavedTrapList::None) || self.errflag() {
            return;
        }
        if self.intrap != 0 && (sig == SIGEXIT || sig == SIGDEBUG || sig == SIGZERR) {
            return;
        }
        let obreaks = self.breaks;
        let oretflag = self.retflag;
        let olastval = self.lastval;
        self.queue_signals();
        self.intrap += 1;
        *sigtr |= ZSIG_IGNORED;
        self.set_sigtrapped_ignored(sig, true);
        self.execsave();
        self.breaks = 0;
        self.retflag = false;
        self.traplocallevel = self.locallevel;
        let isfunc;
        match sigfn {
            SavedTrapList::Func(fname, shf) => {
                let name = match self.gettrapnode(sig, false) {
                    Some((n, _)) => n,
                    None => fname,
                };
                let args = vec![name, sig.to_string().into_bytes()];
                self.trap_return = -1;
                self.trap_state = TRAP_STATE_PRIMED;
                self.trapisfunc = true;
                isfunc = true;
                let osc = self.sfcontext;
                self.sfcontext = crate::exec::SFC_SIGNAL;
                let _ = self.doshfunc(&shf, Some(args), true);
                self.sfcontext = osc;
            }
            SavedTrapList::List(prog) => {
                self.trap_return = -2;
                self.trap_state = TRAP_STATE_PRIMED;
                self.trapisfunc = false;
                isfunc = false;
                self.execode(&prog, true, false, "trap");
            }
            SavedTrapList::None => {
                isfunc = false;
            }
        }
        let traperr = self.errflag.get();
        let new_trap_state = self.trap_state;
        let new_trap_return = self.trap_return;
        self.execrestore();
        if new_trap_state == TRAP_STATE_FORCE_RETURN && !(isfunc && new_trap_return == 0) {
            if isfunc {
                self.breaks = self.loops;
                if sig == libc::SIGINT as usize || sig == libc::SIGQUIT as usize {
                    self.errflag.set(self.errflag.get() | ERRFLAG_INT);
                } else {
                    self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
                }
            }
            self.lastval = new_trap_return;
            self.retflag = true;
        } else {
            if traperr != 0 && !self.emulation_is(EMULATE_SH) {
                self.lastval = 1;
            } else {
                self.lastval = olastval;
            }
            if self.try_tryflag != 0 {
                if traperr != 0 {
                    self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
                } else {
                    self.errflag.set(self.errflag.get() & !ERRFLAG_ERROR);
                }
            }
            self.breaks += obreaks;
            self.retflag = oretflag;
            if self.breaks > self.loops {
                self.breaks = self.loops;
            }
        }
        if *sigtr != ZSIG_IGNORED {
            *sigtr &= !ZSIG_IGNORED;
        }
        self.set_sigtrapped_ignored(sig, false);
        self.intrap -= 1;
        self.unqueue_signals();
    }

    /// Mirror the ZSIG_IGNORED bit dotrapargs sets on `*sigtr` into the
    /// table when `sigtr` points into it.
    fn set_sigtrapped_ignored(&mut self, sig: usize, on: bool) {
        if !self.dotrap_in_table {
            return;
        }
        if let Some(t) = self.sigtrapped.get_mut(sig) {
            if on {
                *t |= ZSIG_IGNORED;
            } else if *t != ZSIG_IGNORED {
                *t &= !ZSIG_IGNORED;
            }
        }
    }

    /// zsh's `dotrap`.
    pub(crate) fn dotrap(&mut self, sig: usize) {
        let q = self.queue_signal_level();
        let flags = self.sigtrapped.get(sig).copied().unwrap_or(0);
        let funcprog = if flags & ZSIG_FUNC != 0 {
            self.gettrapnode(sig, false)
                .map(|(n, f)| SavedTrapList::Func(n, Box::new(f)))
        } else {
            self.siglists
                .get(sig)
                .cloned()
                .flatten()
                .map(SavedTrapList::List)
        };
        if flags & ZSIG_IGNORED != 0 || funcprog.is_none() || self.errflag() {
            return;
        }
        self.dont_queue_signals();
        if sig == SIGEXIT {
            self.in_exit_trap += 1;
        }
        let mut tr = flags;
        self.dotrap_in_table = true;
        self.dotrapargs(sig, &mut tr, funcprog);
        self.dotrap_in_table = false;
        if sig == SIGEXIT {
            self.in_exit_trap -= 1;
        }
        self.restore_queue_signals(q);
    }

    /// zsh's `getsignum`.
    pub(crate) fn getsignum(s: &[u8]) -> i32 {
        let x = crate::utils::atoi(s);
        if s.first().is_some_and(u8::is_ascii_digit)
            && x >= 0
            && usize::try_from(x).is_ok_and(|v| v < VSIGCOUNT)
        {
            return i32::try_from(x).unwrap_or(-1);
        }
        let name = s.strip_prefix(b"SIG").unwrap_or(s);
        for (i, n) in SIGS.iter().enumerate() {
            if n.as_bytes() == name {
                return i32::try_from(i).unwrap_or(-1);
            }
        }
        for (n, num) in ALT_SIGS {
            if n.as_bytes() == name {
                return i32::try_from(num).unwrap_or(-1);
            }
        }
        -1
    }

    /// zsh's `getsigname`.
    pub(crate) fn getsigname(&self, sig: usize) -> &'static str {
        if self.sigtrapped.get(sig).copied().unwrap_or(0) & ZSIG_ALIAS != 0 {
            for (n, num) in ALT_SIGS {
                if num == sig {
                    return n;
                }
            }
            ""
        } else {
            SIGS.get(sig).copied().unwrap_or("")
        }
    }

    /// zsh's `gettrapnode`: the name and function of `sig`'s trap.
    pub(crate) fn gettrapnode(&self, sig: usize, ignoredisable: bool) -> Option<(Vec<u8>, Shfunc)> {
        let get = |name: &[u8]| -> Option<Shfunc> {
            let f = self.shfunctab.get(name)?;
            if !ignoredisable && f.flags & crate::tables::DISABLED != 0 {
                return None;
            }
            Some(f.clone())
        };
        let fname = format!("TRAP{}", SIGS.get(sig).copied().unwrap_or("")).into_bytes();
        if let Some(f) = get(&fname) {
            return Some((fname, f));
        }
        for (n, num) in ALT_SIGS {
            if num == sig {
                let fname = format!("TRAP{n}").into_bytes();
                if let Some(f) = get(&fname) {
                    return Some((fname, f));
                }
            }
        }
        None
    }

    /// zsh's `removetrapnode`.
    pub(crate) fn removetrapnode(&mut self, sig: usize) {
        if let Some((name, _)) = self.gettrapnode(sig, true) {
            let _ = self.shfunctab.remove(&name);
        }
    }
}

/// The current `errno`.
pub(crate) fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}
