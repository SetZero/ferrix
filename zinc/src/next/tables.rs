//! The shell's hash tables (zsh's `hashtable.c` and `hashnameddir.c`):
//! shell functions, external commands, reserved words, aliases and named
//! directories, with the way each prints its entries.

use std::rc::Rc;

use crate::ast::{List, Redir};
use crate::params::*;
use crate::shell::{Shell, Sticky};
use crate::utils::Qt;

pub(crate) const DISABLED: u32 = 1 << 0;
pub(crate) const HASHED: u32 = 1 << 1;
pub(crate) const ALIAS_GLOBAL: u32 = 1 << 1;
pub(crate) const ALIAS_SUFFIX: u32 = 1 << 2;
pub(crate) const ND_USERNAME: u32 = 1 << 1;
pub(crate) const ND_NOABBREV: u32 = 1 << 2;

pub(crate) const EF_RUN: u32 = 8;

/// A parsed program (zsh's `Eprog`), shared between the places that hold it.
#[derive(Debug, Clone)]
pub(crate) struct Eprog {
    pub(crate) list: Rc<List>,
    /// `EF_RUN`: an autoloaded file to run once and then call the function
    /// it defined.
    pub(crate) flags: u32,
}

impl Eprog {
    pub(crate) fn new(list: List) -> Eprog {
        Eprog {
            list: Rc::new(list),
            flags: 0,
        }
    }

    pub(crate) fn from_rc(list: Rc<List>) -> Eprog {
        Eprog { list, flags: 0 }
    }
}

/// A shell function (zsh's `struct shfunc`).
#[derive(Debug, Clone, Default)]
pub(crate) struct Shfunc {
    /// `PM_*` function flags and `DISABLED`.
    pub(crate) flags: u32,
    pub(crate) filename: Option<Vec<u8>>,
    pub(crate) lineno: i64,
    pub(crate) funcdef: Option<Eprog>,
    /// Redirections applied every time the function runs.
    pub(crate) redir: Option<Rc<Vec<Redir>>>,
    pub(crate) sticky: Option<Sticky>,
}

/// Where an external command was found (zsh's `struct cmdnam`).
#[derive(Debug, Clone)]
pub(crate) struct Cmdnam {
    pub(crate) flags: u32,
    /// Without `HASHED`: the index into `$path` of its directory.
    pub(crate) name: Option<usize>,
    /// With `HASHED`: the full path given to `hash`.
    pub(crate) cmd: Vec<u8>,
}

/// An alias (zsh's `struct alias`).
#[derive(Debug, Clone)]
pub(crate) struct Alias {
    pub(crate) flags: u32,
    pub(crate) text: Vec<u8>,
    pub(crate) inuse: i32,
}

/// A named directory (zsh's `struct nameddir`).
#[derive(Debug, Clone)]
pub(crate) struct Nameddir {
    pub(crate) flags: u32,
    pub(crate) dir: Vec<u8>,
    pub(crate) diff: i64,
}

/// A reserved word (zsh's `struct reswd`).
#[derive(Debug, Clone)]
pub(crate) struct Reswd {
    pub(crate) flags: u32,
}

/// zsh's reserved words, in `reswds[]` order.
pub(crate) const RESWDS: [&str; 31] = [
    "!",
    "[[",
    "{",
    "}",
    "case",
    "coproc",
    "declare",
    "do",
    "done",
    "elif",
    "else",
    "end",
    "esac",
    "export",
    "fi",
    "float",
    "for",
    "foreach",
    "function",
    "if",
    "integer",
    "local",
    "nocorrect",
    "readonly",
    "repeat",
    "select",
    "then",
    "time",
    "typeset",
    "until",
    "while",
];

impl Shell {
    /// zsh's `createreswdtable`.
    pub(crate) fn createreswdtable(&mut self) {
        for w in RESWDS {
            let _ = self
                .reswdtab
                .insert(w.as_bytes().to_vec(), Reswd { flags: 0 });
        }
    }

    /// zsh's `createaliastables`.
    pub(crate) fn createaliastables(&mut self) {
        let _ = self.aliastab.insert(
            b"run-help".to_vec(),
            Alias {
                flags: 0,
                text: b"man".to_vec(),
                inuse: 0,
            },
        );
        let _ = self.aliastab.insert(
            b"which-command".to_vec(),
            Alias {
                flags: 0,
                text: b"whence".to_vec(),
                inuse: 0,
            },
        );
    }

    /// `shfunctab->getnode`: an enabled function.
    pub(crate) fn getshfunc(&self, name: &[u8]) -> Option<Shfunc> {
        self.shfunctab
            .get(name)
            .filter(|f| f.flags & DISABLED == 0)
            .cloned()
    }

    /// zsh's `removeshfuncnode`.
    pub(crate) fn removeshfuncnode(&mut self, nam: &[u8]) -> Option<Shfunc> {
        if let Some(rest) = nam.strip_prefix(b"TRAP") {
            let signum = Shell::getsignum(rest);
            if signum != -1 {
                return self.removetrap(signum).map(|(_, f)| f);
            }
        }
        self.shfunctab.remove(nam)
    }

    /// zsh's `disableshfuncnode`.
    pub(crate) fn disableshfuncnode(&mut self, nam: &[u8]) {
        if let Some(f) = self.shfunctab.get_mut(nam) {
            f.flags |= DISABLED;
        }
        if let Some(rest) = nam.strip_prefix(b"TRAP") {
            let signum = Shell::getsignum(rest);
            if signum != -1 {
                if let Some(t) = usize::try_from(signum)
                    .ok()
                    .and_then(|s| self.sigtrapped.get_mut(s))
                {
                    *t &= !crate::signals::ZSIG_FUNC;
                }
                self.unsettrap(signum);
            }
        }
    }

    /// zsh's `enableshfuncnode`.
    pub(crate) fn enableshfuncnode(&mut self, nam: &[u8]) {
        if let Some(f) = self.shfunctab.get_mut(nam) {
            f.flags &= !DISABLED;
        }
        if let Some(rest) = nam.strip_prefix(b"TRAP") {
            let signum = Shell::getsignum(rest);
            if signum != -1 {
                let _ = self.settrap(signum, None, crate::signals::ZSIG_FUNC);
            }
        }
    }

    /// zsh's `emptycmdnamtable`.
    pub(crate) fn emptycmdnamtable(&mut self) {
        self.cmdnamtab.clear();
        self.pathchecked = 0;
    }

    /// `cmdnamtab_empty` as params.c calls it when `$path` changes.
    pub(crate) fn cmdnamtab_empty(&mut self) {
        self.emptycmdnamtable();
    }

    /// zsh's `hashdir`.
    pub(crate) fn hashdir(&mut self, dirp: usize) {
        let Some(dir) = self.arrvar(ArrVar::Path).get(dirp).cloned() else {
            return;
        };
        if crate::exec::isrelative(&dir) {
            return;
        }
        let unmetadir = crate::tok::unmetafy(&dir);
        let Ok(rd) = std::fs::read_dir(std::ffi::OsStr::from_bytes_compat(&unmetadir)) else {
            return;
        };
        let exec_only = self.isset(crate::options::HASHEXECUTABLESONLY);
        for ent in rd.flatten() {
            use std::os::unix::ffi::OsStrExt;
            let fn_raw = ent.file_name();
            let fnb = fn_raw.as_bytes();
            let fname = crate::tok::metafy(fnb);
            if self.cmdnamtab.contains(&fname) {
                continue;
            }
            let mut add = true;
            if exec_only {
                let mut full = unmetadir.clone();
                full.push(b'/');
                full.extend_from_slice(fnb);
                add = crate::exec::is_executable_file(&full);
            }
            if add {
                let _ = self.cmdnamtab.insert(
                    fname,
                    Cmdnam {
                        flags: 0,
                        name: Some(dirp),
                        cmd: Vec::new(),
                    },
                );
            }
        }
    }

    /// zsh's `fillcmdnamtable`.
    pub(crate) fn fillcmdnamtable(&mut self) {
        let n = self.arrvar(ArrVar::Path).len();
        for pq in self.pathchecked..n {
            self.hashdir(pq);
        }
        self.pathchecked = n;
    }

    /// zsh's `printcmdnamnode`.
    pub(crate) fn printcmdnamnode(&self, nam: &[u8], cn: &Cmdnam, printflags: i32) -> Vec<u8> {
        let mut out = Vec::new();
        let dir = |sh: &Shell| {
            cn.name
                .and_then(|i| sh.arrvar(ArrVar::Path).get(i).cloned())
                .unwrap_or_default()
        };
        if printflags & PRINT_WHENCE_WORD != 0 {
            out.extend(crate::tok::unmetafy(nam));
            out.extend_from_slice(if cn.flags & HASHED != 0 {
                b": hashed\n"
            } else {
                b": command\n"
            });
            return out;
        }
        if printflags & (PRINT_WHENCE_CSH | PRINT_WHENCE_SIMPLE) != 0 {
            if cn.flags & HASHED != 0 {
                out.extend(crate::tok::unmetafy(&cn.cmd));
            } else {
                out.extend(crate::tok::unmetafy(&dir(self)));
                out.push(b'/');
                out.extend(crate::tok::unmetafy(nam));
            }
            out.push(b'\n');
            return out;
        }
        if printflags & PRINT_WHENCE_VERBOSE != 0 {
            out.extend(self.nicezputs(nam));
            if cn.flags & HASHED != 0 {
                out.extend_from_slice(b" is hashed to ");
                out.extend(self.nicezputs(&cn.cmd));
            } else {
                out.extend_from_slice(b" is ");
                out.extend(self.nicezputs(&dir(self)));
                out.push(b'/');
                out.extend(self.nicezputs(nam));
            }
            out.push(b'\n');
            return out;
        }
        if printflags & PRINT_LIST != 0 {
            out.extend_from_slice(b"hash ");
            if nam.first() == Some(&b'-') {
                out.extend_from_slice(b"-- ");
            }
        }
        out.extend(self.quotedzputs_out(nam));
        out.push(b'=');
        if cn.flags & HASHED != 0 {
            out.extend(self.quotedzputs_out(&cn.cmd));
        } else {
            out.extend(self.quotedzputs_out(&dir(self)));
            out.push(b'/');
            out.extend(self.quotedzputs_out(nam));
        }
        out.push(b'\n');
        out
    }

    /// zsh's `printshfuncnode`.
    pub(crate) fn printshfuncnode(&mut self, nam: &[u8], f: &Shfunc, printflags: i32) -> Vec<u8> {
        let mut out = Vec::new();
        if printflags & PRINT_NAMEONLY != 0
            || (printflags & PRINT_WHENCE_SIMPLE != 0 && printflags & PRINT_WHENCE_FUNCDEF == 0)
        {
            out.extend(crate::tok::unmetafy(nam));
            out.push(b'\n');
            return out;
        }
        if printflags & (PRINT_WHENCE_VERBOSE | PRINT_WHENCE_WORD) != 0
            && printflags & PRINT_WHENCE_FUNCDEF == 0
        {
            out.extend(self.nicezputs(nam));
            out.extend_from_slice(if printflags & PRINT_WHENCE_WORD != 0 {
                b": function"
            } else if f.flags & PM_UNDEFINED != 0 {
                b" is an autoload shell function"
            } else {
                b" is a shell function"
            });
            if printflags & PRINT_WHENCE_VERBOSE != 0
                && let Some(file) = &f.filename
            {
                out.extend_from_slice(b" from ");
                out.extend(self.quotedzputs_out(file));
                if f.flags & PM_LOADDIR != 0 {
                    out.push(b'/');
                    out.extend(self.quotedzputs_out(nam));
                }
            }
            out.push(b'\n');
            return out;
        }
        out.extend(self.quotedzputs_out(nam));
        if f.funcdef.is_some() || f.flags & PM_UNDEFINED != 0 {
            out.extend_from_slice(b" () {\n");
            out.extend(self.zoutputtab());
            let mut t: Option<Vec<u8>> = None;
            if f.flags & PM_UNDEFINED != 0 {
                out.extend_from_slice(
                    format!("{} undefined\n", char::from(self.hashchar)).as_bytes(),
                );
                out.extend(self.zoutputtab());
            } else if let Some(fd) = &f.funcdef {
                t = Some(self.getpermtext(&fd.list, true));
            }
            if f.flags & (PM_TAGGED | PM_TAGGED_LOCAL) != 0 {
                out.extend_from_slice(format!("{} traced\n", char::from(self.hashchar)).as_bytes());
                out.extend(self.zoutputtab());
            }
            match t {
                None => {
                    out.extend_from_slice(b"builtin autoload -X");
                    for (c, fl) in [
                        (b'U', PM_UNALIASED),
                        (b't', PM_TAGGED),
                        (b'T', PM_TAGGED_LOCAL),
                        (b'k', PM_KSHSTORED),
                        (b'z', PM_ZSHSTORED),
                        (b'c', PM_CUR_FPATH),
                    ] {
                        if f.flags & fl != 0 {
                            out.push(c);
                        }
                    }
                    if f.flags & PM_LOADDIR != 0
                        && let Some(file) = &f.filename
                    {
                        out.push(b' ');
                        out.extend(crate::tok::unmetafy(file));
                    }
                }
                Some(t) => {
                    out.extend(crate::tok::unmetafy(&t));
                    if f.funcdef.as_ref().is_some_and(|d| d.flags & EF_RUN != 0) {
                        out.push(b'\n');
                        out.extend(self.zoutputtab());
                        out.extend(self.quotedzputs_out(nam));
                        out.extend_from_slice(b" \"$@\"");
                    }
                }
            }
            out.extend_from_slice(b"\n}");
        } else {
            out.extend_from_slice(b" () { }");
        }
        if let Some(r) = &f.redir {
            let t = self.getredirtext(r);
            out.extend(crate::tok::unmetafy(&t));
        }
        out.push(b'\n');
        out
    }

    /// zsh's `getshfuncfile`.
    pub(crate) fn getshfuncfile(nam: &[u8], shf: &Shfunc) -> Option<Vec<u8>> {
        if shf.flags & PM_LOADDIR != 0 {
            let mut f = shf.filename.clone().unwrap_or_default();
            f.push(b'/');
            f.extend_from_slice(nam);
            Some(f)
        } else {
            shf.filename.clone()
        }
    }

    /// zsh's `printreswdnode`.
    pub(crate) fn printreswdnode(nam: &[u8], printflags: i32) -> Vec<u8> {
        let n = crate::utils::lossy(nam);
        let s = if printflags & PRINT_WHENCE_WORD != 0 {
            format!("{n}: reserved\n")
        } else if printflags & PRINT_WHENCE_CSH != 0 {
            format!("{n}: shell reserved word\n")
        } else if printflags & PRINT_WHENCE_VERBOSE != 0 {
            format!("{n} is a reserved word\n")
        } else {
            format!("{n}\n")
        };
        s.into_bytes()
    }

    /// zsh's `printaliasnode`.
    pub(crate) fn printaliasnode(&self, nam: &[u8], a: &Alias, printflags: i32) -> Vec<u8> {
        let mut out = Vec::new();
        if printflags & PRINT_NAMEONLY != 0 {
            out.extend(crate::tok::unmetafy(nam));
            out.push(b'\n');
            return out;
        }
        if printflags & PRINT_WHENCE_WORD != 0 {
            out.extend(crate::tok::unmetafy(nam));
            out.extend_from_slice(if a.flags & ALIAS_SUFFIX != 0 {
                b": suffix alias\n"
            } else if a.flags & ALIAS_GLOBAL != 0 {
                b": global alias\n"
            } else {
                b": alias\n"
            });
            return out;
        }
        if printflags & PRINT_WHENCE_SIMPLE != 0 {
            out.extend(crate::tok::unmetafy(&a.text));
            out.push(b'\n');
            return out;
        }
        if printflags & PRINT_WHENCE_CSH != 0 {
            out.extend(self.nicezputs(nam));
            out.extend_from_slice(b": ");
            if a.flags & ALIAS_SUFFIX != 0 {
                out.extend_from_slice(b"suffix ");
            } else if a.flags & ALIAS_GLOBAL != 0 {
                out.extend_from_slice(b"globally ");
            }
            out.extend_from_slice(b"aliased to ");
            out.extend(self.nicezputs(&a.text));
            out.push(b'\n');
            return out;
        }
        if printflags & PRINT_WHENCE_VERBOSE != 0 {
            out.extend(self.nicezputs(nam));
            out.extend_from_slice(b" is a");
            if a.flags & ALIAS_SUFFIX != 0 {
                out.extend_from_slice(b" suffix");
            } else if a.flags & ALIAS_GLOBAL != 0 {
                out.extend_from_slice(b" global");
            } else {
                out.push(b'n');
            }
            out.extend_from_slice(b" alias for ");
            out.extend(self.nicezputs(&a.text));
            out.push(b'\n');
            return out;
        }
        if printflags & PRINT_LIST != 0 {
            if nam.contains(&b'=') {
                self.zwarn(&format!(
                    "invalid alias '{}' encountered while printing aliases",
                    crate::utils::lossy(nam)
                ));
                return out;
            }
            out.extend_from_slice(b"alias ");
            if a.flags & ALIAS_SUFFIX != 0 {
                out.extend_from_slice(b"-s ");
            } else if a.flags & ALIAS_GLOBAL != 0 {
                out.extend_from_slice(b"-g ");
            }
            if nam.first().is_some_and(|&c| c == b'-' || c == b'+') {
                out.extend_from_slice(b"-- ");
            }
        }
        out.extend(self.quotedzputs_out(nam));
        out.push(b'=');
        out.extend(self.quotedzputs_out(&a.text));
        out.push(b'\n');
        out
    }

    /// zsh's `addnameddirnode`.
    pub(crate) fn addnameddirnode(&mut self, nam: Vec<u8>, mut nd: Nameddir) {
        nd.diff = i64::try_from(nd.dir.len()).unwrap_or(0) - i64::try_from(nam.len()).unwrap_or(0);
        self.finddir_reset();
        let _ = self.nameddirtab.insert(nam, nd);
    }

    /// zsh's `removenameddirnode`.
    pub(crate) fn removenameddirnode(&mut self, nam: &[u8]) -> Option<Nameddir> {
        let r = self.nameddirtab.remove(nam);
        if r.is_some() {
            self.finddir_reset();
        }
        r
    }

    /// zsh's `emptynameddirtable`.
    pub(crate) fn emptynameddirtable(&mut self) {
        self.nameddirtab.clear();
        self.allusersadded = false;
        self.finddir_reset();
    }

    /// zsh's `fillnameddirtable`.
    pub(crate) fn fillnameddirtable(&mut self) {
        if !self.allusersadded {
            for (name, dir) in crate::utils::all_passwd_entries() {
                if self.errflag() {
                    break;
                }
                self.adduserdir(&name, Some(&dir), ND_USERNAME, true);
            }
            self.allusersadded = true;
        }
    }

    /// zsh's `printnameddirnode`.
    pub(crate) fn printnameddirnode(&self, nam: &[u8], nd: &Nameddir, printflags: i32) -> Vec<u8> {
        let mut out = Vec::new();
        if printflags & PRINT_NAMEONLY != 0 {
            out.extend(crate::tok::unmetafy(nam));
            out.push(b'\n');
            return out;
        }
        if printflags & PRINT_LIST != 0 {
            out.extend_from_slice(b"hash -d ");
            if nam.first() == Some(&b'-') {
                out.extend_from_slice(b"-- ");
            }
        }
        out.extend(self.quotedzputs_out(nam));
        out.push(b'=');
        out.extend(self.quotedzputs_out(&nd.dir));
        out.push(b'\n');
        out
    }

    /// `quotedzputs` to a buffer, unmetafied as stdout sees it.
    pub(crate) fn quotedzputs_out(&self, s: &[u8]) -> Vec<u8> {
        crate::tok::unmetafy(&self.quotedzputs(s))
    }

    /// `nicezputs` to a buffer.
    pub(crate) fn nicezputs(&self, s: &[u8]) -> Vec<u8> {
        crate::tok::unmetafy(&self.nicedup(s))
    }

    /// `quotestring` with backslashes, unmetafied.
    pub(crate) fn backslash_quoted(&self, s: &[u8]) -> Vec<u8> {
        crate::tok::unmetafy(&self.quotestring(s, Qt::Backslash))
    }
}

/// Bytes as an `OsStr`.
pub(crate) trait OsStrCompat {
    fn from_bytes_compat(b: &[u8]) -> &std::ffi::OsStr;
}

impl OsStrCompat for std::ffi::OsStr {
    fn from_bytes_compat(b: &[u8]) -> &std::ffi::OsStr {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(b)
    }
}
