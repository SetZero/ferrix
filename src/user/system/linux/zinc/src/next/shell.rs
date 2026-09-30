//! The shell's global state: what zsh keeps in C globals, gathered in one
//! struct, plus the error machinery of `utils.c` (`zerr`, `zwarn`,
//! `errflag`).

use std::cell::Cell;

use crate::hashtable::HashTable;
use crate::lex::{AliasDef, LexEnv, LexOpts};
use crate::options::{OPT_SIZE, OptName};
use crate::params::{Param, U};
use crate::utils::lossy;

/// `ERRFLAG_ERROR`: an error happened.
pub(crate) const ERRFLAG_ERROR: i32 = 1;
/// `ERRFLAG_INT`: interrupted.
pub(crate) const ERRFLAG_INT: i32 = 2;
/// `ERRFLAG_HARD`: an error that `always` blocks do not clear.
pub(crate) const ERRFLAG_HARD: i32 = 4;

/// End the process now, without running exit hooks.
pub(crate) fn exit_now(status: i32) -> ! {
    // SAFETY: _exit has no preconditions.
    unsafe { libc::_exit(status) }
}

/// The sticky emulation a function was defined under.
#[derive(Debug, Clone, Default)]
pub(crate) struct Sticky {
    pub(crate) emulation: u32,
    pub(crate) n_on_opts: Vec<i32>,
    pub(crate) n_off_opts: Vec<i32>,
}

/// The whole state of the shell.
#[expect(clippy::struct_excessive_bools, reason = "zsh's globals")]
pub(crate) struct Shell {
    // Options (options.c).
    pub(crate) opts: [bool; OPT_SIZE],
    pub(crate) emulation: u32,
    pub(crate) sticky: Option<Sticky>,
    pub(crate) optiontab: HashTable<OptName>,
    pub(crate) keyboardhackchar: u8,

    // Character classes (utils.c).
    pub(crate) typtab: [u16; 256],
    pub(crate) typtab_flags: u32,
    pub(crate) ifs: Option<Vec<u8>>,
    pub(crate) wordchars: Option<Vec<u8>>,
    pub(crate) bangchar: u8,
    pub(crate) hatchar: u8,
    pub(crate) hashchar: u8,

    // Errors.
    pub(crate) errflag: Cell<i32>,
    pub(crate) noerrs: i32,
    pub(crate) errno: i32,
    pub(crate) lastval: i32,

    // Parameters (params.c).
    pub(crate) realparamtab: HashTable<Param>,
    pub(crate) paramtab_override: Option<Box<HashTable<Param>>>,
    pub(crate) argvparam: Param,
    pub(crate) intvars: [i64; 15],
    pub(crate) strvars: [Option<Vec<u8>>; 11],
    pub(crate) arrvars: [Vec<Vec<u8>>; 10],
    pub(crate) locallevel: i32,
    pub(crate) forklevel: i32,
    pub(crate) argzero: Vec<u8>,
    pub(crate) posixzero: Vec<u8>,
    pub(crate) home: Vec<u8>,
    pub(crate) term: Vec<u8>,
    pub(crate) zsh_terminfo: Option<Vec<u8>>,
    pub(crate) zsh_terminfodirs: Option<Vec<u8>>,
    pub(crate) zunderscore: Vec<u8>,
    pub(crate) histsiz: i64,
    pub(crate) savehistsiz: i64,
    pub(crate) shtimer: (i64, i64),
    pub(crate) pipestats: [i32; crate::params::MAX_PIPESTATS],
    pub(crate) numpipestats: usize,
    pub(crate) pathchecked: usize,
    pub(crate) environ: Vec<Vec<u8>>,
    pub(crate) mypid: i32,
    pub(crate) pwd: Vec<u8>,
    pub(crate) dirstack: Vec<Vec<u8>>,
    /// zsh's `oldpwd` global.
    pub(crate) oldpwd_var: Vec<u8>,
    /// zsh's `chasinglinks`: `cd -P` or CHASE_LINKS is in effect.
    pub(crate) chasinglinks: bool,

    // Prompts (prompt.c).
    pub(crate) txtattrmask: u64,
    pub(crate) cmdstack: Vec<u8>,
    pub(crate) fg_bg_sequences: [crate::prompt::ColourSeq; 2],
    /// The history number of the line being read (zsh's `curhist`).
    pub(crate) curhist: i64,
    /// The terminal's device name (zsh's `ttystrname`).
    pub(crate) ttystrname: Vec<u8>,
    pub(crate) shtty: i32,

    // Math (math.c).
    pub(crate) mlevel: i32,
    pub(crate) noeval: i32,
    pub(crate) lastbase: i32,
    pub(crate) outputradix: i32,
    pub(crate) outputunderscore: i32,
    pub(crate) lastmathval: crate::math::MNumber,

    // Patterns and globbing (pattern.c, glob.c).
    pub(crate) zpc_disables: [bool; 19],
    pub(crate) zpc_disables_stack: Vec<u32>,
    pub(crate) pat_file_special: [u8; 19],
    pub(crate) pat_file_globflags: i32,
    pub(crate) errsfound: Cell<i32>,
    pub(crate) forceerrs: Cell<i32>,
    pub(crate) badcshglob: i32,
    pub(crate) glob_pre: Option<Vec<u8>>,
    pub(crate) glob_suf: Option<Vec<u8>>,

    // History substitution (hist.c).
    pub(crate) hsubl: Option<Vec<u8>>,
    pub(crate) hsubr: Option<Vec<u8>>,

    // Aliases (hashtable.c).
    pub(crate) aliastab: HashTable<crate::tables::Alias>,
    pub(crate) sufaliastab: HashTable<crate::tables::Alias>,
    pub(crate) at_prompt: bool,

    // The other command tables (hashtable.c).
    pub(crate) shfunctab: HashTable<crate::tables::Shfunc>,
    pub(crate) cmdnamtab: HashTable<crate::tables::Cmdnam>,
    pub(crate) reswdtab: HashTable<crate::tables::Reswd>,
    pub(crate) nameddirtab: HashTable<crate::tables::Nameddir>,
    pub(crate) allusersadded: bool,

    // Execution (exec.c, loop.c).
    pub(crate) noerrexit: i32,
    pub(crate) this_noerrexit: bool,
    pub(crate) nohistsave: bool,
    pub(crate) trap_state: i32,
    pub(crate) trap_return: i32,
    pub(crate) subsh: bool,
    pub(crate) retflag: bool,
    pub(crate) breaks: i32,
    pub(crate) loops: i32,
    pub(crate) contflag: i32,
    pub(crate) try_tryflag: i64,
    pub(crate) lastval2: i32,
    pub(crate) fdtable: Vec<u8>,
    pub(crate) max_zsh_fd: i32,
    pub(crate) fdtable_flocks: i32,
    pub(crate) coprocin: i32,
    pub(crate) coprocout: i32,
    pub(crate) zleactive: bool,
    pub(crate) cmdoutpid: i32,
    pub(crate) procsubstpid: i32,
    pub(crate) cmdoutval: i32,
    pub(crate) use_cmdoutval: bool,
    pub(crate) sfcontext: i32,
    pub(crate) exstack: Vec<crate::exec::ExecStack>,
    pub(crate) funcstack: Vec<crate::exec::Funcstack>,
    pub(crate) doneps4: bool,
    pub(crate) sttyval: Option<Vec<u8>>,
    pub(crate) list_pipe: bool,
    pub(crate) simple_pline: bool,
    pub(crate) list_pipe_pid: i32,
    pub(crate) nowait: bool,
    pub(crate) pline_level: i32,
    pub(crate) list_pipe_child: bool,
    pub(crate) list_pipe_job: i32,
    pub(crate) list_pipe_text: Vec<u8>,
    pub(crate) xtrerr: i32,
    pub(crate) zsh_subshell: i64,
    pub(crate) lastpid: i64,
    pub(crate) chline_active: bool,

    // Jobs (jobs.c).
    pub(crate) jobtab: Vec<crate::jobs::Job>,
    pub(crate) maxjob: usize,
    pub(crate) thisjob: i32,
    pub(crate) curjob: i32,
    pub(crate) prevjob: i32,
    pub(crate) oldjobtab: Option<Vec<crate::jobs::Job>>,
    pub(crate) oldmaxjob: usize,
    pub(crate) mypgrp: i32,
    pub(crate) origpgrp: i32,
    pub(crate) last_attached_pgrp: i32,
    pub(crate) ttyfrozen: bool,
    pub(crate) prev_errflag: i32,
    pub(crate) prev_breaks: i32,
    pub(crate) errbrk_saved: bool,
    pub(crate) child_usage: libc::rusage,
    pub(crate) bgstatus: std::collections::VecDeque<(i32, i32)>,
    pub(crate) stopmsg: i32,
    pub(crate) shout: i32,
    pub(crate) shttyinfo: crate::jobs::TtyInfo,
    pub(crate) islogin: bool,

    // Signals and traps (signals.c).
    pub(crate) sigtrapped: [i32; crate::signames::VSIGCOUNT],
    pub(crate) siglists: Vec<Option<crate::tables::Eprog>>,
    pub(crate) nsigtrapped: i32,
    pub(crate) in_exit_trap: i32,
    pub(crate) exit_trap_posix: bool,
    pub(crate) queueing_enabled: i32,
    pub(crate) trap_queueing_enabled: bool,
    pub(crate) trap_queue: std::collections::VecDeque<usize>,
    pub(crate) savetraps: Vec<crate::signals::SaveTrap>,
    pub(crate) dontsavetrap: i32,
    pub(crate) intrap: i32,
    pub(crate) trapisfunc: bool,
    pub(crate) traplocallevel: i32,
    pub(crate) dotrap_in_table: bool,

    // Leaving the shell and the terminal (builtin.c, utils.c).
    pub(crate) exit_val: i32,
    pub(crate) exit_pending: bool,
    pub(crate) shell_exiting: i32,
    pub(crate) attachtty_ep: bool,
    pub(crate) getwinsz: bool,
    pub(crate) zterm_lines: i64,
    pub(crate) zterm_columns: i64,
    pub(crate) tclines: i64,
    pub(crate) tccolumns: i64,
    pub(crate) cached_uid: Option<u32>,
    pub(crate) cached_username: Vec<u8>,
    pub(crate) text_expand_tabs: i32,
    pub(crate) pending_input: Vec<u8>,
    pub(crate) builtintab: HashTable<crate::builtin::Builtin>,
    pub(crate) sourcelevel: i32,
    /// zsh's `noexitct`: EOFs ignored in a row under IGNORE_EOF.
    pub(crate) noexitct: i32,
    pub(crate) ineval: i32,
    pub(crate) donetrap: bool,
    pub(crate) lineno: i64,
    pub(crate) lastwj: i32,
    pub(crate) lpforked: i32,
    pub(crate) list_pipe_start: (i64, i64),
    pub(crate) zsh_eval_context: Vec<Vec<u8>>,
    pub(crate) esprefork: i32,
    pub(crate) esglob: bool,
    pub(crate) shlvl: i64,
    pub(crate) nullcmd: Option<Vec<u8>>,
    pub(crate) readnullcmd: Option<Vec<u8>>,
    pub(crate) doprintdir: i32,
    pub(crate) exit_level: i32,
    pub(crate) funcdepth: i64,
    pub(crate) noaliases: bool,
    pub(crate) oflags: u32,
    pub(crate) optcind: i64,
    pub(crate) zoptind: i64,
    pub(crate) scriptname: Option<Vec<u8>>,
    pub(crate) scriptfilename: Option<Vec<u8>>,
    pub(crate) tracingcond: i32,
    pub(crate) try_errflag: i64,
    pub(crate) try_interrupt: i64,
    pub(crate) zsh_funcnest: i64,
    pub(crate) random: crate::misc::GlibcRand,
    pub(crate) term_unknown: bool,
    pub(crate) mathfuncs: Vec<crate::misc::MathFunc>,
}

impl Shell {
    pub(crate) fn new() -> Shell {
        // SAFETY: getpid has no preconditions.
        let mypid = unsafe { libc::getpid() };
        let mut sh = Shell {
            opts: [false; OPT_SIZE],
            emulation: 0,
            sticky: None,
            optiontab: crate::options::createoptiontable(),
            keyboardhackchar: 0,
            typtab: [0; 256],
            typtab_flags: 0,
            ifs: None,
            wordchars: None,
            bangchar: b'!',
            hatchar: b'^',
            hashchar: b'#',
            errflag: Cell::new(0),
            noerrs: 0,
            errno: 0,
            lastval: 0,
            realparamtab: HashTable::new(151),
            paramtab_override: None,
            argvparam: Param::new(crate::params::PM_ARRAY),
            intvars: [0; 15],
            strvars: Default::default(),
            arrvars: Default::default(),
            locallevel: 0,
            forklevel: 0,
            argzero: Vec::new(),
            posixzero: Vec::new(),
            home: Vec::new(),
            term: Vec::new(),
            zsh_terminfo: None,
            zsh_terminfodirs: None,
            zunderscore: Vec::new(),
            histsiz: 30,
            savehistsiz: 0,
            shtimer: crate::params::now_tv(),
            pipestats: [0; crate::params::MAX_PIPESTATS],
            numpipestats: 0,
            pathchecked: 0,
            environ: Vec::new(),
            mypid,
            pwd: Vec::new(),
            dirstack: Vec::new(),
            oldpwd_var: Vec::new(),
            chasinglinks: false,
            txtattrmask: 0,
            cmdstack: Vec::new(),
            fg_bg_sequences: crate::prompt::default_colour_sequences(),
            curhist: 0,
            ttystrname: Vec::new(),
            shtty: -1,
            mlevel: 0,
            noeval: 0,
            lastbase: -1,
            outputradix: 0,
            outputunderscore: 0,
            lastmathval: crate::math::MNumber::Int(0),
            zpc_disables: [false; 19],
            zpc_disables_stack: Vec::new(),
            pat_file_special: [0; 19],
            pat_file_globflags: 0,
            errsfound: Cell::new(0),
            forceerrs: Cell::new(-1),
            badcshglob: 0,
            glob_pre: None,
            glob_suf: None,
            hsubl: None,
            hsubr: None,
            aliastab: HashTable::new(23),
            sufaliastab: HashTable::new(11),
            at_prompt: false,
            shfunctab: HashTable::new(7),
            cmdnamtab: HashTable::new(201),
            reswdtab: HashTable::new(23),
            nameddirtab: HashTable::new(201),
            allusersadded: false,
            noerrexit: 0,
            this_noerrexit: false,
            nohistsave: false,
            trap_state: 0,
            trap_return: 0,
            subsh: false,
            retflag: false,
            breaks: 0,
            loops: 0,
            contflag: 0,
            try_tryflag: 0,
            lastval2: 0,
            fdtable: vec![0; 32],
            max_zsh_fd: 0,
            fdtable_flocks: 0,
            coprocin: -1,
            coprocout: -1,
            zleactive: false,
            cmdoutpid: 0,
            procsubstpid: 0,
            cmdoutval: 0,
            use_cmdoutval: false,
            sfcontext: 0,
            exstack: Vec::new(),
            funcstack: Vec::new(),
            doneps4: false,
            sttyval: None,
            list_pipe: false,
            simple_pline: false,
            list_pipe_pid: 0,
            nowait: false,
            pline_level: 0,
            list_pipe_child: false,
            list_pipe_job: 0,
            list_pipe_text: Vec::new(),
            xtrerr: -1,
            zsh_subshell: 0,
            lastpid: 0,
            chline_active: false,
            jobtab: vec![crate::jobs::Job::default(); crate::jobs::MAXJOBS_ALLOC],
            maxjob: 0,
            thisjob: -1,
            curjob: -1,
            prevjob: -1,
            oldjobtab: None,
            oldmaxjob: 0,
            mypgrp: 0,
            origpgrp: 0,
            last_attached_pgrp: 0,
            ttyfrozen: false,
            prev_errflag: 0,
            prev_breaks: 0,
            errbrk_saved: false,
            // SAFETY: an all-zero rusage is a valid value.
            child_usage: unsafe { std::mem::zeroed() },
            bgstatus: std::collections::VecDeque::new(),
            stopmsg: 0,
            shout: -1,
            shttyinfo: crate::jobs::TtyInfo::default(),
            islogin: false,
            sigtrapped: [0; crate::signames::VSIGCOUNT],
            siglists: vec![None; crate::signames::VSIGCOUNT],
            nsigtrapped: 0,
            in_exit_trap: 0,
            exit_trap_posix: false,
            queueing_enabled: 0,
            trap_queueing_enabled: false,
            trap_queue: std::collections::VecDeque::new(),
            savetraps: Vec::new(),
            dontsavetrap: 0,
            intrap: 0,
            trapisfunc: false,
            traplocallevel: 0,
            dotrap_in_table: false,
            exit_val: 0,
            exit_pending: false,
            shell_exiting: 0,
            attachtty_ep: false,
            getwinsz: true,
            zterm_lines: -1,
            zterm_columns: -1,
            tclines: -1,
            tccolumns: -1,
            cached_uid: None,
            cached_username: Vec::new(),
            text_expand_tabs: 0,
            pending_input: Vec::new(),
            builtintab: HashTable::new(85),
            sourcelevel: 0,
            noexitct: 0,
            ineval: 0,
            donetrap: false,
            lineno: 0,
            lastwj: 0,
            lpforked: 0,
            list_pipe_start: (0, 0),
            zsh_eval_context: Vec::new(),
            esprefork: 0,
            esglob: true,
            shlvl: 0,
            nullcmd: None,
            readnullcmd: None,
            doprintdir: 0,
            exit_level: 0,
            funcdepth: 0,
            noaliases: false,
            oflags: 0,
            optcind: 0,
            zoptind: 1,
            scriptname: None,
            scriptfilename: None,
            tracingcond: 0,
            try_errflag: -1,
            try_interrupt: -1,
            zsh_funcnest: -1,
            random: crate::misc::GlibcRand::new(1),
            term_unknown: true,
            mathfuncs: Vec::new(),
        };
        sh.argvparam.u = U::Arr(Vec::new());
        sh
    }

    /// The whole of `main` for now: set up, then run the arguments.
    pub(crate) fn main_entry(&mut self, _args: Vec<std::ffi::OsString>) -> i32 {
        self.emulation = crate::options::EMULATE_ZSH;
        crate::options::installemulation(self.emulation, &mut self.opts);
        self.inittyptab();
        self.createbuiltintable();
        self.lastval
    }

    /// `errflag` as a truth value.
    pub(crate) fn errflag(&self) -> bool {
        self.errflag.get() != 0
    }

    pub(crate) fn errflag_int(&self) -> bool {
        self.errflag.get() & ERRFLAG_INT != 0
    }

    pub(crate) fn errflag_set_error(&self) {
        self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
    }

    pub(crate) fn errflag_clear_error(&mut self) {
        self.errflag.set(self.errflag.get() & !ERRFLAG_ERROR);
    }

    pub(crate) fn lastval(&self) -> i32 {
        self.lastval
    }

    pub(crate) fn set_lastval(&mut self, v: i32) {
        self.lastval = v;
    }

    /// zsh's `zwarning` and `zerrmsg`: `prefix:cmd:lineno: msg`.
    fn zwarning(&self, cmd: Option<&str>, msg: &str) {
        use crate::options::SHINSTDIN;
        let prefix: Vec<u8> = self
            .scriptname
            .clone()
            .unwrap_or_else(|| self.argzero.clone());
        let outside = !self.isset(SHINSTDIN) || self.locallevel != 0;
        let mut out = Vec::new();
        match cmd {
            Some(c) => {
                if outside {
                    out.extend(self.nicezputs(&prefix));
                    out.push(b':');
                }
                out.extend(self.nicezputs(c.as_bytes()));
                out.push(b':');
            }
            None => {
                if outside {
                    out.extend(self.nicezputs(&prefix));
                } else {
                    out.extend_from_slice(b"zsh");
                }
                out.push(b':');
            }
        }
        if outside && self.lineno != 0 {
            out.extend_from_slice(format!("{}: ", self.lineno).as_bytes());
        } else {
            out.push(b' ');
        }
        out.extend_from_slice(msg.as_bytes());
        out.push(b'\n');
        write_fd(2, &out);
    }

    /// zsh's `zerr`.
    pub(crate) fn zerr(&self, msg: &str) {
        if self.errflag.get() != 0 || self.noerrs != 0 {
            if self.noerrs < 2 {
                self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
            }
            return;
        }
        self.zwarning(None, msg);
        self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
    }

    /// zsh's `zerrnam`.
    pub(crate) fn zerrnam(&self, cmd: &str, msg: &str) {
        if self.errflag.get() != 0 || self.noerrs != 0 {
            return;
        }
        self.zwarning(Some(cmd), msg);
        self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
    }

    /// zsh's `zwarn`.
    pub(crate) fn zwarn(&self, msg: &str) {
        if self.errflag.get() != 0 || self.noerrs != 0 {
            return;
        }
        self.zwarning(None, msg);
    }

    /// zsh's `zwarnnam`.
    pub(crate) fn zwarnnam(&self, cmd: &str, msg: &str) {
        if self.errflag.get() != 0 || self.noerrs != 0 {
            return;
        }
        self.zwarning(Some(cmd), msg);
    }

    pub(crate) fn write_stdout(&self, bytes: &[u8]) {
        write_fd(1, bytes);
    }

    pub(crate) fn lex_opts(&self) -> LexOpts {
        use crate::options::*;
        LexOpts {
            shglob: self.isset(SHGLOB),
            kshglob: self.isset(KSHGLOB),
            ignorebraces: self.isset(IGNOREBRACES),
            ignoreclosebraces: self.isset(IGNORECLOSEBRACES),
            comments: !self.at_prompt || self.isset(INTERACTIVECOMMENTS),
            rcquotes: self.isset(RCQUOTES),
            cshjunkiequotes: self.isset(CSHJUNKIEQUOTES),
            cshjunkieloops: self.isset(CSHJUNKIELOOPS),
            aliases: self.isset(ALIASESOPT),
            posixaliases: self.isset(POSIXALIASES),
            shortloops: self.isset(SHORTLOOPS),
            shortrepeat: self.isset(SHORTREPEAT),
            multifuncdef: self.isset(MULTIFUNCDEF),
            aliasfuncdef: self.isset(ALIASFUNCDEF),
            execopt: self.isset(EXECOPT),
        }
    }

    /// A message naming bytes, for errors.
    pub(crate) fn shown(s: &[u8]) -> String {
        lossy(s)
    }
}

impl LexEnv for Shell {
    fn alias(&self, name: &[u8]) -> Option<AliasDef> {
        let a = self.aliastab.get(name)?;
        if a.flags & crate::tables::DISABLED != 0 {
            return None;
        }
        Some(AliasDef {
            text: a.text.clone(),
            global: a.flags & crate::tables::ALIAS_GLOBAL != 0,
        })
    }

    fn suffix_alias(&self, ext: &[u8]) -> Option<AliasDef> {
        let a = self.sufaliastab.get(ext)?;
        if a.flags & crate::tables::DISABLED != 0 {
            return None;
        }
        Some(AliasDef {
            text: a.text.clone(),
            global: false,
        })
    }

    fn opts(&self) -> LexOpts {
        self.lex_opts()
    }
}

/// Write all of `bytes` to `fd`, retrying on EINTR.
pub(crate) fn write_fd(fd: i32, bytes: &[u8]) {
    let mut rest = bytes;
    while !rest.is_empty() {
        // SAFETY: the pointer and length describe `rest`.
        let n = unsafe { libc::write(fd, rest.as_ptr().cast(), rest.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return;
        }
        rest = rest.get(usize::try_from(n).unwrap_or(0)..).unwrap_or(&[]);
    }
}

/// Flush what the shell buffered and end the process, as C's `exit` does.
pub(crate) fn flush_and_exit(status: i32) -> ! {
    std::process::exit(status)
}
