//! System helpers from zsh's `utils.c` and `builtin.c`: the file
//! descriptor table, temporary files, the terminal's process group and
//! modes, window size, and leaving the shell.

use crate::exec::{FDT_EXTERNAL, FDT_FLOCK, FDT_FLOCK_EXEC, FDT_INTERNAL, FDT_UNUSED};
use crate::options::*;
use crate::params::IntVar;
use crate::shell::{Shell, write_fd};
use crate::signals::{ZEXIT_DEFERRED, ZEXIT_NORMAL, ZEXIT_SIGNAL, errno};

/// `DEFAULT_TMPPREFIX`.
const DEFAULT_TMPPREFIX: &[u8] = b"/tmp/zsh";

/// `stat(2)` on an unmetafied path.
pub(crate) fn stat_bytes(path: &[u8]) -> Option<libc::stat> {
    let c = std::ffi::CString::new(path).ok()?;
    // SAFETY: an all-zero stat is a valid value.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: c is NUL-terminated and st is writable.
    (unsafe { libc::stat(c.as_ptr(), &mut st) } == 0).then_some(st)
}

/// `lstat(2)` on an unmetafied path.
pub(crate) fn lstat_bytes(path: &[u8]) -> Option<libc::stat> {
    let c = std::ffi::CString::new(path).ok()?;
    // SAFETY: an all-zero stat is a valid value.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: c is NUL-terminated and st is writable.
    (unsafe { libc::lstat(c.as_ptr(), &mut st) } == 0).then_some(st)
}

/// `access(2)` on an unmetafied path.
pub(crate) fn access_bytes(path: &[u8], mode: i32) -> bool {
    let Ok(c) = std::ffi::CString::new(path) else {
        return false;
    };
    // SAFETY: c is NUL-terminated.
    unsafe { libc::access(c.as_ptr(), mode) == 0 }
}

/// `isatty(3)`.
pub(crate) fn isatty(fd: i32) -> bool {
    // SAFETY: isatty has no memory preconditions.
    unsafe { libc::isatty(fd) != 0 }
}

/// `getpid(2)`.
pub(crate) fn getpid() -> i32 {
    // SAFETY: getpid has no preconditions.
    unsafe { libc::getpid() }
}

/// `getpgrp(2)`.
pub(crate) fn getpgrp() -> i32 {
    // SAFETY: getpgrp has no preconditions.
    unsafe { libc::getpgrp() }
}

/// `close(2)`.
pub(crate) fn close_fd(fd: i32) -> i32 {
    // SAFETY: close has no memory preconditions.
    unsafe { libc::close(fd) }
}

/// `unlink(2)` on an unmetafied path.
pub(crate) fn unlink_bytes(path: &[u8]) -> i32 {
    let Ok(c) = std::ffi::CString::new(path) else {
        return -1;
    };
    // SAFETY: c is NUL-terminated.
    unsafe { libc::unlink(c.as_ptr()) }
}

/// `open(2)` on an unmetafied path.
pub(crate) fn open_bytes(path: &[u8], flags: i32, mode: libc::mode_t) -> i32 {
    let Ok(c) = std::ffi::CString::new(path) else {
        return -1;
    };
    // SAFETY: c is NUL-terminated.
    unsafe { libc::open(c.as_ptr(), flags, mode) }
}

/// `tcsetpgrp(3)`.
pub(crate) fn tcsetpgrp(fd: i32, pgrp: i32) -> i32 {
    // SAFETY: tcsetpgrp has no memory preconditions.
    unsafe { libc::tcsetpgrp(fd, pgrp) }
}

/// `setpgid(2)`.
pub(crate) fn setpgid(pid: i32, pgrp: i32) -> i32 {
    // SAFETY: setpgid has no memory preconditions.
    unsafe { libc::setpgid(pid, pgrp) }
}

/// glibc's messages for the errors the shell reports, so output matches
/// zsh built against glibc.
fn glibc_strerror(e: i32) -> Option<&'static str> {
    Some(match e {
        libc::EPERM => "Operation not permitted",
        libc::ENOENT => "No such file or directory",
        libc::ESRCH => "No such process",
        libc::EINTR => "Interrupted system call",
        libc::EIO => "Input/output error",
        libc::ENXIO => "No such device or address",
        libc::E2BIG => "Argument list too long",
        libc::ENOEXEC => "Exec format error",
        libc::EBADF => "Bad file descriptor",
        libc::ECHILD => "No child processes",
        libc::EAGAIN => "Resource temporarily unavailable",
        libc::ENOMEM => "Cannot allocate memory",
        libc::EACCES => "Permission denied",
        libc::EFAULT => "Bad address",
        libc::EBUSY => "Device or resource busy",
        libc::EEXIST => "File exists",
        libc::EXDEV => "Invalid cross-device link",
        libc::ENODEV => "No such device",
        libc::ENOTDIR => "Not a directory",
        libc::EISDIR => "Is a directory",
        libc::EINVAL => "Invalid argument",
        libc::EMFILE => "Too many open files",
        libc::ENOSPC => "No space left on device",
        libc::ESPIPE => "Illegal seek",
        libc::EROFS => "Read-only file system",
        libc::EPIPE => "Broken pipe",
        libc::ERANGE => "Numerical result out of range",
        libc::ENAMETOOLONG => "File name too long",
        libc::ENOTEMPTY => "Directory not empty",
        libc::ELOOP => "Too many levels of symbolic links",
        libc::ENOTTY => "Inappropriate ioctl for device",
        libc::ETXTBSY => "Text file busy",
        libc::ENOSYS => "Function not implemented",
        _ => return None,
    })
}

/// `strerror(e)` as glibc words it.
pub(crate) fn strerror_raw(e: i32) -> String {
    if let Some(s) = glibc_strerror(e) {
        return s.to_owned();
    }
    // SAFETY: strerror has no memory preconditions.
    let p = unsafe { libc::strerror(e) };
    // SAFETY: strerror returns a valid C string.
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
}

/// zsh's `%e`: the message with its first letter lowered (except EIO).
pub(crate) fn errmsg(e: i32) -> String {
    if e == libc::EINTR {
        return "interrupt".to_owned();
    }
    let m = strerror_raw(e);
    if e == libc::EIO {
        return m;
    }
    let mut c = m.chars();
    match c.next() {
        Some(f) => f.to_ascii_lowercase().to_string() + c.as_str(),
        None => m,
    }
}

impl Shell {
    /// The `%e` text for the last error.
    pub(crate) fn errmsg_last(&self) -> String {
        errmsg(errno())
    }

    /// `fdtable[fd]`, or `FDT_UNUSED` beyond it.
    pub(crate) fn fdtable_get(&self, fd: i32) -> u8 {
        usize::try_from(fd)
            .ok()
            .and_then(|f| self.fdtable.get(f))
            .copied()
            .unwrap_or(FDT_UNUSED)
    }

    fn fdtable_set(&mut self, fd: i32, v: u8) {
        if let Ok(f) = usize::try_from(fd) {
            if f >= self.fdtable.len() {
                self.fdtable
                    .resize((f + 1).max(self.fdtable.len() * 2), FDT_UNUSED);
            }
            if let Some(slot) = self.fdtable.get_mut(f) {
                *slot = v;
            }
        }
    }

    /// zsh's `check_fd_table`.
    fn check_fd_table(&mut self, fd: i32) {
        if fd <= self.max_zsh_fd {
            return;
        }
        if let Ok(f) = usize::try_from(fd)
            && f >= self.fdtable.len()
        {
            let mut n = self.fdtable.len().max(1);
            while f >= n {
                n *= 2;
            }
            self.fdtable.resize(n, FDT_UNUSED);
        }
        self.max_zsh_fd = fd;
    }

    /// zsh's `movefd`.
    pub(crate) fn movefd(&mut self, fd: i32) -> i32 {
        let mut fd = fd;
        if fd != -1 && fd < 10 {
            // SAFETY: fcntl F_DUPFD has no memory preconditions.
            let fe = unsafe { libc::fcntl(fd, libc::F_DUPFD, 10) };
            let _ = self.zclose(fd);
            fd = fe;
        }
        if fd != -1 {
            self.check_fd_table(fd);
            self.fdtable_set(fd, FDT_INTERNAL);
        }
        fd
    }

    /// zsh's `redup`.
    pub(crate) fn redup(&mut self, x: i32, y: i32) -> i32 {
        let mut ret = y;
        if x < 0 {
            let _ = self.zclose(y);
        } else if x != y {
            // SAFETY: dup2 has no memory preconditions.
            if unsafe { libc::dup2(x, y) } == -1 {
                ret = -1;
            } else {
                self.check_fd_table(y);
                let mut t = self.fdtable_get(x);
                if t == FDT_FLOCK || t == FDT_FLOCK_EXEC {
                    t = FDT_INTERNAL;
                }
                self.fdtable_set(y, t);
            }
            if self.fdtable_get(x) == FDT_FLOCK {
                self.fdtable_flocks -= 1;
            }
            let _ = self.zclose(x);
        }
        ret
    }

    /// zsh's `addmodulefd`.
    pub(crate) fn addmodulefd(&mut self, fd: i32, fdt: u8) {
        if fd >= 0 {
            self.check_fd_table(fd);
            self.fdtable_set(fd, fdt);
        }
    }

    /// zsh's `addlockfd`.
    pub(crate) fn addlockfd(&mut self, fd: i32, cloexec: bool) {
        if cloexec {
            if self.fdtable_get(fd) != FDT_FLOCK {
                self.fdtable_flocks += 1;
            }
            self.fdtable_set(fd, FDT_FLOCK);
        } else {
            self.fdtable_set(fd, FDT_FLOCK_EXEC);
        }
    }

    /// zsh's `zclose`.
    pub(crate) fn zclose(&mut self, fd: i32) -> i32 {
        if fd >= 0 {
            if fd <= self.max_zsh_fd {
                if self.fdtable_get(fd) == FDT_FLOCK {
                    self.fdtable_flocks -= 1;
                }
                self.fdtable_set(fd, FDT_UNUSED);
                while self.max_zsh_fd > 0 && self.fdtable_get(self.max_zsh_fd) == FDT_UNUSED {
                    self.max_zsh_fd -= 1;
                }
                if fd == self.coprocin {
                    self.coprocin = -1;
                }
                if fd == self.coprocout {
                    self.coprocout = -1;
                }
            }
            // SAFETY: closing a descriptor number.
            return unsafe { libc::close(fd) };
        }
        -1
    }

    /// zsh's `zcloselockfd`.
    pub(crate) fn zcloselockfd(&mut self, fd: i32) -> i32 {
        if fd > self.max_zsh_fd {
            return -1;
        }
        let t = self.fdtable_get(fd);
        if t != FDT_FLOCK && t != FDT_FLOCK_EXEC {
            return -1;
        }
        let _ = self.zclose(fd);
        0
    }

    /// zsh's `gettempname`: an unused name (unmetafied).
    pub(crate) fn gettempname(&mut self, prefix: Option<&[u8]>) -> Option<Vec<u8>> {
        self.queue_signals();
        let suffix: &[u8] = if prefix.is_some() {
            b".XXXXXX"
        } else {
            b"XXXXXX"
        };
        let pfx = match prefix {
            Some(p) => p.to_vec(),
            None => self
                .getsparam(b"TMPPREFIX")
                .unwrap_or_else(|| DEFAULT_TMPPREFIX.to_vec()),
        };
        let mut name = crate::tok::unmetafy(&pfx);
        name.extend_from_slice(suffix);
        name.push(0);
        // SAFETY: name is a writable NUL-terminated template.
        let r = unsafe { libc::mkstemp(name.as_mut_ptr().cast()) };
        let _ = name.pop();
        let result = if r < 0 {
            None
        } else {
            let _ = close_fd(r);
            let _ = unlink_bytes(&name);
            Some(name)
        };
        self.unqueue_signals();
        result
    }

    /// zsh's `gettempfile`: the descriptor and name (unmetafied).
    pub(crate) fn gettempfile(&mut self, prefix: Option<&[u8]>) -> (i32, Option<Vec<u8>>) {
        self.queue_signals();
        // SAFETY: umask has no preconditions.
        let old_umask = unsafe { libc::umask(0o177) };
        let suffix: &[u8] = if prefix.is_some() {
            b".XXXXXX"
        } else {
            b"XXXXXX"
        };
        let pfx = match prefix {
            Some(p) => p.to_vec(),
            None => self
                .getsparam(b"TMPPREFIX")
                .unwrap_or_else(|| DEFAULT_TMPPREFIX.to_vec()),
        };
        let mut name = crate::tok::unmetafy(&pfx);
        name.extend_from_slice(suffix);
        name.push(0);
        // SAFETY: name is a writable NUL-terminated template.
        let fd = unsafe { libc::mkstemp(name.as_mut_ptr().cast()) };
        let _ = name.pop();
        // SAFETY: umask has no preconditions.
        unsafe {
            libc::umask(old_umask);
        }
        self.unqueue_signals();
        if fd < 0 { (fd, None) } else { (fd, Some(name)) }
    }

    /// zsh's `attachtty`.
    pub(crate) fn attachtty(&mut self, pgrp: i32) {
        if self.jobbing() && self.interact() {
            if self.shtty != -1 && tcsetpgrp(self.shtty, pgrp) == -1 && !self.attachtty_ep {
                if pgrp != self.mypgrp && crate::signals::kill(-pgrp, 0) == -1 {
                    let mp = self.mypgrp;
                    self.attachtty(mp);
                } else {
                    let e = errno();
                    if e != libc::ENOTTY {
                        self.zwarn(&format!("can't set tty pgrp: {}", errmsg(e)));
                    }
                    self.opts[MONITOR] = false;
                    self.attachtty_ep = true;
                }
            } else {
                self.last_attached_pgrp = pgrp;
            }
        }
    }

    /// zsh's `gettygrp`.
    pub(crate) fn gettygrp(&self) -> i32 {
        if self.shtty == -1 {
            return -1;
        }
        // SAFETY: tcgetpgrp has no memory preconditions.
        unsafe { libc::tcgetpgrp(self.shtty) }
    }

    /// zsh's `gettyinfo` into a new value.
    pub(crate) fn gettyinfo_now(&self) -> crate::jobs::TtyInfo {
        let mut ti = crate::jobs::TtyInfo::default();
        if self.shtty != -1 {
            // SAFETY: ti.tio is a valid out-pointer.
            if unsafe { libc::tcgetattr(self.shtty, &mut ti.tio) } == -1 {
                self.zerr(&format!("bad tcgets: {}", errmsg(errno())));
            }
            // SAFETY: ti.winsize is a valid out-pointer for TIOCGWINSZ.
            unsafe {
                libc::ioctl(self.shtty, libc::TIOCGWINSZ, &mut ti.winsize);
            }
        }
        ti
    }

    /// zsh's `settyinfo`.
    pub(crate) fn settyinfo(&self, ti: &crate::jobs::TtyInfo) {
        if self.shtty != -1 {
            // SAFETY: ti.tio is a valid termios.
            while unsafe { libc::tcsetattr(self.shtty, libc::TCSADRAIN, &ti.tio) } == -1
                && errno() == libc::EINTR
            {}
        }
    }

    fn adjustlines(&mut self, signalled: bool) -> bool {
        let old = self.zterm_lines;
        if signalled || self.zterm_lines <= 0 {
            self.zterm_lines = i64::from(self.shttyinfo.winsize.ws_row);
        } else {
            self.shttyinfo.winsize.ws_row = u16::try_from(self.zterm_lines).unwrap_or(0);
        }
        if self.zterm_lines <= 0 {
            self.zterm_lines = if self.tclines > 0 { self.tclines } else { 24 };
        }
        self.zterm_lines != old
    }

    fn adjustcolumns(&mut self, signalled: bool) -> bool {
        let old = self.zterm_columns;
        if signalled || self.zterm_columns <= 0 {
            self.zterm_columns = i64::from(self.shttyinfo.winsize.ws_col);
        } else {
            self.shttyinfo.winsize.ws_col = u16::try_from(self.zterm_columns).unwrap_or(0);
        }
        if self.zterm_columns <= 0 {
            self.zterm_columns = if self.tccolumns > 0 {
                self.tccolumns
            } else {
                80
            };
        }
        self.zterm_columns != old
    }

    /// zsh's `adjustwinsize`.
    pub(crate) fn adjustwinsize(&mut self, from: i32) {
        let mut from = from;
        let mut ttyrows = self.shttyinfo.winsize.ws_row;
        let mut ttycols = self.shttyinfo.winsize.ws_col;
        let mut resetzle = false;
        if self.getwinsz || from == 1 {
            if self.shtty == -1 {
                return;
            }
            // SAFETY: the out pointer is valid.
            if unsafe { libc::ioctl(self.shtty, libc::TIOCGWINSZ, &mut self.shttyinfo.winsize) }
                == 0
            {
                resetzle = ttyrows != self.shttyinfo.winsize.ws_row
                    || ttycols != self.shttyinfo.winsize.ws_col;
                if from == 0 && resetzle && ttyrows != 0 && ttycols != 0 {
                    from = 1;
                }
                ttyrows = self.shttyinfo.winsize.ws_row;
                ttycols = self.shttyinfo.winsize.ws_col;
            } else {
                self.shttyinfo.winsize.ws_row = u16::try_from(self.zterm_lines).unwrap_or(0);
                self.shttyinfo.winsize.ws_col = u16::try_from(self.zterm_columns).unwrap_or(0);
                resetzle = from == 1;
            }
        }
        let _ = (ttyrows, ttycols);
        match from {
            0 | 1 => {
                self.getwinsz = false;
                if self.adjustlines(from != 0) && self.zgetenv(b"LINES").is_some() {
                    let v = self.zterm_lines;
                    let _ = self.setiparam(b"LINES", v);
                }
                if self.adjustcolumns(from != 0) && self.zgetenv(b"COLUMNS").is_some() {
                    let v = self.zterm_columns;
                    let _ = self.setiparam(b"COLUMNS", v);
                }
                self.getwinsz = true;
            }
            2 => resetzle = self.adjustlines(false),
            3 => resetzle = self.adjustcolumns(false),
            _ => {}
        }
        if self.zleactive && resetzle {
            self.zleentry_reset_prompt();
            self.zleentry_refresh();
        }
    }

    /// `$LINES` and `$COLUMNS` live in these (zsh's `zterm_lines` etc.).
    pub(crate) fn intvar_winsize(&self, v: IntVar) -> i64 {
        match v {
            IntVar::Lines => self.zterm_lines,
            IntVar::Columns => self.zterm_columns,
            _ => 0,
        }
    }

    /// zsh's `zgetenv`: the value in the environment, metafied.
    pub(crate) fn zgetenv(&self, name: &[u8]) -> Option<Vec<u8>> {
        for e in &self.environ {
            if e.len() > name.len() && e.starts_with(name) && e.get(name.len()) == Some(&b'=') {
                return Some(crate::tok::metafy(e.get(name.len() + 1..).unwrap_or(&[])));
            }
        }
        None
    }

    /// zsh's `read_loop`.
    pub(crate) fn read_loop(&self, fd: i32, buf: &mut [u8]) -> isize {
        let len = buf.len();
        let mut off = 0usize;
        loop {
            let rest = buf.get_mut(off..).unwrap_or(&mut []);
            // SAFETY: rest is a valid writable buffer.
            let ret = unsafe { libc::read(fd, rest.as_mut_ptr().cast(), rest.len()) };
            if ret >= 0 && usize::try_from(ret).ok() == Some(rest.len()) {
                break;
            }
            if ret <= 0 {
                if ret < 0 {
                    if errno() == libc::EINTR {
                        continue;
                    }
                    if fd != self.shtty {
                        self.zwarn(&format!("read failed: {}", errmsg(errno())));
                    }
                }
                return ret;
            }
            off += usize::try_from(ret).unwrap_or(0);
        }
        isize::try_from(len).unwrap_or(0)
    }

    /// zsh's `write_loop`.
    pub(crate) fn write_loop(&self, fd: i32, buf: &[u8]) -> isize {
        let mut off = 0usize;
        loop {
            let rest = buf.get(off..).unwrap_or(&[]);
            // SAFETY: rest is a valid buffer.
            let ret = unsafe { libc::write(fd, rest.as_ptr().cast(), rest.len()) };
            if ret >= 0 && usize::try_from(ret).ok() == Some(rest.len()) {
                break;
            }
            if ret < 0 {
                if errno() == libc::EINTR {
                    continue;
                }
                if fd != self.shtty {
                    self.zwarn(&format!("write failed: {}", errmsg(errno())));
                }
                return -1;
            }
            off += usize::try_from(ret).unwrap_or(0);
        }
        isize::try_from(buf.len()).unwrap_or(0)
    }

    /// zsh's `fixdir`: normalise `src` in place; true if a `..` forces
    /// links to be chased.
    pub(crate) fn fixdir(&self, src: &mut Vec<u8>) -> bool {
        let s = src.clone();
        let at = |i: usize| s.get(i).copied().unwrap_or(0);
        let mut chasedots: i32 = if at(0) == b'.'
            && self.pwd.as_slice() == b"."
            && (at(1) == b'/' || (at(1) == b'.' && at(2) == b'/'))
        {
            2
        } else {
            0
        };
        let mut dest: Vec<u8> = Vec::with_capacity(s.len());
        let mut i = 0usize;
        loop {
            if at(i) == b'/' {
                dest.push(b'/');
                i += 1;
                while at(i) == b'/' {
                    i += 1;
                }
            }
            if i >= s.len() {
                while dest.len() > 1 && dest.last() == Some(&b'/') {
                    let _ = dest.pop();
                }
                *src = dest;
                return chasedots != 0;
            }
            if at(i) == b'.' && at(i + 1) == b'.' && (at(i + 2) == 0 || at(i + 2) == b'/') {
                if self.isset(CHASEDOTS) || chasedots > 1 {
                    chasedots = 1;
                } else {
                    if dest.len() > 1 {
                        let mut ptrd = dest.len() - 1;
                        while ptrd > 0 && dest.get(ptrd - 1) != Some(&b'/') {
                            ptrd -= 1;
                        }
                        let seg = dest.get(ptrd..).unwrap_or(&[]).to_vec();
                        let seg = seg.strip_suffix(b"/").unwrap_or(&seg).to_vec();
                        if seg == b".." {
                            // A leading ".." we could not remove: keep it.
                        } else {
                            let mut probe = crate::tok::unmetafy(&dest);
                            probe.push(0);
                            dest.truncate(ptrd);
                            while dest.len() > 1 && dest.last() == Some(&b'/') {
                                let _ = dest.pop();
                            }
                            if dest.last() != Some(&b'/') {
                                dest.push(b'/');
                            }
                            i += 2;
                            if at(i) == b'/' {
                                i += 1;
                            }
                            continue;
                        }
                    } else if dest.first() == Some(&b'/') {
                        i += 2;
                        if at(i) == b'/' {
                            i += 1;
                        }
                        continue;
                    }
                }
            }
            if at(i) == b'.' && (at(i + 1) == b'/' || at(i + 1) == 0) {
                i += 1;
                while at(i) == b'/' {
                    i += 1;
                }
                continue;
            }
            while i < s.len() && at(i) != b'/' {
                dest.push(at(i));
                i += 1;
            }
        }
    }

    /// zsh's `realexit`.
    pub(crate) fn realexit(&mut self) -> ! {
        let v = if self.shell_exiting != 0 || self.exit_pending {
            self.exit_val
        } else {
            self.lastval
        };
        crate::shell::flush_and_exit(v)
    }

    /// zsh's `_realexit`.
    pub(crate) fn _realexit(&mut self) -> ! {
        let v = if self.shell_exiting != 0 || self.exit_pending {
            self.exit_val
        } else {
            self.lastval
        };
        crate::shell::exit_now(v)
    }

    /// zsh's `zexit`.
    pub(crate) fn zexit(&mut self, val: i32, from_where: i32) {
        self.exit_val = val;
        if self.shell_exiting == -1 {
            self.retflag = true;
            self.breaks = self.loops;
            return;
        }
        if self.isset(MONITOR) && self.stopmsg == 0 && from_where != ZEXIT_SIGNAL {
            self.scanjobs();
            if self.isset(CHECKJOBS) {
                self.checkjobs();
            }
            if self.stopmsg != 0 {
                self.stopmsg = 2;
                return;
            }
        }
        let was = self.shell_exiting;
        if from_where != ZEXIT_DEFERRED {
            self.shell_exiting += 1;
        }
        if from_where == ZEXIT_DEFERRED || (was != 0 && from_where != ZEXIT_NORMAL) {
            return;
        }
        self.shell_exiting = -1;
        self.errflag.set(0);
        if self.isset(MONITOR) {
            self.killrunjobs(from_where == ZEXIT_SIGNAL);
        }
        self.cleanfilelists();
        if self.isset(RCS) && self.interact() {
            if !self.nohistsave {
                let mut writeflags = crate::hist::HFILE_USE_OPTIONS;
                if from_where == ZEXIT_SIGNAL {
                    writeflags |= crate::hist::HFILE_NO_REWRITE;
                }
                self.saveandpophiststack(1, writeflags);
                self.savehistfile(None, true, writeflags);
            }
            if self.islogin && !self.subsh {
                self.sourcehome(b".zlogout");
                if self.isset(RCS) && self.isset(GLOBALRCS) {
                    let _ = self.source(b"/etc/zlogout");
                }
            }
        }
        self.lastval = self.exit_val;
        self.errflag.set(0);
        self.intrap = 0;
        if self.sigtrapped.first().copied().unwrap_or(0) != 0 {
            self.dotrap(crate::signames::SIGEXIT);
        }
        let _ = self.callhookfunc(b"zshexit", None, true);
        if self.opts[MONITOR] && self.interact() && self.shtty != -1 {
            self.release_pgrp();
        }
        let ev = self.exit_val;
        // SAFETY: getpid has no preconditions.
        if self.mypid != unsafe { libc::getpid() } {
            crate::shell::exit_now(ev);
        } else {
            crate::shell::flush_and_exit(ev);
        }
    }

    /// zsh's `checkrmall`.
    pub(crate) fn checkrmall(&mut self, s: &[u8]) -> bool {
        if self.shout < 0 {
            return true;
        }
        let mut s = s.to_vec();
        if s.first() != Some(&b'/') {
            let mut p = if self.pwd.len() > 1 {
                self.pwd.clone()
            } else {
                Vec::new()
            };
            p.push(b'/');
            p.extend(s);
            s = p;
        }
        let max_count = 100;
        let mut count = 0;
        let ignoredots = !self.isset(GLOBDOTS);
        if let Ok(rd) = std::fs::read_dir(std::ffi::OsStr::from_bytes_compat(
            &crate::tok::unmetafy(&s),
        )) {
            use std::os::unix::ffi::OsStrExt;
            for e in rd.flatten() {
                if ignoredots && e.file_name().as_bytes().first() == Some(&b'.') {
                    continue;
                }
                count += 1;
                if count > max_count {
                    break;
                }
            }
        }
        let mut out: Vec<u8> = if count > max_count {
            format!("zsh: sure you want to delete more than {max_count} files in ").into_bytes()
        } else if count == 1 {
            b"zsh: sure you want to delete the only file in ".to_vec()
        } else if count > 0 {
            format!("zsh: sure you want to delete all {count} files in ").into_bytes()
        } else {
            b"zsh: sure you want to delete all the files in ".to_vec()
        };
        out.extend(self.nicezputs(&s));
        let shout = self.shout;
        if self.isset(RMSTARWAIT) {
            out.extend_from_slice(b"? (waiting ten seconds)");
            write_fd(shout, &out);
            out.clear();
            self.zbeep();
            std::thread::sleep(std::time::Duration::from_secs(10));
            write_fd(shout, b"\n");
        }
        if self.errflag() {
            return false;
        }
        out.extend_from_slice(b" [yn]? ");
        write_fd(shout, &out);
        self.zbeep();
        self.getquery(b"ny", true) == i32::from(b'y')
    }
}

use crate::tables::OsStrCompat;

/// Whether `path` (unmetafied) is an executable regular file, as the `*`
/// glob qualifier tests.
pub(crate) fn exec_regular(path: &[u8]) -> bool {
    access_bytes(path, libc::X_OK)
        && stat_bytes(path).is_some_and(|st| {
            (st.st_mode & libc::S_IFMT) == libc::S_IFREG && st.st_mode & 0o111 != 0
        })
}

/// `FDT_EXTERNAL` re-exported for modules.
pub(crate) const FDT_EXTERNAL_: u8 = FDT_EXTERNAL;
