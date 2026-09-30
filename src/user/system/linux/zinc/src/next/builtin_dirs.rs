//! Directory builtins (zsh's `builtin.c`): `cd`, `chdir`, `pushd`, `popd`,
//! `dirs` and `pwd`, with `fixdir` and `lchdir` from `utils.c`/`compat.c`.

use crate::builtin::Options;
use crate::options::*;
use crate::params::*;
use crate::shell::Shell;
use crate::signals::errno;
use crate::sysutil::errmsg;
use crate::tok;
use crate::utils::lossy;

use crate::builtin::{BIN_CD, BIN_POPD, BIN_PUSHD};

fn stat_path(p: &[u8]) -> Option<libc::stat> {
    crate::sysutil::stat_bytes(p)
}

fn chdir_bytes(p: &[u8]) -> i32 {
    match std::ffi::CString::new(p) {
        // SAFETY: c is NUL-terminated.
        Ok(c) => unsafe { libc::chdir(c.as_ptr()) },
        Err(_) => -1,
    }
}

impl Shell {
    /// zsh's `fixdir`: normalise the metafied path `src`, which becomes
    /// unmetafied. True when a `..` means links must be chased.
    pub(crate) fn fixdir_exact(&self, src: &mut Vec<u8>) -> bool {
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
        let mut dest: Vec<u8> = Vec::new();
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
                        let isdir = stat_path(&dest)
                            .is_some_and(|st| st.st_mode & libc::S_IFMT == libc::S_IFDIR);
                        if !isdir {
                            let mut rest = tok::unmetafy(s.get(i..).unwrap_or(&[]));
                            if dest.len() == i {
                                // zsh writes '.' over the NUL it put at dest.
                                if let Some(first) = rest.first_mut() {
                                    *first = b'.';
                                }
                            }
                            dest.append(&mut rest);
                            *src = dest;
                            return true;
                        }
                        let _ = dest.pop();
                        while dest.len() > 1 && dest.last() != Some(&b'/') {
                            let _ = dest.pop();
                        }
                        if dest.last() != Some(&b'/') {
                            let _ = dest.pop();
                        }
                    }
                    i += 1;
                    loop {
                        i += 1;
                        if at(i) != b'/' {
                            break;
                        }
                    }
                    continue;
                }
            }
            if at(i) == b'.' && (at(i + 1) == b'/' || at(i + 1) == 0) {
                loop {
                    i += 1;
                    if at(i) != b'/' {
                        break;
                    }
                }
            } else {
                while i < s.len() && at(i) != b'/' {
                    let c = at(i);
                    i += 1;
                    if c == tok::META {
                        dest.push(at(i) ^ 32);
                        i += 1;
                    } else {
                        dest.push(c);
                    }
                }
            }
        }
    }

    /// zsh's `lchdir` without a saved-directory argument.
    fn lchdir(&mut self, path: &[u8], hard: bool) -> i32 {
        if !hard {
            return chdir_bytes(path);
        }
        // SAFETY: opening "." read-only.
        let dirfd = unsafe { libc::open(c".".as_ptr(), libc::O_RDONLY | libc::O_NOCTTY) };
        if path.first() == Some(&b'/') && chdir_bytes(b"/") < 0 {
            self.zwarn(&format!("failed to chdir(/): {}", errmsg(errno())));
        }
        let mut err = 0;
        for comp in path.split(|&c| c == b'/').filter(|c| !c.is_empty()) {
            let Ok(c) = std::ffi::CString::new(comp) else {
                err = libc::ENOENT;
                break;
            };
            let Some(st1) = crate::sysutil::lstat_bytes(comp) else {
                err = errno();
                break;
            };
            if st1.st_mode & libc::S_IFMT != libc::S_IFDIR {
                err = libc::ENOTDIR;
                break;
            }
            // SAFETY: c is NUL-terminated.
            if unsafe { libc::chdir(c.as_ptr()) } != 0 {
                err = errno();
                break;
            }
            let Some(st2) = crate::sysutil::lstat_bytes(b".") else {
                err = errno();
                break;
            };
            if st1.st_dev != st2.st_dev || st1.st_ino != st2.st_ino {
                err = libc::ENOTDIR;
                break;
            }
        }
        if err == 0 {
            if dirfd >= 0 {
                // SAFETY: closing the saved descriptor.
                unsafe {
                    libc::close(dirfd);
                }
            }
            return 0;
        }
        // SAFETY: returning to the saved directory, then closing it.
        let restored = dirfd >= 0 && unsafe { libc::fchdir(dirfd) } == 0;
        if dirfd >= 0 {
            // SAFETY: closing the saved descriptor.
            unsafe {
                libc::close(dirfd);
            }
        }
        if !restored {
            let restoreerr = errno();
            let home = self.home.clone();
            let mut ok = false;
            for cdest in [home, b"/".to_vec()] {
                if cdest.is_empty() {
                    continue;
                }
                self.pwd = cdest.clone();
                if chdir_bytes(&cdest) == 0 {
                    ok = true;
                    break;
                }
            }
            if ok {
                self.zerr(&format!(
                    "lost current directory: {}: changed to `{}'",
                    errmsg(restoreerr),
                    lossy(&self.pwd)
                ));
            } else {
                self.zerr(&format!(
                    "lost current directory, failed to cd to /: {}",
                    errmsg(errno())
                ));
            }
            crate::exec_redir::set_errno(err);
            return -2;
        }
        crate::exec_redir::set_errno(err);
        -1
    }

    /// zsh's `set_pwd_env`.
    pub(crate) fn set_pwd_env(&mut self) {
        for name in [&b"PWD"[..], b"OLDPWD"] {
            if self
                .paramtab()
                .get(name)
                .is_some_and(|pm| pm_type(pm.flags) != PM_SCALAR)
            {
                if let Some(pm) = self.paramtab_mut().get_mut(name) {
                    pm.flags &= !PM_READONLY;
                }
                let mut r = PmRef::Name(name.to_vec());
                let _ = self.unsetparam_pm(&mut r, false, true);
            }
        }
        let pwd = self.pwd.clone();
        let oldpwd = self.oldpwd_var.clone();
        let _ = self.assignsparam(b"PWD", pwd.clone(), 0);
        let _ = self.assignsparam(b"OLDPWD", oldpwd.clone(), 0);
        if self
            .paramtab()
            .get(b"PWD")
            .is_some_and(|pm| pm.flags & PM_EXPORTED == 0)
        {
            self.addenv(b"PWD", &pwd, 0);
        }
        if self
            .paramtab()
            .get(b"OLDPWD")
            .is_some_and(|pm| pm.flags & PM_EXPORTED == 0)
        {
            self.addenv(b"OLDPWD", &oldpwd, 0);
        }
    }

    /// zsh's `bin_pwd`.
    pub(crate) fn bin_pwd(
        &mut self,
        _name: &[u8],
        _argv: Vec<Vec<u8>>,
        ops: &Options,
        _func: i32,
    ) -> i32 {
        if ops.isset(b'r') || ops.isset(b'P') || (self.isset(CHASELINKS) && !ops.isset(b'L')) {
            let mut out = self.zgetcwd();
            out.push(b'\n');
            self.write_stdout(&out);
        } else {
            let mut out = tok::unmetafy(&self.pwd);
            out.push(b'\n');
            self.write_stdout(&out);
        }
        0
    }

    /// zsh's `bin_dirs`.
    pub(crate) fn bin_dirs(
        &mut self,
        _name: &[u8],
        argv: Vec<Vec<u8>>,
        ops: &Options,
        _func: i32,
    ) -> i32 {
        self.queue_signals();
        if !(!argv.is_empty() || ops.isset(b'c')) || ops.isset(b'v') || ops.isset(b'p') {
            let mut out = Vec::new();
            if ops.isset(b'v') {
                out.extend_from_slice(b"0\t");
            }
            let show = |sh: &mut Shell, d: &[u8]| {
                if ops.isset(b'l') {
                    tok::unmetafy(d)
                } else {
                    sh.fprintdir(d)
                }
            };
            let pwd = self.pwd.clone();
            out.extend(show(self, &pwd));
            let stack = self.dirstack.clone();
            for (k, d) in stack.iter().enumerate() {
                if ops.isset(b'v') {
                    out.extend_from_slice(format!("\n{}\t", k + 1).as_bytes());
                } else if ops.isset(b'p') {
                    out.push(b'\n');
                } else {
                    out.push(b' ');
                }
                out.extend(show(self, d));
            }
            self.unqueue_signals();
            out.push(b'\n');
            self.write_stdout(&out);
            return 0;
        }
        self.dirstack = argv;
        self.unqueue_signals();
        0
    }

    /// zsh's `bin_cd`.
    pub(crate) fn bin_cd(
        &mut self,
        nam: &[u8],
        argv: Vec<Vec<u8>>,
        ops: &Options,
        func: i32,
    ) -> i32 {
        let namstr = lossy(nam);
        if self.isset(RESTRICTED) {
            self.zwarnnam(&namstr, "restricted");
            return 1;
        }
        self.doprintdir = i32::from(self.doprintdir == -1);
        self.chasinglinks = ops.isset(b'P') || (self.isset(CHASELINKS) && !ops.isset(b'L'));
        self.queue_signals();
        let pwd = self.pwd.clone();
        self.dirstack.insert(0, pwd);
        let Some(dir) = self.cd_get_dest(&namstr, &argv, ops.isset(b's'), func) else {
            if !self.dirstack.is_empty() {
                let _ = self.dirstack.remove(0);
            }
            self.unqueue_signals();
            return 1;
        };
        self.cd_new_pwd(func, dir, ops.isset(b'q'));
        self.unqueue_signals();
        0
    }

    /// zsh's `cd_get_dest`: the index in `dirstack` of the destination.
    fn cd_get_dest(&mut self, nam: &str, argv: &[Vec<u8>], hard: bool, func: i32) -> Option<usize> {
        let mut dir: Option<usize> = None;
        match argv {
            [] => {
                if func == BIN_POPD && self.dirstack.len() < 2 {
                    self.zwarnnam(nam, "directory stack empty");
                    return None;
                }
                if func == BIN_PUSHD && self.unset_opt(PUSHDTOHOME) && self.dirstack.len() >= 2 {
                    dir = Some(1);
                }
                if let Some(d) = dir {
                    let first = self.dirstack.remove(0);
                    self.dirstack.insert(d, first);
                } else if func != BIN_POPD {
                    if self.home.is_empty() && self.getsparam(b"HOME").is_none() {
                        self.zwarnnam(nam, "HOME not set");
                        return None;
                    }
                    let home = self.home.clone();
                    self.dirstack.insert(0, home);
                }
            }
            [a] => {
                self.doprintdir += 1;
                let digits = a.get(1..).unwrap_or(&[]);
                if !self.isset(POSIXCD)
                    && a.len() > 1
                    && (a.first() == Some(&b'+') || a.first() == Some(&b'-'))
                    && digits.iter().all(u8::is_ascii_digit)
                {
                    let dd =
                        usize::try_from(crate::utils::zstrtol(digits, 10).0).unwrap_or(usize::MAX);
                    let from_top = (a.first() == Some(&b'+')) ^ self.isset(PUSHDMINUS);
                    let n = self.dirstack.len();
                    let idx = if from_top {
                        dd
                    } else {
                        n.checked_sub(1 + dd).unwrap_or(usize::MAX)
                    };
                    if idx >= n {
                        self.zwarnnam(nam, "no such entry in dir stack");
                        return None;
                    }
                    dir = Some(idx);
                }
                if dir.is_none() {
                    let entry = if a.as_slice() != b"-" {
                        self.doprintdir -= 1;
                        a.clone()
                    } else {
                        self.oldpwd_var.clone()
                    };
                    self.dirstack.insert(0, entry);
                }
            }
            [a, b, ..] => {
                let Some(pos) = self
                    .pwd
                    .windows(a.len().max(1))
                    .position(|w| w == a.as_slice())
                    .filter(|_| !a.is_empty())
                else {
                    self.zwarnnam(nam, &format!("string not in pwd: {}", lossy(a)));
                    return None;
                };
                let mut d = self.pwd.get(..pos).unwrap_or(&[]).to_vec();
                d.extend_from_slice(b);
                d.extend_from_slice(self.pwd.get(pos + a.len()..).unwrap_or(&[]));
                self.dirstack.insert(0, d);
                self.doprintdir += 1;
            }
        }
        let target = dir;
        if func == BIN_POPD {
            match dir {
                None => {
                    dir = Some(0);
                }
                Some(d) if d != 0 => return Some(d),
                _ => {}
            }
            dir = dir.map(|d| d + 1);
        }
        let d = match dir {
            Some(d) if d < self.dirstack.len() => d,
            _ => 0,
        };
        let entry = self.dirstack.get(d).cloned()?;
        match self.cd_do_chdir(nam, &entry, hard) {
            None => {
                if target.is_none() && func != BIN_POPD && !self.dirstack.is_empty() {
                    let _ = self.dirstack.remove(0);
                }
                if func == BIN_POPD && d < self.dirstack.len() {
                    let _ = self.dirstack.remove(d);
                }
                None
            }
            Some(dest) => {
                if let Some(slot) = self.dirstack.get_mut(d) {
                    *slot = dest;
                }
                Some(if func == BIN_POPD {
                    target.unwrap_or(0)
                } else {
                    target.unwrap_or(d)
                })
            }
        }
    }

    /// zsh's `cd_do_chdir`.
    fn cd_do_chdir(&mut self, cnam: &str, dest: &[u8], hard: bool) -> Option<Vec<u8>> {
        let at = |i: usize| dest.get(i).copied().unwrap_or(0);
        let nocdpath = at(0) == b'.'
            && (at(1) == b'/' || at(1) == 0 || (at(1) == b'.' && (at(2) == b'/' || at(2) == 0)));
        if at(0) == b'/' {
            if let Some(r) = self.cd_try_chdir(None, dest, hard) {
                return Some(r);
            }
            self.zwarnnam(cnam, &format!("{}: {}", errmsg(errno()), lossy(dest)));
            return None;
        }
        let cdpath = self.arrvar(ArrVar::Cdpath).to_vec();
        let mut eno = libc::ENOENT;
        let mut hasdot = false;
        if !nocdpath && !self.isset(POSIXCD) {
            hasdot = cdpath.iter().any(|p| p.is_empty() || p.as_slice() == b".");
        }
        if !hasdot && !self.isset(POSIXCD) {
            if let Some(r) = self.cd_try_chdir(None, dest, hard) {
                return Some(r);
            }
            if errno() != libc::ENOENT {
                eno = errno();
            }
        }
        if !nocdpath {
            for pp in &cdpath {
                if let Some(r) = self.cd_try_chdir(Some(pp), dest, hard) {
                    if self.isset(POSIXCD) {
                        if !pp.is_empty() {
                            self.doprintdir += 1;
                        }
                    } else if pp.as_slice() != b"." {
                        self.doprintdir += 1;
                    }
                    return Some(r);
                }
                if errno() != libc::ENOENT {
                    eno = errno();
                }
            }
        }
        if self.isset(POSIXCD) {
            if let Some(r) = self.cd_try_chdir(None, dest, hard) {
                return Some(r);
            }
            if errno() != libc::ENOENT {
                eno = errno();
            }
        }
        if let Some(t) = self.cd_able_vars(dest) {
            if let Some(r) = self.cd_try_chdir(None, &t, hard) {
                self.doprintdir += 1;
                return Some(r);
            }
            if errno() != libc::ENOENT {
                eno = errno();
            }
        }
        self.zwarnnam(cnam, &format!("{}: {}", errmsg(eno), lossy(dest)));
        None
    }

    /// zsh's `cd_able_vars`.
    pub(crate) fn cd_able_vars(&mut self, s: &[u8]) -> Option<Vec<u8>> {
        if !self.isset(CDABLEVARS) {
            return None;
        }
        let slash = s.iter().position(|&c| c == b'/').unwrap_or(s.len());
        let mut d = self.getnameddir(s.get(..slash).unwrap_or(&[]))?;
        d.extend_from_slice(s.get(slash..).unwrap_or(&[]));
        Some(d)
    }

    /// zsh's `cd_try_chdir`.
    fn cd_try_chdir(&mut self, pfix: Option<&[u8]>, dest: &[u8], hard: bool) -> Option<Vec<u8>> {
        let mut buf = match pfix {
            Some(p) if !p.is_empty() => {
                if p.first() == Some(&b'/') {
                    let mut b = p.to_vec();
                    b.push(b'/');
                    b.extend_from_slice(dest);
                    b
                } else {
                    let mut b = if self.pwd.as_slice() == b"/" {
                        Vec::new()
                    } else {
                        self.pwd.clone()
                    };
                    b.push(b'/');
                    b.extend_from_slice(p);
                    b.push(b'/');
                    b.extend_from_slice(dest);
                    b
                }
            }
            _ if dest.first() == Some(&b'/') => dest.to_vec(),
            _ => {
                let mut b = self.pwd.clone();
                if b.last() == Some(&b'/') {
                    let _ = b.pop();
                }
                b.push(b'/');
                b.extend_from_slice(dest);
                b
            }
        };
        let mut dochaselinks = false;
        if self.chasinglinks {
            buf = tok::unmetafy(&buf);
        } else {
            dochaselinks = self.fixdir_exact(&mut buf);
        }
        if self.lchdir(&buf, hard) != 0
            && (pfix.is_some()
                || dest.first() == Some(&b'/')
                || self.lchdir(&tok::unmetafy(dest), hard) != 0)
        {
            return None;
        }
        if dochaselinks {
            self.chasinglinks = true;
        }
        Some(tok::metafy(&buf))
    }

    /// zsh's `cd_new_pwd`.
    fn cd_new_pwd(&mut self, func: i32, dir: usize, quiet: bool) {
        if func == BIN_PUSHD {
            // rolllist: the entries from `dir` on move to the front.
            let tail = self.dirstack.split_off(dir.min(self.dirstack.len()));
            let head = std::mem::replace(&mut self.dirstack, tail);
            self.dirstack.extend(head);
        }
        let idx = if func == BIN_PUSHD { 0 } else { dir };
        let mut new_pwd = if idx < self.dirstack.len() {
            self.dirstack.remove(idx)
        } else {
            self.pwd.clone()
        };
        if func == BIN_POPD && !self.dirstack.is_empty() {
            new_pwd = self.dirstack.remove(0);
        } else if func == BIN_CD && self.unset_opt(AUTOPUSHD) && !self.dirstack.is_empty() {
            let _ = self.dirstack.remove(0);
        }
        if self.chasinglinks
            && let Some(s) = self.findpwd(&new_pwd)
        {
            new_pwd = s;
        }
        if self.isset(PUSHDIGNOREDUPS)
            && let Some(p) = self.dirstack.iter().position(|d| *d == new_pwd)
        {
            let _ = self.dirstack.remove(p);
        }
        match (stat_path(&tok::unmetafy(&new_pwd)), stat_path(b".")) {
            (None, _) => new_pwd = tok::metafy(&self.zgetcwd()),
            (Some(_), None) => {
                if chdir_bytes(&tok::unmetafy(&new_pwd)) < 0 {
                    self.zwarn(&format!(
                        "unable to chdir({}): {}",
                        lossy(&new_pwd),
                        errmsg(errno())
                    ));
                }
            }
            (Some(st1), Some(st2)) => {
                if st1.st_ino != st2.st_ino || st1.st_dev != st2.st_dev {
                    if self.chasinglinks {
                        new_pwd = tok::metafy(&self.zgetcwd());
                    } else if chdir_bytes(&tok::unmetafy(&new_pwd)) < 0 {
                        self.zwarn(&format!(
                            "unable to chdir({}): {}",
                            lossy(&new_pwd),
                            errmsg(errno())
                        ));
                    }
                }
            }
        }
        self.oldpwd_var = std::mem::take(&mut self.pwd);
        self.setjobpwd();
        self.pwd = new_pwd;
        self.set_pwd_env();
        if self.isset(INTERACTIVE) || self.isset(POSIXCD) {
            if func != BIN_CD && self.isset(INTERACTIVE) {
                if self.unset_opt(PUSHDSILENT) && !quiet {
                    self.printdirstack();
                }
            } else if self.unset_opt(CDSILENT) && self.doprintdir != 0 {
                let pwd = self.pwd.clone();
                let mut out = self.fprintdir(&pwd);
                out.push(b'\n');
                self.write_stdout(&out);
            }
        }
        if !quiet {
            let _ = self.callhookfunc(b"chpwd", None, true);
        }
        let dirstacksize = self.getiparam(b"DIRSTACKSIZE");
        if dirstacksize > 0 {
            let keep = usize::try_from(dirstacksize.max(2)).unwrap_or(2);
            let n = self.dirstack.len();
            // zsh removes count - size + 1 entries from the end.
            let remove = (n + 1).saturating_sub(keep);
            for _ in 0..remove {
                let _ = self.dirstack.pop();
            }
        }
    }

    /// zsh's `printdirstack`.
    fn printdirstack(&mut self) {
        let pwd = self.pwd.clone();
        let mut out = self.fprintdir(&pwd);
        for d in self.dirstack.clone() {
            out.push(b' ');
            out.extend(self.fprintdir(&d));
        }
        out.push(b'\n');
        self.write_stdout(&out);
    }

    /// zsh's `findpwd`.
    fn findpwd(&mut self, s: &[u8]) -> Option<Vec<u8>> {
        if s.first() == Some(&b'/') {
            return self.xsymlink(s);
        }
        let mut p = if self.pwd.len() > 1 {
            self.pwd.clone()
        } else {
            Vec::new()
        };
        p.push(b'/');
        p.extend_from_slice(s);
        self.xsymlink(&p)
    }
}
