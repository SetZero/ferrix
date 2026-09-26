//! Builtin commands (zsh's `builtin.c`): the flags and the parsed options
//! every builtin receives. The builtins themselves are ported into this
//! module one family at a time.

use crate::options::XTRACE;
use crate::shell::{ERRFLAG_ERROR, Shell, write_fd};
use crate::utils::lossy;

pub(crate) const BINF_PLUSOPTS: u32 = 1 << 1;
pub(crate) const BINF_PRINTOPTS: u32 = 1 << 2;
pub(crate) const BINF_ADDED: u32 = 1 << 3;
pub(crate) const BINF_MAGICEQUALS: u32 = 1 << 4;
pub(crate) const BINF_PREFIX: u32 = 1 << 5;
pub(crate) const BINF_DASH: u32 = 1 << 6;
pub(crate) const BINF_BUILTIN: u32 = 1 << 7;
pub(crate) const BINF_COMMAND: u32 = 1 << 8;
pub(crate) const BINF_EXEC: u32 = 1 << 9;
pub(crate) const BINF_NOGLOB: u32 = 1 << 10;
pub(crate) const BINF_PSPECIAL: u32 = 1 << 11;
pub(crate) const BINF_SKIPINVALID: u32 = 1 << 12;
pub(crate) const BINF_KEEPNUM: u32 = 1 << 13;
pub(crate) const BINF_SKIPDASH: u32 = 1 << 14;
pub(crate) const BINF_DASHDASHVALID: u32 = 1 << 15;
pub(crate) const BINF_CLEARENV: u32 = 1 << 16;
pub(crate) const BINF_AUTOALL: u32 = 1 << 17;
pub(crate) const BINF_HANDLES_OPTS: u32 = 1 << 18;
pub(crate) const BINF_ASSIGN: u32 = 1 << 19;

/// `MAX_OPS`: one slot per byte an option letter can be.
const MAX_OPS: usize = 128;

/// The options a builtin was given (zsh's `struct options`).
#[derive(Debug, Clone)]
pub(crate) struct Options {
    /// Per letter: 0 unset, 1 `-x`, 2 `+x`, and `(n + 1) << 2` plus that for
    /// an option whose argument is `args[n]`.
    pub(crate) ind: [u8; MAX_OPS],
    pub(crate) args: Vec<Vec<u8>>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            ind: [0; MAX_OPS],
            args: Vec::new(),
        }
    }
}

impl Options {
    /// `OPT_ISSET`.
    pub(crate) fn isset(&self, c: u8) -> bool {
        self.ind.get(usize::from(c)).is_some_and(|&v| v != 0)
    }

    /// `OPT_MINUS`.
    pub(crate) fn minus(&self, c: u8) -> bool {
        self.ind.get(usize::from(c)).is_some_and(|&v| v & 1 != 0)
    }

    /// `OPT_PLUS`.
    pub(crate) fn plus(&self, c: u8) -> bool {
        self.ind.get(usize::from(c)).is_some_and(|&v| v & 2 != 0)
    }

    /// `OPT_HASARG`.
    pub(crate) fn hasarg(&self, c: u8) -> bool {
        self.ind.get(usize::from(c)).is_some_and(|&v| v > 3)
    }

    /// `OPT_ARG_SAFE`.
    pub(crate) fn arg(&self, c: u8) -> Option<&[u8]> {
        let v = *self.ind.get(usize::from(c))?;
        if v <= 3 {
            return None;
        }
        self.args
            .get(usize::from(v >> 2).checked_sub(1)?)
            .map(Vec::as_slice)
    }
}

/// `ASG_ARRAY`: an assignment's value is an array.
pub(crate) const ASG_ARRAY: i32 = 1;
/// `ASG_KEY_VALUE`: an array assignment in `[key]=value` form.
pub(crate) const ASG_KEY_VALUE: i32 = 2;

/// An assignment among a typeset-family builtin's arguments (zsh's
/// `struct asgment`).
#[derive(Debug, Clone)]
pub(crate) struct Asgment {
    pub(crate) name: Vec<u8>,
    pub(crate) flags: i32,
    pub(crate) scalar: Option<Vec<u8>>,
    pub(crate) array: Vec<Vec<u8>>,
}

/// A builtin's implementation: name, arguments, options, function id.
pub(crate) type HandlerFunc = fn(&mut Shell, &[u8], Vec<Vec<u8>>, &Options, i32) -> i32;

/// A typeset-family builtin's implementation, which also takes assignments.
pub(crate) type AssignFunc =
    fn(&mut Shell, &[u8], Vec<Vec<u8>>, Vec<Asgment>, &Options, i32) -> i32;

/// How a builtin is run.
#[derive(Clone, Copy)]
pub(crate) enum Handler {
    Plain(HandlerFunc),
    Assign(AssignFunc),
    /// A precommand modifier (`command`, `exec`, `noglob`, `-`, `builtin`).
    Prefix,
}

impl std::fmt::Debug for Handler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Handler::Plain(_) => "Plain",
            Handler::Assign(_) => "Assign",
            Handler::Prefix => "Prefix",
        })
    }
}

/// A builtin (zsh's `struct builtin`).
#[derive(Debug, Clone)]
pub(crate) struct Builtin {
    pub(crate) flags: u32,
    pub(crate) handler: Handler,
    pub(crate) minargs: i32,
    pub(crate) maxargs: i32,
    pub(crate) funcid: i32,
    pub(crate) optstr: Option<&'static [u8]>,
    pub(crate) defopts: Option<&'static [u8]>,
}

// The function ids zsh passes to handlers that serve several builtins.
pub(crate) const BIN_TYPESET: i32 = 0;
pub(crate) const BIN_BG: i32 = 1;
pub(crate) const BIN_FG: i32 = 2;
pub(crate) const BIN_JOBS: i32 = 3;
pub(crate) const BIN_WAIT: i32 = 4;
pub(crate) const BIN_DISOWN: i32 = 5;
pub(crate) const BIN_BREAK: i32 = 6;
pub(crate) const BIN_CONTINUE: i32 = 7;
pub(crate) const BIN_EXIT: i32 = 8;
pub(crate) const BIN_RETURN: i32 = 9;
pub(crate) const BIN_CD: i32 = 10;
pub(crate) const BIN_POPD: i32 = 11;
pub(crate) const BIN_PUSHD: i32 = 12;
pub(crate) const BIN_PRINT: i32 = 13;
pub(crate) const BIN_EVAL: i32 = 14;
pub(crate) const BIN_SCHED: i32 = 15;
pub(crate) const BIN_FC: i32 = 16;
pub(crate) const BIN_R: i32 = 17;
pub(crate) const BIN_PUSHLINE: i32 = 18;
pub(crate) const BIN_LOGOUT: i32 = 19;
pub(crate) const BIN_TEST: i32 = 20;
pub(crate) const BIN_BRACKET: i32 = 21;
pub(crate) const BIN_READONLY: i32 = 22;
pub(crate) const BIN_ECHO: i32 = 23;
pub(crate) const BIN_DISABLE: i32 = 24;
pub(crate) const BIN_ENABLE: i32 = 25;
pub(crate) const BIN_PRINTF: i32 = 26;
pub(crate) const BIN_COMMAND: i32 = 27;
pub(crate) const BIN_UNHASH: i32 = 28;
pub(crate) const BIN_UNALIAS: i32 = 29;
pub(crate) const BIN_UNFUNCTION: i32 = 30;
pub(crate) const BIN_UNSET: i32 = 31;
pub(crate) const BIN_EXPORT: i32 = 32;

/// `BUILTIN(name, flags, handler, min, max, funcid, optstr, NULL)`.
const fn builtin(
    flags: u32,
    handler: HandlerFunc,
    minargs: i32,
    maxargs: i32,
    funcid: i32,
    optstr: Option<&'static [u8]>,
) -> Builtin {
    Builtin {
        flags,
        handler: Handler::Plain(handler),
        minargs,
        maxargs,
        funcid,
        optstr,
        defopts: None,
    }
}

/// `BIN_PREFIX(name, flags)`.
const fn prefix(flags: u32) -> Builtin {
    Builtin {
        flags: flags | BINF_PREFIX,
        handler: Handler::Prefix,
        minargs: 0,
        maxargs: 0,
        funcid: 0,
        optstr: None,
        defopts: None,
    }
}

const CD_FLAGS: u32 = BINF_SKIPINVALID | BINF_SKIPDASH | BINF_DASHDASHVALID;

/// zsh's `builtins[]`, as far as the handlers are ported.
fn builtins() -> Vec<(&'static [u8], Builtin)> {
    vec![
        (b"-", prefix(BINF_DASH)),
        (b"builtin", prefix(BINF_BUILTIN)),
        (b"command", prefix(BINF_COMMAND)),
        (b"exec", prefix(BINF_EXEC)),
        (b"noglob", prefix(BINF_NOGLOB)),
        (
            b"cd",
            builtin(CD_FLAGS, Shell::bin_cd, 0, 2, BIN_CD, Some(b"qsPL")),
        ),
        (
            b"chdir",
            builtin(CD_FLAGS, Shell::bin_cd, 0, 2, BIN_CD, Some(b"qsPL")),
        ),
        (
            b"dirs",
            builtin(0, Shell::bin_dirs, 0, -1, 0, Some(b"clpv")),
        ),
        (
            b"popd",
            builtin(CD_FLAGS, Shell::bin_cd, 0, 1, BIN_POPD, Some(b"q")),
        ),
        (
            b"pushd",
            builtin(CD_FLAGS, Shell::bin_cd, 0, 2, BIN_PUSHD, Some(b"qsPL")),
        ),
        (b"pwd", builtin(0, Shell::bin_pwd, 0, 0, 0, Some(b"rLP"))),
    ]
}

impl Shell {
    /// zsh's `createbuiltintable`.
    pub(crate) fn createbuiltintable(&mut self) {
        for (name, b) in builtins() {
            let _ = self.builtintab.insert(name.to_vec(), b);
        }
    }

    /// zsh's `execbuiltin`: `args` holds the command name, then its words.
    #[expect(clippy::too_many_lines, reason = "one procedure in zsh")]
    pub(crate) fn execbuiltin(
        &mut self,
        _name: &[u8],
        args: &mut Vec<Vec<u8>>,
        assigns: &mut Vec<Asgment>,
        bn: &Builtin,
    ) -> i32 {
        let xtr = self.isset(XTRACE);
        let mut ops = Options::default();
        let mut words = std::mem::take(args).into_iter();
        let name = words.next().unwrap_or_default();
        let namestr = lossy(&name);
        let argarr: Vec<Vec<u8>> = words.collect();
        if matches!(bn.handler, Handler::Prefix) {
            let _ = self.builtintab.remove(&name);
            return 1;
        }
        let mut flags = bn.flags;
        let mut optstr = bn.optstr;
        let mut ai = 0usize;
        let set = |ops: &mut Options, c: u8, v: u8| {
            if let Some(slot) = ops.ind.get_mut(usize::from(c)) {
                *slot = v;
            }
        };
        let get = |ops: &Options, c: u8| ops.ind.get(usize::from(c)).copied().unwrap_or(0);
        if optstr.is_some() {
            while let Some(word) = argarr.get(ai) {
                let sense = word.first() == Some(&b'-');
                if !(sense || (flags & BINF_PLUSOPTS != 0 && word.first() == Some(&b'+'))) {
                    break;
                }
                let w1 = word.get(1).copied().unwrap_or(0);
                if flags & BINF_KEEPNUM == 0 && self.idigit(w1) {
                    break;
                }
                if flags & BINF_SKIPDASH != 0 && w1 == 0 {
                    break;
                }
                if flags & BINF_DASHDASHVALID != 0 && word.as_slice() == b"--" {
                    ai += 1;
                    break;
                }
                let os = optstr.unwrap_or(b"");
                if flags & BINF_SKIPINVALID != 0 && word.iter().skip(1).any(|c| !os.contains(c)) {
                    break;
                }
                let mut cur = word.clone();
                let mut p = usize::from(w1 == b'-');
                if cur.get(p + 1).is_none() {
                    set(&mut ops, b'-', 1);
                    if !sense {
                        set(&mut ops, b'+', 1);
                    }
                }
                loop {
                    p += 1;
                    let Some(&c) = cur.get(p) else { break };
                    let Some(op) = os.iter().position(|&o| o == c) else {
                        break;
                    };
                    set(&mut ops, c, if sense { 1 } else { 2 });
                    if os.get(op + 1) != Some(&b':') {
                        continue;
                    }
                    let mut argptr: Option<Vec<u8>> = None;
                    match os.get(op + 2) {
                        Some(b':') => {
                            if cur.get(p + 1).is_some() {
                                argptr = cur.get(p + 1..).map(<[u8]>::to_vec);
                            }
                        }
                        Some(b'%') => {
                            if cur.get(p + 1).is_some_and(|&d| self.idigit(d)) {
                                argptr = cur.get(p + 1..).map(<[u8]>::to_vec);
                            } else if let Some(next) = argarr
                                .get(ai + 1)
                                .filter(|w| self.idigit(w.first().copied().unwrap_or(0)))
                            {
                                ai += 1;
                                cur = next.clone();
                                argptr = Some(cur.clone());
                            }
                        }
                        _ => {
                            if cur.get(p + 1).is_some() {
                                argptr = cur.get(p + 1..).map(<[u8]>::to_vec);
                            } else if let Some(next) = argarr.get(ai + 1) {
                                ai += 1;
                                cur = next.clone();
                                argptr = Some(cur.clone());
                            } else {
                                self.zwarnnam(
                                    &namestr,
                                    &format!("argument expected: -{}", lossy(&[c])),
                                );
                                return 1;
                            }
                        }
                    }
                    if let Some(a) = argptr {
                        if ops.args.len() == 63 {
                            self.zwarnnam(&namestr, "too many option arguments");
                            return 1;
                        }
                        ops.args.push(a);
                        let n = u8::try_from(ops.args.len() << 2).unwrap_or(0);
                        let v = get(&ops, c) | n;
                        set(&mut ops, c, v);
                        p = cur.len().saturating_sub(1);
                    }
                }
                if let Some(&c) = cur.get(p) {
                    let ch = if c == crate::tok::META {
                        cur.get(p + 1).copied().unwrap_or(0) ^ 32
                    } else {
                        c
                    };
                    self.zwarnnam(
                        &namestr,
                        &format!(
                            "bad option: {}{}",
                            if sense { '-' } else { '+' },
                            lossy(&[ch])
                        ),
                    );
                    return 1;
                }
                ai += 1;
                if flags & BINF_PRINTOPTS != 0 && get(&ops, b'R') != 0 && get(&ops, b'f') == 0 {
                    optstr = Some(b"ne");
                    flags |= BINF_SKIPINVALID;
                }
                if get(&ops, b'-') != 0 {
                    break;
                }
            }
        } else if flags & BINF_HANDLES_OPTS == 0
            && argarr.first().is_some_and(|a| a.as_slice() == b"--")
        {
            set(&mut ops, b'-', 1);
            ai += 1;
        }
        if let Some(defopts) = bn.defopts {
            for &c in defopts {
                if get(&ops, c) == 0 {
                    set(&mut ops, c, 1);
                }
            }
        }
        let argc = i32::try_from(argarr.len().saturating_sub(ai)).unwrap_or(i32::MAX);
        if self.errflag() {
            self.errflag.set(self.errflag.get() & !ERRFLAG_ERROR);
            return 1;
        }
        if argc < bn.minargs || (argc > bn.maxargs && bn.maxargs != -1) {
            self.zwarnnam(
                &namestr,
                if argc < bn.minargs {
                    "not enough arguments"
                } else {
                    "too many arguments"
                },
            );
            return 1;
        }
        if xtr {
            self.printprompt4();
            let fd = self.xtrerr_fd();
            let mut out = crate::tok::unmetafy(&name);
            for a in &argarr {
                out.push(b' ');
                out.extend(self.quotedzputs(a));
            }
            for asg in assigns.iter() {
                out.push(b' ');
                out.extend(self.quotedzputs(&asg.name));
                if asg.flags & ASG_ARRAY != 0 {
                    out.extend_from_slice(b"=(");
                    if asg.flags & ASG_KEY_VALUE != 0 {
                        for pair in asg.array.chunks_exact(2) {
                            let [k, v] = pair else { continue };
                            out.push(b'[');
                            out.extend(self.quotedzputs(k));
                            // zsh writes this piece to stderr, not xtrerr.
                            write_fd(fd, &out);
                            out.clear();
                            write_fd(2, b"]=");
                            out.extend(self.quotedzputs(v));
                        }
                    } else {
                        for a in &asg.array {
                            out.push(b' ');
                            out.extend(self.quotedzputs(a));
                        }
                    }
                    out.extend_from_slice(b" )");
                } else if let Some(s) = &asg.scalar {
                    out.push(b'=');
                    out.extend(self.quotedzputs(s));
                }
            }
            out.push(b'\n');
            write_fd(fd, &out);
        }
        let argv = argarr.get(ai..).unwrap_or(&[]).to_vec();
        match bn.handler {
            Handler::Assign(f) => f(self, &name, argv, std::mem::take(assigns), &ops, bn.funcid),
            Handler::Plain(f) => f(self, &name, argv, &ops, bn.funcid),
            Handler::Prefix => 1,
        }
    }

    /// `command -v`/`-V`: zsh's `commandbn`, `whence` with `command`'s
    /// options.
    pub(crate) fn bin_command_whence(&mut self, args: &mut Vec<Vec<u8>>) -> i32 {
        let bn = builtin(0, Shell::bin_whence, 0, -1, BIN_COMMAND, Some(b"pvV"));
        self.execbuiltin(b"command", args, &mut Vec::new(), &bn)
    }

    /// zsh's `bin_whence`: not ported yet.
    pub(crate) fn bin_whence(
        &mut self,
        name: &[u8],
        _argv: Vec<Vec<u8>>,
        _ops: &Options,
        _func: i32,
    ) -> i32 {
        self.zwarnnam(&lossy(name), "not yet available in zinc-next");
        1
    }
}
