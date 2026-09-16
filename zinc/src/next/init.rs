//! Start-up and the command loop (zsh's `init.c`): `loop`, `source`,
//! `sourcehome`, `init_io` and `init_shout`, and `checkjobs` from
//! `builtin.c`.
//!
//! zsh's `loop` reads events from `SHIN` through its input stack. zinc-next
//! parses from a lexer over the whole text instead, so `source` reads the
//! file first and `zloop` takes that text; the event-at-a-time parse keeps
//! zsh's order, in which an alias defined on one line applies to the next.

use crate::jobs::{STAT_LOCKED, STAT_NOPRINT, STAT_STOPPED};
use crate::options::*;
use crate::shell::{ERRFLAG_ERROR, Shell, write_fd};
use crate::signals::TRAP_STATE_INACTIVE;
use crate::tables::Eprog;
use crate::tok;

/// zsh's `enum loop_return`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopReturn {
    Ok,
    Empty,
    Error,
}

/// zsh's `enum source_return`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceReturn {
    Ok,
    NotFound,
    Error,
}

fn open_rdwr(path: &[u8]) -> i32 {
    match std::ffi::CString::new(path) {
        // SAFETY: c is NUL-terminated.
        Ok(c) => unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) },
        Err(_) => -1,
    }
}

fn ttyname_of(fd: i32) -> Option<Vec<u8>> {
    // SAFETY: ttyname returns NULL or a NUL-terminated static string.
    let p = unsafe { libc::ttyname(fd) };
    if p.is_null() {
        return None;
    }
    // SAFETY: p is non-null and NUL-terminated.
    Some(unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes().to_vec())
}

fn isatty(fd: i32) -> bool {
    // SAFETY: isatty has no memory preconditions.
    unsafe { libc::isatty(fd) == 1 }
}

/// `rdwrtty(fd)`: the descriptor is open read-write.
fn rdwrtty(fd: i32) -> bool {
    // SAFETY: F_GETFL has no memory preconditions.
    let fl = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    fl != -1 && fl & libc::O_RDWR == libc::O_RDWR
}

impl Shell {
    /// zsh's `loop` over the metafied `text`.
    pub(crate) fn zloop(&mut self, text: &[u8], toplevel: bool, justonce: bool) -> LoopReturn {
        let mut non_empty = false;
        self.queue_signals();
        let mut lx = crate::lex::Lexer::new(text.to_vec(), self.lex_opts());
        lx.input.lineno = u64::try_from(self.lineno.max(1)).unwrap_or(1);
        loop {
            self.intr();
            lx.opts = self.lex_opts();
            let parsed = {
                let mut p = crate::parse::Parser::new(&mut lx, &*self);
                p.parse_event()
            };
            match parsed {
                Ok(None) => break,
                Err(e) => {
                    self.lineno = i64::try_from(e.lineno).unwrap_or(0);
                    if self.noerrs < 2 {
                        self.zerr(&e.msg);
                    }
                    if self.lastval == 0 {
                        self.lastval = 1;
                    }
                    // The lexer cannot resynchronise inside a text, which is
                    // where zsh stops too unless it reads a terminal.
                    break;
                }
                Ok(Some(list)) => {
                    non_empty = true;
                    let prog = Eprog::new(list);
                    if toplevel
                        && (self.getshfunc(b"preexec").is_some()
                            || self.paramtab().get(b"preexec_functions").is_some())
                    {
                        let args = vec![
                            b"preexec".to_vec(),
                            Vec::new(),
                            self.getjobtext_list(&prog.list),
                            self.getpermtext(&prog.list, false),
                        ];
                        let _ = self.callhookfunc(b"preexec", Some(args), true);
                        self.errflag.set(self.errflag.get() & !ERRFLAG_ERROR);
                    }
                    if self.stopmsg != 0 {
                        self.stopmsg -= 1;
                    }
                    self.execode(
                        &prog,
                        false,
                        false,
                        if toplevel { "toplevel" } else { "file" },
                    );
                    if toplevel {
                        self.noexitct = 0;
                    }
                }
            }
            if self.subsh {
                self.realexit();
            }
            if ((!self.interact() || self.sourcelevel != 0) && self.errflag()) || self.retflag {
                break;
            }
            if self.isset(SINGLECOMMAND) && toplevel {
                self.dont_queue_signals();
                if self.sigtrapped.first().copied().unwrap_or(0) != 0 {
                    self.dotrap(crate::signames::SIGEXIT);
                }
                self.realexit();
            }
            if justonce {
                break;
            }
        }
        let err = self.errflag();
        self.unqueue_signals();
        if err {
            LoopReturn::Error
        } else if !non_empty {
            LoopReturn::Empty
        } else {
            LoopReturn::Ok
        }
    }

    /// zsh's `source`: `s` is metafied.
    pub(crate) fn source(&mut self, s: &[u8]) -> SourceReturn {
        let us = tok::unmetafy(s);
        let Ok(raw) =
            std::fs::read(std::os::unix::ffi::OsStrExt::from_bytes(&us) as &std::ffi::OsStr)
        else {
            return SourceReturn::NotFound;
        };
        let text = tok::metafy(&raw);
        let osubsh = self.subsh;
        let cj = self.thisjob;
        let oldlineno = self.lineno;
        let oloops = self.loops;
        let oldshst = self.opts[SHINSTDIN];
        let ocs = std::mem::take(&mut self.cmdstack);
        let old_scriptname = self.scriptname.clone();
        let old_scriptfilename = self.scriptfilename.clone();
        let otrap_return = self.trap_return;
        let otrap_state = self.trap_state;
        let mut ret = SourceReturn::Ok;

        self.subsh = false;
        self.lineno = 1;
        self.loops = 0;
        let _ = self.dosetopt(i32::try_from(SHINSTDIN).unwrap_or(0), false, true);
        self.scriptname = Some(s.to_vec());
        self.scriptfilename = Some(s.to_vec());
        if self.isset(SOURCETRACE) {
            self.printprompt4();
            write_fd(self.xtrerr_fd(), b"<sourcetrace>\n");
        }
        self.trap_state = TRAP_STATE_INACTIVE;
        self.sourcelevel += 1;
        let caller = match self.funcstack.last() {
            Some(f) => f.name.clone(),
            None => old_scriptfilename
                .clone()
                .unwrap_or_else(|| b"zsh".to_vec()),
        };
        self.funcstack.push(crate::exec::Funcstack {
            name: s.to_vec(),
            filename: Some(s.to_vec()),
            caller,
            flineno: 0,
            lineno: oldlineno,
            tp: crate::exec::FS_SOURCE,
        });
        match self.zloop(&text, false, false) {
            LoopReturn::Ok => {}
            LoopReturn::Empty => self.lastval = 0,
            LoopReturn::Error => ret = SourceReturn::Error,
        }
        let _ = self.funcstack.pop();
        self.sourcelevel -= 1;
        self.trap_state = otrap_state;
        self.trap_return = otrap_return;
        self.subsh = osubsh;
        self.thisjob = cj;
        self.lineno = oldlineno;
        self.loops = oloops;
        let _ = self.dosetopt(i32::try_from(SHINSTDIN).unwrap_or(0), oldshst, true);
        self.errflag.set(self.errflag.get() & !ERRFLAG_ERROR);
        if !self.exit_pending {
            self.retflag = false;
        }
        self.scriptname = old_scriptname;
        self.scriptfilename = old_scriptfilename;
        self.cmdstack = ocs;
        ret
    }

    /// zsh's `sourcehome`: source `s` from `$ZDOTDIR`, or `$HOME`.
    pub(crate) fn sourcehome(&mut self, s: &[u8]) {
        self.queue_signals();
        let zdotdir = if self.emulation_is(EMULATE_SH | EMULATE_KSH) {
            None
        } else {
            self.getsparam_u(b"ZDOTDIR")
        };
        let h = match zdotdir {
            Some(h) => h,
            None => {
                if self.home.is_empty() {
                    self.unqueue_signals();
                    return;
                }
                self.home.clone()
            }
        };
        let mut buf = h;
        buf.push(b'/');
        buf.extend_from_slice(s);
        self.unqueue_signals();
        let _ = self.source(&buf);
    }

    /// zsh's `init_io`: find the terminal and set up `shout`.
    pub(crate) fn init_io(&mut self) {
        if self.shout >= 0 {
            if self.shout != 2 && self.shout != self.shtty {
                let _ = self.zclose(self.shout);
            }
            self.shout = -1;
        }
        if self.shtty != -1 {
            let _ = self.zclose(self.shtty);
            self.shtty = -1;
        }
        self.xtrerr = 2;
        if isatty(0) {
            if let Some(name) = ttyname_of(0) {
                let fd = open_rdwr(&name);
                self.shtty = self.movefd(fd);
                self.ttystrname = name;
            }
            if self.shtty == -1 && rdwrtty(0) {
                // SAFETY: dup has no memory preconditions.
                let fd = unsafe { libc::dup(0) };
                self.shtty = self.movefd(fd);
            }
        }
        if self.shtty == -1 && isatty(1) && rdwrtty(1) {
            // SAFETY: as above.
            let fd = unsafe { libc::dup(1) };
            self.shtty = self.movefd(fd);
            if self.shtty != -1 {
                self.ttystrname = ttyname_of(1).unwrap_or_default();
            }
        }
        if self.shtty == -1 {
            let fd = open_rdwr(b"/dev/tty");
            self.shtty = self.movefd(fd);
            if self.shtty != -1 {
                self.ttystrname = ttyname_of(self.shtty).unwrap_or_default();
            }
        }
        if self.shtty == -1 {
            self.ttystrname.clear();
        } else {
            // SAFETY: F_GETFD has no memory preconditions.
            let fdflags = unsafe { libc::fcntl(self.shtty, libc::F_GETFD, 0) };
            if fdflags != -1 {
                // SAFETY: F_SETFD has no memory preconditions.
                unsafe {
                    libc::fcntl(self.shtty, libc::F_SETFD, fdflags | libc::FD_CLOEXEC);
                }
            }
            if self.ttystrname.is_empty() {
                self.ttystrname = b"/dev/tty".to_vec();
            }
        }
        if self.interact() {
            self.init_shout();
            if self.shtty == 0 || self.shout < 0 {
                self.opts[USEZLE] = false;
            }
        } else {
            self.opts[USEZLE] = false;
        }
        // SAFETY: getpid has no preconditions.
        self.mypid = unsafe { libc::getpid() };
        if self.opts[MONITOR] && self.shtty != -1 {
            // SAFETY: getpgrp has no preconditions.
            self.origpgrp = unsafe { libc::getpgrp() };
            self.acquire_pgrp();
        } else {
            self.opts[MONITOR] = false;
        }
    }

    /// `init_io(NULL)` after `exec < file` in an interactive shell.
    pub(crate) fn init_io_stdin(&mut self) {
        self.init_io();
    }

    /// zsh's `init_shout`.
    pub(crate) fn init_shout(&mut self) {
        if self.shtty == -1 {
            self.shout = 2;
            return;
        }
        self.shout = self.shtty;
        self.shttyinfo = self.gettyinfo_now();
    }

    /// zsh's `checkjobs`: warn before leaving with jobs about.
    pub(crate) fn checkjobs(&mut self) {
        let found = (1..=self.maxjob).find(|&i| {
            i32::try_from(i).ok() != Some(self.thisjob)
                && self.jobtab.get(i).is_some_and(|j| {
                    j.stat & STAT_LOCKED != 0
                        && j.stat & STAT_NOPRINT == 0
                        && (self.isset(CHECKRUNNINGJOBS) || j.stat & STAT_STOPPED != 0)
                })
        });
        if let Some(i) = found {
            if self
                .jobtab
                .get(i)
                .is_some_and(|j| j.stat & STAT_STOPPED != 0)
            {
                self.zerr("you have suspended jobs.");
            } else {
                self.zerr("you have running jobs.");
            }
            self.stopmsg = 1;
        }
    }
}
