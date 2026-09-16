//! Smaller pieces of zsh's `params.c`, `utils.c` and `module.c`: `$RANDOM`
//! (glibc's `rand`, so seeded sequences match zsh on Linux), the locale
//! parameters, `$USERNAME`, `$histchars`, `$TERM`, `getquery`, math
//! function registration, and the hooks into parts not yet ported (the
//! line editor and module conditions) that do nothing until those exist.

use crate::math::MNumber;
use crate::options::*;
use crate::shell::{Shell, write_fd};
use crate::tok;

/// glibc's `random_r` state for `TYPE_3` (degree 31, separation 3), the
/// generator behind `rand`.
#[derive(Debug, Clone)]
pub(crate) struct GlibcRand {
    r: [i32; 34],
    fptr: usize,
    rptr: usize,
}

impl GlibcRand {
    /// `srand(seed)`.
    pub(crate) fn new(seed: u32) -> GlibcRand {
        let mut g = GlibcRand {
            r: [0; 34],
            fptr: 3,
            rptr: 0,
        };
        g.seed(seed);
        g
    }

    fn seed(&mut self, seed: u32) {
        let seed = if seed == 0 { 1 } else { seed };
        let mut word = i32::from_ne_bytes(seed.to_ne_bytes());
        let mut state = [0i32; 31];
        state[0] = word;
        for slot in state.iter_mut().skip(1) {
            let hi = word / 127_773;
            let lo = word % 127_773;
            word = 16807i32
                .wrapping_mul(lo)
                .wrapping_sub(2836i32.wrapping_mul(hi));
            if word < 0 {
                word = word.wrapping_add(2_147_483_647);
            }
            *slot = word;
        }
        self.r[..31].copy_from_slice(&state);
        self.fptr = 3;
        self.rptr = 0;
        for _ in 0..310 {
            let _ = self.next();
        }
    }

    /// `rand()`.
    pub(crate) fn next(&mut self) -> i32 {
        let f = self.r.get(self.fptr).copied().unwrap_or(0);
        let rv = self.r.get(self.rptr).copied().unwrap_or(0);
        let val = f.wrapping_add(rv);
        if let Some(slot) = self.r.get_mut(self.fptr) {
            *slot = val;
        }
        let result = (u32::from_ne_bytes(val.to_ne_bytes()) >> 1) as i32;
        self.fptr += 1;
        if self.fptr >= 31 {
            self.fptr = 0;
            self.rptr += 1;
        } else {
            self.rptr += 1;
            if self.rptr >= 31 {
                self.rptr = 0;
            }
        }
        result
    }
}

/// A math function (zsh's `struct mathfunc`).
#[derive(Debug, Clone)]
pub(crate) struct MathFunc {
    pub(crate) name: Vec<u8>,
    /// For a user function, the shell function to call; for a module's, the
    /// module.
    pub(crate) module: Option<Vec<u8>>,
    pub(crate) minargs: usize,
    pub(crate) maxargs: i32,
    /// `MFF_STR`: the argument is a string.
    pub(crate) string: bool,
    /// `MFF_USERFUNC`.
    pub(crate) user: bool,
}

/// The locale categories zsh sets from `LC_*` (its `lc_names`).
const LC_NAMES: [(&[u8], i32); 5] = [
    (b"LC_COLLATE", libc::LC_COLLATE),
    (b"LC_CTYPE", libc::LC_CTYPE),
    (b"LC_MESSAGES", libc::LC_MESSAGES),
    (b"LC_NUMERIC", libc::LC_NUMERIC),
    (b"LC_TIME", libc::LC_TIME),
];

fn setlocale(cat: i32, value: &[u8]) {
    if let Ok(c) = std::ffi::CString::new(tok::unmetafy(value)) {
        // SAFETY: c is NUL-terminated.
        unsafe {
            libc::setlocale(cat, c.as_ptr());
        }
    }
}

impl Shell {
    /// `rand()`.
    pub(crate) fn rand(&mut self) -> i32 {
        self.random.next()
    }

    /// `srand(seed)`.
    pub(crate) fn srand(&mut self, seed: u32) {
        self.random = GlibcRand::new(seed);
    }

    /// zsh's `setlang`.
    pub(crate) fn setlang(&mut self, x: Option<&[u8]>) {
        if self.getsparam_u(b"LC_ALL").is_some_and(|v| !v.is_empty()) {
            return;
        }
        setlocale(libc::LC_ALL, x.unwrap_or(b""));
        self.queue_signals();
        for (name, cat) in LC_NAMES {
            if let Some(v) = self.getsparam_u(name).filter(|v| !v.is_empty()) {
                setlocale(cat, &v);
            }
        }
        self.unqueue_signals();
    }

    /// The locale half of zsh's `lc_allsetfn`.
    pub(crate) fn lc_allset(&mut self, x: Option<&[u8]>) {
        match x.filter(|x| !x.is_empty()) {
            None => {
                if let Some(lang) = self.getsparam_u(b"LANG").filter(|v| !v.is_empty()) {
                    self.queue_signals();
                    self.setlang(Some(&lang));
                    self.unqueue_signals();
                }
            }
            Some(v) => setlocale(libc::LC_ALL, v),
        }
    }

    /// The locale half of zsh's `lcsetfn`.
    pub(crate) fn lcset(&mut self, name: &[u8], x: Option<&[u8]>) {
        if self.getsparam(b"LC_ALL").is_some_and(|v| !v.is_empty()) {
            return;
        }
        self.queue_signals();
        let value = match x.filter(|x| !x.is_empty()) {
            Some(v) => Some(v.to_vec()),
            None => self.getsparam(b"LANG"),
        };
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            for (n, cat) in LC_NAMES {
                if n == name {
                    setlocale(cat, &v);
                }
            }
        }
        self.unqueue_signals();
    }

    /// Reset the locale after a local `LANG` or `LC_*` goes out of scope, as
    /// `endparamscope` does with `setlang(getsparam("LANG"))`.
    pub(crate) fn lc_restore(&mut self) {
        let lang = self.getsparam_u(b"LANG");
        self.setlang(lang.as_deref());
    }

    /// zsh's `usernamesetfn`.
    pub(crate) fn usernamesetfn(&mut self, x: Option<Vec<u8>>) {
        let Some(x) = x else { return };
        let Some((uid, gid)) = crate::utils::passwd_ids(&tok::unmetafy(&x)) else {
            return;
        };
        if Some(uid) == self.cached_uid {
            return;
        }
        let Ok(cname) = std::ffi::CString::new(tok::unmetafy(&x)) else {
            return;
        };
        // SAFETY: cname is NUL-terminated.
        unsafe {
            libc::initgroups(cname.as_ptr(), gid);
        }
        // SAFETY: setgid has no memory preconditions.
        if unsafe { libc::setgid(gid) } != 0 {
            self.zwarn(&format!(
                "failed to change group ID: {}",
                self.errmsg_last()
            ));
            return;
        }
        // SAFETY: setuid has no memory preconditions.
        if unsafe { libc::setuid(uid) } != 0 {
            self.zwarn(&format!("failed to change user ID: {}", self.errmsg_last()));
            return;
        }
        self.cached_username = x;
        self.cached_uid = Some(uid);
    }

    /// zsh's `histcharssetfn`.
    pub(crate) fn histcharssetfn(&mut self, x: Option<Vec<u8>>) {
        match x {
            Some(x) => {
                let u = tok::unmetafy(&x);
                let chars = u.get(..u.len().min(3)).unwrap_or(&[]);
                if chars.iter().any(|&c| !c.is_ascii()) {
                    self.zwarn("HISTCHARS can only contain ASCII characters");
                    return;
                }
                self.bangchar = chars.first().copied().unwrap_or(0);
                self.hatchar = chars.get(1).copied().unwrap_or(0);
                self.hashchar = chars.get(2).copied().unwrap_or(0);
            }
            None => {
                self.bangchar = b'!';
                self.hashchar = b'#';
                self.hatchar = b'^';
            }
        }
        self.inittyptab();
    }

    /// zsh's `term_reinit_from_pm`: the terminal is set up on first use.
    pub(crate) fn term_reinit_from_pm(&mut self) {
        self.term_unknown = true;
    }

    /// zsh's `setlimits`: nothing to apply until `limit` sets one.
    pub(crate) fn setlimits(&mut self, _nam: Option<&str>) -> i32 {
        0
    }

    /// zsh's `getquery`: read a one-key answer from the terminal.
    pub(crate) fn getquery(&mut self, valid_chars: &[u8], purge: bool) -> i32 {
        let isem = self.term.as_slice() == b"emacs";
        let mp = self.mypgrp;
        self.attachtty(mp);
        let mut ti = self.gettyinfo_now();
        ti.tio.c_lflag &= !libc::ECHO;
        if !isem {
            ti.tio.c_lflag &= !libc::ICANON;
            ti.tio.c_cc[libc::VMIN] = 1;
            ti.tio.c_cc[libc::VTIME] = 0;
        }
        self.settyinfo(&ti);
        if purge {
            // SAFETY: tcflush has no memory preconditions.
            unsafe {
                libc::tcflush(self.shtty, libc::TCIFLUSH);
            }
        }
        let mut c: i32 = -1;
        let mut nl = false;
        loop {
            let mut b = [0u8; 1];
            // SAFETY: b is one writable byte.
            let n = unsafe { libc::read(self.shtty, b.as_mut_ptr().cast(), 1) };
            if n < 0 && crate::signals::errno() == libc::EINTR {
                continue;
            }
            if n <= 0 {
                break;
            }
            let [mut ch] = b;
            if ch == b'Y' {
                ch = b'y';
            } else if ch == b'N' {
                ch = b'n';
            }
            c = i32::from(ch);
            if valid_chars.is_empty() {
                break;
            }
            if ch == b'\n' {
                c = i32::from(valid_chars.first().copied().unwrap_or(b'n'));
                nl = true;
                break;
            }
            if valid_chars.contains(&ch) {
                nl = true;
                break;
            }
            self.zbeep();
        }
        if c >= 0 {
            let shown = [u8::try_from(c).unwrap_or(b'n')];
            write_fd(self.shtty, &shown);
        }
        if nl {
            write_fd(self.shtty, b"\n");
        }
        let sti = self.shttyinfo;
        if !isem {
            self.settyinfo(&sti);
        }
        c
    }

    /// zsh's `getmathfunc`.
    pub(crate) fn getmathfunc(&mut self, name: &[u8], _autol: bool) -> Option<MathFunc> {
        self.mathfuncs.iter().find(|f| f.name == name).cloned()
    }

    /// zsh's `addmathfunc`: new functions go first, as zsh's list does.
    pub(crate) fn addmathfunc(&mut self, f: MathFunc) {
        self.mathfuncs.retain(|g| g.name != f.name);
        self.mathfuncs.insert(0, f);
    }

    /// A module's string math function: none is loaded yet.
    pub(crate) fn call_string_mathfunc(
        &mut self,
        f: &MathFunc,
        _name: &[u8],
        _arg: &[u8],
    ) -> MNumber {
        self.zerr(&format!(
            "unknown function: {}",
            crate::utils::lossy(&f.name)
        ));
        MNumber::Int(0)
    }

    /// A module's numeric math function: none is loaded yet.
    pub(crate) fn call_numeric_mathfunc(
        &mut self,
        f: &MathFunc,
        _name: &[u8],
        _args: &[MNumber],
    ) -> MNumber {
        self.zerr(&format!(
            "unknown function: {}",
            crate::utils::lossy(&f.name)
        ));
        MNumber::Int(0)
    }

    /// A condition a loaded module provides: none is loaded yet.
    pub(crate) fn module_cond(
        &mut self,
        _name: &[u8],
        _args: &mut [Vec<u8>],
        _infix: bool,
    ) -> Option<i32> {
        None
    }

    /// `zleentry(ZLE_CMD_TRASH)`: the line editor is not active.
    pub(crate) fn zleentry_trash(&mut self) {}

    /// `zleentry(ZLE_CMD_REFRESH)`.
    pub(crate) fn zleentry_refresh(&mut self) {}

    /// `zleentry(ZLE_CMD_RESET_PROMPT)`.
    pub(crate) fn zleentry_reset_prompt(&mut self) {}

    /// `zleentry(ZLE_CMD_SET_KEYMAP)` for the `vi`/`emacs` options.
    pub(crate) fn zle_set_keymap_for_option(&mut self, _o: usize) {}

    /// Whether the option marks this shell as able to use the editor.
    pub(crate) fn uses_zle(&self) -> bool {
        self.isset(USEZLE)
    }
}
