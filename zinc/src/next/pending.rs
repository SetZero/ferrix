//! Stand-ins for the modules of zinc-next that have not landed yet.
//!
//! zinc-next lands in slices, and every module hangs off `Shell`, so each
//! slice carries the types and functions of the later slices it calls, under
//! the module names the real code will have. The types are the real
//! definitions; the functions do nothing useful. Each slice deletes what it
//! lands from here, and the last slice deletes the file.

/// `builtin.c`: option parsing, the builtin type and its flags.
pub(crate) mod builtin {
    use crate::shell::Shell;

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

    impl Shell {
        /// zsh's `createbuiltintable`: no builtin has landed.
        pub(crate) fn createbuiltintable(&mut self) {}

        pub(crate) fn execbuiltin(
            &mut self,
            _name: &[u8],
            _args: &mut Vec<Vec<u8>>,
            _assigns: &mut Vec<Asgment>,
            _bn: &Builtin,
        ) -> i32 {
            1
        }

        pub(crate) fn bin_command_whence(&mut self, _args: &mut Vec<Vec<u8>>) -> i32 {
            1
        }
    }
}

/// `builtin.c`: the directory builtins.
pub(crate) mod builtin_dirs {
    use crate::shell::Shell;

    impl Shell {
        pub(crate) fn cd_able_vars(&mut self, _s: &[u8]) -> Option<Vec<u8>> {
            None
        }
    }
}

/// `prompt.c`: the colour sequences `Shell` keeps, and prompt expansion.
pub(crate) mod prompt {
    use crate::shell::Shell;

    /// One entry of zsh's `fg_bg_sequences`.
    #[derive(Debug, Clone)]
    pub(crate) struct ColourSeq {
        pub(crate) start: Vec<u8>,
        pub(crate) end: Vec<u8>,
        pub(crate) def: Vec<u8>,
    }

    /// zsh's `set_default_colour_sequences`.
    pub(crate) fn default_colour_sequences() -> [ColourSeq; 2] {
        [
            ColourSeq {
                start: b"\x1b[3".to_vec(),
                end: b"m".to_vec(),
                def: b"9".to_vec(),
            },
            ColourSeq {
                start: b"\x1b[4".to_vec(),
                end: b"m".to_vec(),
                def: b"9".to_vec(),
            },
        ]
    }

    impl Shell {
        pub(crate) fn promptexpand(
            &mut self,
            s: &[u8],
            _ns: bool,
            _rs: Option<&[u8]>,
            _rs2: Option<&[u8]>,
        ) -> (Vec<u8>, u64) {
            (s.to_vec(), 0)
        }

        pub(crate) fn printprompt4(&mut self) {}
    }
}

/// `init.c`: sourcing files, the terminal, and checking jobs before exit.
pub(crate) mod init {
    use crate::shell::Shell;

    /// zsh's `enum source_return`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum SourceReturn {
        Ok,
        NotFound,
        Error,
    }

    impl Shell {
        pub(crate) fn checkjobs(&mut self) {}

        pub(crate) fn source(&mut self, _s: &[u8]) -> SourceReturn {
            SourceReturn::NotFound
        }

        pub(crate) fn sourcehome(&mut self, _s: &[u8]) {}

        pub(crate) fn init_io_stdin(&mut self) {}
    }
}
