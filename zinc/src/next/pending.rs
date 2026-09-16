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

    impl Shell {
        /// zsh's `createbuiltintable`: no builtin has landed.
        pub(crate) fn createbuiltintable(&mut self) {}
    }
}

/// `dirs`: named directories and the terminal helpers from `utils.c`.
pub(crate) mod dirs {
    use crate::shell::Shell;

    impl Shell {
        pub(crate) fn xsymlink(&mut self, s: &[u8]) -> Option<Vec<u8>> {
            Some(s.to_vec())
        }

        pub(crate) fn get_username(&mut self) -> Vec<u8> {
            Vec::new()
        }

        pub(crate) fn finddir_reset(&mut self) {}

        pub(crate) fn adduserdir(
            &mut self,
            _s: &[u8],
            _t: Option<&[u8]>,
            _flags: u32,
            _always: bool,
        ) {
        }

        pub(crate) fn zbeep(&mut self) {}

        pub(crate) fn getnameddir(&mut self, name: &[u8]) -> Option<Vec<u8>> {
            self.nameddirtab.get(name).map(|nd| nd.dir.clone())
        }

        pub(crate) fn oldpwd(&mut self) -> Option<Vec<u8>> {
            self.getsparam(b"OLDPWD")
        }

        pub(crate) fn subst_string_by_hook(
            &mut self,
            _name: &[u8],
            _arg1: Option<&[u8]>,
            _orig: &[u8],
        ) -> Option<Vec<Vec<u8>>> {
            None
        }

        pub(crate) fn substnamedir(&mut self, s: &[u8]) -> Vec<u8> {
            s.to_vec()
        }

        pub(crate) fn ttyidle(&self) -> i64 {
            -1
        }
    }
}

/// `exec.c`: the execution stacks and finding commands.
pub(crate) mod exec {
    use crate::shell::Shell;

    impl Shell {
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
    }
}

/// `exec.c`: calling shell functions.
pub(crate) mod exec_func {
    use crate::shell::Shell;

    impl Shell {
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

/// `jobs.c`: the job table's types.
pub(crate) mod jobs {
    use crate::shell::Shell;

    pub(crate) const MAXJOBS_ALLOC: usize = 50;

    /// A process of a job (zsh's `struct process`).
    #[derive(Clone)]
    pub(crate) struct Process {
        pub(crate) pid: i32,
        pub(crate) text: Vec<u8>,
        pub(crate) status: i32,
        pub(crate) ti: libc::rusage,
        pub(crate) bgtime: (i64, i64),
        pub(crate) endtime: (i64, i64),
    }

    impl std::fmt::Debug for Process {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Process")
                .field("pid", &self.pid)
                .field("status", &self.status)
                .finish_non_exhaustive()
        }
    }

    /// A file to delete or descriptor to close when a job ends.
    #[derive(Debug, Clone)]
    pub(crate) enum JobFile {
        Name(Vec<u8>),
        Fd(i32),
    }

    /// Saved terminal state (zsh's `struct ttyinfo`).
    #[derive(Clone, Copy)]
    pub(crate) struct TtyInfo {
        pub(crate) tio: libc::termios,
        pub(crate) winsize: libc::winsize,
    }

    impl Default for TtyInfo {
        fn default() -> TtyInfo {
            // SAFETY: all-zero termios and winsize are valid values.
            unsafe { std::mem::zeroed() }
        }
    }

    impl std::fmt::Debug for TtyInfo {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("TtyInfo")
        }
    }

    /// A job (zsh's `struct job`).
    #[derive(Debug, Clone, Default)]
    pub(crate) struct Job {
        pub(crate) gleader: i32,
        pub(crate) other: i32,
        pub(crate) stat: i32,
        pub(crate) pwd: Option<Vec<u8>>,
        pub(crate) procs: Vec<Process>,
        pub(crate) auxprocs: Vec<Process>,
        pub(crate) filelist: Option<Vec<JobFile>>,
        pub(crate) stty_in_env: bool,
        pub(crate) ty: Option<TtyInfo>,
    }

    impl Shell {
        pub(crate) fn acquire_pgrp(&mut self) {}
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

/// `signals.c`: saved traps and signal queueing.
pub(crate) mod signals {
    use crate::shell::Shell;
    use crate::tables::{Eprog, Shfunc};

    /// A saved trap (zsh's `struct savetrap`).
    #[derive(Debug, Clone)]
    pub(crate) struct SaveTrap {
        pub(crate) sig: usize,
        pub(crate) flags: i32,
        pub(crate) local: i32,
        pub(crate) posix: bool,
        pub(crate) list: SavedTrapList,
    }

    #[derive(Debug, Clone)]
    pub(crate) enum SavedTrapList {
        None,
        Func(Vec<u8>, Box<Shfunc>),
        List(Eprog),
    }

    pub(crate) const ZEXIT_NORMAL: i32 = 0;

    /// The current `errno`.
    pub(crate) fn errno() -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    impl Shell {
        pub(crate) fn queue_signals(&mut self) {
            self.queueing_enabled += 1;
        }

        pub(crate) fn unqueue_signals(&mut self) {
            self.queueing_enabled = (self.queueing_enabled - 1).max(0);
        }
    }
}

/// `utils.c` and `builtin.c`: errors and the terminal.
pub(crate) mod sysutil {
    use crate::jobs::TtyInfo;
    use crate::shell::Shell;

    /// zsh's `%e`; the glibc wording lands with the module.
    pub(crate) fn errmsg(e: i32) -> String {
        std::io::Error::from_raw_os_error(e).to_string()
    }

    impl Shell {
        pub(crate) fn errmsg_last(&self) -> String {
            errmsg(crate::signals::errno())
        }

        pub(crate) fn attachtty(&mut self, _pgrp: i32) {}

        pub(crate) fn gettyinfo_now(&self) -> TtyInfo {
            TtyInfo::default()
        }

        pub(crate) fn settyinfo(&self, _ti: &TtyInfo) {}

        pub(crate) fn adjustwinsize(&mut self, _from: i32) {}

        pub(crate) fn zexit(&mut self, val: i32, _from_where: i32) {
            self.exit_val = val;
        }
    }
}

/// `hashtable.c`: the command tables' entries.
pub(crate) mod tables {
    use std::rc::Rc;

    use crate::ast::{List, Redir};
    use crate::shell::{Shell, Sticky};

    pub(crate) const DISABLED: u32 = 1 << 0;
    pub(crate) const ALIAS_GLOBAL: u32 = 1 << 1;

    /// A parsed program (zsh's `Eprog`), shared between the places that hold it.
    #[derive(Debug, Clone)]
    pub(crate) struct Eprog {
        pub(crate) list: Rc<List>,
        pub(crate) flags: u32,
    }

    /// A shell function (zsh's `struct shfunc`).
    #[derive(Debug, Clone, Default)]
    pub(crate) struct Shfunc {
        pub(crate) flags: u32,
        pub(crate) filename: Option<Vec<u8>>,
        pub(crate) lineno: i64,
        pub(crate) funcdef: Option<Eprog>,
        pub(crate) redir: Option<Rc<Vec<Redir>>>,
        pub(crate) sticky: Option<Sticky>,
    }

    /// Where an external command was found (zsh's `struct cmdnam`).
    #[derive(Debug, Clone)]
    pub(crate) struct Cmdnam {
        pub(crate) flags: u32,
        pub(crate) name: Option<usize>,
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

    impl Shell {
        pub(crate) fn getshfunc(&self, name: &[u8]) -> Option<Shfunc> {
            self.shfunctab.get(name).cloned()
        }

        pub(crate) fn cmdnamtab_empty(&mut self) {
            self.cmdnamtab.clear();
        }

        pub(crate) fn nicezputs(&self, s: &[u8]) -> Vec<u8> {
            crate::tok::unmetafy(s)
        }
    }
}
