//! Named directories, symlink resolution and hook functions (zsh's
//! `utils.c`): `~name`, what `%~` abbreviates to, `precmd`-style hooks and
//! the `zsh_directory_name` protocol.

use crate::options::*;
use crate::params::*;
use crate::shell::{Shell, write_fd};
use crate::tables::{ND_NOABBREV, ND_USERNAME, Nameddir};
use crate::tok;

/// A named directory found for a path (zsh's `Nameddir` result of `finddir`).
#[derive(Debug, Clone)]
pub(crate) struct Found {
    pub(crate) name: Vec<u8>,
    pub(crate) dir: Vec<u8>,
}

/// zsh's `dircmp`: `s` is `t` or a directory prefix of it.
fn dircmp(s: &[u8], t: &[u8]) -> bool {
    t.starts_with(s) && (t.len() == s.len() || t.get(s.len()) == Some(&b'/'))
}

impl Shell {
    /// zsh's `xsymlink`: `s` with symlinks and `.`/`..` resolved.
    pub(crate) fn xsymlink(&mut self, s: &[u8]) -> Option<Vec<u8>> {
        if s.first() != Some(&b'/') {
            return None;
        }
        let mut p = s.to_vec();
        if !self.chrealpath(&mut p, b'P') {
            self.zwarn("path expansion failed, using root directory");
            return Some(b"/".to_vec());
        }
        Some(p)
    }

    /// zsh's `get_username`.
    pub(crate) fn get_username(&mut self) -> Vec<u8> {
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        if self.cached_uid != Some(uid) {
            self.cached_uid = Some(uid);
            self.cached_username = crate::utils::user_name_of(uid).unwrap_or_default();
        }
        self.cached_username.clone()
    }

    /// zsh's `finddir(NULL)`: nothing is cached, so nothing to clear.
    pub(crate) fn finddir_reset(&mut self) {}

    /// zsh's `finddir`: the named directory that best abbreviates `s`.
    pub(crate) fn finddir(&mut self, s: &[u8]) -> Option<Found> {
        let mut best: i64 = 0;
        let mut last: Option<Found> = None;
        let home = self.home.clone();
        if !home.is_empty() {
            let diff = if home.len() == 1 {
                0
            } else {
                i64::try_from(home.len()).unwrap_or(0)
            };
            if diff > best && dircmp(&home, s) {
                best = diff;
                last = Some(Found {
                    name: Vec::new(),
                    dir: home,
                });
            }
        }
        for (name, nd) in self.nameddirtab.iter() {
            if nd.diff > best && dircmp(&nd.dir, s) && nd.flags & ND_NOABBREV == 0 {
                best = nd.diff;
                last = Some(Found {
                    name: name.clone(),
                    dir: nd.dir.clone(),
                });
            }
        }
        if let Some(ares) = self.subst_string_by_hook(b"zsh_directory_name", Some(b"d"), s)
            && ares.len() >= 2
        {
            let (len, _) = crate::utils::zstrtol(ares.get(1).map_or(&[][..], Vec::as_slice), 10);
            if len > best {
                let mut name = b"[".to_vec();
                name.extend_from_slice(ares.first().map_or(&[][..], Vec::as_slice));
                name.push(b']');
                let l = usize::try_from(len).unwrap_or(0).min(s.len());
                last = Some(Found {
                    name,
                    dir: s.get(..l).unwrap_or(&[]).to_vec(),
                });
            }
        }
        last
    }

    /// zsh's `fprintdir`, into a buffer.
    pub(crate) fn fprintdir(&mut self, s: &[u8]) -> Vec<u8> {
        match self.finddir(s) {
            None => tok::unmetafy(s),
            Some(d) => {
                let mut out = b"~".to_vec();
                out.extend(tok::unmetafy(&d.name));
                out.extend(tok::unmetafy(s.get(d.dir.len()..).unwrap_or(&[])));
                out
            }
        }
    }

    /// zsh's `substnamedir`.
    pub(crate) fn substnamedir(&mut self, s: &[u8]) -> Vec<u8> {
        match self.finddir(s) {
            None => self.quotestring(s, crate::utils::Qt::Backslash),
            Some(d) => {
                let mut out = b"~".to_vec();
                out.extend_from_slice(&d.name);
                out.extend(self.quotestring(
                    s.get(d.dir.len()..).unwrap_or(&[]),
                    crate::utils::Qt::Backslash,
                ));
                out
            }
        }
    }

    /// zsh's `adduserdir`.
    pub(crate) fn adduserdir(&mut self, s: &[u8], t: Option<&[u8]>, flags: u32, always: bool) {
        if !self.interact() {
            return;
        }
        if flags & ND_USERNAME != 0 && self.nameddirtab.contains(s) {
            return;
        }
        if !always && self.unset_opt(AUTONAMEDIRS) && !self.nameddirtab.contains(s) {
            return;
        }
        let Some(t) = t.filter(|t| t.first() == Some(&b'/') && t.len() < libc::PATH_MAX as usize)
        else {
            let _ = self.removenameddirnode(s);
            return;
        };
        let mut end = t.len();
        while end > 0 && t.get(end - 1) == Some(&b'/') {
            end -= 1;
        }
        let dir = if end == 0 {
            t.to_vec()
        } else {
            t.get(..end).unwrap_or(t).to_vec()
        };
        let mut nflags = flags;
        if s == b"PWD" || s == b"OLDPWD" {
            nflags |= ND_NOABBREV;
        }
        self.addnameddirnode(
            s.to_vec(),
            Nameddir {
                flags: nflags,
                dir,
                diff: 0,
            },
        );
    }

    /// zsh's `getnameddir`.
    pub(crate) fn getnameddir(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        if let Some(nd) = self.nameddirtab.get(name) {
            return Some(nd.dir.clone());
        }
        let scalar = self
            .paramtab()
            .get(name)
            .is_some_and(|pm| pm_type(pm.flags) == PM_SCALAR);
        if scalar
            && let Some(s) = self.getsparam(name)
            && s.first() == Some(&b'/')
        {
            if let Some(pm) = self.paramtab_mut().get_mut(name) {
                pm.flags |= PM_NAMEDDIR;
            }
            self.adduserdir(name, Some(&s), 0, true);
            return Some(s);
        }
        let home = crate::utils::user_home_of(&tok::unmetafy(name))?;
        let home = tok::metafy(&home);
        let dir = if self.isset(CHASELINKS) {
            self.xsymlink(&home)
        } else {
            Some(home.clone())
        };
        match dir {
            Some(d) => {
                self.adduserdir(name, Some(&d), ND_USERNAME, true);
                Some(d)
            }
            None => Some(home),
        }
    }

    /// `$OLDPWD`.
    pub(crate) fn oldpwd(&mut self) -> Option<Vec<u8>> {
        self.getsparam(b"OLDPWD")
    }

    /// zsh's `callhookfunc`: false when some function ran.
    pub(crate) fn callhookfunc(
        &mut self,
        name: &[u8],
        lnklst: Option<Vec<Vec<u8>>>,
        arrayp: bool,
    ) -> bool {
        let osc = self.sfcontext;
        let osm = self.stopmsg;
        let mut stat = true;
        self.sfcontext = crate::exec::SFC_HOOK;
        let mut args = lnklst;
        if let Some(shf) = self.getshfunc(name) {
            let a = args.get_or_insert_with(|| vec![name.to_vec()]).clone();
            let _ = self.doshfunc(&shf, Some(a), true);
            stat = false;
        }
        if arrayp {
            let mut arrnam = name.to_vec();
            arrnam.extend_from_slice(b"_functions");
            if let Some(arr) = self.getaparam(&arrnam) {
                for fname in arr {
                    if let Some(shf) = self.getshfunc(&fname) {
                        let mut a = vec![fname.clone()];
                        if let Some(orig) = &args {
                            a.extend(orig.iter().skip(1).cloned());
                        }
                        let _ = self.doshfunc(&shf, Some(a), true);
                        stat = false;
                    }
                }
            }
        }
        self.sfcontext = osc;
        self.stopmsg = osm;
        stat
    }

    /// zsh's `subst_string_by_func`.
    fn subst_string_by_func(
        &mut self,
        name: &[u8],
        arg1: Option<&[u8]>,
        orig: &[u8],
    ) -> Option<Vec<Vec<u8>>> {
        let shf = self.getshfunc(name)?;
        let osc = self.sfcontext;
        let osm = self.stopmsg;
        let mut l = vec![name.to_vec()];
        if let Some(a) = arg1 {
            l.push(a.to_vec());
        }
        l.push(orig.to_vec());
        self.sfcontext = crate::exec::SFC_SUBST;
        let ret = if self.doshfunc(&shf, Some(l), true) != 0 {
            None
        } else {
            self.getaparam(b"reply")
        };
        self.sfcontext = osc;
        self.stopmsg = osm;
        ret
    }

    /// zsh's `subst_string_by_hook`.
    pub(crate) fn subst_string_by_hook(
        &mut self,
        name: &[u8],
        arg1: Option<&[u8]>,
        orig: &[u8],
    ) -> Option<Vec<Vec<u8>>> {
        let mut ret = None;
        if self.getshfunc(name).is_some() {
            ret = self.subst_string_by_func(name, arg1, orig);
        }
        if ret.is_none() {
            let mut arrnam = name.to_vec();
            arrnam.extend_from_slice(b"_functions");
            if let Some(arr) = self.getaparam(&arrnam) {
                for f in arr {
                    if self.getshfunc(&f).is_some() {
                        ret = self.subst_string_by_func(&f, arg1, orig);
                        if ret.is_some() {
                            break;
                        }
                    }
                }
            }
        }
        ret
    }

    /// zsh's `zputenv`: set `name=value` in the environment children get.
    pub(crate) fn zputenv(&mut self, s: &[u8]) {
        let Some(eq) = s.iter().position(|&c| c == b'=') else {
            return;
        };
        if s.get(..eq).is_some_and(|n| n.iter().any(|&c| c >= 128)) {
            return;
        }
        let key = s.get(..=eq).unwrap_or(&[]);
        match self.environ.iter().position(|e| e.starts_with(key)) {
            Some(i) => {
                if let Some(slot) = self.environ.get_mut(i) {
                    *slot = s.to_vec();
                }
            }
            None => self.environ.push(s.to_vec()),
        }
    }

    /// zsh's `zbeep`.
    pub(crate) fn zbeep(&mut self) {
        self.queue_signals();
        if let Some(vb) = self.getsparam_u(b"ZBEEP") {
            let (k, _) = self.getkeystring(&vb, crate::utils::GETKEYS_BINDKEY);
            write_fd(self.shtty, &k);
        } else if self.isset(BEEP) {
            write_fd(self.shtty, b"\x07");
        }
        self.unqueue_signals();
    }

    /// zsh's `ttyidlegetfn`.
    pub(crate) fn ttyidle(&self) -> i64 {
        if self.shtty == -1 {
            return -1;
        }
        // SAFETY: an all-zero stat is valid for fstat to fill.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: st is a valid out-pointer.
        if unsafe { libc::fstat(self.shtty, &mut st) } != 0 {
            return -1;
        }
        now_tv().0 - st.st_atime
    }

    /// zsh's `zoutputtab`.
    pub(crate) fn zoutputtab(&self) -> Vec<u8> {
        match self.text_expand_tabs {
            n if n < 0 => Vec::new(),
            0 => b"\t".to_vec(),
            n => vec![b' '; usize::try_from(n).unwrap_or(0)],
        }
    }

    /// zsh's `inerrflush`: discard typed-ahead input after an interrupt.
    pub(crate) fn inerrflush(&mut self) {
        self.pending_input.clear();
    }
}
