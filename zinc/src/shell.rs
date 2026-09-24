//! The shell's state: parameters, scopes, functions, aliases, options.

use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::List;
use crate::lex::{AliasDef, LexEnv, LexOpts};

/// FNV-1a, the hash of the tables the shell looks a name up in on nearly
/// every word it expands: parameters, functions, options.
///
/// std's default is SipHash, which is keyed so that a hostile peer cannot
/// choose keys that collide; nothing hostile chooses a shell's parameter
/// names, and SipHash on names this short cost more than the look-up it
/// served -- `Shell::opt` alone was a tenth of oh-my-zsh's start-up.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl std::hash::Hasher for Fnv {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// A hash map keyed by [`Fnv`].
pub(crate) type Table<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<Fnv>>;

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
    /// The line the definition began on. An error inside the body counts its
    /// line from here, as zsh counts one.
    pub(crate) line: u64,
}

/// The whole state of the shell.
#[derive(Debug)]
pub(crate) struct Shell {
    pub(crate) vars: Table<Vec<u8>, Var>,
    /// One frame per function call: the values `local` replaced.
    pub(crate) locals: Vec<Vec<(Vec<u8>, Option<Var>)>>,
    pub(crate) functions: Table<Vec<u8>, Function>,
    /// Names `autoload` marked. zsh looks the file up along `fpath` when the
    /// function is first called, not when it is marked, so a later `fpath`
    /// still counts.
    pub(crate) autoloads: std::collections::HashSet<Vec<u8>>,
    /// The file each marked name was found in when `autoload` ran, read at
    /// the first call.
    pub(crate) autoload_files: HashMap<Vec<u8>, Vec<u8>>,
    pub(crate) aliases: HashMap<Vec<u8>, AliasDef>,
    pub(crate) suffix_aliases: HashMap<Vec<u8>, AliasDef>,
    pub(crate) positional: Vec<Vec<u8>>,
    pub(crate) argzero: Vec<u8>,
    /// The name used in messages (`zsh:`, `sh:`).
    pub(crate) name: String,
    pub(crate) status: i32,
    pub(crate) flow: Flow,
    pub(crate) loop_depth: u32,
    pub(crate) options: Table<String, bool>,
    pub(crate) interactive: bool,
    pub(crate) pid: i32,
    pub(crate) last_bg: i32,
    /// Commands for `trap ... EXIT`.
    pub(crate) exit_trap: Option<Vec<u8>>,
    /// Inside a forked child: `exit` must not run the EXIT trap twice.
    pub(crate) subshell: bool,
    /// Line number of the command being run.
    pub(crate) lineno: u64,
    /// The line an error counts from: the first line of the file, or of the
    /// body of the function being run.
    pub(crate) line_base: u64,
    /// The jobs this shell started, and its side of the terminal.
    pub(crate) jobs: crate::jobs::Jobs,
    /// The job being built: the pipeline whose processes are being forked
    /// now. Every fork made while it is set joins that job's process group
    /// and is waited for by the pipeline rather than by the command.
    pub(crate) building: Option<crate::jobs::JobBuild>,
    /// The text of the line being run, which names a job that is not a
    /// simple command -- `for ... done &`, a subshell.
    pub(crate) line_text: Vec<u8>,
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
    /// The file being run, which an error names in place of the shell: what
    /// zsh prints as `/path/to/file:12: ...`. Empty while the shell's own
    /// input is being run.
    pub(crate) script: Vec<u8>,
    /// The shell's ends of the pipes `<(...)` and `>(...)` in the command
    /// being run opened, closed once it has finished with them.
    pub(crate) procsubs: Vec<i32>,
    /// `zstyle`'s database: a style name and the patterns defined for it.
    pub(crate) styles: Vec<crate::zstyle::Style>,
}

/// The id `name` asks for, read from the kernel each time rather than kept:
/// a set-user-id program starts with an effective id that is not its real
/// one, and `%#` and oh-my-zsh's prompts ask which.
fn id_of(name: &[u8]) -> Vec<u8> {
    let id = match name {
        // SAFETY: getuid has no preconditions.
        b"UID" => unsafe { libc::getuid() },
        // SAFETY: geteuid has no preconditions.
        b"EUID" => unsafe { libc::geteuid() },
        // SAFETY: getgid has no preconditions.
        b"GID" => unsafe { libc::getgid() },
        // SAFETY: getegid has no preconditions.
        _ => unsafe { libc::getegid() },
    };
    id.to_string().into_bytes()
}

/// The host zsh function tree, in the same pre-order as zsh 5.9's compiled
/// `fpath`. A Ferrix image without that tree simply starts with an empty path;
/// distributors can provide the functions without rebuilding zinc.
fn default_fpath() -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = [
        "/usr/local/share/zsh/site-functions",
        "/usr/share/zsh/vendor-functions",
        "/usr/share/zsh/vendor-completions",
    ]
    .into_iter()
    .filter(|path| std::path::Path::new(path).is_dir())
    .map(|path| path.as_bytes().to_vec())
    .collect();
    collect_fpath_dirs(std::path::Path::new("/usr/share/zsh/functions"), &mut out);
    out
}

fn collect_fpath_dirs(root: &std::path::Path, out: &mut Vec<Vec<u8>>) {
    use std::os::unix::ffi::OsStrExt;

    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut dirs: Vec<std::path::PathBuf> = entries
        .flatten()
        .filter_map(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir())
                .then(|| entry.path())
        })
        .collect();
    dirs.sort();
    for dir in dirs {
        out.push(dir.as_os_str().as_bytes().to_vec());
        collect_fpath_dirs(&dir, out);
    }
}

/// The one name a prompt parameter is stored under.
///
/// zsh's `PS` prompts answer to a second name each -- `PS1` is `PROMPT` and
/// `prompt`, `PS2` is `PROMPT2` -- and they are one parameter under both,
/// not two kept in step. A theme that sets `PROMPT`, as oh-my-zsh's do, has
/// set the prompt the shell prints.
///
/// The right-hand prompts are *not* in this: `RPROMPT` and `RPS1` really are
/// two parameters in zsh, which reads whichever is set, so tying them here
/// would be inventing a rule zsh does not have.
pub(crate) fn prompt_name(name: &[u8]) -> &[u8] {
    match name {
        b"PROMPT" | b"prompt" => b"PS1",
        b"PROMPT2" => b"PS2",
        b"PROMPT3" => b"PS3",
        b"PROMPT4" => b"PS4",
        other => other,
    }
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
        let mut vars = Table::default();
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
            functions: Table::default(),
            autoloads: std::collections::HashSet::new(),
            autoload_files: HashMap::new(),
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
            line_base: 0,
            jobs: crate::jobs::Jobs::new(),
            building: None,
            line_text: Vec::new(),
            source_depth: 0,
            optpos: 1,
            subst_status: None,
            at_prompt: false,
            script: Vec::new(),
            procsubs: Vec::new(),
            styles: Vec::new(),
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
        if !sh.vars.contains_key(&b"fpath"[..]) {
            let fpath = match sh.vars.get(&b"FPATH"[..]) {
                Some(value) => value
                    .value
                    .joined()
                    .split(|&c| c == b':')
                    .map(<[u8]>::to_vec)
                    .collect(),
                None => default_fpath(),
            };
            sh.set_value(b"fpath", Value::Array(fpath));
        }
        // These are always-present integer special parameters in zsh.  OMZ's
        // history setup compares their defaults before raising them.
        for (name, value) in [
            (b"HISTSIZE".as_slice(), b"30".as_slice()),
            (b"SAVEHIST", b"0"),
            (b"OPTIND", b"1"),
        ] {
            if !sh.vars.contains_key(name) {
                let _old = sh.vars.insert(
                    name.to_vec(),
                    Var {
                        value: Value::Scalar(value.to_vec()),
                        export: false,
                        readonly: false,
                        integer: true,
                    },
                );
            }
        }
        // zsh sets USERNAME from the password database at startup, whatever
        // the environment says; a theme reads it to decide whether the user
        // is worth naming in the prompt, so a shell started with no
        // environment still has one.
        let name = crate::prompt::username();
        if !name.is_empty() {
            sh.set_scalar(b"USERNAME", name.clone());
            // LOGNAME comes from the same place when the environment has
            // none of its own. Themes compare the two to tell whether the
            // user has become somebody else, and two names that disagree
            // only because one is missing read as exactly that.
            if !sh.vars.contains_key(&b"LOGNAME"[..]) {
                sh.set_scalar(b"LOGNAME", name);
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

    /// `$commands`: every external command on `$PATH`, by the name that
    /// finds it. `(( $+commands[git] ))` is how a script asks whether a
    /// program is installed, and oh-my-zsh asks it constantly.
    ///
    /// zsh keeps this as a hash it fills in as commands are looked up; here
    /// it is read from the directories each time, so the answer is never a
    /// stale one. The first name found wins, as `$PATH` order says.
    fn commands(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        use std::os::unix::ffi::OsStrExt;
        let path = self.get(b"PATH").map_or_else(
            || b"/bin:/usr/bin".to_vec(),
            |v| crate::tok::unmetafy(&v.joined()),
        );
        let mut seen: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
        let mut out = Vec::new();
        for dir in path.split(|&c| c == b':') {
            let d = if dir.is_empty() { &b"."[..] } else { dir };
            let Ok(rd) = std::fs::read_dir(std::ffi::OsStr::from_bytes(d)) else {
                continue;
            };
            for ent in rd.flatten() {
                let name = ent.file_name();
                let name = name.as_bytes().to_vec();
                if seen.contains(&name) {
                    continue;
                }
                let mut full = d.to_vec();
                full.push(b'/');
                full.extend_from_slice(&name);
                if crate::exec::is_executable(&full) {
                    let _new = seen.insert(name.clone());
                    out.push((name, full));
                }
            }
        }
        out
    }

    /// `$commands[name]`: where `$PATH` finds the program `name`, looked up
    /// on its own rather than by listing every directory as [`commands`]
    /// does. The answer is the one that table would give -- the first
    /// directory holding an executable file of that name -- at one probe a
    /// directory instead of one per program installed, which is the
    /// difference between a prompt that asks `(( $+commands[git] ))` costing
    /// a few system calls and costing a few hundred.
    ///
    /// [`commands`]: Shell::commands
    fn command_path(&self, name: &[u8]) -> Option<Vec<u8>> {
        // A directory listing never names anything with a slash in it, nor
        // anything empty, so neither is ever a key of the whole table.
        if name.is_empty() || name.contains(&b'/') {
            return None;
        }
        let path = self.get(b"PATH").map_or_else(
            || b"/bin:/usr/bin".to_vec(),
            |v| crate::tok::unmetafy(&v.joined()),
        );
        path.split(|&c| c == b':').find_map(|dir| {
            let mut full = if dir.is_empty() { &b"."[..] } else { dir }.to_vec();
            full.push(b'/');
            full.extend_from_slice(name);
            crate::exec::is_executable(&full).then_some(full)
        })
    }

    /// Whether `name` is a parameter [`get`] computes rather than one it
    /// reads out of the table: the ones [`stored`] must not answer for.
    ///
    /// [`get`]: Shell::get
    /// [`stored`]: Shell::stored
    pub(crate) fn is_special(name: &[u8]) -> bool {
        matches!(
            prompt_name(name),
            b"?" | b"$"
                | b"#"
                | b"ARGC"
                | b"@"
                | b"*"
                | b"argv"
                | b"0"
                | b"!"
                | b"LINENO"
                | b"RANDOM"
                | b"EPOCHSECONDS"
                | b"commands"
                | b"-"
                | b"UID"
                | b"EUID"
                | b"GID"
                | b"EGID"
                | b"aliases"
                | b"functions"
                | b"galiases"
                | b"path"
        ) || name.first().is_some_and(u8::is_ascii_digit)
    }

    /// The value of an ordinary parameter, where it is kept, for reading
    /// without the copy [`get`] makes. `None` for a special parameter, which
    /// has no place of its own ([`is_special`]), as well as for one unset.
    ///
    /// A copy of a hash of every completion function is what reading one of
    /// its elements cost when [`get`] was the only way in: compinit's
    /// `$_comps[$cmd]` copied the whole table on each look, and the cold
    /// start of a shell with oh-my-zsh spent most of its time copying.
    ///
    /// [`get`]: Shell::get
    /// [`is_special`]: Shell::is_special
    pub(crate) fn stored(&self, name: &[u8]) -> Option<&Value> {
        if Shell::is_special(name) {
            return None;
        }
        self.vars.get(prompt_name(name)).map(|v| &v.value)
    }

    /// Whether parameter `name` is set, without copying its value.
    pub(crate) fn is_set(&self, name: &[u8]) -> bool {
        if Shell::is_special(name) {
            return self.get(name).is_some();
        }
        self.vars.contains_key(prompt_name(name))
    }

    /// One element of a special hash, `key` in `$commands`, `$functions`,
    /// `$aliases` or `$galiases`, answered without building the whole hash
    /// as [`get`] does. The outer `None` is for a name that is not one of
    /// those; the inner one for a key it does not have.
    ///
    /// [`get`]: Shell::get
    pub(crate) fn special_element(&self, name: &[u8], key: &[u8]) -> Option<Option<Vec<u8>>> {
        Some(match name {
            b"commands" => self.command_path(key),
            b"functions" => {
                if self.functions.contains_key(key) {
                    Some(b"{ ... }".to_vec())
                } else if self.autoloads.contains(key) {
                    Some(b"builtin autoload -XU".to_vec())
                } else {
                    None
                }
            }
            b"aliases" | b"galiases" => {
                let global = name == b"galiases";
                self.aliases
                    .get(key)
                    .filter(|definition| definition.global == global)
                    .map(|definition| definition.text.clone())
            }
            _ => return None,
        })
    }

    /// The value of parameter `name`, including the special ones.
    pub(crate) fn get(&self, name: &[u8]) -> Option<Value> {
        let name = prompt_name(name);
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
            b"commands" => return Some(Value::Assoc(self.commands())),
            b"-" => {
                return Some(Value::Scalar(if self.interactive {
                    b"i".to_vec()
                } else {
                    Vec::new()
                }));
            }
            b"UID" | b"EUID" | b"GID" | b"EGID" => return Some(Value::Scalar(id_of(name))),
            b"aliases" => {
                let mut aliases: Vec<(Vec<u8>, Vec<u8>)> = self
                    .aliases
                    .iter()
                    .filter(|(_, definition)| !definition.global)
                    .map(|(name, definition)| (name.clone(), definition.text.clone()))
                    .collect();
                aliases.sort_by(|left, right| left.0.cmp(&right.0));
                return Some(Value::Assoc(aliases));
            }
            // `$functions`: every function the shell knows, defined or only
            // marked by `autoload`. `(( $+functions[VCS_INFO_detect_git] ))`
            // is how vcs_info asks whether a backend is there, and it counts
            // a marked autoload, whose body has not been read yet.
            //
            // zsh's value is the function's body, printed back from the tree
            // it parsed; zinc has no such printer yet, so a defined function
            // answers with the same `{ ... }` the `functions` builtin shows.
            // A name marked but not yet read answers exactly as zsh does.
            b"functions" => {
                let mut fns: Vec<(Vec<u8>, Vec<u8>)> = self
                    .functions
                    .keys()
                    .map(|name| (name.clone(), b"{ ... }".to_vec()))
                    .chain(
                        self.autoloads
                            .iter()
                            .filter(|name| !self.functions.contains_key(*name))
                            .map(|name| (name.clone(), b"builtin autoload -XU".to_vec())),
                    )
                    .collect();
                fns.sort_by(|left, right| left.0.cmp(&right.0));
                return Some(Value::Assoc(fns));
            }
            b"galiases" => {
                let mut aliases: Vec<(Vec<u8>, Vec<u8>)> = self
                    .aliases
                    .iter()
                    .filter(|(_, definition)| definition.global)
                    .map(|(name, definition)| (name.clone(), definition.text.clone()))
                    .collect();
                aliases.sort_by(|left, right| left.0.cmp(&right.0));
                return Some(Value::Assoc(aliases));
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
        let name = prompt_name(name);
        if name == b"path" {
            let joined = match &value {
                Value::Array(a) => a.join(&b':'),
                other => other.joined(),
            };
            self.set_scalar(b"PATH", joined);
            return;
        }
        if name == b"fpath" {
            let fpath = match value {
                Value::Array(a) => a,
                other => other
                    .joined()
                    .split(|&c| c == b':')
                    .map(<[u8]>::to_vec)
                    .collect(),
            };
            let joined = fpath.join(&b':');
            self.store_value(b"fpath", Value::Array(fpath));
            self.store_value(b"FPATH", Value::Scalar(joined));
            return;
        }
        if name == b"FPATH" {
            let joined = value.joined();
            let fpath = joined.split(|&c| c == b':').map(<[u8]>::to_vec).collect();
            self.store_value(b"FPATH", Value::Scalar(joined));
            self.store_value(b"fpath", Value::Array(fpath));
            return;
        }
        if matches!(name, b"aliases" | b"galiases") {
            let global = name == b"galiases";
            self.aliases
                .retain(|_, definition| definition.global != global);
            if let Value::Assoc(aliases) = value {
                for (name, text) in aliases {
                    let _old = self.aliases.insert(name, AliasDef { text, global });
                }
            }
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
        self.store_value(name, value);
    }

    fn store_value(&mut self, name: &[u8], value: Value) {
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
        let _old = self.vars.remove(prompt_name(name));
    }

    /// Make `name` local to the current function, saving its old value.
    pub(crate) fn make_local(&mut self, name: &[u8]) {
        if name == b"fpath" || name == b"FPATH" {
            self.make_one_local(b"fpath");
            self.make_one_local(b"FPATH");
            return;
        }
        self.make_one_local(name);
    }

    fn make_one_local(&mut self, name: &[u8]) {
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
        self.report(None, msg);
    }

    /// Print an error a builtin raised. zsh names the builtin between the
    /// file and the line: `zsh:kill:13: kill 1 failed: no such process`.
    pub(crate) fn error_at(&self, nam: &str, msg: &str) {
        self.report(Some(nam), msg);
    }

    fn report(&self, nam: Option<&str>, msg: &str) {
        use std::io::Write;
        // The file being run names the error, as zsh names it; the shell
        // itself names what it read from its own input.
        let mut who = if self.script.is_empty() {
            self.name.clone()
        } else {
            String::from_utf8_lossy(&crate::tok::unmetafy(&self.script)).into_owned()
        };
        if let Some(n) = nam {
            who.push(':');
            who.push_str(n);
        }
        let lineno = self.lineno.saturating_sub(self.line_base);
        let line = if lineno == 0 || (self.interactive && self.script.is_empty()) {
            format!("{who}: {msg}\n")
        } else {
            format!("{who}:{lineno}: {msg}\n")
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
