//! Conditions (zsh's `cond.c`): `[[ ... ]]` and, through the same
//! evaluator, `test` and `[`.

use crate::ast::{Cond, CondOp};
use crate::math::MNumber;
use crate::options::*;
use crate::shell::{ERRFLAG_ERROR, Shell, write_fd};
use crate::subst::WordList;
use crate::tok;
use crate::utils::{has_token, lossy};

fn cstat(s: &[u8], follow: bool) -> Option<libc::stat> {
    let us = tok::unmetafy(s);
    // SAFETY: an all-zero stat is valid for fstat/stat to fill.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if !follow {
        let c = std::ffi::CString::new(us).ok()?;
        // SAFETY: c is NUL-terminated; st is a valid out-pointer.
        return (unsafe { libc::lstat(c.as_ptr(), &mut st) } >= 0).then_some(st);
    }
    if let Some(n) = us.strip_prefix(b"/dev/fd/") {
        let fd = i32::try_from(crate::utils::atoi(n)).ok()?;
        // SAFETY: st is a valid out-pointer.
        return (unsafe { libc::fstat(fd, &mut st) } == 0).then_some(st);
    }
    let c = std::ffi::CString::new(us).ok()?;
    // SAFETY: c is NUL-terminated; st is a valid out-pointer.
    (unsafe { libc::stat(c.as_ptr(), &mut st) } == 0).then_some(st)
}

fn dostat(s: &[u8]) -> u32 {
    cstat(s, true).map_or(0, |st| st.st_mode)
}

fn doaccess(s: &[u8], mode: i32) -> bool {
    std::ffi::CString::new(tok::unmetafy(s))
        // SAFETY: c is NUL-terminated.
        .is_ok_and(|c| unsafe { libc::access(c.as_ptr(), mode) } == 0)
}

fn is_fmt(mode: u32, fmt: u32) -> bool {
    mode & libc::S_IFMT == fmt
}

impl Shell {
    /// zsh's `execcond`.
    pub(crate) fn execcond(&mut self, c: &Cond) -> i32 {
        if self.isset(XTRACE) {
            self.printprompt4();
            write_fd(self.xtrerr_fd(), b"[[");
            self.tracingcond += 1;
        }
        let stat = self.evalcond(c, None);
        if stat == 2 {
            self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
        }
        if self.isset(XTRACE) {
            write_fd(self.xtrerr_fd(), b" ]]\n");
            self.tracingcond -= 1;
        }
        stat
    }

    /// zsh's `cond_subst`.
    fn cond_subst(&mut self, s: &[u8], glob_ok: bool) -> Vec<u8> {
        if glob_ok && self.checkglobqual(s, true).0 != 0 {
            let mut args = WordList::one(s.to_vec());
            let mut rf = 0;
            self.prefork(&mut args, 0, &mut rf);
            while !self.errflag() && args.words.first().is_some_and(|w| has_token(w)) {
                let _ = self.zglob(&mut args.words, 0, false);
            }
            return self.sepjoin(&args.words, None);
        }
        self.singsub(s)
    }

    fn trace(&self, bytes: &[u8]) {
        if self.tracingcond != 0 {
            write_fd(self.xtrerr_fd(), bytes);
        }
    }

    /// zsh's `evalcond`: 0 true, 1 false, 2 syntax error, 3 no such option.
    #[expect(clippy::too_many_lines, reason = "zsh's evalcond")]
    pub(crate) fn evalcond(&mut self, c: &Cond, fromtest: Option<&str>) -> i32 {
        let nam = fromtest.unwrap_or("");
        match c {
            Cond::Not(inner) => {
                self.trace(b" !");
                let r = self.evalcond(inner, fromtest);
                if r == 0 || r == 1 {
                    i32::from(r == 0)
                } else {
                    r
                }
            }
            Cond::And(a, b) => {
                let r = self.evalcond(a, fromtest);
                if r != 0 {
                    return r;
                }
                self.trace(b" &&");
                self.evalcond(b, fromtest)
            }
            Cond::Or(a, b) => {
                let r = self.evalcond(a, fromtest);
                if r != 1 && r != 3 {
                    return r;
                }
                self.trace(b" ||");
                self.evalcond(b, fromtest)
            }
            Cond::Module { name, args, infix } => {
                self.evalcond_module(name, args, *infix, fromtest)
            }
            Cond::Binary(CondOp::Regex, l, r) => {
                let name = if self.isset(REMATCHPCRE) {
                    b"-pcre-match".to_vec()
                } else {
                    b"-regex-match".to_vec()
                };
                self.evalcond_module(&name, &[l.clone(), r.clone()], true, fromtest)
            }
            Cond::Unary(op, word) => {
                let mut left = word.clone();
                if has_token(&left) {
                    left = self.cond_subst(&left, fromtest.is_none());
                    tok::untokenize(&mut left);
                }
                if self.tracingcond != 0 {
                    let mut t = format!(" -{} ", char::from(*op)).into_bytes();
                    t.extend(self.quotedzputs_out(&left));
                    write_fd(self.xtrerr_fd(), &t);
                }
                self.cond_unary(*op, &left, nam)
            }
            Cond::Binary(op, l, r) => {
                let mut left = l.clone();
                if has_token(&left) {
                    left = self.cond_subst(&left, fromtest.is_none());
                    tok::untokenize(&mut left);
                }
                let is_pat = matches!(op, CondOp::StrEq | CondOp::StrDeq | CondOp::StrNeq);
                let mut right = r.clone();
                if !is_pat && has_token(&right) {
                    right = self.cond_subst(&right, fromtest.is_none());
                    tok::untokenize(&mut right);
                }
                if self.tracingcond != 0 {
                    let mut t = b" ".to_vec();
                    t.extend(self.quotedzputs_out(&left));
                    t.push(b' ');
                    t.extend_from_slice(condstr(*op).as_bytes());
                    t.push(b' ');
                    if is_pat {
                        let rt = self.cond_subst(r, fromtest.is_none());
                        t.extend(self.quote_tokenized_output(&rt));
                    } else {
                        t.extend(self.quotedzputs_out(&right));
                    }
                    write_fd(self.xtrerr_fd(), &t);
                }
                match op {
                    CondOp::Eq | CondOp::Ne | CondOp::Lt | CondOp::Gt | CondOp::Le | CondOp::Ge => {
                        let (mut m1, mut m2) = if fromtest.is_some() {
                            let (a, ua) = crate::utils::zstrtol(&left, 10);
                            if ua != left.len() {
                                self.zwarnnam(
                                    nam,
                                    &format!("integer expression expected: {}", lossy(&left)),
                                );
                                return 2;
                            }
                            let (b, ub) = crate::utils::zstrtol(&right, 10);
                            if ub != right.len() {
                                self.zwarnnam(
                                    nam,
                                    &format!("integer expression expected: {}", lossy(&right)),
                                );
                                return 2;
                            }
                            (MNumber::Int(a), MNumber::Int(b))
                        } else {
                            (self.matheval(&left), self.matheval(&right))
                        };
                        if m1.is_float() != m2.is_float() {
                            m1 = MNumber::Float(m1.as_float());
                            m2 = MNumber::Float(m2.as_float());
                        }
                        let res = match (m1, m2) {
                            (MNumber::Float(a), MNumber::Float(b)) => match op {
                                CondOp::Eq => a == b,
                                CondOp::Ne => a != b,
                                CondOp::Lt => a < b,
                                CondOp::Gt => a > b,
                                CondOp::Le => a <= b,
                                _ => a >= b,
                            },
                            (a, b) => {
                                let (a, b) = (a.as_int(), b.as_int());
                                match op {
                                    CondOp::Eq => a == b,
                                    CondOp::Ne => a != b,
                                    CondOp::Lt => a < b,
                                    CondOp::Gt => a > b,
                                    CondOp::Le => a <= b,
                                    _ => a >= b,
                                }
                            }
                        };
                        i32::from(!res)
                    }
                    CondOp::StrEq | CondOp::StrDeq | CondOp::StrNeq => {
                        self.queue_signals();
                        let pat = self.singsub(r);
                        let prog = self.patcompile(&pat, crate::pattern::PAT_STATIC, None);
                        let Some(prog) = prog else {
                            let mut shown = pat.clone();
                            tok::untokenize(&mut shown);
                            self.zwarnnam(nam, &format!("bad pattern: {}", lossy(&shown)));
                            self.unqueue_signals();
                            return 2;
                        };
                        let test = self.pattry(&prog, &left);
                        self.unqueue_signals();
                        let t = if *op == CondOp::StrNeq { !test } else { test };
                        i32::from(!t)
                    }
                    CondOp::StrLt => i32::from(!(left < right)),
                    CondOp::StrGt => i32::from(!(left > right)),
                    CondOp::Nt | CondOp::Ot => {
                        let Some(a) = cstat(&left, true) else {
                            return 1;
                        };
                        let Some(b) = cstat(&right, true) else {
                            return 1;
                        };
                        if a.st_mtime == b.st_mtime {
                            let r = if *op == CondOp::Nt {
                                a.st_mtime_nsec > b.st_mtime_nsec
                            } else {
                                a.st_mtime_nsec < b.st_mtime_nsec
                            };
                            return i32::from(!r);
                        }
                        let r = if *op == CondOp::Nt {
                            a.st_mtime > b.st_mtime
                        } else {
                            a.st_mtime < b.st_mtime
                        };
                        i32::from(!r)
                    }
                    CondOp::Ef => {
                        let Some(a) = cstat(&left, true) else {
                            return 1;
                        };
                        let Some(b) = cstat(&right, true) else {
                            return 1;
                        };
                        i32::from(!(a.st_dev == b.st_dev && a.st_ino == b.st_ino))
                    }
                    CondOp::Regex => 2,
                }
            }
        }
    }

    /// The single-letter tests.
    fn cond_unary(&mut self, op: u8, left: &[u8], nam: &str) -> i32 {
        let r = match op {
            b'e' | b'a' => doaccess(left, libc::F_OK),
            b'b' => is_fmt(dostat(left), libc::S_IFBLK),
            b'c' => is_fmt(dostat(left), libc::S_IFCHR),
            b'd' => is_fmt(dostat(left), libc::S_IFDIR),
            b'f' => is_fmt(dostat(left), libc::S_IFREG),
            b'g' => dostat(left) & libc::S_ISGID != 0,
            b'k' => dostat(left) & libc::S_ISVTX != 0,
            b'n' => !left.is_empty(),
            b'o' => return self.optison(nam, left),
            b'p' => is_fmt(dostat(left), libc::S_IFIFO),
            b'r' => doaccess(left, libc::R_OK),
            b's' => cstat(left, true).is_some_and(|st| st.st_size != 0),
            b'S' => is_fmt(dostat(left), libc::S_IFSOCK),
            b'u' => dostat(left) & libc::S_ISUID != 0,
            b'v' => self.issetvar(left),
            b'w' => doaccess(left, libc::W_OK),
            b'x' => {
                // SAFETY: geteuid has no preconditions.
                if unsafe { libc::geteuid() } == 0 {
                    let mode = dostat(left);
                    mode & 0o111 != 0 || is_fmt(mode, libc::S_IFDIR)
                } else {
                    doaccess(left, libc::X_OK)
                }
            }
            b'z' => left.is_empty(),
            b'h' | b'L' => cstat(left, false).is_some_and(|st| is_fmt(st.st_mode, libc::S_IFLNK)),
            // SAFETY: geteuid and getegid have no preconditions.
            b'O' => cstat(left, true).is_some_and(|st| st.st_uid == unsafe { libc::geteuid() }),
            // SAFETY: getegid has no preconditions.
            b'G' => cstat(left, true).is_some_and(|st| st.st_gid == unsafe { libc::getegid() }),
            b'N' => match cstat(left, true) {
                None => false,
                Some(st) => {
                    if st.st_atime == st.st_mtime {
                        st.st_atime_nsec <= st.st_mtime_nsec
                    } else {
                        st.st_atime <= st.st_mtime
                    }
                }
            },
            b't' => {
                let fd = self.mathevali(left);
                // SAFETY: isatty has no preconditions.
                i32::try_from(fd).is_ok_and(|fd| unsafe { libc::isatty(fd) } != 0)
            }
            _ => {
                self.zwarnnam(nam, "bad cond code");
                return 2;
            }
        };
        i32::from(!r)
    }

    /// zsh's `optison`.
    fn optison(&mut self, name: &str, s: &[u8]) -> i32 {
        let i = if s.len() == 1 {
            optlookupc(self, s.first().copied().unwrap_or(0))
        } else {
            optlookup(self, s)
        };
        if i == 0 {
            if self.isset(POSIXBUILTINS) {
                return 1;
            }
            self.zwarnnam(name, &format!("no such option: {}", lossy(s)));
            return 3;
        }
        if i < 0 {
            i32::from(self.unset_opt(usize::try_from(-i).unwrap_or(0)))
        } else {
            i32::from(!self.isset(usize::try_from(i).unwrap_or(0)))
        }
    }

    /// A condition a module provides: none is loaded yet except what zsh
    /// builds in, so every name is unknown.
    fn evalcond_module(
        &mut self,
        name: &[u8],
        args: &[Vec<u8>],
        infix: bool,
        fromtest: Option<&str>,
    ) -> i32 {
        let nam = fromtest.unwrap_or("");
        let mut strs: Vec<Vec<u8>> = args.to_vec();
        let (cname, errname) = if infix {
            (name.to_vec(), name.to_vec())
        } else {
            (
                name.to_vec(),
                strs.first().cloned().unwrap_or_else(|| b"<null>".to_vec()),
            )
        };
        if let Some(r) = self.module_cond(&cname, &mut strs, infix) {
            return r;
        }
        let mut shown = if cname.first().is_some_and(|&c| c == b'-' || c == tok::DASH) {
            cname.clone()
        } else {
            errname
        };
        tok::untokenize(&mut shown);
        if name.ends_with(b"-regex-match") || name.ends_with(b"-pcre-match") {
            self.zerrnam(nam, &format!("{} not available for regex", lossy(name)));
            return 2;
        }
        self.zwarnnam(nam, &format!("unknown condition: {}", lossy(&shown)));
        2
    }
}

/// zsh's `condstr`.
fn condstr(op: CondOp) -> &'static str {
    match op {
        CondOp::StrEq => "=",
        CondOp::StrDeq => "==",
        CondOp::StrNeq => "!=",
        CondOp::StrLt => "<",
        CondOp::StrGt => ">",
        CondOp::Nt => "-nt",
        CondOp::Ot => "-ot",
        CondOp::Ef => "-ef",
        CondOp::Eq => "-eq",
        CondOp::Ne => "-ne",
        CondOp::Lt => "-lt",
        CondOp::Gt => "-gt",
        CondOp::Le => "-le",
        CondOp::Ge => "-ge",
        CondOp::Regex => "=~",
    }
}
