//! Command execution, part four (zsh's `exec.c`): defining and calling
//! shell functions, autoloading, and `AUTO_CD`'s directory test.

use std::rc::Rc;

use crate::ast::{CmdKind, Command, ListMode, Redir};
use crate::exec::*;
use crate::options::*;
use crate::params::*;
use crate::shell::{ERRFLAG_ERROR, Shell, write_fd};
use crate::signals::{NOERREXIT_RETURN, TRAP_STATE_PRIMED, ZSIG_FUNC};
use crate::subst::WordList;
use crate::tables::{EF_RUN, Eprog, Shfunc};
use crate::tok;
use crate::utils::{has_token, lossy};

/// `ANONYMOUS_FUNCTION_NAME`.
pub(crate) const ANONYMOUS_FUNCTION_NAME: &[u8] = b"(anon)";

/// Saved state around a function call (zsh's `struct funcsave`).
struct Funcsave {
    opts: [bool; OPT_SIZE],
    argv0: Option<Vec<u8>>,
    zoptind: i64,
    lastval: i32,
    optcind: i64,
    numpipestats: usize,
    pipestats: Option<Vec<i32>>,
    scriptname: Option<Vec<u8>>,
    breaks: i32,
    contflag: i32,
    loops: i32,
    emulation: u32,
    noerrexit: i32,
    oflags: u32,
    restore_sticky: bool,
    sticky: Option<crate::shell::Sticky>,
}

impl Shell {
    /// zsh's `execfuncdef`.
    pub(crate) fn execfuncdef(&mut self, cmd: &Command, redir_prog: Option<Rc<Vec<Redir>>>) -> i32 {
        let CmdKind::FuncDef {
            names,
            body,
            tracing,
            args,
        } = &cmd.kind
        else {
            return 0;
        };
        let mut ret = 0;
        let tracing_flags = if *tracing { PM_TAGGED_LOCAL } else { 0 };
        let mut names_v = names.clone();
        if names_v.iter().any(|n| has_token(n)) {
            let mut wl = WordList {
                words: names_v,
                flags: 0,
            };
            self.execsubst(&mut wl);
            if self.errflag() {
                return 1;
            }
            names_v = wl.words;
        }
        let lineno = match self.funcstack.last() {
            Some(fs) if fs.tp == FS_FUNC || fs.tp == FS_EVAL => fs.flineno + self.lineno,
            _ => self.lineno,
        };
        let base = Shfunc {
            flags: tracing_flags,
            filename: self.scriptfilename.clone(),
            lineno,
            funcdef: Some(Eprog::from_rc(Rc::clone(body))),
            redir: redir_prog,
            sticky: self.sticky.clone(),
        };
        if names.is_empty() {
            let mut shf = base;
            shf.flags |= PM_ANONYMOUS;
            let mut argwl = WordList {
                words: args.clone(),
                flags: 0,
            };
            if args.iter().any(|a| has_token(a)) {
                self.execsubst(&mut argwl);
                if self.errflag() {
                    return 1;
                }
            }
            let last = argwl.words.last().cloned().unwrap_or_default();
            self.setunderscore(&last);
            let mut a = vec![ANONYMOUS_FUNCTION_NAME.to_vec()];
            a.extend(argwl.words);
            self.execshfunc_named(ANONYMOUS_FUNCTION_NAME, &shf, a);
            ret = self.lastval;
            if self.isset(PRINTEXITVALUE) && self.isset(SHINSTDIN) && self.lastval != 0 {
                write_fd(2, format!("zsh: exit {}\n", self.lastval).as_bytes());
            }
            return ret;
        }
        for s in names_v {
            let mut shf = base.clone();
            if let Some(rest) = s.strip_prefix(b"TRAP") {
                let signum = Shell::getsignum(rest);
                if signum != -1 {
                    if self.settrap(signum, None, ZSIG_FUNC) != 0 {
                        return 1;
                    }
                    self.removetrapnode(usize::try_from(signum).unwrap_or(0));
                }
            }
            if let Some(fs) = self.funcstack.last()
                && fs.tp == FS_FUNC
                && fs.name == s
                && let Some(old) = self.shfunctab.get(&s)
            {
                shf.flags |= old.flags & (PM_TAGGED | PM_TAGGED_LOCAL);
            }
            let _ = self.shfunctab.insert(s, shf);
        }
        self.setunderscore(b"");
        ret
    }

    /// zsh's `execshfunc`.
    pub(crate) fn execshfunc(&mut self, shf: &Shfunc, args: Vec<Vec<u8>>) {
        let name = args.first().cloned().unwrap_or_default();
        self.execshfunc_named(&name, shf, args);
    }

    fn execshfunc_named(&mut self, _name: &[u8], shf: &Shfunc, args: Vec<Vec<u8>>) {
        if self.errflag() {
            return;
        }
        let mut last_file_list = None;
        let tj = self.thisjob;
        if !self.list_pipe && tj != -1 && tj != self.list_pipe_job && !self.hasprocs(tj) {
            last_file_list = self.job_mut(tj).and_then(|j| j.filelist.take());
            if let Ok(t) = usize::try_from(tj) {
                self.deletejob(t, false);
            }
        }
        if self.isset(XTRACE) {
            self.printprompt4();
            let mut out = Vec::new();
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                out.extend(self.quotedzputs_out(a));
            }
            out.push(b'\n');
            write_fd(self.xtrerr_fd(), &out);
        }
        self.queue_signals();
        let osfc = self.sfcontext;
        if osfc == SFC_NONE {
            self.sfcontext = SFC_DIRECT;
        }
        self.xtrerr = -1;
        let _ = self.doshfunc(shf, Some(args), false);
        self.sfcontext = osfc;
        if !self.list_pipe {
            self.deletefilelist(last_file_list, false);
        }
        self.unqueue_signals();
    }

    /// zsh's `loadautofn`: load the function `name` from `$fpath`.
    pub(crate) fn loadautofn(
        &mut self,
        name: &[u8],
        fksh: i32,
        autol: bool,
        current_fpath: bool,
    ) -> Option<Shfunc> {
        let shf = self.shfunctab.get(name).cloned()?;
        let noalias = self.noaliases;
        self.noaliases = shf.flags & PM_UNALIASED != 0;
        let mut ksh = 1;
        let mut found;
        if let Some(f) = shf
            .filename
            .clone()
            .filter(|f| f.first() == Some(&b'/') && shf.flags & PM_LOADDIR != 0)
        {
            found = self.getfpfunc(name, &mut ksh, Some(&[f]), false);
            if found.is_none() && (current_fpath || shf.flags & PM_CUR_FPATH != 0) {
                found = self.getfpfunc(name, &mut ksh, None, false);
            }
        } else {
            found = self.getfpfunc(name, &mut ksh, None, false);
        }
        self.noaliases = noalias;
        if ksh == 1 {
            ksh = fksh;
            if ksh == 1 {
                ksh = if shf.flags & PM_KSHSTORED != 0 {
                    2
                } else if shf.flags & PM_ZSHSTORED != 0 {
                    0
                } else {
                    1
                };
            }
        }
        let Some((prog, fdir)) = found else {
            self.locallevel -= 1;
            self.zwarn(&format!(
                "{}: function definition file not found",
                lossy(name)
            ));
            self.locallevel += 1;
            return None;
        };
        let prog = prog?;
        if ksh == 2 || (ksh == 1 && self.isset(KSHAUTOLOAD)) {
            if autol {
                let mut p = prog;
                p.flags |= EF_RUN;
                if let Some(f) = self.shfunctab.get_mut(name) {
                    f.funcdef = Some(p);
                    f.flags &= !PM_UNDEFINED;
                    loadautofnsetfile(f, fdir);
                }
            } else {
                self.execode(&prog, true, false, "evalautofunc");
                match self.shfunctab.get(name) {
                    Some(f) if f.flags & PM_UNDEFINED == 0 => {}
                    _ => {
                        self.locallevel -= 1;
                        self.zwarn(&format!("{}: function not defined by file", lossy(name)));
                        self.locallevel += 1;
                        return None;
                    }
                }
            }
        } else {
            let stripped = stripkshdef(prog, name);
            if let Some(f) = self.shfunctab.get_mut(name) {
                f.funcdef = Some(stripped);
                f.flags &= !PM_UNDEFINED;
                loadautofnsetfile(f, fdir);
            }
        }
        self.shfunctab.get(name).cloned()
    }

    /// zsh's `getfpfunc`: the parsed file for `s` along `$fpath` (or
    /// `alt_path`) and the directory it was in. `Some((None, dir))` for
    /// `test_only`; `None` when not found.
    pub(crate) fn getfpfunc(
        &mut self,
        s: &[u8],
        _ksh: &mut i32,
        alt_path: Option<&[Vec<u8>]>,
        test_only: bool,
    ) -> Option<(Option<Eprog>, Option<Vec<u8>>)> {
        let path: Vec<Vec<u8>> = match alt_path {
            Some(p) => p.to_vec(),
            None => self.arrvar(ArrVar::Fpath).to_vec(),
        };
        for pp in path {
            if pp.len() + s.len() + 1 >= libc::PATH_MAX as usize {
                continue;
            }
            let mut buf = if pp.is_empty() {
                Vec::new()
            } else {
                pp.clone()
            };
            if !pp.is_empty() {
                buf.push(b'/');
            }
            buf.extend_from_slice(s);
            let ubuf = tok::unmetafy(&buf);
            let Ok(c) = std::ffi::CString::new(ubuf.clone()) else {
                continue;
            };
            // SAFETY: c is NUL-terminated.
            if unsafe { libc::access(c.as_ptr(), libc::R_OK) } != 0 {
                continue;
            }
            use std::os::unix::ffi::OsStrExt;
            let p = std::ffi::OsStr::from_bytes(&ubuf);
            if !std::fs::metadata(p).is_ok_and(|m| m.is_file()) {
                continue;
            }
            if test_only {
                return Some((None, Some(pp)));
            }
            let Ok(d) = std::fs::read(p) else { continue };
            let oldscriptname = self.scriptname.replace(s.to_vec());
            let r = self.parse_string(&tok::metafy(&d), true);
            self.scriptname = oldscriptname;
            return Some((r, Some(pp)));
        }
        None
    }

    /// zsh's `doshfunc`.
    #[expect(clippy::too_many_lines, reason = "zsh's doshfunc")]
    pub(crate) fn doshfunc(
        &mut self,
        shfunc: &Shfunc,
        doshargs: Option<Vec<Vec<u8>>>,
        noreturnval: bool,
    ) -> i32 {
        let name = doshargs
            .as_ref()
            .and_then(|a| a.first().cloned())
            .unwrap_or_default();
        let mut flags = shfunc.flags;
        let fname = name.clone();
        self.queue_signals();
        let mut fs = Funcsave {
            opts: self.opts,
            argv0: None,
            zoptind: self.zoptind,
            lastval: self.lastval,
            optcind: self.optcind,
            numpipestats: self.numpipestats,
            pipestats: None,
            scriptname: self.scriptname.clone(),
            breaks: self.breaks,
            contflag: self.contflag,
            loops: self.loops,
            emulation: self.emulation,
            noerrexit: self.noerrexit,
            oflags: self.oflags,
            restore_sticky: false,
            sticky: self.sticky.clone(),
        };
        if self.trap_state == TRAP_STATE_PRIMED {
            self.trap_return -= 1;
        }
        self.noerrexit &= !NOERREXIT_RETURN;
        if noreturnval {
            fs.pipestats = Some(
                self.pipestats
                    .iter()
                    .take(self.numpipestats)
                    .copied()
                    .collect(),
            );
        }
        self.starttrapscope();
        self.startpatternscope();
        let pptab = self.arrvar(ArrVar::Pparams).to_vec();
        if flags & PM_UNDEFINED == 0 {
            self.scriptname = Some(name.clone());
        }
        if !self.isset(POSIXBUILTINS) {
            self.zoptind = 1;
            self.optcind = 0;
        }
        if sticky_emulation_differs(self.sticky.as_ref(), shfunc.sticky.as_ref()) {
            let st = shfunc.sticky.clone().unwrap_or_default();
            self.emulation = st.emulation;
            fs.restore_sticky = true;
            installemulation(self.emulation, &mut self.opts);
            for &o in &st.n_on_opts {
                if let Some(slot) = usize::try_from(o).ok().and_then(|o| self.opts.get_mut(o)) {
                    *slot = true;
                }
            }
            for &o in &st.n_off_opts {
                if let Some(slot) = usize::try_from(o).ok().and_then(|o| self.opts.get_mut(o)) {
                    *slot = false;
                }
            }
            self.sticky = Some(st);
            self.clearpatterndisables();
        }
        let anon = name.as_slice() == ANONYMOUS_FUNCTION_NAME && shfunc.flags & PM_ANONYMOUS != 0;
        if flags & (PM_TAGGED | PM_TAGGED_LOCAL) != 0 {
            self.opts[XTRACE] = true;
        } else if self.oflags & PM_TAGGED_LOCAL != 0 {
            if anon {
                flags |= PM_TAGGED_LOCAL;
            } else {
                self.opts[XTRACE] = false;
            }
        }
        if flags & PM_WARNNESTED != 0 {
            self.opts[WARNNESTEDVAR] = true;
        } else if self.oflags & PM_WARNNESTED != 0 {
            if anon {
                flags |= PM_WARNNESTED;
            } else {
                self.opts[WARNNESTEDVAR] = false;
            }
        }
        self.oflags = flags;
        self.opts[PRINTEXITVALUE] = false;
        match &doshargs {
            Some(a) => {
                if self.isset(FUNCTIONARGZERO) {
                    fs.argv0 = Some(std::mem::replace(
                        &mut self.argzero,
                        a.first().cloned().unwrap_or_default(),
                    ));
                }
                self.set_arrvar(ArrVar::Pparams, a.iter().skip(1).cloned().collect());
            }
            None => {
                self.set_arrvar(ArrVar::Pparams, Vec::new());
                if self.isset(FUNCTIONARGZERO) {
                    fs.argv0 = Some(self.argzero.clone());
                }
            }
        }
        self.funcdepth += 1;
        let funcnest = self.zsh_funcnest;
        let mut skip_to_undo = false;
        if funcnest >= 0 && self.funcdepth > funcnest {
            self.zerr("maximum nested function level reached; increase FUNCNEST?");
            self.lastval = 1;
            skip_to_undo = true;
        }
        if !skip_to_undo {
            let caller = match self.funcstack.last() {
                Some(f) => f.name.clone(),
                None => fs.argv0.clone().unwrap_or_else(|| self.argzero.clone()),
            };
            let entry = Funcstack {
                name: name.clone(),
                caller,
                lineno: self.lineno,
                tp: FS_FUNC,
                flineno: shfunc.lineno,
                filename: Shell::getshfuncfile(&name, shfunc),
            };
            self.funcstack.push(entry);
            if flags & PM_UNDEFINED != 0 {
                // An autoloaded function: load it inside its own scope.
                self.runshfunc_autoload(&fname, noreturnval);
            } else if let Some(prog) = shfunc.funcdef.clone() {
                if prog.flags & EF_RUN != 0 {
                    let mut p = prog.clone();
                    p.flags &= !EF_RUN;
                    if let Some(f) = self.shfunctab.get_mut(&fname) {
                        f.funcdef = Some(p.clone());
                    }
                    self.runshfunc(&p);
                    match self.shfunctab.get(&fname).and_then(|f| f.funcdef.clone()) {
                        Some(p2)
                            if self
                                .shfunctab
                                .get(&fname)
                                .is_some_and(|f| f.flags & PM_UNDEFINED == 0) =>
                        {
                            self.runshfunc(&p2)
                        }
                        _ => {
                            self.zwarn(&format!("{}: function not defined by file", lossy(&fname)));
                            if noreturnval {
                                self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
                            } else {
                                self.lastval = 1;
                            }
                        }
                    }
                } else {
                    self.runshfunc(&prog);
                }
            }
            let _ = self.funcstack.pop();
        }
        self.funcdepth -= 1;
        if self.retflag {
            self.retflag = false;
            self.this_noerrexit = false;
            self.breaks = fs.breaks;
        }
        self.set_arrvar(ArrVar::Pparams, pptab);
        if let Some(a0) = fs.argv0.take() {
            self.argzero = a0;
        }
        if !self.isset(POSIXBUILTINS) {
            self.zoptind = fs.zoptind;
            self.optcind = fs.optcind;
        }
        self.scriptname = fs.scriptname.take();
        self.oflags = fs.oflags;
        self.endpatternscope();
        if fs.restore_sticky {
            self.opts = fs.opts;
            self.emulation = fs.emulation;
            self.sticky = fs.sticky.take();
        } else if self.isset(LOCALOPTIONS) {
            fs.opts[PRIVILEGED] = self.opts[PRIVILEGED];
            fs.opts[RESTRICTED] = self.opts[RESTRICTED];
            self.opts = fs.opts;
            self.emulation = fs.emulation;
        } else {
            for o in [
                XTRACE,
                PRINTEXITVALUE,
                LOCALOPTIONS,
                LOCALLOOPS,
                WARNNESTEDVAR,
            ] {
                if let (Some(d), Some(&v)) = (self.opts.get_mut(o), fs.opts.get(o)) {
                    *d = v;
                }
            }
        }
        if self.opts[LOCALLOOPS] {
            if self.contflag != 0 {
                self.zwarn("`continue' active at end of function scope");
            }
            if self.breaks != 0 {
                self.zwarn("`break' active at end of function scope");
            }
            self.breaks = fs.breaks;
            self.contflag = fs.contflag;
            self.loops = fs.loops;
        }
        self.endtrapscope();
        if self.trap_state == TRAP_STATE_PRIMED {
            self.trap_return += 1;
        }
        let ret = self.lastval;
        self.noerrexit = fs.noerrexit;
        if noreturnval {
            self.lastval = fs.lastval;
            self.numpipestats = fs.numpipestats;
            if let Some(ps) = fs.pipestats {
                for (i, v) in ps.into_iter().enumerate() {
                    if let Some(slot) = self.pipestats.get_mut(i) {
                        *slot = v;
                    }
                }
            }
        }
        self.unqueue_signals();
        if self.exit_pending && self.exit_level > self.locallevel && self.in_exit_trap == 0 {
            if self.locallevel > self.forklevel {
                self.retflag = true;
                self.breaks = self.loops;
            } else {
                self.stopmsg = 1;
                let ev = self.exit_val;
                self.zexit(ev, crate::signals::ZEXIT_NORMAL);
            }
        }
        ret
    }

    /// zsh's `runshfunc`, without module wrappers.
    pub(crate) fn runshfunc(&mut self, prog: &Eprog) {
        self.queue_signals();
        let ou = self.zunderscore.clone();
        self.startparamscope();
        self.execode(prog, true, false, "shfunc");
        self.setunderscore(&ou);
        self.endparamscope();
        self.unqueue_signals();
    }

    /// `runshfunc` on an undefined function: zsh's `execautofn` run as
    /// the function's body.
    fn runshfunc_autoload(&mut self, name: &[u8], _noreturnval: bool) {
        self.queue_signals();
        let ou = self.zunderscore.clone();
        self.startparamscope();
        self.zsh_eval_context.push(b"shfunc".to_vec());
        match self.loadautofn(name, 1, false, false) {
            None => self.lastval = 1,
            Some(shf) => {
                if let Some(fs) = self.funcstack.last_mut()
                    && fs.filename.is_none()
                {
                    fs.filename = Shell::getshfuncfile(name, &shf);
                }
                let oldscriptname = self.scriptname.replace(name.to_vec());
                let oldscriptfilename =
                    std::mem::replace(&mut self.scriptfilename, Shell::getshfuncfile(name, &shf));
                if let Some(prog) = shf.funcdef.clone() {
                    self.execode(&prog, true, false, "loadautofunc");
                }
                self.scriptname = oldscriptname;
                self.scriptfilename = oldscriptfilename;
            }
        }
        let _ = self.zsh_eval_context.pop();
        self.setunderscore(&ou);
        self.endparamscope();
        self.unqueue_signals();
    }

    /// Call the function named `name` with `args`.
    pub(crate) fn doshfunc_by_name(
        &mut self,
        name: &[u8],
        args: Vec<Vec<u8>>,
        noreturnval: bool,
    ) -> i32 {
        match self.getshfunc(name) {
            Some(shf) => self.doshfunc(&shf, Some(args), noreturnval),
            None => 1,
        }
    }

    /// The name of the innermost function being run, if any.
    pub(crate) fn innermost_function_name(&self) -> Option<Vec<u8>> {
        self.funcstack
            .iter()
            .rev()
            .find(|f| f.tp == FS_FUNC)
            .map(|f| f.name.clone())
    }

    /// Whether the function being run is traced (`functions -t`).
    pub(crate) fn current_function_traced(&self) -> bool {
        self.oflags & (PM_TAGGED | PM_TAGGED_LOCAL) != 0
    }

    /// zsh's `cancd`.
    pub(crate) fn cancd(&mut self, s: &[u8]) -> Option<Vec<u8>> {
        let at = |i: usize| s.get(i).copied().unwrap_or(0);
        let nocdpath = at(0) == b'.'
            && (at(1) == b'/' || at(1) == 0 || (at(1) == b'.' && (at(2) == b'/' || at(1) == 0)));
        if at(0) != b'/' {
            if self.cancd2(s) {
                return Some(s.to_vec());
            }
            if let Ok(c) = std::ffi::CString::new(tok::unmetafy(s))
                // SAFETY: c is NUL-terminated.
                && unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0
            {
                return None;
            }
            if !nocdpath {
                for cp in self.arrvar(ArrVar::Cdpath).to_vec() {
                    let mut sbuf = cp.clone();
                    if !cp.is_empty() {
                        sbuf.push(b'/');
                    }
                    sbuf.extend_from_slice(s);
                    if self.cancd2(&sbuf) {
                        self.doprintdir = -1;
                        return Some(sbuf);
                    }
                }
            }
            if let Some(t) = self.cd_able_vars(s)
                && self.cancd2(&t)
            {
                self.doprintdir = -1;
                return Some(t);
            }
            return None;
        }
        if self.cancd2(s) {
            Some(s.to_vec())
        } else {
            None
        }
    }

    /// zsh's `cancd2`.
    fn cancd2(&mut self, s: &[u8]) -> bool {
        let us = if !self.isset(CHASEDOTS) && !self.isset(CHASELINKS) {
            let mut u = if s.first() != Some(&b'/') {
                let mut p = if self.pwd.len() > 1 {
                    self.pwd.clone()
                } else {
                    Vec::new()
                };
                p.push(b'/');
                p.extend_from_slice(s);
                p
            } else {
                s.to_vec()
            };
            let _ = self.fixdir(&mut u);
            tok::unmetafy(&u)
        } else {
            tok::unmetafy(s)
        };
        crate::sysutil::access_bytes(&us, libc::X_OK)
            && crate::sysutil::stat_bytes(&us)
                .is_some_and(|st| st.st_mode & libc::S_IFMT == libc::S_IFDIR)
    }
}

/// zsh's `sticky_emulation_differs`.
fn sticky_emulation_differs(
    cur: Option<&crate::shell::Sticky>,
    new: Option<&crate::shell::Sticky>,
) -> bool {
    let Some(n) = new else { return false };
    let Some(c) = cur else { return true };
    c.emulation != n.emulation || c.n_on_opts != n.n_on_opts || c.n_off_opts != n.n_off_opts
}

/// zsh's `loadautofnsetfile`.
fn loadautofnsetfile(shf: &mut Shfunc, fdir: Option<Vec<u8>>) {
    if shf.flags & PM_LOADDIR == 0 || shf.filename != fdir {
        match fdir {
            Some(d) => {
                shf.flags |= PM_LOADDIR;
                shf.filename = Some(d);
            }
            None => {
                shf.flags &= !PM_LOADDIR;
                shf.filename = None;
            }
        }
    }
}

/// zsh's `stripkshdef`: a file that only defines `name` becomes that
/// function's body.
pub(crate) fn stripkshdef(prog: Eprog, name: &[u8]) -> Eprog {
    let [item] = prog.list.items.as_slice() else {
        return prog;
    };
    if item.mode != ListMode::Sync
        || !item.sublist.rest.is_empty()
        || crate::exec_list::list_complex(&prog.list)
    {
        return prog;
    }
    let Some(pl) = &item.sublist.first.pipeline else {
        return prog;
    };
    let [cmd] = pl.cmds.as_slice() else {
        return prog;
    };
    let CmdKind::FuncDef { names, body, .. } = &cmd.kind else {
        return prog;
    };
    let [fname] = names.as_slice() else {
        return prog;
    };
    let same = fname.len() == name.len()
        && fname.iter().zip(name.iter()).all(|(&a, &b)| {
            a == b || ((a == b'-' || a == tok::DASH) && (b == b'-' || b == tok::DASH))
        });
    if !same {
        return prog;
    }
    Eprog::from_rc(Rc::clone(body))
}
