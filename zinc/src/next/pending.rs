//! Stand-ins for the modules of zinc-next that have not landed yet.
//!
//! zinc-next lands in slices, and every module hangs off `Shell`, so each
//! slice carries the types and functions of the later slices it calls, under
//! the module names the real code will have. The types are the real
//! definitions; the functions do nothing useful. Each slice deletes what it
//! lands from here, and the last slice deletes the file.

/// `builtin.c`: the builtin table.
pub(crate) mod builtin {
    use crate::shell::Shell;

    /// A builtin (zsh's `struct builtin`); its handler lands with the table.
    #[derive(Debug, Clone)]
    pub(crate) struct Builtin {
        pub(crate) flags: u32,
    }

    /// `MAX_OPS`: one slot per byte an option letter can be.
    const MAX_OPS: usize = 128;

    /// The options a builtin was given (zsh's `struct options`).
    #[derive(Debug, Clone)]
    pub(crate) struct Options {
        /// Per letter: 0 unset, 1 `-x`, 2 `+x`, and `(n + 1) << 2` plus that
        /// for an option whose argument is `args[n]`.
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

    impl Shell {
        /// zsh's `createbuiltintable`: no builtin has landed.
        pub(crate) fn createbuiltintable(&mut self) {}
    }
}

/// `exec.c`: the execution stacks and finding commands.
pub(crate) mod exec {
    use crate::shell::Shell;

    pub(crate) const FDT_UNUSED: u8 = 0;
    pub(crate) const FDT_INTERNAL: u8 = 1;
    pub(crate) const FDT_EXTERNAL: u8 = 2;
    pub(crate) const FDT_FLOCK: u8 = 5;
    pub(crate) const FDT_FLOCK_EXEC: u8 = 6;
    pub(crate) const FDT_PROC_SUBST: u8 = 7;

    pub(crate) const SFC_SIGNAL: i32 = 2;
    pub(crate) const SFC_HOOK: i32 = 3;
    pub(crate) const SFC_SUBST: i32 = 7;

    pub(crate) fn is_executable_file(_us: &[u8]) -> bool {
        false
    }

    pub(crate) fn isrelative(s: &[u8]) -> bool {
        s.first() != Some(&b'/')
    }

    impl Shell {
        pub(crate) fn execsave(&mut self) {}

        pub(crate) fn execrestore(&mut self) {}

        pub(crate) fn findcmd(
            &mut self,
            _arg0: &[u8],
            _docopy: bool,
            _default_path: bool,
        ) -> Option<Vec<u8>> {
            None
        }
    }

    /// One entry of `$funcstack` and friends (zsh's `struct funcstack`).
    #[derive(Debug, Clone)]
    pub(crate) struct Funcstack {
        pub(crate) name: Vec<u8>,
        pub(crate) filename: Option<Vec<u8>>,
        pub(crate) caller: Vec<u8>,
        pub(crate) flineno: i64,
        pub(crate) lineno: i64,
        pub(crate) tp: i32,
    }

    /// What `execsave` keeps around a trap (zsh's `struct execstack`).
    #[derive(Debug, Clone, Default)]
    pub(crate) struct ExecStack {
        pub(crate) list_pipe_pid: i32,
        pub(crate) nowait: bool,
        pub(crate) pline_level: i32,
        pub(crate) list_pipe_child: bool,
        pub(crate) list_pipe_job: i32,
        pub(crate) list_pipe_text: Vec<u8>,
        pub(crate) lastval: i32,
        pub(crate) noeval: i32,
        pub(crate) badcshglob: i32,
        pub(crate) cmdoutpid: i32,
        pub(crate) cmdoutval: i32,
        pub(crate) use_cmdoutval: bool,
        pub(crate) procsubstpid: i32,
        pub(crate) trap_return: i32,
        pub(crate) trap_state: i32,
        pub(crate) trapisfunc: bool,
        pub(crate) traplocallevel: i32,
        pub(crate) noerrs: i32,
        pub(crate) this_noerrexit: bool,
        pub(crate) underscore: Vec<u8>,
    }
}

/// `exec.c`: command and process substitution.
pub(crate) mod exec_cmd {
    use crate::shell::Shell;

    impl Shell {
        pub(crate) fn getoutput(&mut self, _cmd: &[u8], _qt: bool) -> Option<Vec<Vec<u8>>> {
            None
        }

        pub(crate) fn getoutputfile(
            &mut self,
            s: &[u8],
            _start: usize,
        ) -> (Option<Vec<u8>>, usize) {
            (None, s.len())
        }

        pub(crate) fn getproc(&mut self, s: &[u8], _start: usize) -> (Option<Vec<u8>>, usize) {
            (None, s.len())
        }
    }
}

/// `exec.c`: running strings.
pub(crate) mod exec_list {
    use crate::shell::Shell;

    impl Shell {
        pub(crate) fn execstring_ctx(&mut self, _s: &[u8], _context: &str) -> bool {
            false
        }

        pub(crate) fn execode(
            &mut self,
            _p: &crate::tables::Eprog,
            _dont_change_job: bool,
            _exiting: bool,
            _context: &str,
        ) {
        }
    }
}

/// `exec.c`: calling shell functions.
pub(crate) mod exec_func {
    use crate::shell::Shell;

    impl Shell {
        pub(crate) fn doshfunc(
            &mut self,
            _shfunc: &crate::tables::Shfunc,
            _doshargs: Option<Vec<Vec<u8>>>,
            _noreturnval: bool,
        ) -> i32 {
            1
        }

        pub(crate) fn doshfunc_by_name(
            &mut self,
            _name: &[u8],
            _args: Vec<Vec<u8>>,
            _noreturnval: bool,
        ) -> i32 {
            1
        }

        pub(crate) fn innermost_function_name(&self) -> Option<Vec<u8>> {
            None
        }

        pub(crate) fn current_function_traced(&self) -> bool {
            false
        }
    }
}

/// `prompt.c`: the colour sequences `Shell` keeps, and prompt expansion.
pub(crate) mod prompt {
    use crate::shell::Shell;

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
    }

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
}

/// `text.c`: the text of parsed code.
pub(crate) mod text {
    use crate::ast::{List, Redir};
    use crate::shell::Shell;

    impl Shell {
        pub(crate) fn getpermtext(&self, _l: &List, _start_indent: bool) -> Vec<u8> {
            Vec::new()
        }

        pub(crate) fn getredirtext(&self, _rs: &[Redir]) -> Vec<u8> {
            Vec::new()
        }
    }
}

/// `init.c`: sourcing files and checking jobs before exit.
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
    }
}
