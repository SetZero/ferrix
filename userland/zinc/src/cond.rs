//! Conditions: `[[ ... ]]` and the `test`/`[` builtin (zsh's `cond.c`).

use std::ffi::CString;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

use crate::ast::{Cond, CondOp};
use crate::expand::{expand_pattern, expand_single};
use crate::pattern::Pattern;
use crate::shell::Shell;
use crate::tok;

fn path_of(s: &[u8]) -> String {
    String::from_utf8_lossy(&tok::unmetafy(s)).into_owned()
}

fn access(s: &[u8], mode: i32) -> bool {
    CString::new(tok::unmetafy(s)).is_ok_and(|c| {
        // SAFETY: c is NUL-terminated.
        unsafe { libc::access(c.as_ptr(), mode) == 0 }
    })
}

/// A single-letter file or string test.
pub(crate) fn unary(sh: &Shell, op: u8, s: &[u8]) -> Result<bool, String> {
    let p = path_of(s);
    let meta = || std::fs::metadata(&p);
    Ok(match op {
        b'a' | b'e' => meta().is_ok() || std::fs::symlink_metadata(&p).is_ok(),
        b'f' => meta().is_ok_and(|m| m.is_file()),
        b'd' => meta().is_ok_and(|m| m.is_dir()),
        b'h' | b'L' => std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()),
        b'p' => meta().is_ok_and(|m| m.file_type().is_fifo()),
        b'S' => meta().is_ok_and(|m| m.file_type().is_socket()),
        b'b' => meta().is_ok_and(|m| m.file_type().is_block_device()),
        b'c' => meta().is_ok_and(|m| m.file_type().is_char_device()),
        b's' => meta().is_ok_and(|m| m.len() > 0),
        b'r' => access(s, libc::R_OK),
        b'w' => access(s, libc::W_OK),
        b'x' => access(s, libc::X_OK),
        b'u' => meta().is_ok_and(|m| m.permissions().mode() & 0o4000 != 0),
        b'g' => meta().is_ok_and(|m| m.permissions().mode() & 0o2000 != 0),
        b'k' => meta().is_ok_and(|m| m.permissions().mode() & 0o1000 != 0),
        // SAFETY: geteuid has no preconditions.
        b'O' => meta().is_ok_and(|m| m.uid() == unsafe { libc::geteuid() }),
        // SAFETY: getegid has no preconditions.
        b'G' => meta().is_ok_and(|m| m.gid() == unsafe { libc::getegid() }),
        b'N' => meta().is_ok_and(|m| m.mtime() >= m.atime()),
        b'n' => !s.is_empty(),
        b'z' => s.is_empty(),
        b't' => {
            let fd: i32 = std::str::from_utf8(s)
                .ok()
                .and_then(|t| t.parse().ok())
                .unwrap_or(-1);
            // SAFETY: isatty has no memory-safety preconditions.
            unsafe { libc::isatty(fd) == 1 }
        }
        b'o' => sh.opt(&crate::shell::option_key(s)),
        b'v' => sh.is_set(s),
        _ => return Err(format!("unknown condition: -{}", char::from(op))),
    })
}

fn mtime(s: &[u8]) -> Option<(i64, i64)> {
    std::fs::metadata(path_of(s))
        .ok()
        .map(|m| (m.mtime(), m.mtime_nsec()))
}

fn numeric(sh: &mut Shell, op: CondOp, a: &[u8], b: &[u8], arith: bool) -> Result<bool, String> {
    let parse = |sh: &mut Shell, x: &[u8]| -> Result<i64, String> {
        if arith {
            crate::arith::eval(sh, x)
        } else {
            let t = String::from_utf8_lossy(x);
            t.trim()
                .parse::<i64>()
                .map_err(|_| format!("integer expression expected: {t}"))
        }
    };
    let (x, y) = (parse(sh, a)?, parse(sh, b)?);
    Ok(match op {
        CondOp::Eq => x == y,
        CondOp::Ne => x != y,
        CondOp::Lt => x < y,
        CondOp::Gt => x > y,
        CondOp::Le => x <= y,
        _ => x >= y,
    })
}

fn files(op: CondOp, a: &[u8], b: &[u8]) -> bool {
    match op {
        CondOp::Nt => match (mtime(a), mtime(b)) {
            (Some(x), Some(y)) => x > y,
            (Some(_), None) => true,
            _ => false,
        },
        CondOp::Ot => match (mtime(a), mtime(b)) {
            (Some(x), Some(y)) => x < y,
            (None, Some(_)) => true,
            _ => false,
        },
        _ => {
            let (ma, mb) = (std::fs::metadata(path_of(a)), std::fs::metadata(path_of(b)));
            matches!((ma, mb), (Ok(x), Ok(y)) if x.dev() == y.dev() && x.ino() == y.ino())
        }
    }
}

/// Evaluate `[[ ]]`.
pub(crate) fn eval_cond(sh: &mut Shell, c: &Cond) -> Result<bool, String> {
    Ok(match c {
        Cond::Not(x) => !eval_cond(sh, x)?,
        Cond::And(x, y) => eval_cond(sh, x)? && eval_cond(sh, y)?,
        Cond::Or(x, y) => eval_cond(sh, x)? || eval_cond(sh, y)?,
        Cond::Unary(op, w) => {
            let s = expand_single(sh, w)?;
            unary(sh, *op, &s)?
        }
        Cond::Binary(op, a, b) => {
            let left = expand_single(sh, a)?;
            match op {
                CondOp::StrEq | CondOp::StrDeq | CondOp::StrNeq => {
                    let pat = expand_pattern(sh, b)?;
                    let prog = Pattern::compile(&pat, sh.opt("extendedglob"));
                    let hit = prog.matches(&tok::unmetafy(&left));
                    // A pattern carrying `(#b)` leaves `$match` behind, which
                    // is how vcs_info's backends read a ref apart.
                    if hit && prog.has_backrefs() {
                        crate::param::set_backrefs(sh, &prog, &left);
                    }
                    hit != (*op == CondOp::StrNeq)
                }
                CondOp::StrLt | CondOp::StrGt => {
                    let right = expand_single(sh, b)?;
                    if *op == CondOp::StrLt {
                        left < right
                    } else {
                        left > right
                    }
                }
                CondOp::Regex => {
                    let right = expand_single(sh, b)?;
                    crate::regex::is_match(&tok::unmetafy(&right), &tok::unmetafy(&left))?
                }
                CondOp::Nt | CondOp::Ot | CondOp::Ef => {
                    let right = expand_single(sh, b)?;
                    files(*op, &left, &right)
                }
                _ => {
                    let right = expand_single(sh, b)?;
                    numeric(sh, *op, &left, &right, true)?
                }
            }
        }
        Cond::Module { name, .. } => {
            let mut n = name.clone();
            tok::untokenize(&mut n);
            return Err(format!(
                "unknown condition: {}",
                String::from_utf8_lossy(&n)
            ));
        }
    })
}

/// The `test` and `[` builtins, on expanded arguments.
pub(crate) fn test(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    let mut t = Test { sh, args, i: 0 };
    let r = match args.len() {
        0 => Ok(false),
        _ => t.or(),
    };
    match r {
        Ok(_) if t.i < args.len() => {
            t.sh.error("test: too many arguments");
            2
        }
        Ok(b) => i32::from(!b),
        Err(e) => {
            t.sh.error(&format!("test: {e}"));
            2
        }
    }
}

struct Test<'a> {
    sh: &'a mut Shell,
    args: &'a [Vec<u8>],
    i: usize,
}

impl Test<'_> {
    fn peek(&self, k: usize) -> Option<&[u8]> {
        self.args.get(self.i + k).map(Vec::as_slice)
    }

    fn or(&mut self) -> Result<bool, String> {
        let mut v = self.and()?;
        while self.peek(0) == Some(b"-o") {
            self.i += 1;
            let r = self.and()?;
            v = v || r;
        }
        Ok(v)
    }

    fn and(&mut self) -> Result<bool, String> {
        let mut v = self.not()?;
        while self.peek(0) == Some(b"-a") {
            self.i += 1;
            let r = self.not()?;
            v = v && r;
        }
        Ok(v)
    }

    fn not(&mut self) -> Result<bool, String> {
        if self.peek(0) == Some(b"!") && self.peek(1).is_some() {
            self.i += 1;
            return Ok(!self.not()?);
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<bool, String> {
        let Some(a) = self.peek(0).map(<[u8]>::to_vec) else {
            return Err("argument expected".to_owned());
        };
        if a == b"(" && self.args.len() - self.i >= 3 {
            self.i += 1;
            let v = self.or()?;
            if self.peek(0) != Some(b")") {
                return Err("')' expected".to_owned());
            }
            self.i += 1;
            return Ok(v);
        }
        if let Some(op) = self.peek(1).map(<[u8]>::to_vec)
            && let Some(b) = self.peek(2).map(<[u8]>::to_vec)
        {
            let cmp = match op.as_slice() {
                b"=" | b"==" => Some(a == b),
                b"!=" => Some(a != b),
                b"<" => Some(a < b),
                b">" => Some(a > b),
                _ => None,
            };
            if let Some(r) = cmp {
                self.i += 3;
                return Ok(r);
            }
            let nop = match op.as_slice() {
                b"-eq" => Some(CondOp::Eq),
                b"-ne" => Some(CondOp::Ne),
                b"-lt" => Some(CondOp::Lt),
                b"-gt" => Some(CondOp::Gt),
                b"-le" => Some(CondOp::Le),
                b"-ge" => Some(CondOp::Ge),
                b"-nt" => Some(CondOp::Nt),
                b"-ot" => Some(CondOp::Ot),
                b"-ef" => Some(CondOp::Ef),
                _ => None,
            };
            if let Some(nop) = nop {
                self.i += 3;
                return if matches!(nop, CondOp::Nt | CondOp::Ot | CondOp::Ef) {
                    Ok(files(nop, &a, &b))
                } else {
                    numeric(self.sh, nop, &a, &b, false)
                };
            }
        }
        if let [b'-', letter] = a.as_slice()
            && let Some(arg) = self.peek(1).map(<[u8]>::to_vec)
            && !(self.args.len() - self.i == 2 && false)
        {
            self.i += 2;
            return unary(self.sh, *letter, &arg);
        }
        self.i += 1;
        Ok(!a.is_empty())
    }
}
