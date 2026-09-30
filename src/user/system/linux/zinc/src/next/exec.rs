//! Command execution (zsh's `exec.c`), part one: constants, finding and
//! executing external commands, and entering subshells.
//!
//! zsh executes word code; zinc executes the parse tree in `ast.rs`, which
//! has the same shape. Where zsh walks `state->pc`, these functions take the
//! node.

use std::ffi::CString;

use crate::options::*;
use crate::params::ArrVar;
use crate::shell::{ERRFLAG_ERROR, Shell};
use crate::signals::{ZSIG_FUNC, ZSIG_IGNORED, errno, signal_default, signal_ignore};
use crate::sysutil::errmsg;
use crate::tok;
use crate::utils::lossy;

pub(crate) const FDT_UNUSED: u8 = 0;
pub(crate) const FDT_INTERNAL: u8 = 1;
pub(crate) const FDT_EXTERNAL: u8 = 2;
pub(crate) const FDT_MODULE: u8 = 3;
pub(crate) const FDT_XTRACE: u8 = 4;
pub(crate) const FDT_FLOCK: u8 = 5;
pub(crate) const FDT_FLOCK_EXEC: u8 = 6;
pub(crate) const FDT_PROC_SUBST: u8 = 7;
pub(crate) const FDT_TYPE_MASK: u8 = 15;
pub(crate) const FDT_SAVED_MASK: u8 = 16;

pub(crate) const Z_TIMED: i32 = 1 << 0;
pub(crate) const Z_SYNC: i32 = 1 << 1;
pub(crate) const Z_ASYNC: i32 = 1 << 2;
pub(crate) const Z_DISOWN: i32 = 1 << 3;

pub(crate) const SFC_NONE: i32 = 0;
pub(crate) const SFC_DIRECT: i32 = 1;
pub(crate) const SFC_SIGNAL: i32 = 2;
pub(crate) const SFC_HOOK: i32 = 3;
pub(crate) const SFC_WIDGET: i32 = 4;
pub(crate) const SFC_COMPLETE: i32 = 5;
pub(crate) const SFC_CWIDGET: i32 = 6;
pub(crate) const SFC_SUBST: i32 = 7;

pub(crate) const FS_SOURCE: i32 = 0;
pub(crate) const FS_FUNC: i32 = 1;
pub(crate) const FS_EVAL: i32 = 2;

pub(crate) const ESUB_ASYNC: i32 = 0x01;
pub(crate) const ESUB_PGRP: i32 = 0x02;
pub(crate) const ESUB_KEEPTRAP: i32 = 0x04;
pub(crate) const ESUB_FAKE: i32 = 0x08;
pub(crate) const ESUB_REVERTPGRP: i32 = 0x10;
pub(crate) const ESUB_NOMONITOR: i32 = 0x20;
pub(crate) const ESUB_JOB_CONTROL: i32 = 0x40;

pub(crate) const ADDVAR_EXPORT: i32 = 1;
pub(crate) const ADDVAR_RESTRICT: i32 = 2;
pub(crate) const ADDVAR_RESTORE: i32 = 4;

/// Word code command types, kept for the comparisons zsh makes on them.
pub(crate) const WC_SIMPLE: i32 = 6;
pub(crate) const WC_TYPESET: i32 = 7;
pub(crate) const WC_SUBSH: i32 = 8;
pub(crate) const WC_CURSH: i32 = 9;
pub(crate) const WC_TIMED: i32 = 10;
pub(crate) const WC_FUNCDEF: i32 = 11;
pub(crate) const WC_FOR: i32 = 12;
pub(crate) const WC_SELECT: i32 = 13;
pub(crate) const WC_WHILE: i32 = 14;
pub(crate) const WC_REPEAT: i32 = 15;
pub(crate) const WC_CASE: i32 = 16;
pub(crate) const WC_IF: i32 = 17;
pub(crate) const WC_COND: i32 = 18;
pub(crate) const WC_ARITH: i32 = 19;
pub(crate) const WC_AUTOFN: i32 = 20;
pub(crate) const WC_TRY: i32 = 21;

/// One entry of `$funcstack` and friends (zsh's `struct funcstack`).
#[derive(Debug, Clone)]
pub(crate) struct Funcstack {
    pub(crate) name: Vec<u8>,
    pub(crate) filename: Option<Vec<u8>>,
    pub(crate) caller: Vec<u8>,
    pub(crate) flineno: i64,
    pub(crate) lineno: i64,
    pub(crate) tp: i32,
}

/// What `execsave` keeps around a trap (zsh's `struct execstack`).
#[derive(Debug, Clone, Default)]
pub(crate) struct ExecStack {
    pub(crate) list_pipe_pid: i32,
    pub(crate) nowait: bool,
    pub(crate) pline_level: i32,
    pub(crate) list_pipe_child: bool,
    pub(crate) list_pipe_job: i32,
    pub(crate) list_pipe_text: Vec<u8>,
    pub(crate) lastval: i32,
    pub(crate) noeval: i32,
    pub(crate) badcshglob: i32,
    pub(crate) cmdoutpid: i32,
    pub(crate) cmdoutval: i32,
    pub(crate) use_cmdoutval: bool,
    pub(crate) procsubstpid: i32,
    pub(crate) trap_return: i32,
    pub(crate) trap_state: i32,
    pub(crate) trapisfunc: bool,
    pub(crate) traplocallevel: i32,
    pub(crate) noerrs: i32,
    pub(crate) this_noerrexit: bool,
    pub(crate) underscore: Vec<u8>,
}

/// `DEFAULT_PATH`.
const DEFAULT_PATH: &[u8] = b"/bin:/usr/bin:/usr/ucb:/usr/local/bin";

fn cstr(b: &[u8]) -> Option<CString> {
    CString::new(b).ok()
}

/// zsh's `iscom` on an unmetafied path: an executable regular file.
pub(crate) fn is_executable_file(us: &[u8]) -> bool {
    crate::sysutil::access_bytes(us, libc::X_OK)
        && crate::sysutil::stat_bytes(us)
            .is_some_and(|st| (st.st_mode & libc::S_IFMT) == libc::S_IFREG)
}

/// zsh's `iscom` on a metafied path.
pub(crate) fn iscom(s: &[u8]) -> bool {
    is_executable_file(&tok::unmetafy(s))
}

/// zsh's `isrelative`.
pub(crate) fn isrelative(s: &[u8]) -> bool {
    if s.first() != Some(&b'/') {
        return true;
    }
    let at = |i: usize| s.get(i).copied().unwrap_or(0);
    for i in 1..s.len() {
        if at(i) == b'.'
            && at(i - 1) == b'/'
            && (at(i + 1) == b'/'
                || at(i + 1) == 0
                || (at(i + 1) == b'.' && (at(i + 2) == b'/' || at(i + 2) == 0)))
        {
            return true;
        }
    }
    false
}

/// zsh's `isgooderr`.
fn isgooderr(e: i32, dir: &[u8]) -> bool {
    let d = cstr(&tok::unmetafy(dir));
    // SAFETY: the pointer is NUL-terminated.
    let dir_ok = d.is_some_and(|d| unsafe { libc::access(d.as_ptr(), libc::X_OK) } == 0);
    (e != libc::EACCES || dir_ok) && e != libc::ENOENT && e != libc::ENOTDIR
}

/// zsh's `search_defpath`.
fn search_defpath(cmd: &[u8]) -> Option<Vec<u8>> {
    for ps in DEFAULT_PATH.split(|&c| c == b':') {
        if ps.first() == Some(&b'/') {
            let mut p = ps.to_vec();
            p.push(b'/');
            p.extend_from_slice(cmd);
            if iscom(&p) {
                return Some(p);
            }
        }
    }
    None
}

impl Shell {
    /// zsh's `findcmd`: the full path of `arg0`, or `arg0` itself when
    /// `docopy` is false.
    pub(crate) fn findcmd(
        &mut self,
        arg0: &[u8],
        docopy: bool,
        default_path: bool,
    ) -> Option<Vec<u8>> {
        if default_path {
            return search_defpath(arg0).map(|p| if docopy { p } else { arg0.to_vec() });
        }
        let mut cn = self.cmdnamtab.get(arg0).cloned();
        if cn.is_none() && self.isset(HASHCMDS) && !isrelative(arg0) {
            let n = self.arrvar(ArrVar::Path).len();
            cn = self.hashcmd(arg0, 0, n);
        }
        if arg0.len() > libc::PATH_MAX as usize {
            return None;
        }
        let ret = |p: &[u8]| if docopy { p.to_vec() } else { arg0.to_vec() };
        if let Some(s) = arg0.iter().position(|&c| c == b'/') {
            if iscom(arg0) {
                return Some(ret(arg0));
            }
            if s == 0
                || self.unset_opt(PATHDIRS)
                || arg0.starts_with(b"./")
                || arg0.starts_with(b"../")
            {
                return None;
            }
        }
        let path = self.arrvar(ArrVar::Path).to_vec();
        if let Some(cn) = cn {
            let nn = if cn.flags & crate::tables::HASHED != 0 {
                cn.cmd.clone()
            } else {
                let upto = cn.name.unwrap_or(0);
                for pp in path.iter().take(upto) {
                    if pp.first() != Some(&b'/') {
                        let mut buf = Vec::new();
                        if !pp.is_empty() {
                            buf.extend_from_slice(pp);
                            buf.push(b'/');
                        }
                        buf.extend_from_slice(arg0);
                        if iscom(&buf) {
                            return Some(ret(&buf));
                        }
                    }
                }
                let mut nn = cn
                    .name
                    .and_then(|i| path.get(i).cloned())
                    .unwrap_or_default();
                nn.push(b'/');
                nn.extend_from_slice(arg0);
                nn
            };
            if iscom(&nn) {
                return Some(ret(&nn));
            }
        }
        for pp in &path {
            let mut buf = Vec::new();
            if !pp.is_empty() {
                buf.extend_from_slice(pp);
                buf.push(b'/');
            }
            buf.extend_from_slice(arg0);
            if iscom(&buf) {
                return Some(ret(&buf));
            }
        }
        None
    }

    /// zsh's `isreallycom`.
    pub(crate) fn isreallycom(&self, nam: &[u8], cn: &crate::tables::Cmdnam) -> bool {
        let full = if cn.flags & crate::tables::HASHED != 0 {
            cn.cmd.clone()
        } else {
            let Some(d) = cn
                .name
                .and_then(|i| self.arrvar(ArrVar::Path).get(i).cloned())
            else {
                return false;
            };
            let mut f = d;
            f.push(b'/');
            f.extend_from_slice(nam);
            f
        };
        iscom(&full)
    }

    /// zsh's `hashcmd`: look for `arg0` in `$path` from index `from`, adding
    /// it to the command table. `checked_to` is `pathchecked`'s index.
    pub(crate) fn hashcmd(
        &mut self,
        arg0: &[u8],
        from: usize,
        _checked_to: usize,
    ) -> Option<crate::tables::Cmdnam> {
        if arg0.first() == Some(&b'/') {
            return None;
        }
        let path = self.arrvar(ArrVar::Path).to_vec();
        let mut found = None;
        for (i, pp) in path.iter().enumerate().skip(from) {
            if pp.first() == Some(&b'/') {
                let mut buf = pp.clone();
                buf.push(b'/');
                if buf.len() + arg0.len() >= libc::PATH_MAX as usize {
                    continue;
                }
                buf.extend_from_slice(arg0);
                if iscom(&buf) {
                    found = Some(i);
                    break;
                }
            }
        }
        let i = found?;
        let cn = crate::tables::Cmdnam {
            flags: 0,
            name: Some(i),
            cmd: Vec::new(),
        };
        let _ = self.cmdnamtab.insert(arg0.to_vec(), cn.clone());
        if self.isset(HASHDIRS) {
            for pq in self.pathchecked..=i {
                self.hashdir(pq);
            }
            self.pathchecked = i + 1;
        }
        Some(cn)
    }

    /// zsh's `zfork`: the pid, 0 in the child, -1 on failure.
    pub(crate) fn zfork(&mut self, tv: Option<&mut (i64, i64)>) -> i32 {
        if self.thisjob != -1
            && usize::try_from(self.thisjob).is_ok_and(|t| t + 1 >= self.jobtab.len())
            && !self.expandjobtab()
        {
            self.zerr("job table full");
            return -1;
        }
        if let Some(tv) = tv {
            *tv = crate::params::now_tv();
        }
        self.queue_signals();
        // SAFETY: fork; the child only continues this single thread.
        let pid = unsafe { libc::fork() };
        self.unqueue_signals();
        if pid == -1 {
            self.zerr(&format!("fork failed: {}", errmsg(errno())));
            return -1;
        }
        if pid == 0 {
            self.setlimits(None);
        }
        pid
    }

    /// zsh's `zexecve`: returns the errno when the program did not start.
    fn zexecve(&mut self, pth: &[u8], argv: &[Vec<u8>], newenvp: Option<&[Vec<u8>]>) -> i32 {
        let upth = tok::unmetafy(pth);
        let mut under = b"_=".to_vec();
        if upth.first() == Some(&b'/') {
            under.extend_from_slice(&upth);
        } else {
            under.extend(tok::unmetafy(&self.pwd));
            under.push(b'/');
            under.extend_from_slice(&upth);
        }
        self.zputenv(&under);
        let uargv: Vec<Vec<u8>> = argv.iter().map(|a| tok::unmetafy(a)).collect();
        let env: Vec<Vec<u8>> = match newenvp {
            Some(e) => e.to_vec(),
            None => self.environ.clone(),
        };
        crate::signals::winch_unblock();
        let eno = raw_execve(&upth, &uargv, &env);
        if eno != libc::ENOEXEC && eno != libc::ENOENT {
            return eno;
        }
        let Some(cp) = cstr(&upth) else { return eno };
        // SAFETY: cp is NUL-terminated.
        let fd = unsafe { libc::open(cp.as_ptr(), libc::O_RDONLY | libc::O_NOCTTY) };
        if fd < 0 {
            return errno();
        }
        let mut buf = [0u8; 128];
        // SAFETY: buf is writable for its length.
        let ct = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        // SAFETY: closing the descriptor just opened.
        unsafe {
            libc::close(fd);
        }
        if ct < 0 {
            return errno();
        }
        let ct = usize::try_from(ct).unwrap_or(0);
        let head = buf.get(..ct).unwrap_or(&[]);
        let rest_args: Vec<Vec<u8>> = uargv.iter().skip(1).cloned().collect();
        if ct >= 2 && head.starts_with(b"#!") {
            let Some(nl) = head.iter().position(|&c| c == b'\n') else {
                self.zerr(&format!(
                    "{}: bad interpreter: {}: {}",
                    lossy(pth),
                    lossy(head.get(2..).unwrap_or(&[])),
                    errmsg(eno)
                ));
                return eno;
            };
            let mut line = head.get(2..nl).unwrap_or(&[]).to_vec();
            while line.last().is_some_and(|&c| c == b' ' || c == b'\t') {
                let _ = line.pop();
            }
            let start = line.iter().position(|&c| c != b' ').unwrap_or(line.len());
            let line = line.get(start..).unwrap_or(&[]).to_vec();
            let (interp, arg) = match line.iter().position(|&c| c == b' ') {
                Some(p) => (
                    line.get(..p).unwrap_or(&[]).to_vec(),
                    Some(line.get(p + 1..).unwrap_or(&[]).to_vec()),
                ),
                None => (line.clone(), None),
            };
            let mut nargv: Vec<Vec<u8>> = vec![interp.clone()];
            if let Some(a) = arg {
                nargv.push(a);
            }
            nargv.push(upth.clone());
            nargv.extend(rest_args);
            if eno == libc::ENOENT {
                if interp.first() != Some(&b'/')
                    && let Some(pprog) = self.pathprog(&interp)
                {
                    crate::signals::winch_unblock();
                    let _ = raw_execve(&tok::unmetafy(&pprog), &nargv, &env);
                }
                self.zerr(&format!(
                    "{}: bad interpreter: {}: {}",
                    lossy(pth),
                    lossy(&interp),
                    errmsg(eno)
                ));
            } else {
                crate::signals::winch_unblock();
                return raw_execve(&interp, &nargv, &env);
            }
        } else if eno == libc::ENOEXEC {
            let isbinary = match head.iter().position(|&c| c == 0) {
                None => false,
                Some(nul) => {
                    let mut hasletter = false;
                    let mut bin = true;
                    for &c in head.get(..nul).unwrap_or(&[]) {
                        if c.is_ascii_lowercase() || c == b'$' || c == b'`' {
                            hasletter = true;
                        }
                        if hasletter && c == b'\n' {
                            bin = false;
                            break;
                        }
                    }
                    bin
                }
            };
            if !isbinary {
                let mut nargv = vec![b"sh".to_vec(), upth.clone()];
                nargv.extend(rest_args);
                crate::signals::winch_unblock();
                return raw_execve(b"/bin/sh", &nargv, &env);
            }
        }
        eno
    }

    /// zsh's `pathprog`: `prog` found on `$path`.
    pub(crate) fn pathprog(&self, prog: &[u8]) -> Option<Vec<u8>> {
        for pp in self.arrvar(ArrVar::Path) {
            let mut buf = if pp.is_empty() {
                b".".to_vec()
            } else {
                pp.clone()
            };
            buf.push(b'/');
            buf.extend_from_slice(prog);
            if iscom(&buf) {
                return Some(buf);
            }
        }
        None
    }

    /// zsh's `commandnotfound`: false when the handler ran.
    fn commandnotfound(&mut self, arg0: &[u8], args: &[Vec<u8>]) -> bool {
        let Some(shf) = self.shfunctab.get(b"command_not_found_handler").cloned() else {
            self.lastval = 127;
            return true;
        };
        let mut a = vec![b"command_not_found_handler".to_vec(), arg0.to_vec()];
        a.extend(args.iter().skip(1).cloned());
        self.lastval = self.doshfunc(&shf, Some(a), true);
        false
    }

    /// zsh's `execute`: start an external command; never returns.
    pub(crate) fn execute(&mut self, args: &mut [Vec<u8>], flags: u32, defpath: bool) -> ! {
        let arg0 = args.first().cloned().unwrap_or_default();
        if self.isset(RESTRICTED) && (arg0.contains(&b'/') || defpath) {
            self.zerr(&format!("{}: restricted", lossy(&arg0)));
            crate::shell::exit_now(1);
        }
        if let Some(s) = self.sttyval.take()
            && !s.is_empty()
            && crate::sysutil::isatty(0)
            && crate::sysutil::getpgrp() == crate::sysutil::getpid()
        {
            let mut t = b"stty ".to_vec();
            t.extend(s);
            self.execstring(&t, true, false, "stty");
        }
        if self.unset_opt(RESTRICTED)
            && let Some(z) = self.zgetenv(b"ARGV0")
        {
            if let Some(a) = args.first_mut() {
                *a = z;
            }
            self.delenv_name(b"ARGV0");
        } else if flags & crate::builtin::BINF_DASH != 0 {
            let mut d = b"-".to_vec();
            d.extend_from_slice(&arg0);
            if let Some(a) = args.first_mut() {
                *a = d;
            }
        }
        self.makecline(args);
        let blank: Vec<Vec<u8>> = Vec::new();
        let newenvp: Option<&[Vec<u8>]> = if flags & crate::builtin::BINF_CLEARENV != 0 {
            Some(&blank)
        } else {
            None
        };
        let newenvp: Option<Vec<Vec<u8>>> = newenvp.map(<[Vec<u8>]>::to_vec);
        self.closem(FDT_XTRACE, false);
        crate::signals::child_unblock();
        if arg0.len() >= libc::PATH_MAX as usize {
            self.zerr(&format!("command too long: {}", lossy(&arg0)));
            crate::shell::exit_now(1);
        }
        let argv = args.to_vec();
        if let Some(s) = arg0.iter().position(|&c| c == b'/') {
            let lerrno = self.zexecve(&arg0, &argv, newenvp.as_deref());
            if s == 0
                || self.unset_opt(PATHDIRS)
                || (arg0.first() == Some(&b'.')
                    && (s == 1 || (arg0.get(1) == Some(&b'.') && s == 2)))
            {
                self.zerr(&format!("{}: {}", errmsg(lerrno), lossy(&arg0)));
                crate::shell::exit_now(if lerrno == libc::EACCES || lerrno == libc::ENOEXEC {
                    126
                } else {
                    127
                });
            }
        }
        let mut eno = 0;
        if defpath {
            let Some(pbuf) = search_defpath(&arg0) else {
                if !self.commandnotfound(&arg0, args) {
                    self._realexit();
                }
                self.zerr(&format!("command not found: {}", lossy(&arg0)));
                crate::shell::exit_now(127);
            };
            let ee = self.zexecve(&pbuf, &argv, newenvp.as_deref());
            let dir = match pbuf.iter().rposition(|&c| c == b'/') {
                Some(0) | None => b"/".to_vec(),
                Some(p) => pbuf.get(..p).unwrap_or(&[]).to_vec(),
            };
            if isgooderr(ee, &dir) {
                eno = ee;
            }
        } else {
            let path = self.arrvar(ArrVar::Path).to_vec();
            if let Some(cn) = self.cmdnamtab.get(&arg0).cloned() {
                let nn = if cn.flags & crate::tables::HASHED != 0 {
                    cn.cmd.clone()
                } else {
                    for pp in path.iter().take(cn.name.unwrap_or(0)) {
                        if pp.is_empty() || pp.as_slice() == b"." {
                            let ee = self.zexecve(&arg0, &argv, newenvp.as_deref());
                            if isgooderr(ee, pp) {
                                eno = ee;
                            }
                        } else if pp.first() != Some(&b'/') {
                            let mut buf = pp.clone();
                            buf.push(b'/');
                            buf.extend_from_slice(&arg0);
                            let ee = self.zexecve(&buf, &argv, newenvp.as_deref());
                            if isgooderr(ee, pp) {
                                eno = ee;
                            }
                        }
                    }
                    let mut nn = cn
                        .name
                        .and_then(|i| path.get(i).cloned())
                        .unwrap_or_default();
                    nn.push(b'/');
                    nn.extend_from_slice(&arg0);
                    nn
                };
                let ee = self.zexecve(&nn, &argv, newenvp.as_deref());
                let dir = match nn.iter().rposition(|&c| c == b'/') {
                    Some(0) | None => b"/".to_vec(),
                    Some(p) => nn.get(..p).unwrap_or(&[]).to_vec(),
                };
                if isgooderr(ee, &dir) {
                    eno = ee;
                }
            }
            for pp in &path {
                if pp.is_empty() || pp.as_slice() == b"." {
                    let ee = self.zexecve(&arg0, &argv, newenvp.as_deref());
                    if isgooderr(ee, pp) {
                        eno = ee;
                    }
                } else {
                    let mut buf = pp.clone();
                    buf.push(b'/');
                    buf.extend_from_slice(&arg0);
                    let ee = self.zexecve(&buf, &argv, newenvp.as_deref());
                    if isgooderr(ee, pp) {
                        eno = ee;
                    }
                }
            }
        }
        if eno != 0 {
            self.zerr(&format!("{}: {}", errmsg(eno), lossy(&arg0)));
        } else if !self.commandnotfound(&arg0, args) {
            self._realexit();
        } else {
            self.zerr(&format!("command not found: {}", lossy(&arg0)));
        }
        crate::shell::exit_now(if eno == libc::EACCES || eno == libc::ENOEXEC {
            126
        } else {
            127
        });
    }

    /// zsh's `makecline`: trace the command line when XTRACE is set.
    pub(crate) fn makecline(&mut self, args: &[Vec<u8>]) {
        if self.isset(XTRACE) {
            if !self.doneps4 {
                self.printprompt4();
            }
            let mut out = Vec::new();
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                out.extend(self.quotedzputs_out(a));
            }
            out.push(b'\n');
            crate::shell::write_fd(self.xtrerr_fd(), &out);
        }
    }

    /// zsh's `entersubsh`.
    pub(crate) fn entersubsh(&mut self, flags: i32) -> (i32, i32) {
        let mut ret = (-1, -1);
        if flags & ESUB_KEEPTRAP == 0 {
            for sig in 0..crate::signames::VSIGCOUNT {
                let t = self.sigtrapped.get(sig).copied().unwrap_or(0);
                if t & ZSIG_FUNC == 0 && !(self.isset(POSIXTRAPS) && t & ZSIG_IGNORED != 0) {
                    self.unsettrap(i32::try_from(sig).unwrap_or(0));
                }
            }
        }
        let monitor = self.isset(MONITOR);
        let job_control_ok = monitor && flags & ESUB_JOB_CONTROL != 0 && self.isset(POSIXJOBS);
        self.exit_val = 0;
        if flags & ESUB_NOMONITOR != 0 {
            self.opts[MONITOR] = false;
        }
        if !self.isset(MONITOR) {
            if flags & ESUB_ASYNC != 0 {
                let _ = self.settrap(libc::SIGINT, None, 0);
                let _ = self.settrap(libc::SIGQUIT, None, 0);
                if crate::sysutil::isatty(0) {
                    let _ = crate::sysutil::close_fd(0);
                    if crate::sysutil::open_bytes(b"/dev/null", libc::O_RDWR | libc::O_NOCTTY, 0)
                        != 0
                    {
                        self.zerr(&format!("can't open /dev/null: {}", errmsg(errno())));
                        crate::shell::exit_now(1);
                    }
                }
            }
        } else if self.thisjob != -1 && flags & ESUB_PGRP != 0 {
            let lpj = self.list_pipe_job;
            let tj = self.thisjob;
            let lpj_gl = self.job(lpj).map_or(0, |j| j.gleader);
            if lpj_gl != 0 && (self.list_pipe || self.list_pipe_child) {
                if crate::sysutil::setpgid(0, lpj_gl) == -1
                    || (crate::signals::killpg(lpj_gl, 0) == -1 && errno() == libc::ESRCH)
                {
                    let gl = if self.list_pipe_child {
                        self.mypgrp
                    } else {
                        crate::sysutil::getpid()
                    };
                    if let Some(j) = self.job_mut(lpj) {
                        j.gleader = gl;
                    }
                    if let Some(j) = self.job_mut(tj) {
                        j.gleader = gl;
                    }
                    // SAFETY: setpgid on ourselves.
                    unsafe {
                        libc::setpgid(0, gl);
                    }
                    if flags & ESUB_ASYNC == 0 {
                        self.attachtty(gl);
                    }
                }
                if flags & ESUB_ASYNC == 0 {
                    ret = (self.job(lpj).map_or(0, |j| j.gleader), lpj);
                }
            } else {
                let tj_gl = self.job(tj).map_or(0, |j| j.gleader);
                // SAFETY: setpgid has no memory preconditions.
                if tj_gl == 0 || unsafe { libc::setpgid(0, tj_gl) } == -1 {
                    // SAFETY: getpid has no preconditions.
                    let me = unsafe { libc::getpid() };
                    if let Some(j) = self.job_mut(tj) {
                        j.gleader = me;
                    }
                    if lpj != tj
                        && self.job(lpj).is_some_and(|j| j.gleader == 0)
                        && let Some(j) = self.job_mut(lpj)
                    {
                        j.gleader = me;
                    }
                    // SAFETY: setpgid on ourselves.
                    unsafe {
                        libc::setpgid(0, me);
                    }
                    if flags & ESUB_ASYNC == 0 {
                        self.attachtty(me);
                        ret = (me, if lpj != tj { lpj } else { -1 });
                    }
                }
            }
        }
        if flags & ESUB_FAKE == 0 {
            self.subsh = true;
        }
        self.zsh_subshell += 1;
        // SAFETY: getpid has no preconditions.
        if flags & ESUB_REVERTPGRP != 0 && unsafe { libc::getpid() } == self.mypgrp {
            self.release_pgrp();
        }
        self.shout = -1;
        if flags & ESUB_NOMONITOR != 0 {
            signal_ignore(libc::SIGTTOU);
            signal_ignore(libc::SIGTTIN);
            signal_ignore(libc::SIGTSTP);
        } else if !job_control_ok {
            signal_default(libc::SIGTTOU);
            signal_default(libc::SIGTTIN);
            signal_default(libc::SIGTSTP);
        }
        if self.interact() {
            signal_default(libc::SIGTERM);
            if self
                .sigtrapped
                .get(libc::SIGINT as usize)
                .copied()
                .unwrap_or(0)
                & ZSIG_IGNORED
                == 0
            {
                signal_default(libc::SIGINT);
            }
            if self
                .sigtrapped
                .get(libc::SIGPIPE as usize)
                .copied()
                .unwrap_or(0)
                == 0
            {
                signal_default(libc::SIGPIPE);
            }
        }
        if self
            .sigtrapped
            .get(libc::SIGQUIT as usize)
            .copied()
            .unwrap_or(0)
            & ZSIG_IGNORED
            == 0
        {
            signal_default(libc::SIGQUIT);
        }
        if self.intrap != 0 {
            for sig in 1..=crate::signames::SIGCOUNT {
                let t = self.sigtrapped.get(sig).copied().unwrap_or(0);
                if t != 0 && t != ZSIG_IGNORED {
                    let _ = crate::signals::signal_unblock(&crate::signals::signal_mask(
                        i32::try_from(sig).unwrap_or(0),
                    ));
                }
            }
        }
        if !job_control_ok {
            self.opts[MONITOR] = false;
        }
        self.opts[USEZLE] = false;
        self.zleactive = false;
        let maxfd = self.max_zsh_fd;
        for i in 10..=maxfd {
            if self.fdtable_get(i) & FDT_SAVED_MASK != 0 {
                let _ = self.zclose(i);
            }
        }
        if flags & ESUB_PGRP != 0 {
            self.clearjobtab(monitor);
        }
        self.get_usage();
        self.forklevel = self.locallevel;
        ret
    }

    /// zsh's `closem`.
    pub(crate) fn closem(&mut self, how: u8, all: bool) {
        let maxfd = self.max_zsh_fd;
        for i in 10..=maxfd {
            let t = self.fdtable_get(i);
            if t != FDT_UNUSED
                && (all || (t != FDT_PROC_SUBST && t != FDT_EXTERNAL))
                && (how == FDT_UNUSED || (t & FDT_TYPE_MASK) == how)
            {
                if i == self.shtty {
                    self.shtty = -1;
                }
                let _ = self.zclose(i);
            }
        }
    }

    /// zsh's `setunderscore`.
    pub(crate) fn setunderscore(&mut self, s: &[u8]) {
        self.zunderscore = s.to_vec();
    }

    /// zsh's `execsave`.
    pub(crate) fn execsave(&mut self) {
        let es = ExecStack {
            list_pipe_pid: self.list_pipe_pid,
            nowait: self.nowait,
            pline_level: self.pline_level,
            list_pipe_child: self.list_pipe_child,
            list_pipe_job: self.list_pipe_job,
            list_pipe_text: self.list_pipe_text.clone(),
            lastval: self.lastval,
            noeval: self.noeval,
            badcshglob: self.badcshglob,
            cmdoutpid: self.cmdoutpid,
            cmdoutval: self.cmdoutval,
            use_cmdoutval: self.use_cmdoutval,
            procsubstpid: self.procsubstpid,
            trap_return: self.trap_return,
            trap_state: self.trap_state,
            trapisfunc: self.trapisfunc,
            traplocallevel: self.traplocallevel,
            noerrs: self.noerrs,
            this_noerrexit: self.this_noerrexit,
            underscore: self.zunderscore.clone(),
        };
        self.exstack.push(es);
        self.noerrs = 0;
        self.cmdoutpid = 0;
    }

    /// zsh's `execrestore`.
    pub(crate) fn execrestore(&mut self) {
        self.queue_signals();
        if let Some(en) = self.exstack.pop() {
            self.list_pipe_pid = en.list_pipe_pid;
            self.nowait = en.nowait;
            self.pline_level = en.pline_level;
            self.list_pipe_child = en.list_pipe_child;
            self.list_pipe_job = en.list_pipe_job;
            self.list_pipe_text = en.list_pipe_text;
            self.lastval = en.lastval;
            self.noeval = en.noeval;
            self.badcshglob = en.badcshglob;
            self.cmdoutpid = en.cmdoutpid;
            self.cmdoutval = en.cmdoutval;
            self.use_cmdoutval = en.use_cmdoutval;
            self.procsubstpid = en.procsubstpid;
            self.trap_return = en.trap_return;
            self.trap_state = en.trap_state;
            self.trapisfunc = en.trapisfunc;
            self.traplocallevel = en.traplocallevel;
            self.noerrs = en.noerrs;
            self.this_noerrexit = en.this_noerrexit;
            self.setunderscore(&en.underscore);
        }
        self.unqueue_signals();
    }

    /// Where XTRACE output goes.
    pub(crate) fn xtrerr_fd(&self) -> i32 {
        if self.xtrerr < 0 { 2 } else { self.xtrerr }
    }
}

/// `execve` with byte strings; returns the errno.
fn raw_execve(path: &[u8], argv: &[Vec<u8>], env: &[Vec<u8>]) -> i32 {
    let Some(p) = cstr(path) else {
        return libc::ENOENT;
    };
    let cargs: Vec<CString> = argv.iter().filter_map(|a| cstr(a)).collect();
    let cenv: Vec<CString> = env.iter().filter_map(|a| cstr(a)).collect();
    let mut aptr: Vec<*const libc::c_char> = cargs.iter().map(|c| c.as_ptr()).collect();
    aptr.push(std::ptr::null());
    let mut eptr: Vec<*const libc::c_char> = cenv.iter().map(|c| c.as_ptr()).collect();
    eptr.push(std::ptr::null());
    // SAFETY: all pointers are NUL-terminated strings in NULL-terminated arrays
    // that outlive the call.
    unsafe {
        libc::execve(p.as_ptr(), aptr.as_ptr(), eptr.as_ptr());
    }
    errno()
}

/// Whether ERRFLAG_ERROR is the only use here.
pub(crate) const ERRFLAG_ERROR_: i32 = ERRFLAG_ERROR;
