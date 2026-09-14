//! The shell's state: parameters, scopes, functions, aliases, options.

use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::List;
use crate::lex::{AliasDef, LexEnv, LexOpts};

/// A parameter's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Scalar(Vec<u8>),
    Array(Vec<Vec<u8>>),
    Assoc(Vec<(Vec<u8>, Vec<u8>)>),
}

impl Value {
    /// The value as one string, array elements joined by a space.
    pub(crate) fn joined(&self) -> Vec<u8> {
        match self {
            Value::Scalar(s) => s.clone(),
            Value::Array(a) => a.join(&b' '),
            Value::Assoc(a) => a
                .iter()
                .map(|(_, v)| v.clone())
                .collect::<Vec<_>>()
                .join(&b' '),
        }
    }
}

/// A parameter.
#[derive(Debug, Clone)]
pub(crate) struct Var {
    pub(crate) value: Value,
    pub(crate) export: bool,
    pub(crate) readonly: bool,
    pub(crate) integer: bool,
}

impl Var {
    pub(crate) fn scalar(v: Vec<u8>) -> Var {
        Var {
            value: Value::Scalar(v),
            export: false,
            readonly: false,
            integer: false,
        }
    }
}

/// How control leaves a construct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    Normal,
    Break(u32),
    Continue(u32),
    Return,
    Exit,
    /// An error that ends the command list, as zsh's `errflag` does: back to
    /// the prompt, or the end of a script.
    Abort,
}

/// A shell function.
#[derive(Debug, Clone)]
pub(crate) struct Function {
    pub(crate) body: Rc<List>,
}

/// The whole state of the shell.
#[derive(Debug)]
pub(crate) struct Shell {
    pub(crate) vars: HashMap<Vec<u8>, Var>,
    /// One frame per function call: the values `local` replaced.
    pub(crate) locals: Vec<Vec<(Vec<u8>, Option<Var>)>>,
    pub(crate) functions: HashMap<Vec<u8>, Function>,
    pub(crate) aliases: HashMap<Vec<u8>, AliasDef>,
    pub(crate) suffix_aliases: HashMap<Vec<u8>, AliasDef>,
    pub(crate) positional: Vec<Vec<u8>>,
    pub(crate) argzero: Vec<u8>,
    /// The name used in messages (`zsh:`, `sh:`).
    pub(crate) name: String,
    pub(crate) status: i32,
    pub(crate) flow: Flow,
    pub(crate) loop_depth: u32,
    pub(crate) options: HashMap<String, bool>,
    pub(crate) interactive: bool,
    pub(crate) pid: i32,
    pub(crate) last_bg: i32,
    /// Commands for `trap ... EXIT`.
    pub(crate) exit_trap: Option<Vec<u8>>,
    /// Inside a forked child: `exit` must not run the EXIT trap twice.
    pub(crate) subshell: bool,
    /// Line number of the command being run.
    pub(crate) lineno: u64,
    /// Children started with `&`.
    pub(crate) jobs: Vec<i32>,
    /// Nesting of `source` and `.`: `return` leaves the file.
    pub(crate) source_depth: u32,
    /// `getopts`' position inside a bundled option argument.
    pub(crate) optpos: usize,
    /// The status of the last command substitution in the words being
    /// expanded: an assignment-only command's status.
    pub(crate) subst_status: Option<i32>,
    /// Reading a line typed at the prompt, where `#` starts a comment only
    /// with INTERACTIVE_COMMENTS; sourced files always have comments.
    pub(crate) at_prompt: bool,
}

/// Normalise an option name: lower case, no underscores.
pub(crate) fn option_key(name: &[u8]) -> String {
    name.iter()
        .filter(|&&c| c != b'_')
        .map(|&c| char::from(c.to_ascii_lowercase()))
        .collect()
}

/// The options zsh has on by default in native mode.
const DEFAULT_ON: &[&str] = &[
    "aliases",
    "alwayslastprompt",
    "appendhistory",
    "autolist",
    "automenu",
    "autoparamkeys",
    "autoparamslash",
    "autoremoveslash",
    "badpattern",
    "banghist",
    "bareglobqual",
    "beep",
    "bgnice",
    "caseglob",
    "casematch",
    "checkjobs",
    "checkrunningjobs",
    "clobber",
    "debugbeforecmd",
    "equals",
    "evallineno",
    "exec",
    "flowcontrol",
    "functionargzero",
    "glob",
    "globalexport",
    "globalrcs",
    "hashcmds",
    "hashdirs",
    "hashlistall",
    "histbeep",
    "histsavebycopy",
    "hup",
    "listambiguous",
    "listbeep",
    "listtypes",
    "multibyte",
    "multifuncdef",
    "multios",
    "nomatch",
    "notify",
    "promptcr",
    "promptpercent",
    "promptsp",
    "rcs",
    "shortloops",
    "unset",
];

impl Shell {
    /// A shell with zsh's defaults, the environment imported and exported.
    pub(crate) fn new(name: String) -> Shell {
        let mut vars = HashMap::new();
        for (k, v) in std::env::vars_os() {
            use std::os::unix::ffi::OsStrExt;
            let mut var = Var::scalar(crate::tok::metafy(v.as_bytes()));
            var.export = true;
            let _old = vars.insert(crate::tok::metafy(k.as_bytes()), var);
        }
        let options = DEFAULT_ON.iter().map(|o| ((*o).to_owned(), true)).collect();
        // SAFETY: getpid has no preconditions.
        let pid = unsafe { libc::getpid() };
        let mut sh = Shell {
            vars,
            locals: Vec::new(),
            functions: HashMap::new(),
            aliases: HashMap::new(),
            suffix_aliases: HashMap::new(),
            positional: Vec::new(),
            argzero: name.clone().into_bytes(),
            name,
            status: 0,
            flow: Flow::Normal,
            loop_depth: 0,
            options,
            interactive: false,
            pid,
            last_bg: 0,
            exit_trap: None,
            subshell: false,
            lineno: 0,
            jobs: Vec::new(),
            source_depth: 0,
            optpos: 1,
            subst_status: None,
            at_prompt: false,
        };
        for (k, v) in [
            ("IFS", &b" \t\n\x83 "[..]),
            ("ZSH_VERSION", b"5.9"),
            ("ZSH_NAME", b"zsh"),
        ] {
            if !sh.vars.contains_key(k.as_bytes()) {
                let _old = sh
                    .vars
                    .insert(k.as_bytes().to_vec(), Var::scalar(v.to_vec()));
            }
        }
        if !sh.vars.contains_key(&b"PS1"[..]) {
            sh.set_scalar(b"PS1", b"%m%# ".to_vec());
        }
        sh.set_scalar(b"PS2", b"> ".to_vec());
        let pwd = std::env::current_dir().map(|p| {
            use std::os::unix::ffi::OsStrExt;
            p.as_os_str().as_bytes().to_vec()
        });
        if let Ok(p) = pwd {
            sh.set_scalar(b"PWD", p);
            if let Some(v) = sh.vars.get_mut(&b"PWD"[..]) {
                v.export = true;
            }
        }
        sh
    }

    /// Whether option `name` is set.
    pub(crate) fn opt(&self, name: &str) -> bool {
        self.options.get(name).copied().unwrap_or(false)
    }

    /// Set or unset an option by any spelling (`NO_` prefixes invert).
    /// Returns false for an unknown name.
    pub(crate) fn set_option(&mut self, name: &[u8], on: bool) -> bool {
        let mut key = option_key(name);
        let mut on = on;
        if let Some(rest) = key.strip_prefix("no")
            && !matches!(key.as_str(), "notify" | "nomatch")
        {
            key = rest.to_owned();
            on = !on;
        }
        let _old = self.options.insert(key, on);
        true
    }

    /// The value of parameter `name`, including the special ones.
    pub(crate) fn get(&self, name: &[u8]) -> Option<Value> {
        match name {
            b"?" => return Some(Value::Scalar(self.status.to_string().into_bytes())),
            b"$" => return Some(Value::Scalar(self.pid.to_string().into_bytes())),
            b"#" | b"ARGC" => {
                return Some(Value::Scalar(
                    self.positional.len().to_string().into_bytes(),
                ));
            }
            b"@" | b"*" | b"argv" => return Some(Value::Array(self.positional.clone())),
            b"0" => return Some(Value::Scalar(self.argzero.clone())),
            b"!" => return Some(Value::Scalar(self.last_bg.to_string().into_bytes())),
            b"LINENO" => return Some(Value::Scalar(self.lineno.to_string().into_bytes())),
            b"RANDOM" => {
                let mut b = [0u8; 2];
                // SAFETY: the buffer is two writable bytes.
                let _n = unsafe { libc::getrandom(b.as_mut_ptr().cast(), 2, 0) };
                return Some(Value::Scalar(
                    (u16::from_ne_bytes(b) & 0x7fff).to_string().into_bytes(),
                ));
            }
            b"EPOCHSECONDS" => {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                return Some(Value::Scalar(secs.to_string().into_bytes()));
            }
            b"-" => {
                return Some(Value::Scalar(if self.interactive {
                    b"i".to_vec()
                } else {
                    Vec::new()
                }));
            }
            _ => {}
        }
        if let Some(n) = std::str::from_utf8(name)
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        {
            return n
                .checked_sub(1)
                .and_then(|i| self.positional.get(i))
                .cloned()
                .map(Value::Scalar);
        }
        if name == b"path" {
            let p = self.vars.get(&b"PATH"[..])?.value.joined();
            return Some(Value::Array(
                p.split(|&c| c == b':').map(<[u8]>::to_vec).collect(),
            ));
        }
        self.vars.get(name).map(|v| v.value.clone())
    }

    /// Set a scalar, keeping the parameter's attributes.
    pub(crate) fn set_scalar(&mut self, name: &[u8], value: Vec<u8>) {
        self.set_value(name, Value::Scalar(value));
    }

    /// Set any value, keeping attributes; `path` and `PATH` stay tied.
    pub(crate) fn set_value(&mut self, name: &[u8], value: Value) {
        if name == b"path" {
            let joined = match &value {
                Value::Array(a) => a.join(&b':'),
                other => other.joined(),
            };
            self.set_scalar(b"PATH", joined);
            return;
        }
        if let Some(n) = std::str::from_utf8(name)
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        {
            if n == 0 {
                self.argzero = value.joined();
            } else {
                while self.positional.len() < n {
                    self.positional.push(Vec::new());
                }
                if let Some(slot) = self.positional.get_mut(n - 1) {
                    *slot = value.joined();
                }
            }
            return;
        }
        if name == b"argv" {
            if let Value::Array(a) = value {
                self.positional = a;
            }
            return;
        }
        match self.vars.get_mut(name) {
            Some(v) => {
                v.value = if v.integer {
                    Value::Scalar(value.joined())
                } else {
                    value
                };
            }
            None => {
                let export = self.opt("allexport");
                let mut v = Var::scalar(Vec::new());
                v.value = value;
                v.export = export;
                let _old = self.vars.insert(name.to_vec(), v);
            }
        }
    }

    /// Remove a parameter.
    pub(crate) fn unset(&mut self, name: &[u8]) {
        let _old = self.vars.remove(name);
    }

    /// Make `name` local to the current function, saving its old value.
    pub(crate) fn make_local(&mut self, name: &[u8]) {
        let Some(frame) = self.locals.last_mut() else {
            return;
        };
        if frame.iter().any(|(n, _)| n == name) {
            return;
        }
        let old = self.vars.remove(name);
        let export = old.as_ref().is_some_and(|v| v.export);
        frame.push((name.to_vec(), old));
        let mut v = Var::scalar(Vec::new());
        v.export = export;
        let _prev = self.vars.insert(name.to_vec(), v);
    }

    /// Enter a function scope.
    pub(crate) fn push_scope(&mut self) {
        self.locals.push(Vec::new());
    }

    /// Leave a function scope, restoring what its locals replaced.
    pub(crate) fn pop_scope(&mut self) {
        if let Some(frame) = self.locals.pop() {
            for (name, old) in frame.into_iter().rev() {
                match old {
                    Some(v) => {
                        let _prev = self.vars.insert(name, v);
                    }
                    None => {
                        let _prev = self.vars.remove(&name);
                    }
                }
            }
        }
    }

    /// The environment for a child: exported parameters, unmetafied.
    pub(crate) fn environ(&self) -> Vec<Vec<u8>> {
        self.vars
            .iter()
            .filter(|(_, v)| v.export)
            .map(|(k, v)| {
                let mut e = crate::tok::unmetafy(k);
                e.push(b'=');
                e.extend_from_slice(&crate::tok::unmetafy(&v.value.joined()));
                e
            })
            .collect()
    }

    /// Print an error the way zsh does: `name: message`.
    pub(crate) fn error(&self, msg: &str) {
        use std::io::Write;
        let line = if self.interactive || self.lineno == 0 {
            format!("{}: {msg}\n", self.name)
        } else {
            format!("{}:{}: {msg}\n", self.name, self.lineno)
        };
        let _ignored = std::io::stderr().write_all(line.as_bytes());
    }

    /// Lexer options from the shell options.
    pub(crate) fn lex_opts(&self) -> LexOpts {
        LexOpts {
            shglob: self.opt("shglob"),
            kshglob: self.opt("kshglob"),
            ignorebraces: self.opt("ignorebraces"),
            ignoreclosebraces: self.opt("ignoreclosebraces"),
            comments: !self.at_prompt || self.opt("interactivecomments"),
            rcquotes: self.opt("rcquotes"),
            cshjunkiequotes: self.opt("cshjunkiequotes"),
            cshjunkieloops: self.opt("cshjunkieloops"),
            aliases: self.opt("aliases"),
            posixaliases: self.opt("posixaliases"),
            shortloops: self.opt("shortloops"),
            shortrepeat: self.opt("shortrepeat"),
            multifuncdef: self.opt("multifuncdef"),
            aliasfuncdef: self.opt("aliasfuncdef"),
            execopt: self.opt("exec"),
        }
    }
}

impl LexEnv for Shell {
    fn alias(&self, name: &[u8]) -> Option<AliasDef> {
        self.aliases.get(name).cloned()
    }
    fn suffix_alias(&self, ext: &[u8]) -> Option<AliasDef> {
        self.suffix_aliases.get(ext).cloned()
    }
    fn opts(&self) -> LexOpts {
        self.lex_opts()
    }
}
