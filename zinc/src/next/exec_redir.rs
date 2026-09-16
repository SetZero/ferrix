//! Redirections (zsh's `exec.c` and `glob.c`): the run-time form of a
//! redirection, multios, `addfd`, here-strings and process substitution in
//! redirections.

use crate::ast::{Redir, RedirKind};
use crate::exec::*;
use crate::options::*;
use crate::shell::Shell;
use crate::signals::{child_block, child_unblock, errno};
use crate::sysutil::errmsg;
use crate::tok;
use crate::utils::lossy;

pub(crate) const REDIR_WRITE: i32 = 0;
pub(crate) const REDIR_WRITENOW: i32 = 1;
pub(crate) const REDIR_APP: i32 = 2;
pub(crate) const REDIR_APPNOW: i32 = 3;
pub(crate) const REDIR_ERRWRITE: i32 = 4;
pub(crate) const REDIR_ERRWRITENOW: i32 = 5;
pub(crate) const REDIR_ERRAPP: i32 = 6;
pub(crate) const REDIR_ERRAPPNOW: i32 = 7;
pub(crate) const REDIR_READWRITE: i32 = 8;
pub(crate) const REDIR_READ: i32 = 9;
pub(crate) const REDIR_HEREDOC: i32 = 10;
pub(crate) const REDIR_HEREDOCDASH: i32 = 11;
pub(crate) const REDIR_HERESTR: i32 = 12;
pub(crate) const REDIR_MERGEIN: i32 = 13;
pub(crate) const REDIR_MERGEOUT: i32 = 14;
pub(crate) const REDIR_CLOSE: i32 = 15;
pub(crate) const REDIR_INPIPE: i32 = 16;
pub(crate) const REDIR_OUTPIPE: i32 = 17;

pub(crate) const REDIRF_FROM_HEREDOC: i32 = 1;

fn is_write_file(t: i32) -> bool {
    (REDIR_WRITE..=REDIR_READWRITE).contains(&t)
}

fn is_append_redir(t: i32) -> bool {
    is_write_file(t) && t & 2 != 0
}

fn is_clobber_redir(t: i32) -> bool {
    is_write_file(t) && t & 1 != 0
}

fn is_error_redir(t: i32) -> bool {
    (REDIR_ERRWRITE..=REDIR_ERRAPPNOW).contains(&t)
}

/// A redirection as execution sees it (zsh's `struct redir`).
#[derive(Debug, Clone)]
pub(crate) struct XRedir {
    pub(crate) typ: i32,
    pub(crate) flags: i32,
    pub(crate) fd1: i32,
    pub(crate) fd2: i32,
    pub(crate) name: Vec<u8>,
    pub(crate) varid: Option<Vec<u8>>,
}

/// zsh's `ecgetredirs`: the run-time redirections for a command's.
pub(crate) fn xredirs(rs: &[Redir]) -> Vec<XRedir> {
    rs.iter()
        .map(|r| {
            let (typ, flags, name) = match r.kind {
                RedirKind::HereDoc | RedirKind::HereDocDash => {
                    let body = r
                        .heredoc
                        .as_ref()
                        .map(|h| h.borrow().body.clone())
                        .unwrap_or_default();
                    (REDIR_HERESTR, REDIRF_FROM_HEREDOC, body)
                }
                k => (
                    match k {
                        RedirKind::Write => REDIR_WRITE,
                        RedirKind::WriteNow => REDIR_WRITENOW,
                        RedirKind::App => REDIR_APP,
                        RedirKind::AppNow => REDIR_APPNOW,
                        RedirKind::ErrWrite => REDIR_ERRWRITE,
                        RedirKind::ErrWriteNow => REDIR_ERRWRITENOW,
                        RedirKind::ErrApp => REDIR_ERRAPP,
                        RedirKind::ErrAppNow => REDIR_ERRAPPNOW,
                        RedirKind::ReadWrite => REDIR_READWRITE,
                        RedirKind::Read => REDIR_READ,
                        RedirKind::HereStr => REDIR_HERESTR,
                        RedirKind::MergeIn => REDIR_MERGEIN,
                        RedirKind::MergeOut => REDIR_MERGEOUT,
                        RedirKind::InPipe => REDIR_INPIPE,
                        RedirKind::OutPipe => REDIR_OUTPIPE,
                        RedirKind::HereDoc | RedirKind::HereDocDash => REDIR_HERESTR,
                    },
                    0,
                    r.target.clone(),
                ),
            };
            XRedir {
                typ,
                flags,
                fd1: r.fd,
                fd2: 0,
                name,
                varid: r.varid.clone(),
            }
        })
        .collect()
}

/// A multio: the descriptors joined on one of 0..9 (zsh's `struct multio`).
#[derive(Debug, Clone, Default)]
pub(crate) struct Multio {
    pub(crate) ct: usize,
    pub(crate) rflag: i32,
    pub(crate) pipe: i32,
    pub(crate) fds: Vec<i32>,
}

pub(crate) type Mfds = [Option<Multio>; 10];

impl Shell {
    /// zsh's `checkclobberparam`.
    fn checkclobberparam(&mut self, f: &XRedir) -> bool {
        let Some(varid) = &f.varid else { return true };
        let s = varid.clone();
        let mut i = 0;
        let Some(mut v) = self.getvalue(&s, &mut i, 0) else {
            return true;
        };
        if self.pm_flags(&v.pm) & crate::params::PM_READONLY != 0 {
            self.zwarn(&format!(
                "can't allocate file descriptor to readonly parameter {}",
                lossy(varid)
            ));
            return false;
        }
        if !self.isset(CLOBBER) {
            let sv = self.getstrvalue(Some(&mut v));
            let (fd, used) = crate::utils::zstrtol(&sv, 10);
            if fd >= 0
                && used == sv.len()
                && !sv.is_empty()
                && i32::try_from(fd)
                    .is_ok_and(|fd| fd <= self.max_zsh_fd && self.fdtable_get(fd) == FDT_EXTERNAL)
            {
                self.zwarn(&format!(
                    "can't clobber parameter {} containing file descriptor {fd}",
                    lossy(varid)
                ));
                return false;
            }
        }
        true
    }

    /// zsh's `clobber_open`.
    fn clobber_open(&mut self, f: &XRedir) -> i32 {
        let Ok(c) = std::ffi::CString::new(tok::unmetafy(&f.name)) else {
            return -1;
        };
        let open = |flags: i32| -> i32 {
            // SAFETY: c is NUL-terminated.
            unsafe { libc::open(c.as_ptr(), flags, 0o666) }
        };
        if self.isset(CLOBBER) || is_clobber_redir(f.typ) {
            return open(libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOCTTY);
        }
        let fd = open(libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOCTTY);
        if fd >= 0 {
            return fd;
        }
        let oerrno = errno();
        let fd = open(libc::O_WRONLY | libc::O_NOCTTY);
        if fd != -1 {
            // SAFETY: an all-zero stat is valid for fstat to fill.
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: st is a valid out-pointer.
            if unsafe { libc::fstat(fd, &mut st) } == 0 {
                if st.st_mode & libc::S_IFMT != libc::S_IFREG {
                    return fd;
                }
                if self.isset(CLOBBEREMPTY) && st.st_size == 0 {
                    return fd;
                }
            }
            // SAFETY: closing the descriptor just opened.
            unsafe {
                libc::close(fd);
            }
        }
        set_errno(oerrno);
        -1
    }

    /// zsh's `closemn`.
    pub(crate) fn closemn(&mut self, mfds: &mut Mfds, fd: i32, typ: i32) {
        let Ok(fdu) = usize::try_from(fd) else { return };
        let Some(Some(mn)) = mfds.get(fdu).cloned() else {
            return;
        };
        if mn.ct >= 2 {
            child_block();
            let mut bgtime = (0, 0);
            let pid = self.zfork(Some(&mut bgtime));
            if pid != 0 {
                for &f in mn.fds.iter().take(mn.ct) {
                    let _ = self.zclose(f);
                }
                let _ = self.zclose(mn.pipe);
                if pid == -1 {
                    if let Some(slot) = mfds.get_mut(fdu) {
                        *slot = None;
                    }
                    child_unblock();
                    return;
                }
                if let Some(Some(m)) = mfds.get_mut(fdu) {
                    m.ct = 1;
                    m.fds = vec![fd];
                }
                self.addproc(pid, None, true, bgtime, -1, -1);
                child_unblock();
                return;
            }
            child_unblock();
            self.closeallelse(&mn);
            let mut buf = [0u8; 4092];
            if mn.rflag != 0 {
                loop {
                    // SAFETY: buf is writable for its length.
                    let len = unsafe { libc::read(mn.pipe, buf.as_mut_ptr().cast(), buf.len()) };
                    if len == 0 {
                        break;
                    }
                    if len < 0 {
                        if errno() == libc::EINTR {
                            continue;
                        }
                        break;
                    }
                    let chunk = buf.get(..usize::try_from(len).unwrap_or(0)).unwrap_or(&[]);
                    for &f in mn.fds.iter().take(mn.ct) {
                        let _ = self.write_loop(f, chunk);
                    }
                }
            } else {
                for &f in mn.fds.iter().take(mn.ct) {
                    loop {
                        // SAFETY: buf is writable for its length.
                        let len = unsafe { libc::read(f, buf.as_mut_ptr().cast(), buf.len()) };
                        if len == 0 {
                            break;
                        }
                        if len < 0 {
                            if errno() == libc::EINTR {
                                continue;
                            }
                            break;
                        }
                        let chunk = buf.get(..usize::try_from(len).unwrap_or(0)).unwrap_or(&[]);
                        let _ = self.write_loop(mn.pipe, chunk);
                    }
                }
            }
            crate::shell::exit_now(0);
        } else if typ == REDIR_CLOSE
            && let Some(slot) = mfds.get_mut(fdu)
        {
            *slot = None;
        }
    }

    /// zsh's `closemnodes`.
    pub(crate) fn closemnodes(&mut self, mfds: &mut Mfds) {
        for slot in mfds.iter_mut() {
            if let Some(m) = slot.take() {
                for &f in m.fds.iter().take(m.ct) {
                    let _ = self.zclose(f);
                }
            }
        }
    }

    /// zsh's `closeallelse`.
    fn closeallelse(&mut self, mn: &Multio) {
        let openmax = i32::try_from(self.fdtable.len()).unwrap_or(0);
        for i in 0..openmax {
            if mn.pipe != i && !mn.fds.iter().take(mn.ct).any(|&f| f == i) {
                let _ = self.zclose(i);
            }
        }
    }

    /// zsh's `addfd`.
    #[expect(clippy::too_many_arguments, reason = "zsh's addfd takes eight")]
    pub(crate) fn addfd(
        &mut self,
        forked: bool,
        save: &mut [i32; 10],
        mfds: &mut Mfds,
        fd1: i32,
        fd2: i32,
        rflag: i32,
        varid: Option<&[u8]>,
    ) {
        if let Some(varid) = varid {
            let nfd = self.movefd(fd2);
            if nfd == -1 {
                self.zerr(&format!("cannot moved fd {fd2}: {}", errmsg(errno())));
                return;
            }
            self.fdtable_mark(nfd, FDT_EXTERNAL);
            let _ = self.setiparam(varid, i64::from(nfd));
            if self.errflag() {
                let _ = self.zclose(nfd);
            }
            return;
        }
        let Ok(f1) = usize::try_from(fd1) else { return };
        let exists = mfds.get(f1).is_some_and(Option::is_some);
        if !exists || self.unset_opt(MULTIOS) {
            if !exists {
                if let Some(slot) = mfds.get_mut(f1) {
                    *slot = Some(Multio::default());
                }
                if !forked && save.get(f1) == Some(&-2) {
                    if fd1 == fd2 {
                        if let Some(s) = save.get_mut(f1) {
                            *s = -1;
                        }
                    } else {
                        let fd_n = self.movefd(fd1);
                        if fd_n < 0 {
                            if errno() != libc::EBADF {
                                self.zerr(&format!(
                                    "cannot duplicate fd {fd1}: {}",
                                    errmsg(errno())
                                ));
                                if let Some(slot) = mfds.get_mut(f1) {
                                    *slot = None;
                                }
                                self.closemnodes(mfds);
                                return;
                            }
                        } else {
                            let t = self.fdtable_get(fd_n);
                            self.fdtable_mark(fd_n, t | FDT_SAVED_MASK);
                        }
                        if let Some(s) = save.get_mut(f1) {
                            *s = fd_n;
                        }
                    }
                }
            }
            let _ = self.redup(fd2, fd1);
            if let Some(Some(m)) = mfds.get_mut(f1) {
                m.ct = 1;
                m.fds = vec![fd1];
                m.rflag = rflag;
            }
            return;
        }
        let Some(Some(m)) = mfds.get(f1).cloned() else {
            return;
        };
        if m.rflag != rflag {
            self.zerr(&format!("file mode mismatch on fd {fd1}"));
            self.closemnodes(mfds);
            return;
        }
        if m.ct == 1 {
            let fd_a = self.movefd(fd1);
            if fd_a < 0 {
                self.zerr(&format!("multio failed for fd {fd1}: {}", errmsg(errno())));
                self.closemnodes(mfds);
                return;
            }
            let fd_b = self.movefd(fd2);
            if fd_b < 0 {
                self.zerr(&format!("multio failed for fd {fd2}: {}", errmsg(errno())));
                self.closemnodes(mfds);
                return;
            }
            let mut pipes = [0i32; 2];
            if self.mpipe(&mut pipes) < 0 {
                self.zerr(&format!("multio failed for fd {fd2}: {}", errmsg(errno())));
                self.closemnodes(mfds);
                return;
            }
            let (keep, give) = if rflag != 0 {
                (pipes[0], pipes[1])
            } else {
                (pipes[1], pipes[0])
            };
            let _ = self.redup(if rflag != 0 { pipes[1] } else { pipes[0] }, fd1);
            let _ = give;
            if let Some(Some(mm)) = mfds.get_mut(f1) {
                mm.fds = vec![fd_a, fd_b];
                mm.pipe = keep;
                mm.ct = 2;
            }
        } else {
            let fd_n = self.movefd(fd2);
            if fd_n < 0 {
                self.zerr(&format!("multio failed for fd {fd2}: {}", errmsg(errno())));
                self.closemnodes(mfds);
                return;
            }
            if let Some(Some(mm)) = mfds.get_mut(f1) {
                mm.fds.push(fd_n);
                mm.ct += 1;
            }
        }
    }

    /// zsh's `fixfds`.
    pub(crate) fn fixfds(&mut self, save: &[i32; 10]) {
        let old = errno();
        for (i, &s) in save.iter().enumerate() {
            if s != -2 {
                let _ = self.redup(s, i32::try_from(i).unwrap_or(0));
            }
        }
        set_errno(old);
    }

    /// zsh's `getherestr`.
    pub(crate) fn getherestr(&mut self, f: &XRedir) -> i32 {
        let mut t = self.singsub(&f.name);
        tok::untokenize(&mut t);
        let mut t = tok::unmetafy(&t);
        if f.flags & REDIRF_FROM_HEREDOC == 0 {
            t.push(b'\n');
        }
        let (fd, name) = self.gettempfile(None);
        let Some(name) = name else { return -1 };
        if fd < 0 {
            return -1;
        }
        let _ = self.write_loop(fd, &t);
        let _ = crate::sysutil::close_fd(fd);
        let r = crate::sysutil::open_bytes(&name, libc::O_RDONLY | libc::O_NOCTTY, 0);
        let _ = crate::sysutil::unlink_bytes(&name);
        r
    }

    /// zsh's `xpandredir`: expand a redirection's word. True when the
    /// redirection was replaced by the ones pushed on `extra`.
    pub(crate) fn xpandredir(&mut self, f: &mut XRedir, extra: &mut Vec<XRedir>) -> bool {
        let mut fake = crate::subst::WordList::one(f.name.clone());
        let mut rf = 0;
        self.prefork(
            &mut fake,
            if self.isset(MULTIOS) {
                0
            } else {
                crate::subst::PREFORK_SINGLE
            },
            &mut rf,
        );
        if !self.errflag() && self.isset(MULTIOS) {
            self.globlist(&mut fake, 0);
        }
        if self.errflag() {
            return false;
        }
        let mut ret = false;
        if fake.words.len() == 1 {
            let mut s = fake.words.pop().unwrap_or_default();
            tok::untokenize(&mut s);
            f.name = s.clone();
            if f.typ == REDIR_MERGEIN || f.typ == REDIR_MERGEOUT {
                if s == b"-" || s == [tok::DASH] {
                    f.typ = REDIR_CLOSE;
                } else if s == b"p" {
                    f.fd2 = -2;
                } else if !s.is_empty() && s.iter().all(u8::is_ascii_digit) {
                    f.fd2 = i32::try_from(crate::utils::zstrtol(&s, 10).0).unwrap_or(-1);
                } else if f.typ == REDIR_MERGEIN {
                    self.zerr("file number expected");
                } else {
                    f.typ = REDIR_ERRWRITE;
                }
            }
        } else if f.typ == REDIR_MERGEIN {
            self.zerr("file number expected");
        } else {
            if f.typ == REDIR_MERGEOUT {
                f.typ = REDIR_ERRWRITE;
            }
            for nam in fake.words {
                let mut ff = f.clone();
                ff.name = nam;
                extra.push(ff);
                ret = true;
            }
        }
        ret
    }

    /// zsh's `spawnpipes`.
    pub(crate) fn spawnpipes(&mut self, l: &mut [XRedir], nullexec: bool) {
        for f in l.iter_mut() {
            if f.typ == REDIR_OUTPIPE || f.typ == REDIR_INPIPE {
                let name = f.name.clone();
                f.fd2 = self.getpipe(&name, nullexec || f.varid.is_some());
            }
        }
    }

    /// Open the file for a read or write redirection.
    pub(crate) fn redir_open_read(&mut self, f: &XRedir) -> i32 {
        let name = tok::unmetafy(&f.name);
        if f.typ == REDIR_READ {
            crate::sysutil::open_bytes(&name, libc::O_RDONLY | libc::O_NOCTTY, 0)
        } else {
            crate::sysutil::open_bytes(&name, libc::O_RDWR | libc::O_CREAT | libc::O_NOCTTY, 0o666)
        }
    }

    /// Open the file for an output redirection (append or clobber).
    pub(crate) fn redir_open_write(&mut self, f: &XRedir) -> i32 {
        if is_append_redir(f.typ) {
            let Ok(c) = std::ffi::CString::new(tok::unmetafy(&f.name)) else {
                return -1;
            };
            let flags = if self.unset_opt(CLOBBER)
                && self.unset_opt(APPENDCREATE)
                && !is_clobber_redir(f.typ)
            {
                libc::O_WRONLY | libc::O_APPEND | libc::O_NOCTTY
            } else {
                libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT | libc::O_NOCTTY
            };
            // SAFETY: c is NUL-terminated.
            unsafe { libc::open(c.as_ptr(), flags, 0o666) }
        } else {
            self.clobber_open(f)
        }
    }

    /// Apply the redirections of a command (the loop in `execcmd_exec`).
    /// False when a redirection failed (zsh's `execerr`).
    #[expect(
        clippy::too_many_lines,
        reason = "the redirection loop of zsh's execcmd_exec"
    )]
    pub(crate) fn do_redirections(
        &mut self,
        redir: &mut Vec<XRedir>,
        forked: bool,
        save: &mut [i32; 10],
        mfds: &mut Mfds,
        nullexec: i32,
    ) -> bool {
        self.spawnpipes(redir, nullexec != 0);
        let mut queue: std::collections::VecDeque<XRedir> = std::mem::take(redir).into();
        while let Some(mut fnr) = queue.pop_front() {
            if fnr.typ == REDIR_INPIPE || fnr.typ == REDIR_OUTPIPE {
                if !self.checkclobberparam(&fnr) || fnr.fd2 == -1 {
                    if fnr.fd2 != -1 {
                        let _ = self.zclose(fnr.fd2);
                    }
                    self.closemnodes(mfds);
                    self.fixfds(save);
                    return false;
                }
                let r = i32::from(fnr.typ == REDIR_OUTPIPE);
                self.addfd(
                    forked,
                    save,
                    mfds,
                    fnr.fd1,
                    fnr.fd2,
                    r,
                    fnr.varid.as_deref(),
                );
                continue;
            }
            if fnr.typ != REDIR_HERESTR {
                let mut extra = Vec::new();
                if self.xpandredir(&mut fnr, &mut extra) {
                    for (i, e) in extra.into_iter().enumerate() {
                        queue.insert(i, e);
                    }
                    continue;
                }
            }
            if self.errflag() {
                self.closemnodes(mfds);
                self.fixfds(save);
                return false;
            }
            if self.isset(RESTRICTED) && is_write_file(fnr.typ) {
                self.zwarn("writing redirection not allowed in restricted mode");
                return false;
            }
            if self.unset_opt(EXECOPT) {
                continue;
            }
            match fnr.typ {
                REDIR_HERESTR => {
                    let fil = if self.checkclobberparam(&fnr) {
                        self.getherestr(&fnr)
                    } else {
                        -1
                    };
                    if fil == -1 {
                        let e = errno();
                        if e != 0 && e != libc::EINTR {
                            self.zwarn(&format!(
                                "can't create temp file for here document: {}",
                                errmsg(e)
                            ));
                        }
                        self.closemnodes(mfds);
                        self.fixfds(save);
                        return false;
                    }
                    self.addfd(forked, save, mfds, fnr.fd1, fil, 0, fnr.varid.as_deref());
                }
                REDIR_READ | REDIR_READWRITE => {
                    let fil = if self.checkclobberparam(&fnr) {
                        self.redir_open_read(&fnr)
                    } else {
                        -1
                    };
                    if fil == -1 {
                        let e = errno();
                        self.closemnodes(mfds);
                        self.fixfds(save);
                        if e != libc::EINTR {
                            self.zwarn(&format!("{}: {}", errmsg(e), lossy(&fnr.name)));
                        }
                        return false;
                    }
                    self.addfd(forked, save, mfds, fnr.fd1, fil, 0, fnr.varid.as_deref());
                    if nullexec == 1
                        && fnr.fd1 == 0
                        && self.isset(SHINSTDIN)
                        && self.interact()
                        && !self.zleactive
                    {
                        self.init_io_stdin();
                    }
                }
                REDIR_CLOSE => {
                    if let Some(varid) = fnr.varid.clone() {
                        let s = varid.clone();
                        let mut i = 0;
                        let mut bad = 0;
                        match self.getvalue(&s, &mut i, 0) {
                            None => bad = 1,
                            Some(mut v) => {
                                if self.pm_flags(&v.pm) & crate::params::PM_READONLY != 0 {
                                    bad = 2;
                                } else {
                                    let sv = self.getstrvalue(Some(&mut v));
                                    if self.errflag() {
                                        bad = 1;
                                    } else {
                                        let (n, used) = crate::utils::zstrtol(&sv, 0);
                                        if used == 0 {
                                            bad = 1;
                                        } else {
                                            let mut val = n;
                                            let mut rest = sv.get(used..).unwrap_or(&[]).to_vec();
                                            if !rest.is_empty() {
                                                if rest.first() == Some(&b'#')
                                                    && sv.first() != Some(&b'0')
                                                {
                                                    let (n2, used2) = crate::utils::zstrtol(
                                                        rest.get(1..).unwrap_or(&[]),
                                                        u32::try_from(n).unwrap_or(10),
                                                    );
                                                    val = n2;
                                                    rest = if used2 == 0 {
                                                        b"x".to_vec()
                                                    } else {
                                                        rest.get(1 + used2..)
                                                            .unwrap_or(&[])
                                                            .to_vec()
                                                    };
                                                }
                                                if !rest.is_empty() {
                                                    bad = 1;
                                                }
                                            }
                                            fnr.fd1 = i32::try_from(val).unwrap_or(-1);
                                            if bad == 0
                                                && fnr.fd1 <= self.max_zsh_fd
                                                && fnr.fd1 >= 10
                                                && self.fdtable_get(fnr.fd1) & FDT_TYPE_MASK
                                                    == FDT_INTERNAL
                                            {
                                                bad = 3;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if bad != 0 {
                            match bad {
                                1 => self.zwarn(&format!(
                                    "parameter {} does not contain a file descriptor",
                                    lossy(&varid)
                                )),
                                2 => self.zwarn(&format!(
                                    "can't close file descriptor from readonly parameter {}",
                                    lossy(&varid)
                                )),
                                _ => self.zwarn(&format!(
                                    "file descriptor {} used by shell, not closed",
                                    fnr.fd1
                                )),
                            }
                            return false;
                        }
                    }
                    let mut closed = false;
                    if let Ok(f1) = usize::try_from(fnr.fd1)
                        && !forked
                        && fnr.fd1 < 10
                        && save.get(f1) == Some(&-2)
                    {
                        let m = self.movefd(fnr.fd1);
                        if let Some(s) = save.get_mut(f1) {
                            *s = m;
                        }
                        if m >= 0 {
                            closed = true;
                        }
                    }
                    if fnr.fd1 < 10 {
                        self.closemn(mfds, fnr.fd1, REDIR_CLOSE);
                    }
                    if !closed && self.zclose(fnr.fd1) < 0 && fnr.varid.is_some() {
                        self.zwarn(&format!(
                            "failed to close file descriptor {}: {}",
                            fnr.fd1,
                            errmsg(errno())
                        ));
                    }
                }
                REDIR_MERGEIN | REDIR_MERGEOUT => {
                    if fnr.fd2 < 10 {
                        self.closemn(mfds, fnr.fd2, fnr.typ);
                    }
                    let fil = if !self.checkclobberparam(&fnr) {
                        -1
                    } else if fnr.fd2 > 9
                        && fnr.fd2 <= self.max_zsh_fd
                        && ((self.fdtable_get(fnr.fd2) != FDT_UNUSED
                            && self.fdtable_get(fnr.fd2) != FDT_EXTERNAL)
                            || fnr.fd2 == self.coprocin
                            || fnr.fd2 == self.coprocout)
                    {
                        set_errno(libc::EBADF);
                        -1
                    } else {
                        let fd = if fnr.fd2 == -2 {
                            if fnr.typ == REDIR_MERGEOUT {
                                self.coprocout
                            } else {
                                self.coprocin
                            }
                        } else {
                            fnr.fd2
                        };
                        // SAFETY: dup has no memory preconditions.
                        let d = unsafe { libc::dup(fd) };
                        self.movefd(d)
                    };
                    if fil == -1 {
                        let e = errno();
                        self.closemnodes(mfds);
                        self.fixfds(save);
                        if e != 0 {
                            let what = if fnr.fd2 == -2 {
                                "coprocess".to_owned()
                            } else {
                                fnr.fd2.to_string()
                            };
                            self.zwarn(&format!("{what}: {}", errmsg(e)));
                        }
                        return false;
                    }
                    let r = i32::from(fnr.typ == REDIR_MERGEOUT);
                    self.addfd(forked, save, mfds, fnr.fd1, fil, r, fnr.varid.as_deref());
                }
                _ => {
                    let fil = if self.checkclobberparam(&fnr) {
                        self.redir_open_write(&fnr)
                    } else {
                        -1
                    };
                    let dfil = if fil != -1 && is_error_redir(fnr.typ) {
                        // SAFETY: dup has no memory preconditions.
                        let d = unsafe { libc::dup(fil) };
                        self.movefd(d)
                    } else {
                        0
                    };
                    if fil == -1 || dfil == -1 {
                        let e = errno();
                        if fil != -1 {
                            // SAFETY: closing the descriptor just opened.
                            unsafe {
                                libc::close(fil);
                            }
                        }
                        self.closemnodes(mfds);
                        self.fixfds(save);
                        if e != 0 && e != libc::EINTR {
                            self.zwarn(&format!("{}: {}", errmsg(e), lossy(&fnr.name)));
                        }
                        return false;
                    }
                    self.addfd(forked, save, mfds, fnr.fd1, fil, 1, fnr.varid.as_deref());
                    if is_error_redir(fnr.typ) {
                        self.addfd(forked, save, mfds, 2, dfil, 1, None);
                    }
                }
            }
            if self.errflag() {
                self.closemnodes(mfds);
                self.fixfds(save);
                return false;
            }
        }
        for i in 0..10 {
            if mfds
                .get(i)
                .and_then(Option::as_ref)
                .is_some_and(|m| m.ct >= 2)
            {
                self.closemn(mfds, i32::try_from(i).unwrap_or(0), REDIR_CLOSE);
            }
        }
        true
    }
}

/// Set `errno`.
pub(crate) fn set_errno(e: i32) {
    // SAFETY: __errno_location has no preconditions.
    let p = unsafe { libc::__errno_location() };
    // SAFETY: p points at this thread's errno.
    unsafe {
        *p = e;
    }
}
