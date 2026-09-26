//! zsh's parameters (`params.c`): the parameter table, local scopes, the
//! special parameters, the environment, and conversion of numbers.
//!
//! A parameter's get/set/unset methods are zsh's `gsu` structures, here the
//! [`Gsu`] enum; specials whose value lives in a C global keep it in a field
//! of the shell ([`IntVar`], [`StrVar`], [`ArrVar`]). A parameter hidden by a
//! `local` stays reachable through `old`, as in zsh.

use crate::hashtable::HashTable;
use crate::math::MNumber;
use crate::options::*;
use crate::shell::Shell;
use crate::tok::{self, META};
use crate::utils::{at, lossy};

pub(crate) const PM_SCALAR: u32 = 0;
pub(crate) const PM_ARRAY: u32 = 1 << 0;
pub(crate) const PM_INTEGER: u32 = 1 << 1;
pub(crate) const PM_EFLOAT: u32 = 1 << 2;
pub(crate) const PM_FFLOAT: u32 = 1 << 3;
pub(crate) const PM_HASHED: u32 = 1 << 4;
pub(crate) const PM_LEFT: u32 = 1 << 5;
pub(crate) const PM_RIGHT_B: u32 = 1 << 6;
pub(crate) const PM_RIGHT_Z: u32 = 1 << 7;
pub(crate) const PM_LOWER: u32 = 1 << 8;
pub(crate) const PM_UPPER: u32 = 1 << 9;
pub(crate) const PM_UNDEFINED: u32 = 1 << 9;
pub(crate) const PM_READONLY: u32 = 1 << 10;
pub(crate) const PM_TAGGED: u32 = 1 << 11;
pub(crate) const PM_EXPORTED: u32 = 1 << 12;
pub(crate) const PM_ABSPATH_USED: u32 = 1 << 12;
pub(crate) const PM_UNIQUE: u32 = 1 << 13;
pub(crate) const PM_UNALIASED: u32 = 1 << 13;
pub(crate) const PM_HIDE: u32 = 1 << 14;
pub(crate) const PM_CUR_FPATH: u32 = 1 << 14;
pub(crate) const PM_HIDEVAL: u32 = 1 << 15;
pub(crate) const PM_WARNNESTED: u32 = 1 << 15;
pub(crate) const PM_TIED: u32 = 1 << 16;
pub(crate) const PM_TAGGED_LOCAL: u32 = 1 << 16;
pub(crate) const PM_DONTIMPORT_SUID: u32 = 1 << 17;
pub(crate) const PM_LOADDIR: u32 = 1 << 17;
pub(crate) const PM_SINGLE: u32 = 1 << 18;
pub(crate) const PM_ANONYMOUS: u32 = 1 << 18;
pub(crate) const PM_LOCAL: u32 = 1 << 19;
pub(crate) const PM_KSHSTORED: u32 = 1 << 19;
pub(crate) const PM_SPECIAL: u32 = 1 << 20;
pub(crate) const PM_ZSHSTORED: u32 = 1 << 20;
pub(crate) const PM_RO_BY_DESIGN: u32 = 1 << 21;
pub(crate) const PM_READONLY_SPECIAL: u32 = PM_SPECIAL | PM_READONLY | PM_RO_BY_DESIGN;
pub(crate) const PM_DONTIMPORT: u32 = 1 << 22;
pub(crate) const PM_DECLARED: u32 = 1 << 22;
pub(crate) const PM_RESTRICTED: u32 = 1 << 23;
pub(crate) const PM_UNSET: u32 = 1 << 24;
pub(crate) const PM_DEFAULTED: u32 = PM_DECLARED | PM_UNSET;
pub(crate) const PM_REMOVABLE: u32 = 1 << 25;
pub(crate) const PM_AUTOLOAD: u32 = 1 << 26;
pub(crate) const PM_NORESTORE: u32 = 1 << 27;
pub(crate) const PM_AUTOALL: u32 = 1 << 27;
pub(crate) const PM_HASHELEM: u32 = 1 << 28;
pub(crate) const PM_NAMEDDIR: u32 = 1 << 29;

/// `PM_TYPE(X)`.
pub(crate) const fn pm_type(f: u32) -> u32 {
    f & (PM_SCALAR | PM_INTEGER | PM_EFLOAT | PM_FFLOAT | PM_ARRAY | PM_HASHED)
}

pub(crate) const SCANPM_WANTVALS: i32 = 1 << 0;
pub(crate) const SCANPM_WANTKEYS: i32 = 1 << 1;
pub(crate) const SCANPM_WANTINDEX: i32 = 1 << 2;
pub(crate) const SCANPM_MATCHKEY: i32 = 1 << 3;
pub(crate) const SCANPM_MATCHVAL: i32 = 1 << 4;
pub(crate) const SCANPM_MATCHMANY: i32 = 1 << 5;
pub(crate) const SCANPM_ASSIGNING: i32 = 1 << 6;
pub(crate) const SCANPM_KEYMATCH: i32 = 1 << 7;
pub(crate) const SCANPM_DQUOTED: i32 = 1 << 8;
pub(crate) const SCANPM_ARRONLY: i32 = 1 << 9;
pub(crate) const SCANPM_CHECKING: i32 = 1 << 10;
pub(crate) const SCANPM_ISVAR_AT: i32 = -1 << 15;

pub(crate) const VALFLAG_INV: i32 = 0x0001;
pub(crate) const VALFLAG_EMPTY: i32 = 0x0002;
pub(crate) const VALFLAG_SUBST: i32 = 0x0004;

pub(crate) const ASSPM_AUGMENT: i32 = 1 << 0;
pub(crate) const ASSPM_WARN_CREATE: i32 = 1 << 1;
pub(crate) const ASSPM_WARN_NESTED: i32 = 1 << 2;
pub(crate) const ASSPM_WARN: i32 = ASSPM_WARN_CREATE | ASSPM_WARN_NESTED;
pub(crate) const ASSPM_ENV_IMPORT: i32 = 1 << 3;
pub(crate) const ASSPM_KEY_VALUE: i32 = 1 << 4;

pub(crate) const PRINT_NAMEONLY: i32 = 1 << 0;
pub(crate) const PRINT_TYPE: i32 = 1 << 1;
pub(crate) const PRINT_LIST: i32 = 1 << 2;
pub(crate) const PRINT_KV_PAIR: i32 = 1 << 3;
pub(crate) const PRINT_INCLUDEVALUE: i32 = 1 << 4;
pub(crate) const PRINT_TYPESET: i32 = 1 << 5;
pub(crate) const PRINT_LINE: i32 = 1 << 6;
pub(crate) const PRINT_POSIX_EXPORT: i32 = 1 << 7;
pub(crate) const PRINT_POSIX_READONLY: i32 = 1 << 8;
pub(crate) const PRINT_WHENCE_CSH: i32 = 1 << 7;
pub(crate) const PRINT_WHENCE_VERBOSE: i32 = 1 << 8;
pub(crate) const PRINT_WHENCE_SIMPLE: i32 = 1 << 9;
pub(crate) const PRINT_WHENCE_FUNCDEF: i32 = 1 << 10;
pub(crate) const PRINT_WHENCE_WORD: i32 = 1 << 11;

pub(crate) const MAX_PIPESTATS: usize = 256;

/// Integer globals behind special parameters (zsh's `&lastval` and friends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntVar {
    Lastpid,
    Mypid,
    Lastval,
    Curhist,
    Lineno,
    Ppid,
    ZshSubshell,
    Columns,
    Lines,
    RpromptIndent,
    Shlvl,
    Funcnest,
    Optind,
    TryErrflag,
    TryInterrupt,
}
pub(crate) const N_INTVARS: usize = 15;

/// Scalar globals behind special parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrVar {
    Optarg,
    Nullcmd,
    Postedit,
    Readnullcmd,
    Prompt,
    Rprompt,
    Prompt2,
    Rprompt2,
    Prompt3,
    Prompt4,
    Sprompt,
}
pub(crate) const N_STRVARS: usize = 11;

/// Array globals behind special parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArrVar {
    Pparams,
    Cdpath,
    Fignore,
    Fpath,
    Mailpath,
    Manpath,
    Path,
    Psvar,
    ZshEvalContext,
    ModulePath,
}
pub(crate) const N_ARRVARS: usize = 10;

/// A parameter's get/set/unset methods (zsh's `gsu_*` structures).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Gsu {
    /// The standard methods for the parameter's type.
    Std,
    NullSetScalar,
    NullSetHash,
    VarScalar(StrVar),
    VarInteger(IntVar),
    VarIntReadonly(IntVar),
    ZleVar(IntVar),
    RpromptIndent,
    VarArray(ArrVar),
    ColonArr(ArrVar),
    Pound,
    Errno,
    Gid,
    Egid,
    HistSize,
    Random,
    SaveHist,
    IntSeconds,
    FloatSeconds,
    Uid,
    Euid,
    TtyIdle,
    ArgZero,
    Username,
    Dash,
    Histchars,
    Home,
    Term,
    Terminfo,
    TerminfoDirs,
    Wordchars,
    Ifs,
    Underscore,
    KeyboardHack,
    Lang,
    LcAll,
    Lc,
    Pipestatus,
    /// The scalar of a pair tied with `typeset -T`, joined with `join`.
    TiedArr(u8),
    /// A parameter a module provides; the kind names the module's methods.
    Module(crate::modules::ModParam),
}

/// A parameter's value (zsh's `u` union).
#[derive(Debug, Clone, Default)]
pub(crate) enum U {
    #[default]
    None,
    Str(Vec<u8>),
    Arr(Vec<Vec<u8>>),
    Int(i64),
    Float(f64),
    Hash(Box<HashTable<Param>>),
}

/// A parameter (zsh's `struct param`).
#[derive(Debug, Clone)]
pub(crate) struct Param {
    pub(crate) flags: u32,
    pub(crate) u: U,
    pub(crate) gsu: Gsu,
    pub(crate) base: i32,
    pub(crate) width: i32,
    /// In the environment (zsh's `pm->env` non-null).
    pub(crate) env: bool,
    pub(crate) ename: Option<Vec<u8>>,
    pub(crate) old: Option<Box<Param>>,
    pub(crate) level: i32,
}

impl Param {
    pub(crate) fn new(flags: u32) -> Param {
        Param {
            flags,
            u: U::None,
            gsu: Gsu::Std,
            base: 0,
            width: 0,
            env: false,
            ename: None,
            old: None,
            level: 0,
        }
    }
}

/// Where a [`Value`]'s parameter is.
#[derive(Debug, Clone)]
pub(crate) enum PmRef {
    /// In the parameter table (or the table being worked on).
    Name(Vec<u8>),
    /// An element of the association `hash`.
    Elem(Box<PmRef>, Vec<u8>),
    /// The positional parameters seen as an array (zsh's `argvparam`).
    Argv,
    /// A parameter made up on the fly by a module's getnode.
    Transient(Box<Param>, Vec<u8>),
}

/// zsh's `struct value`: a parameter reference with a subscript.
#[derive(Debug, Clone)]
pub(crate) struct Value {
    pub(crate) isarr: i32,
    pub(crate) pm: PmRef,
    pub(crate) flags: i32,
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) arr: Option<Vec<Vec<u8>>>,
    /// zsh's `foundparam`: the element a hash subscript scan matched.
    pub(crate) found: Option<PmRef>,
}

impl Value {
    pub(crate) fn new(pm: PmRef) -> Value {
        Value {
            isarr: 0,
            pm,
            flags: 0,
            start: 0,
            end: -1,
            arr: None,
            found: None,
        }
    }
}

impl Shell {
    // ------------------------------------------------------------------
    // Finding parameters.
    // ------------------------------------------------------------------

    /// The table parameters are looked up in: an association's own table
    /// while its keys are being assigned, the shell's otherwise.
    pub(crate) fn paramtab(&self) -> &HashTable<Param> {
        self.paramtab_override
            .as_deref()
            .unwrap_or(&self.realparamtab)
    }

    pub(crate) fn paramtab_mut(&mut self) -> &mut HashTable<Param> {
        match self.paramtab_override.as_deref_mut() {
            Some(t) => t,
            None => &mut self.realparamtab,
        }
    }

    /// True while an association's table stands in for the parameter table.
    pub(crate) fn in_hash_table(&self) -> bool {
        self.paramtab_override.is_some()
    }

    /// zsh's `paramtab->getnode`, which autoloads a module's parameter.
    pub(crate) fn getparamnode(&mut self, name: &[u8]) -> Option<&Param> {
        let autoload = self
            .paramtab()
            .get(name)
            .is_some_and(|pm| pm.flags & PM_AUTOLOAD != 0);
        if autoload && !self.in_hash_table() {
            self.autoload_module_param(name);
        }
        self.paramtab().get(name)
    }

    /// The parameter a reference names.
    pub(crate) fn pm<'a>(&'a self, r: &'a PmRef) -> Option<&'a Param> {
        match r {
            PmRef::Name(n) => self.paramtab().get(n),
            PmRef::Elem(h, k) => match &self.pm(h)?.u {
                U::Hash(t) => t.get(k),
                _ => None,
            },
            PmRef::Argv => Some(&self.argvparam),
            PmRef::Transient(p, _) => Some(p),
        }
    }

    pub(crate) fn pm_mut<'a>(&'a mut self, r: &'a mut PmRef) -> Option<&'a mut Param> {
        match r {
            PmRef::Name(n) => self.paramtab_mut().get_mut(n),
            PmRef::Elem(h, k) => {
                let hp = self.pm_mut_ref(h)?;
                match &mut hp.u {
                    U::Hash(t) => t.get_mut(k),
                    _ => None,
                }
            }
            PmRef::Argv => Some(&mut self.argvparam),
            PmRef::Transient(p, _) => Some(p),
        }
    }

    fn pm_mut_ref(&mut self, r: &PmRef) -> Option<&mut Param> {
        match r {
            PmRef::Name(n) => self.paramtab_mut().get_mut(n),
            PmRef::Elem(h, k) => {
                let hp = self.pm_mut_ref(h)?;
                match &mut hp.u {
                    U::Hash(t) => t.get_mut(k),
                    _ => None,
                }
            }
            PmRef::Argv => Some(&mut self.argvparam),
            PmRef::Transient(..) => None,
        }
    }

    /// Mutate the parameter a reference names; transient parameters are
    /// changed in the reference itself.
    pub(crate) fn with_pm<R>(
        &mut self,
        r: &mut PmRef,
        f: impl FnOnce(&mut Param) -> R,
    ) -> Option<R> {
        match r {
            PmRef::Transient(p, _) => Some(f(p)),
            other => {
                let other = other.clone();
                self.pm_mut_ref(&other).map(f)
            }
        }
    }

    /// The name of the parameter a reference names.
    pub(crate) fn pm_name(&self, r: &PmRef) -> Vec<u8> {
        match r {
            PmRef::Name(n) | PmRef::Elem(_, n) | PmRef::Transient(_, n) => n.clone(),
            PmRef::Argv => Vec::new(),
        }
    }

    pub(crate) fn pm_flags(&self, r: &PmRef) -> u32 {
        self.pm(r).map_or(0, |p| p.flags)
    }

    pub(crate) fn set_pm_flags(&mut self, r: &mut PmRef, f: impl FnOnce(u32) -> u32) {
        let _ = self.with_pm(r, |p| p.flags = f(p.flags));
    }

    // ------------------------------------------------------------------
    // The get/set/unset methods.
    // ------------------------------------------------------------------

    fn intvar(&self, v: IntVar) -> i64 {
        self.intvars.get(v as usize).copied().unwrap_or(0)
    }

    fn set_intvar(&mut self, v: IntVar, x: i64) {
        if let Some(slot) = self.intvars.get_mut(v as usize) {
            *slot = x;
        }
    }

    /// The scalar getter: `pm->gsu.s->getfn(pm)`.
    pub(crate) fn getsfn(&mut self, r: &PmRef) -> Vec<u8> {
        let Some(pm) = self.pm(r) else {
            return Vec::new();
        };
        let gsu = pm.gsu.clone();
        match gsu {
            Gsu::Std | Gsu::NullSetScalar => match &pm.u {
                U::Str(s) => s.clone(),
                _ => Vec::new(),
            },
            Gsu::VarScalar(v) => self
                .strvars
                .get(v as usize)
                .cloned()
                .flatten()
                .unwrap_or_default(),
            Gsu::ColonArr(a) => crate::utils::zjoin(self.arrvar(a), b':'),
            Gsu::TiedArr(join) => {
                let ename = pm.ename.clone().unwrap_or_default();
                match self.paramtab().get(&ename).map(|p| &p.u) {
                    Some(U::Arr(a)) => crate::utils::zjoin(a, join),
                    _ => Vec::new(),
                }
            }
            Gsu::ArgZero => {
                if self.isset(POSIXARGZERO) {
                    self.posixzero.clone()
                } else {
                    self.argzero.clone()
                }
            }
            Gsu::Username => self.get_username(),
            Gsu::Dash => self.dashgetfn(),
            Gsu::Histchars => {
                let mut b = vec![self.bangchar, self.hatchar, self.hashchar];
                if let Some(p) = b.iter().position(|&c| c == 0) {
                    b.truncate(p);
                }
                b
            }
            Gsu::Home => self.home.clone(),
            Gsu::Term => self.term.clone(),
            Gsu::Terminfo => self.zsh_terminfo.clone().unwrap_or_default(),
            Gsu::TerminfoDirs => self.zsh_terminfodirs.clone().unwrap_or_default(),
            Gsu::Wordchars => self.wordchars.clone().unwrap_or_default(),
            Gsu::Ifs => self.ifs.clone().unwrap_or_default(),
            Gsu::Underscore => {
                let mut u = self.zunderscore.clone();
                tok::untokenize(&mut u);
                u
            }
            Gsu::KeyboardHack => {
                if self.keyboardhackchar == 0 {
                    Vec::new()
                } else {
                    vec![self.keyboardhackchar]
                }
            }
            Gsu::Lang | Gsu::LcAll | Gsu::Lc => match &pm.u {
                U::Str(s) => s.clone(),
                _ => Vec::new(),
            },
            Gsu::Module(m) => {
                let name = self.pm_name(r);
                self.module_getsfn(m, r, &name)
            }
            _ => Vec::new(),
        }
    }

    /// The scalar setter: `pm->gsu.s->setfn(pm, x)`; `None` is NULL.
    pub(crate) fn setsfn(&mut self, r: &mut PmRef, x: Option<Vec<u8>>) {
        let Some(gsu) = self.pm(r).map(|p| p.gsu.clone()) else {
            return;
        };
        match gsu {
            Gsu::Std => self.strsetfn(r, x),
            Gsu::NullSetScalar | Gsu::Dash | Gsu::Underscore => {}
            Gsu::VarScalar(v) => {
                if let Some(slot) = self.strvars.get_mut(v as usize) {
                    *slot = x;
                }
            }
            Gsu::ColonArr(a) => {
                let uniq = self.pm_flags(r) & PM_UNIQUE != 0;
                let arr = match &x {
                    Some(s) => crate::utils::colonsplit(s, uniq),
                    None => Vec::new(),
                };
                self.set_arrvar(a, arr.clone());
                let name = self.pm_name(r);
                self.arrfixenv(&name, Some(&arr), a == ArrVar::Path);
            }
            Gsu::TiedArr(join) => self.tiedarrsetfn(r, x, join),
            Gsu::ArgZero => {
                if let Some(x) = x {
                    if self.isset(POSIXARGZERO) {
                        self.zerr("read-only variable: 0");
                    } else {
                        self.argzero = x;
                    }
                }
            }
            Gsu::Username => self.usernamesetfn(x),
            Gsu::Histchars => self.histcharssetfn(x),
            Gsu::Home => {
                self.home = match x {
                    Some(x) if self.isset(CHASELINKS) => self.xsymlink(&x).unwrap_or(x),
                    Some(x) => x,
                    None => Vec::new(),
                };
                self.finddir_reset();
            }
            Gsu::Term => {
                self.term = x.unwrap_or_default();
                self.term_reinit_from_pm();
            }
            Gsu::Terminfo => {
                if let (Some(v), true) = (&x, self.pm_flags(r) & PM_EXPORTED != 0) {
                    let name = self.pm_name(r);
                    let flags = self.pm_flags(r);
                    self.addenv(&name, v, flags);
                }
                self.zsh_terminfo = x;
                self.term_reinit_from_pm();
            }
            Gsu::TerminfoDirs => {
                if let (Some(v), true) = (&x, self.pm_flags(r) & PM_EXPORTED != 0) {
                    let name = self.pm_name(r);
                    let flags = self.pm_flags(r);
                    self.addenv(&name, v, flags);
                }
                self.zsh_terminfodirs = x;
                self.term_reinit_from_pm();
            }
            Gsu::Wordchars => {
                self.wordchars = x;
                self.inittyptab();
            }
            Gsu::Ifs => {
                self.ifs = x;
                self.inittyptab();
            }
            Gsu::KeyboardHack => {
                if let Some(x) = x {
                    let raw = tok::unmetafy(&x);
                    if raw.len() > 1 {
                        self.zwarn("Only one KEYBOARD_HACK character can be defined");
                    }
                    let raw: Vec<u8> = raw.into_iter().take(1).collect();
                    if raw.iter().any(|c| !c.is_ascii()) {
                        self.zwarn("KEYBOARD_HACK can only contain ASCII characters");
                        return;
                    }
                    self.keyboardhackchar = raw.first().copied().unwrap_or(0);
                } else {
                    self.keyboardhackchar = 0;
                }
            }
            Gsu::Lang => {
                self.strsetfn(r, x.clone());
                self.setlang(x.as_deref());
            }
            Gsu::LcAll => {
                self.strsetfn(r, x.clone());
                self.lc_allset(x.as_deref());
            }
            Gsu::Lc => {
                self.strsetfn(r, x.clone());
                let name = self.pm_name(r);
                self.lcset(&name, x.as_deref());
            }
            Gsu::Module(m) => {
                let name = self.pm_name(r);
                self.module_setsfn(m, r, &name, x);
            }
            _ => {}
        }
    }

    fn strsetfn(&mut self, r: &mut PmRef, x: Option<Vec<u8>>) {
        let flags = self.pm_flags(r);
        let _ = self.with_pm(r, |p| {
            p.u = match &x {
                Some(v) => U::Str(v.clone()),
                None => U::None,
            }
        });
        if flags & PM_HASHELEM == 0 && (flags & PM_NAMEDDIR != 0 || self.isset(AUTONAMEDIRS)) {
            self.set_pm_flags(r, |f| f | PM_NAMEDDIR);
            let name = self.pm_name(r);
            self.adduserdir(&name, x.as_deref(), 0, false);
        }
    }

    fn tiedarrsetfn(&mut self, r: &mut PmRef, x: Option<Vec<u8>>, join: u8) {
        let Some(pm) = self.pm(r) else { return };
        let ename = pm.ename.clone().unwrap_or_default();
        let unique = pm.flags & PM_UNIQUE != 0;
        let had = matches!(self.paramtab().get(&ename).map(|p| &p.u), Some(U::Arr(_)));
        if !had && let Some(alt) = self.paramtab_mut().get_mut(&ename) {
            alt.flags &= !PM_DEFAULTED;
        }
        let new = x.map(|x| {
            let sep = if tok::is_meta(join) {
                vec![META, join ^ 32]
            } else {
                vec![join]
            };
            let mut a = self.sepsplit(&x, Some(&sep), false);
            if unique {
                uniqarray(&mut a);
            }
            a
        });
        if let Some(apm) = self.paramtab_mut().get_mut(&ename) {
            apm.u = match &new {
                Some(a) => U::Arr(a.clone()),
                None => U::None,
            };
        }
        let name = self.pm_name(r);
        let has_ename = self.pm(r).is_some_and(|p| p.ename.is_some());
        if has_ename {
            self.arrfixenv(&name, new.as_ref(), false);
        }
    }

    /// The integer getter.
    pub(crate) fn getifn(&mut self, r: &PmRef) -> i64 {
        let Some(pm) = self.pm(r) else { return 0 };
        match pm.gsu.clone() {
            Gsu::Std => match pm.u {
                U::Int(v) => v,
                _ => 0,
            },
            Gsu::VarInteger(v) | Gsu::VarIntReadonly(v) | Gsu::ZleVar(v) => self.intvar(v),
            Gsu::RpromptIndent => self.intvar(IntVar::RpromptIndent),
            Gsu::Pound => i64::try_from(self.pparams().len()).unwrap_or(0),
            Gsu::Errno => i64::from(self.errno),
            // SAFETY: the id getters have no preconditions.
            Gsu::Gid => i64::from(unsafe { libc::getgid() }),
            // SAFETY: as above.
            Gsu::Egid => i64::from(unsafe { libc::getegid() }),
            // SAFETY: as above.
            Gsu::Uid => i64::from(unsafe { libc::getuid() }),
            // SAFETY: as above.
            Gsu::Euid => i64::from(unsafe { libc::geteuid() }),
            Gsu::HistSize => self.histsiz,
            Gsu::SaveHist => self.savehistsiz,
            Gsu::Random => i64::from(self.rand() & 0x7fff),
            Gsu::IntSeconds => {
                let (s, us) = now_tv();
                s - self.shtimer.0 - i64::from(us < self.shtimer.1)
            }
            Gsu::FloatSeconds => self.floatsecondsget() as i64,
            Gsu::TtyIdle => self.ttyidle(),
            Gsu::Module(m) => {
                let name = self.pm_name(r);
                self.module_getifn(m, r, &name)
            }
            _ => 0,
        }
    }

    /// The integer setter.
    pub(crate) fn setifn(&mut self, r: &mut PmRef, x: i64) {
        let Some(gsu) = self.pm(r).map(|p| p.gsu.clone()) else {
            return;
        };
        match gsu {
            Gsu::Std => {
                let _ = self.with_pm(r, |p| p.u = U::Int(x));
            }
            Gsu::VarInteger(v) => self.set_intvar(v, x),
            Gsu::ZleVar(v) => {
                self.set_intvar(v, x);
                if matches!(v, IntVar::Lines | IntVar::Columns) {
                    self.adjustwinsize(if v == IntVar::Columns { 3 } else { 2 });
                }
            }
            Gsu::RpromptIndent => self.set_intvar(IntVar::RpromptIndent, x),
            Gsu::Errno => self.errno = i32::try_from(x).unwrap_or(0),
            Gsu::Gid => {
                // SAFETY: setgid has no memory-safety preconditions.
                if unsafe { libc::setgid(u32::try_from(x).unwrap_or(u32::MAX)) } != 0 {
                    self.zerr(&format!(
                        "failed to change group ID: {}",
                        self.errmsg_last()
                    ));
                }
            }
            Gsu::Egid => {
                // SAFETY: setegid has no memory-safety preconditions.
                if unsafe { libc::setegid(u32::try_from(x).unwrap_or(u32::MAX)) } != 0 {
                    self.zerr(&format!(
                        "failed to change effective group ID: {}",
                        self.errmsg_last()
                    ));
                }
            }
            Gsu::Uid => {
                // SAFETY: setuid has no memory-safety preconditions.
                if unsafe { libc::setuid(u32::try_from(x).unwrap_or(u32::MAX)) } != 0 {
                    self.zerr(&format!("failed to change user ID: {}", self.errmsg_last()));
                }
            }
            Gsu::Euid => {
                // SAFETY: seteuid has no memory-safety preconditions.
                if unsafe { libc::seteuid(u32::try_from(x).unwrap_or(u32::MAX)) } != 0 {
                    self.zerr(&format!(
                        "failed to change effective user ID: {}",
                        self.errmsg_last()
                    ));
                }
            }
            Gsu::HistSize => {
                self.histsiz = x.max(1);
                self.resizehistents();
            }
            Gsu::SaveHist => self.savehistsiz = x.max(0),
            Gsu::Random => self.srand(u32::try_from(x & 0xffff_ffff).unwrap_or(0)),
            Gsu::IntSeconds => {
                let (s, us) = now_tv();
                self.shtimer = (s - x, us);
            }
            Gsu::Module(m) => {
                let name = self.pm_name(r);
                self.module_setifn(m, r, &name, x);
            }
            _ => {}
        }
    }

    /// The float getter.
    pub(crate) fn getffn(&mut self, r: &PmRef) -> f64 {
        let Some(pm) = self.pm(r) else { return 0.0 };
        match pm.gsu {
            Gsu::Std => match pm.u {
                U::Float(v) => v,
                _ => 0.0,
            },
            Gsu::FloatSeconds => self.floatsecondsget(),
            Gsu::Module(m) => {
                let name = self.pm_name(r);
                self.module_getffn(m, r, &name)
            }
            _ => 0.0,
        }
    }

    /// The float setter.
    pub(crate) fn setffn(&mut self, r: &mut PmRef, x: f64) {
        let Some(gsu) = self.pm(r).map(|p| p.gsu.clone()) else {
            return;
        };
        match gsu {
            Gsu::Std => {
                let _ = self.with_pm(r, |p| p.u = U::Float(x));
            }
            Gsu::FloatSeconds => {
                let (s, us) = now_tv();
                #[expect(clippy::cast_possible_truncation, reason = "zsh truncates to zlong")]
                let whole = x as i64;
                #[expect(clippy::cast_possible_truncation, reason = "as above")]
                let frac = ((x - whole as f64) * 1_000_000.0) as i64;
                self.shtimer = (s - whole, us - frac);
            }
            _ => {}
        }
    }

    fn floatsecondsget(&self) -> f64 {
        let (s, us) = now_tv();
        (s - self.shtimer.0) as f64 + (us - self.shtimer.1) as f64 / 1_000_000.0
    }

    /// The array getter.
    pub(crate) fn getafn(&mut self, r: &PmRef) -> Vec<Vec<u8>> {
        let Some(pm) = self.pm(r) else {
            return Vec::new();
        };
        match pm.gsu.clone() {
            Gsu::Std => match &pm.u {
                U::Arr(a) => a.clone(),
                _ => Vec::new(),
            },
            Gsu::VarArray(a) => self.arrvar(a).to_vec(),
            Gsu::Pipestatus => self
                .pipestats
                .iter()
                .take(self.numpipestats)
                .map(|n| n.to_string().into_bytes())
                .collect(),
            Gsu::Module(m) => {
                let name = self.pm_name(r);
                self.module_getafn(m, r, &name)
            }
            _ => Vec::new(),
        }
    }

    /// The array setter; `None` is NULL.
    pub(crate) fn setafn(&mut self, r: &mut PmRef, x: Option<Vec<Vec<u8>>>) {
        let Some(pm) = self.pm(r) else { return };
        let gsu = pm.gsu.clone();
        let flags = pm.flags;
        let ename = pm.ename.clone();
        match gsu {
            Gsu::Std => {
                let mut x = x;
                if flags & PM_UNIQUE != 0
                    && let Some(a) = x.as_mut()
                {
                    uniqarray(a);
                }
                let _ = self.with_pm(r, |p| {
                    p.u = match &x {
                        Some(a) => U::Arr(a.clone()),
                        None => U::None,
                    }
                });
                if let (Some(en), Some(a)) = (&ename, &x) {
                    self.arrfixenv(en, Some(a), false);
                }
            }
            Gsu::VarArray(v) => {
                let mut x = x;
                if flags & PM_UNIQUE != 0
                    && let Some(a) = x.as_mut()
                {
                    uniqarray(a);
                }
                let arr = x.clone().unwrap_or_default();
                self.set_arrvar(v, arr.clone());
                if let Some(en) = &ename {
                    if x.is_some() {
                        self.arrfixenv(en, Some(&arr), v == ArrVar::Path);
                    } else if v == ArrVar::Path {
                        self.pathchecked = 0;
                    }
                }
            }
            Gsu::Pipestatus => {
                if let Some(a) = x {
                    let mut i = 0;
                    for v in a.iter().take(MAX_PIPESTATS) {
                        let (n, _) = crate::utils::zstrtol(v, 10);
                        if let Some(slot) = self.pipestats.get_mut(i) {
                            *slot = i32::try_from(n).unwrap_or(0);
                        }
                        i += 1;
                    }
                    self.numpipestats = i;
                } else {
                    self.numpipestats = 0;
                }
            }
            Gsu::Module(m) => {
                let name = self.pm_name(r);
                self.module_setafn(m, r, &name, x);
            }
            _ => {}
        }
    }

    pub(crate) fn arrvar(&self, a: ArrVar) -> &[Vec<u8>] {
        self.arrvars.get(a as usize).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn set_arrvar(&mut self, a: ArrVar, v: Vec<Vec<u8>>) {
        if a == ArrVar::Path {
            self.cmdnamtab_empty();
        }
        if let Some(slot) = self.arrvars.get_mut(a as usize) {
            *slot = v;
        }
    }

    pub(crate) fn pparams(&self) -> &[Vec<u8>] {
        self.arrvar(ArrVar::Pparams)
    }

    /// Does the parameter have a hash table (possibly made by a module)?
    pub(crate) fn has_hash(&self, r: &PmRef) -> bool {
        matches!(self.pm(r).map(|p| &p.u), Some(U::Hash(_)))
    }

    /// The hash setter: replace the table (`None` empties it).
    pub(crate) fn sethfn(&mut self, r: &mut PmRef, x: Option<HashTable<Param>>) {
        let Some(gsu) = self.pm(r).map(|p| p.gsu.clone()) else {
            return;
        };
        match gsu {
            Gsu::NullSetHash => {}
            Gsu::Module(m) => {
                let name = self.pm_name(r);
                self.module_sethfn(m, r, &name, x);
            }
            _ => {
                let _ = self.with_pm(r, |p| {
                    p.u = match x {
                        Some(t) => U::Hash(Box::new(t)),
                        None => U::None,
                    }
                });
            }
        }
    }

    /// The unset method.
    pub(crate) fn unsetfn(&mut self, r: &mut PmRef, exp: bool) {
        let Some(pm) = self.pm(r) else { return };
        match pm.gsu.clone() {
            Gsu::NullSetHash | Gsu::ArgZero => {}
            Gsu::RpromptIndent => {
                self.stdunsetfn(r, exp);
                self.set_intvar(IntVar::RpromptIndent, 1);
            }
            Gsu::TiedArr(j) => {
                self.setsfn(r, None);
                let _ = j;
                let _ = self.with_pm(r, |p| {
                    p.u = U::None;
                    p.ename = None;
                    p.flags &= !PM_TIED;
                    p.flags |= PM_UNSET;
                });
            }
            Gsu::Module(m) => {
                let name = self.pm_name(r);
                self.module_unsetfn(m, r, &name, exp);
            }
            _ => self.stdunsetfn(r, exp),
        }
    }

    /// zsh's `stdunsetfn`.
    pub(crate) fn stdunsetfn(&mut self, r: &mut PmRef, _exp: bool) {
        let flags = self.pm_flags(r);
        match pm_type(flags) {
            PM_SCALAR => self.setsfn(r, None),
            PM_ARRAY => self.setafn(r, None),
            PM_HASHED => self.sethfn(r, None),
            _ => {
                if flags & PM_SPECIAL == 0 {
                    let _ = self.with_pm(r, |p| p.u = U::None);
                }
            }
        }
        let _ = self.with_pm(r, |p| {
            if p.flags & (PM_SPECIAL | PM_TIED) == PM_TIED {
                p.ename = None;
                p.flags &= !PM_TIED;
            }
            p.flags |= PM_UNSET;
        });
    }

    // ------------------------------------------------------------------
    // Creating and removing parameters.
    // ------------------------------------------------------------------

    /// zsh's `isident`.
    pub(crate) fn isident(&self, s: &[u8]) -> bool {
        if s.is_empty() {
            return false;
        }
        let mut i;
        let start;
        if s.first().is_some_and(u8::is_ascii_digit) {
            start = 1;
            i = 1;
            while s.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
        } else {
            start = 0;
            i = self.itype_end(s, 0, crate::utils::IIDENT, false);
        }
        if i >= s.len() {
            return true;
        }
        if i == start && start == 0 {
            return false;
        }
        if at(s, i) != b'[' {
            return false;
        }
        match self.parse_subscript(s.get(i + 1..).unwrap_or(&[]), true, b']') {
            Some(end) => i + 1 + end + 1 == s.len(),
            None => false,
        }
    }

    /// zsh's `createparam`: the new parameter, or `None` if one exists that
    /// can be used as it is (its PM_UNSET is cleared) or it cannot be made.
    pub(crate) fn createparam(&mut self, name: &[u8], flags: u32) -> Option<PmRef> {
        let mut flags = flags;
        if self.in_hash_table() {
            flags = (flags & !PM_EXPORTED) | PM_HASHELEM;
        }
        let oldpm_exists = if self.in_hash_table() {
            self.getparamnode(name).is_some()
        } else {
            self.paramtab().get(name).is_some()
        };
        let locallevel = self.locallevel;
        let mut keep_old: Option<Box<Param>> = None;
        let mut reuse = false;
        if oldpm_exists {
            let (olevel, oflags, oename) = {
                let p = self.paramtab().get(name)?;
                (p.level, p.flags, p.ename.clone())
            };
            if olevel == locallevel || flags & PM_LOCAL == 0 {
                if self.isset(POSIXBUILTINS) && oflags & PM_READONLY != 0 {
                    self.zerr(&format!("read-only variable: {}", lossy(name)));
                    return None;
                }
                if oflags & PM_RESTRICTED != 0 && self.isset(RESTRICTED) {
                    self.zerr(&format!("{}: restricted", lossy(name)));
                    return None;
                }
                if oflags & PM_UNSET == 0
                    || oflags & PM_SPECIAL != 0
                    || (self.isset(POSIXBUILTINS) && oflags & PM_EXPORTED != 0)
                {
                    if oflags & PM_RO_BY_DESIGN != 0 {
                        self.zerr(&format!(
                            "{}: can't change parameter attribute",
                            lossy(name)
                        ));
                        return None;
                    }
                    if let Some(p) = self.paramtab_mut().get_mut(name) {
                        p.flags &= !PM_UNSET;
                    }
                    if oflags & PM_SPECIAL != 0
                        && let Some(en) = oename
                        && let Some(alt) = self.paramtab_mut().get_mut(&en)
                    {
                        alt.flags &= !PM_UNSET;
                    }
                    return None;
                }
                reuse = true;
            } else {
                // Hide the outer parameter under a new local one.
                let mut old = self.paramtab_mut().remove(name)?;
                if old.env {
                    self.delenv_name(name);
                    old.env = false;
                }
                keep_old = Some(Box::new(old));
            }
        }
        if self.isset(ALLEXPORT) && flags & PM_HASHELEM == 0 {
            flags |= PM_EXPORTED;
        }
        let new_flags = flags & !PM_LOCAL;
        if reuse {
            if let Some(p) = self.paramtab_mut().get_mut(name) {
                p.base = 0;
                p.width = 0;
                p.flags = new_flags;
                if new_flags & PM_SPECIAL == 0 {
                    p.gsu = Gsu::Std;
                }
            }
        } else {
            let mut p = Param::new(new_flags);
            p.old = keep_old;
            let _ = self.paramtab_mut().insert(name.to_vec(), p);
        }
        Some(PmRef::Name(name.to_vec()))
    }

    /// zsh's `unsetparam`.
    pub(crate) fn unsetparam(&mut self, s: &[u8]) {
        if self.paramtab().get(s).is_some() {
            let mut r = PmRef::Name(s.to_vec());
            let _ = self.unsetparam_pm(&mut r, false, true);
        }
    }

    /// zsh's `unsetparam_pm`.
    pub(crate) fn unsetparam_pm(&mut self, r: &mut PmRef, altflag: bool, exp: bool) -> i32 {
        let Some(pm) = self.pm(r) else { return 0 };
        let (flags, level, ename) = (pm.flags, pm.level, pm.ename.clone());
        let name = self.pm_name(r);
        if flags & PM_READONLY != 0 && level <= self.locallevel {
            self.zerr(&format!("read-only variable: {}", lossy(&name)));
            return 1;
        }
        if flags & PM_RESTRICTED != 0 && self.isset(RESTRICTED) {
            self.zerr(&format!("{}: restricted", lossy(&name)));
            return 1;
        }
        let altremove = if altflag { None } else { ename };
        self.set_pm_flags(r, |f| f & !PM_DECLARED);
        if self.pm_flags(r) & PM_UNSET == 0 {
            self.unsetfn(r, exp);
        }
        if self.pm(r).is_some_and(|p| p.env) {
            self.delenv_name(&name);
            let _ = self.with_pm(r, |p| p.env = false);
        }
        if let Some(alt) = altremove {
            // Tied parameters are at the same local level as each other.
            let alt_special = self
                .paramtab()
                .get(&alt)
                .is_some_and(|p| p.flags & PM_SPECIAL != 0);
            if alt_special || self.paramtab().get(&alt).is_none() {
                if self.paramtab().get(&alt).is_some() {
                    let mut ar = PmRef::Name(alt.clone());
                    let _ = self.unsetparam_pm(&mut ar, true, exp);
                }
            } else {
                self.unset_alt_at_level(&alt, level, exp);
            }
            if flags & PM_SPECIAL == 0 {
                let _ = self.with_pm(r, |p| {
                    if matches!(p.gsu, Gsu::TiedArr(_)) {
                        p.gsu = Gsu::Std;
                    }
                });
            }
        }
        let (flags, level) = match self.pm(r) {
            Some(p) => (p.flags, p.level),
            None => return 0,
        };
        if (level != 0 && self.locallevel >= level)
            || flags & (PM_SPECIAL | PM_REMOVABLE) == PM_SPECIAL
        {
            return 0;
        }
        // Remove the node, restoring what it hid.
        let PmRef::Name(n) = r.clone() else {
            if let PmRef::Elem(h, k) = r.clone() {
                let _ = self.with_pm(&mut h.as_ref().clone(), |hp| {
                    if let U::Hash(t) = &mut hp.u {
                        let _ = t.remove(&k);
                    }
                });
            }
            return 0;
        };
        let Some(removed) = self.paramtab_mut().remove(&n) else {
            return 0;
        };
        if let Some(old) = removed.old {
            let oldpm = *old;
            let named = pm_type(oldpm.flags) == PM_SCALAR
                && flags & PM_HASHELEM == 0
                && oldpm.flags & PM_NAMEDDIR != 0
                && oldpm.gsu == Gsu::Std;
            let exported = oldpm.flags & PM_EXPORTED != 0;
            let value = match &oldpm.u {
                U::Str(s) => Some(s.clone()),
                _ => None,
            };
            let _ = self.paramtab_mut().insert(n.clone(), oldpm);
            if named {
                self.adduserdir(&n, value.as_deref(), 0, false);
            }
            if exported {
                let mut or = PmRef::Name(n);
                self.export_param(&mut or);
            }
        }
        0
    }

    /// Unset the tied partner `alt` at `level`, looking beneath locals.
    fn unset_alt_at_level(&mut self, alt: &[u8], level: i32, exp: bool) {
        // Walk down the chain of hidden parameters to the one at `level`.
        let Some(top) = self.paramtab().get(alt) else {
            return;
        };
        if top.level <= level {
            let mut ar = PmRef::Name(alt.to_vec());
            let _ = self.unsetparam_pm(&mut ar, true, exp);
            return;
        }
        // A deeper local hides it: unset the hidden one in place.
        let Some(top) = self.paramtab_mut().get_mut(alt) else {
            return;
        };
        let mut cur: &mut Param = top;
        loop {
            let next_level = cur.old.as_ref().map(|o| o.level);
            match next_level {
                Some(l) if l > level => {
                    let Some(o) = cur.old.as_deref_mut() else {
                        return;
                    };
                    cur = o;
                }
                Some(_) => {
                    if let Some(o) = cur.old.as_deref_mut() {
                        o.u = U::None;
                        o.flags |= PM_UNSET;
                        o.flags &= !PM_TIED;
                        o.ename = None;
                        if o.level == 0 {
                            cur.old = None;
                        }
                    }
                    return;
                }
                None => return,
            }
        }
    }

    /// zsh's `startparamscope`.
    pub(crate) fn startparamscope(&mut self) {
        self.locallevel += 1;
    }

    /// zsh's `endparamscope`.
    pub(crate) fn endparamscope(&mut self) {
        self.locallevel -= 1;
        self.saveandpophiststack(0, crate::hist::HFILE_USE_OPTIONS);
        let mut lc_update = false;
        for name in self.realparamtab.keys() {
            let Some(pm) = self.realparamtab.get(&name) else {
                continue;
            };
            if pm.level <= self.locallevel {
                continue;
            }
            if pm.flags & (PM_SPECIAL | PM_REMOVABLE) == PM_SPECIAL {
                if name.starts_with(b"LC_") || name == b"LANG" {
                    lc_update = true;
                }
                self.restore_special_scope(&name);
            } else {
                let mut r = PmRef::Name(name);
                let _ = self.unsetparam_pm(&mut r, false, false);
            }
        }
        if lc_update {
            self.lc_restore();
        }
    }

    /// Restore a special made local by stealth (`scanendscope`).
    fn restore_special_scope(&mut self, name: &[u8]) {
        let Some(pm) = self.realparamtab.get_mut(name) else {
            return;
        };
        let Some(tpm) = pm.old.take() else { return };
        let mut tpm = *tpm;
        let mut norestore = false;
        if name == b"SECONDS" {
            let ttype = pm_type(tpm.flags);
            pm.gsu = if ttype == PM_EFLOAT || ttype == PM_FFLOAT {
                Gsu::FloatSeconds
            } else {
                Gsu::IntSeconds
            };
            if let U::Float(d) = tpm.u {
                #[expect(clippy::cast_possible_truncation, reason = "zsh's setrawseconds")]
                let whole = d as i64;
                #[expect(clippy::cast_possible_truncation, reason = "as above")]
                let frac = ((d - whole as f64) * 1_000_000.0) as i64;
                self.shtimer = (whole, frac);
            }
            norestore = true;
        }
        let Some(pm) = self.realparamtab.get_mut(name) else {
            return;
        };
        pm.old = tpm.old.take();
        pm.flags = tpm.flags & !PM_NORESTORE;
        pm.level = tpm.level;
        pm.base = tpm.base;
        pm.width = tpm.width;
        let was_env = pm.env;
        pm.env = false;
        if was_env {
            self.delenv_name(name);
        }
        let mut r = PmRef::Name(name.to_vec());
        if tpm.flags & (PM_NORESTORE | PM_READONLY) == 0 && !norestore {
            match (pm_type(tpm.flags), tpm.u) {
                (PM_SCALAR, U::Str(s)) => self.setsfn(&mut r, Some(s)),
                (PM_SCALAR, _) => self.setsfn(&mut r, Some(Vec::new())),
                (PM_INTEGER, U::Int(v)) => self.setifn(&mut r, v),
                (PM_EFLOAT | PM_FFLOAT, U::Float(v)) => self.setffn(&mut r, v),
                (PM_ARRAY, U::Arr(a)) => self.setafn(&mut r, Some(a)),
                (PM_ARRAY, _) => self.setafn(&mut r, Some(Vec::new())),
                (PM_HASHED, U::Hash(h)) => self.sethfn(&mut r, Some(*h)),
                _ => {}
            }
        }
        if self.pm_flags(&r) & PM_EXPORTED != 0 {
            self.export_param(&mut r);
        }
    }

    // ------------------------------------------------------------------
    // The environment.
    // ------------------------------------------------------------------

    fn findenv(&self, name: &[u8]) -> Option<usize> {
        let raw = tok::unmetafy(name);
        self.environ
            .iter()
            .position(|e| e.len() > raw.len() && e.starts_with(&raw) && at(e, raw.len()) == b'=')
    }

    /// zsh's `addenv`: put `name=value` in the environment.
    pub(crate) fn addenv(&mut self, name: &[u8], value: &[u8], flags: u32) {
        let raw_name = tok::unmetafy(name);
        if raw_name.iter().any(|&c| c >= 128) {
            return;
        }
        let mut e = raw_name;
        e.push(b'=');
        for c in tok::unmetafy(value) {
            e.push(if flags & PM_LOWER != 0 {
                c.to_ascii_lowercase()
            } else if flags & PM_UPPER != 0 {
                c.to_ascii_uppercase()
            } else {
                c
            });
        }
        match self.findenv(name) {
            Some(i) => {
                if let Some(slot) = self.environ.get_mut(i) {
                    *slot = e;
                }
            }
            None => self.environ.push(e),
        }
        let mut r = PmRef::Name(name.to_vec());
        let _ = self.with_pm(&mut r, |p| {
            p.env = true;
            p.flags |= PM_EXPORTED;
        });
    }

    /// Remove `name` from the environment (zsh's `delenv`).
    pub(crate) fn delenv_name(&mut self, name: &[u8]) {
        if let Some(i) = self.findenv(name) {
            let _ = self.environ.remove(i);
        }
    }

    /// zsh's `export_param`.
    pub(crate) fn export_param(&mut self, r: &mut PmRef) {
        let flags = self.pm_flags(r);
        let val = if pm_type(flags) & (PM_ARRAY | PM_HASHED) != 0 {
            return;
        } else if pm_type(flags) == PM_INTEGER {
            let v = self.getifn(r);
            let base = self.pm(r).map_or(0, |p| p.base);
            self.convbase(v, base)
        } else if flags & (PM_EFLOAT | PM_FFLOAT) != 0 {
            let v = self.getffn(r);
            let base = self.pm(r).map_or(0, |p| p.base);
            convfloat(v, base, flags)
        } else {
            self.getsfn(r)
        };
        let name = self.pm_name(r);
        self.addenv(&name, &val, flags);
    }

    /// zsh's `arrfixenv`: keep the colon form of a tied array exported.
    pub(crate) fn arrfixenv(&mut self, name: &[u8], t: Option<&Vec<Vec<u8>>>, is_path: bool) {
        if is_path {
            self.cmdnamtab_empty();
        }
        let Some(pm) = self.paramtab().get(name) else {
            return;
        };
        if pm.flags & PM_HASHELEM != 0 {
            return;
        }
        let allexport = self.isset(ALLEXPORT);
        let special = pm.flags & PM_SPECIAL != 0;
        let joinchar = match &pm.gsu {
            Gsu::TiedArr(j) => *j,
            _ => b':',
        };
        let Some(pm) = self.paramtab_mut().get_mut(name) else {
            return;
        };
        if allexport {
            pm.flags |= PM_EXPORTED;
        }
        pm.flags &= !PM_DEFAULTED;
        if pm.flags & PM_EXPORTED == 0 {
            return;
        }
        let flags = pm.flags;
        let j = if special { b':' } else { joinchar };
        let text = t.map(|a| crate::utils::zjoin(a, j)).unwrap_or_default();
        self.addenv(name, &text, flags);
    }

    // ------------------------------------------------------------------
    // Numbers.
    // ------------------------------------------------------------------

    /// zsh's `convbase_ptr`: `(text, digits)`.
    pub(crate) fn convbase_ptr(&self, v: i64, base: i32) -> (Vec<u8>, usize) {
        let mut out = Vec::new();
        let neg = v < 0;
        if neg {
            out.push(b'-');
        }
        let mut base = if (-1..=1).contains(&base) { -10 } else { base };
        if base > 0 {
            if self.isset(CBASES) && base == 16 {
                out.extend_from_slice(b"0x");
            } else if self.isset(CBASES) && base == 8 && self.isset(OCTALZEROES) {
                out.push(b'0');
            } else if base != 10 {
                out.extend(format!("{base}#").bytes());
            }
        } else {
            base = -base;
        }
        let b = u64::try_from(base).unwrap_or(10);
        let mut x = v.unsigned_abs();
        let mut digits = Vec::new();
        loop {
            let d = u8::try_from(x % b).unwrap_or(0);
            digits.push(if d < 10 { b'0' + d } else { d - 10 + b'A' });
            x /= b;
            if x == 0 {
                break;
            }
        }
        digits.reverse();
        let n = digits.len();
        out.extend(digits);
        (out, n)
    }

    /// zsh's `convbase`.
    pub(crate) fn convbase(&self, v: i64, base: i32) -> Vec<u8> {
        self.convbase_ptr(v, base).0
    }

    /// zsh's `convbase_underscore`.
    pub(crate) fn convbase_underscore(&self, v: i64, base: i32, underscore: i32) -> Vec<u8> {
        let (s, ndigits) = self.convbase_ptr(v, base);
        if underscore <= 0 {
            return s;
        }
        let u = usize::try_from(underscore).unwrap_or(1);
        if (ndigits.saturating_sub(1)) / u == 0 {
            return s;
        }
        let prefix = s.len() - ndigits;
        let mut out: Vec<u8> = s.get(..prefix).unwrap_or(&[]).to_vec();
        let digits = s.get(prefix..).unwrap_or(&[]);
        for (k, &d) in digits.iter().enumerate() {
            if k > 0 && (ndigits - k) % u == 0 {
                out.push(b'_');
            }
            out.push(d);
        }
        out
    }
}

/// Seconds and microseconds now.
pub(crate) fn now_tv() -> (i64, i64) {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (
        i64::try_from(d.as_secs()).unwrap_or(0),
        i64::from(d.subsec_micros()),
    )
}

/// zsh's `convfloat` (to a string).
pub(crate) fn convfloat(dval: f64, digits: i32, flags: u32) -> Vec<u8> {
    if dval.is_infinite() {
        return if dval < 0.0 {
            b"-Inf".to_vec()
        } else {
            b"Inf".to_vec()
        };
    }
    if dval.is_nan() {
        return b"NaN".to_vec();
    }
    let s = if flags & (PM_EFLOAT | PM_FFLOAT) == 0 {
        let d = if digits == 0 { 17 } else { digits };
        crate::math::format_g(dval, d)
    } else if flags & PM_FFLOAT != 0 {
        let d = if digits <= 0 { 10 } else { digits };
        format!("{:.*}", usize::try_from(d).unwrap_or(10), dval)
    } else {
        let d = if digits <= 0 { 10 } else { digits } - 1;
        crate::math::format_e(dval, d)
    };
    let mut s = s.into_bytes();
    if !s.contains(&b'e') && !s.contains(&b'.') {
        s.push(b'.');
    }
    s
}

/// zsh's `convfloat_underscore`.
pub(crate) fn convfloat_underscore(dval: f64, underscore: i32) -> Vec<u8> {
    let s = convfloat(dval, 0, 0);
    if underscore <= 0 {
        return s;
    }
    let u = usize::try_from(underscore).unwrap_or(1);
    let mut i = 0;
    let neg = at(&s, 0) == b'-';
    if neg {
        i = 1;
    }
    let int_start = i;
    while at(&s, i).is_ascii_digit() {
        i += 1;
    }
    let nint = i - int_start;
    let mut nfrac: usize = 0;
    if at(&s, i) == b'.' {
        let mut j = i + 1;
        while at(&s, j).is_ascii_digit() {
            j += 1;
            nfrac += 1;
        }
    }
    if nint.saturating_sub(1) / u + nfrac.saturating_sub(1) / u == 0 {
        return s;
    }
    let mut out = Vec::new();
    let mut k = 0;
    if neg {
        out.push(b'-');
        k = 1;
    }
    let mut left = nint;
    while left > 0 {
        out.push(at(&s, k));
        k += 1;
        left -= 1;
        if left != 0 && left % u == 0 {
            out.push(b'_');
        }
    }
    if nfrac > 0 {
        out.push(at(&s, k));
        k += 1;
        let mut m = 0;
        let mut left = nfrac;
        while left > 0 {
            out.push(at(&s, k));
            k += 1;
            m += 1;
            left -= 1;
            if left != 0 && m == u {
                out.push(b'_');
                m = 0;
            }
        }
    }
    out.extend_from_slice(s.get(k..).unwrap_or(&[]));
    out
}

/// zsh's `uniqarray`: drop repeated elements, keeping the first.
pub(crate) fn uniqarray(x: &mut Vec<Vec<u8>>) {
    let mut seen = std::collections::HashSet::new();
    x.retain(|e| seen.insert(e.clone()));
}

/// The value of an [`MNumber`] for a parameter of `flags`.
pub(crate) fn mnumber_int(m: MNumber) -> i64 {
    match m {
        MNumber::Int(i) => i,
        #[expect(clippy::cast_possible_truncation, reason = "zsh casts to zlong")]
        MNumber::Float(f) => f as i64,
    }
}
