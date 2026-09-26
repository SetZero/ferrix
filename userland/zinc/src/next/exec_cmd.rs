//! Command execution, part three (zsh's `exec.c`): one command —
//! `execcmd_exec`, its fork, assignments, saved parameters, and command
//! and process substitution.

use crate::ast::{Assign, AssignValue, CmdKind, Command};
use crate::builtin::*;
use crate::exec::*;
use crate::exec_list::wc_type;
use crate::exec_redir::*;
use crate::jobs::*;
use crate::options::*;
use crate::params::*;
use crate::shell::{ERRFLAG_ERROR, ERRFLAG_INT, Shell, write_fd};
use crate::signals::{child_block, child_unblock, errno};
use crate::subst::{PREFORK_ASSIGN, PREFORK_KEY_VALUE, PREFORK_SINGLE, PREFORK_TYPESET, WordList};
use crate::sysutil::errmsg;
use crate::tok;
use crate::utils::{has_token, lossy};

/// A parameter saved around a builtin or function (`save_params`).
#[derive(Debug, Clone)]
pub(crate) struct Saved {
    pub(crate) name: Vec<u8>,
    pub(crate) pm: Param,
}

fn is_dash(c: u8) -> bool {
    c == b'-' || c == tok::DASH
}

impl Shell {
    /// zsh's `addvars`.
    pub(crate) fn addvars(&mut self, assigns: &[Assign], addflags: i32) {
        let flags = if addflags & ADDVAR_RESTORE == 0 {
            ASSPM_WARN
        } else {
            0
        };
        let xtr = self.isset(XTRACE);
        if xtr {
            self.printprompt4();
            self.doneps4 = true;
        }
        for a in assigns {
            let mut myflags = flags;
            let mut name = a.name.clone();
            if has_token(&name) {
                tok::untokenize(&mut name);
            }
            if a.append {
                myflags |= ASSPM_AUGMENT;
            }
            if xtr {
                let mut t = name.clone();
                t.extend_from_slice(if a.append { b"+=" } else { b"=" });
                write_fd(self.xtrerr_fd(), &tok::unmetafy(&t));
            }
            let (isstr, words) = match &a.value {
                AssignValue::Scalar(v) => (true, vec![v.clone()]),
                AssignValue::Array(ws) => (false, ws.clone()),
                AssignValue::None => (true, vec![Vec::new()]),
            };
            let mut vl = WordList { words, flags: 0 };
            let htok = vl.words.iter().any(|w| has_token(w));
            if htok {
                let mut prefork_ret = 0;
                self.prefork(
                    &mut vl,
                    if isstr {
                        PREFORK_SINGLE | PREFORK_ASSIGN
                    } else {
                        PREFORK_ASSIGN
                    },
                    &mut prefork_ret,
                );
                if self.errflag() {
                    return;
                }
                if prefork_ret & PREFORK_KEY_VALUE != 0 {
                    myflags |= ASSPM_KEY_VALUE;
                }
                let globnow = !isstr
                    || (self.isset(GLOBASSIGN)
                        && vl.words.first().is_some_and(|w| {
                            let mut w = w.clone();
                            self.haswilds(&mut w)
                        }));
                if globnow {
                    self.globlist(&mut vl, prefork_ret);
                    if self.isset(GLOBASSIGN) && isstr {
                        self.unsetparam(&name);
                    }
                    if self.errflag() {
                        return;
                    }
                }
            }
            if isstr && vl.words.len() <= 1 {
                let mut val = vl.words.pop().unwrap_or_default();
                tok::untokenize(&mut val);
                if xtr {
                    let mut t = self.quotedzputs_out(&val);
                    t.push(b' ');
                    write_fd(self.xtrerr_fd(), &t);
                }
                if addflags & ADDVAR_EXPORT != 0 && !name.contains(&b'[') {
                    if addflags & ADDVAR_RESTRICT != 0
                        && self.isset(RESTRICTED)
                        && self
                            .paramtab()
                            .get(&name)
                            .is_some_and(|pm| pm.flags & PM_RESTRICTED != 0)
                    {
                        self.zerr(&format!("{}: restricted", lossy(&name)));
                        return;
                    }
                    if name == b"STTY" {
                        self.sttyval = Some(val.clone());
                    }
                    let allexp = self.opts[ALLEXPORT];
                    self.opts[ALLEXPORT] = true;
                    if self.isset(KSHARRAYS) {
                        self.unsetparam(&name);
                    }
                    let _ = self.assignsparam(&name, val, myflags);
                    self.opts[ALLEXPORT] = allexp;
                } else {
                    let _ = self.assignsparam(&name, val, myflags);
                }
                if self.errflag() {
                    return;
                }
                continue;
            }
            let arr = vl.words;
            if xtr {
                let mut t = b"( ".to_vec();
                for w in &arr {
                    t.extend(self.quotedzputs_out(w));
                    t.push(b' ');
                }
                t.extend_from_slice(b") ");
                write_fd(self.xtrerr_fd(), &t);
            }
            let _ = self.assignaparam(&name, arr, myflags);
            if self.errflag() {
                return;
            }
        }
    }

    /// zsh's `execsubst`.
    pub(crate) fn execsubst(&mut self, strs: &mut WordList) {
        let mut rf = 0;
        let ep = self.esprefork;
        self.prefork(strs, ep, &mut rf);
        if self.esglob && !self.errflag() {
            self.globlist(strs, 0);
        }
    }

    /// zsh's `save_params`.
    fn save_params(&mut self, assigns: &[Assign]) -> (Vec<Saved>, Vec<Vec<u8>>) {
        let mut restore = Vec::new();
        let mut remove = Vec::new();
        for a in assigns {
            let mut s = a.name.clone();
            tok::untokenize(&mut s);
            if let Some(pm) = self.paramtab().get(&s).cloned() {
                if pm.env {
                    self.delenv_name(&s);
                }
                if pm.flags & PM_SPECIAL == 0
                    || (pm.flags & PM_READONLY == 0
                        && (self.unset_opt(RESTRICTED) || pm.flags & PM_RESTRICTED == 0))
                {
                    let mut copy = pm.clone();
                    if pm.flags & PM_SPECIAL != 0 {
                        copy.u = self.special_value(&s, &pm);
                    }
                    restore.push(Saved {
                        name: s.clone(),
                        pm: copy,
                    });
                }
                remove.push(s);
            } else {
                remove.push(s);
            }
        }
        (restore, remove)
    }

    /// The value of a special parameter, as `copyparam` takes it.
    fn special_value(&mut self, name: &[u8], pm: &Param) -> U {
        let r = PmRef::Name(name.to_vec());
        match pm_type(pm.flags) {
            PM_INTEGER => U::Int(self.getifn(&r)),
            PM_EFLOAT | PM_FFLOAT => U::Float(self.getffn(&r)),
            PM_ARRAY => U::Arr(self.getafn(&r)),
            PM_HASHED => pm.u.clone(),
            _ => U::Str(self.getsfn(&r)),
        }
    }

    /// zsh's `restore_params`.
    fn restore_params(&mut self, restorelist: Vec<Saved>, removelist: Vec<Vec<u8>>) {
        for s in removelist {
            if self
                .paramtab()
                .get(&s)
                .is_some_and(|pm| pm.flags & PM_SPECIAL == 0)
            {
                if let Some(pm) = self.paramtab_mut().get_mut(&s) {
                    pm.flags &= !PM_READONLY;
                }
                let mut r = PmRef::Name(s.clone());
                let _ = self.unsetparam_pm(&mut r, false, false);
            }
        }
        for saved in restorelist {
            let name = saved.name.clone();
            let pm = saved.pm;
            if pm.flags & PM_SPECIAL != 0 {
                let tenv = self.paramtab().get(&name).is_some_and(|t| t.env);
                if !pm.env && tenv {
                    self.delenv_name(&name);
                }
                if let Some(t) = self.paramtab_mut().get_mut(&name) {
                    t.flags = pm.flags;
                }
                let mut r = PmRef::Name(name.clone());
                match (pm_type(pm.flags), pm.u) {
                    (PM_INTEGER, U::Int(v)) => self.setifn(&mut r, v),
                    (PM_EFLOAT | PM_FFLOAT, U::Float(v)) => self.setffn(&mut r, v),
                    (PM_ARRAY, U::Arr(v)) => self.setafn(&mut r, Some(v)),
                    (PM_HASHED, U::Hash(h)) => self.sethfn(&mut r, Some(*h)),
                    (_, U::Str(v)) => self.setsfn(&mut r, Some(v)),
                    _ => {}
                }
            } else {
                let _ = self.paramtab_mut().insert(name.clone(), pm);
            }
            let exported = self
                .paramtab()
                .get(&name)
                .is_some_and(|p| p.flags & PM_EXPORTED != 0);
            if exported && let Some(s) = self.getsparam(&name) {
                self.addenv(&name, &s, 0);
            }
        }
    }

    /// zsh's `execcmd_fork`: the pid in the parent, 0 in the child, -1 on
    /// failure.
    fn execcmd_fork(
        &mut self,
        how: i32,
        typ: i32,
        varspc: &[Assign],
        text: Option<&[u8]>,
        oautocont: i32,
        close_if_forked: i32,
    ) -> i32 {
        child_block();
        let mut synch = [0i32; 2];
        // SAFETY: synch is a valid two-element array.
        if unsafe { libc::pipe(synch.as_mut_ptr()) } < 0 {
            self.zerr(&format!("pipe failed: {}", errmsg(errno())));
            return -1;
        }
        let mut bgtime = (0, 0);
        let pid = self.zfork(Some(&mut bgtime));
        if pid == -1 {
            let _ = crate::sysutil::close_fd(synch[0]);
            let _ = crate::sysutil::close_fd(synch[1]);
            self.lastval = 1;
            self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
            return -1;
        }
        if pid != 0 {
            // SAFETY: closing the write end in the parent.
            unsafe {
                libc::close(synch[1]);
            }
            let mut buf = [0u8; 8];
            let _ = self.read_loop(synch[0], &mut buf);
            // SAFETY: closing the read end.
            unsafe {
                libc::close(synch[0]);
            }
            let gleader = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
            let lpj = i32::from_ne_bytes([buf[4], buf[5], buf[6], buf[7]]);
            if how & Z_ASYNC != 0 {
                self.lastpid = i64::from(pid);
            } else if !self.job(self.thisjob).is_some_and(|j| j.stty_in_env)
                && varspc.iter().any(|a| a.name.as_slice() == b"STTY")
            {
                let tj = self.thisjob;
                if let Some(j) = self.job_mut(tj) {
                    j.stty_in_env = true;
                }
            }
            self.addproc(pid, text, false, bgtime, gleader, lpj);
            if oautocont >= 0 {
                self.opts[AUTOCONTINUE] = oautocont != 0;
            }
            let tj = self.thisjob;
            self.pipecleanfilelist_job(tj, true);
            return pid;
        }
        // SAFETY: closing the read end in the child.
        unsafe {
            libc::close(synch[0]);
        }
        let mut flags = (if how & Z_ASYNC != 0 { ESUB_ASYNC } else { 0 }) | ESUB_PGRP;
        if typ != WC_SUBSH && how & Z_ASYNC == 0 {
            flags |= ESUB_KEEPTRAP;
        }
        if typ == WC_SUBSH && how & Z_ASYNC == 0 {
            flags |= ESUB_JOB_CONTROL;
        }
        let (gl, lpj) = self.entersubsh(flags);
        let mut buf = [0u8; 8];
        buf[..4].copy_from_slice(&gl.to_ne_bytes());
        buf[4..].copy_from_slice(&lpj.to_ne_bytes());
        if self.write_loop(synch[1], &buf) != 8 {
            self.zerr(&format!(
                "Failed to send entersubsh_ret report: {}",
                errmsg(errno())
            ));
            return -1;
        }
        // SAFETY: closing the write end.
        unsafe {
            libc::close(synch[1]);
        }
        let _ = self.zclose(close_if_forked);
        if self
            .sigtrapped
            .get(libc::SIGINT as usize)
            .copied()
            .unwrap_or(0)
            & crate::signals::ZSIG_IGNORED
            != 0
        {
            self.holdintr();
        }
        if let Some(t) = self.sigtrapped.get_mut(0) {
            *t = 0;
        }
        if how & Z_ASYNC != 0 && self.isset(BGNICE) {
            set_errno(0);
            // SAFETY: nice has no memory preconditions.
            if unsafe { libc::nice(5) } == -1 && errno() != 0 {
                self.zwarn(&format!("nice(5) failed: {}", errmsg(errno())));
            }
        }
        0
    }

    /// Expand the next word of `args` into `preargs` (`execcmd_getargs`).
    fn execcmd_getargs(
        &mut self,
        preargs: &mut Vec<Vec<u8>>,
        args: &mut Vec<Vec<u8>>,
        expand: bool,
    ) {
        if args.is_empty() {
            return;
        }
        let w = args.remove(0);
        if expand {
            let mut svl = WordList::one(w);
            let mut rf = 0;
            self.prefork(&mut svl, 0, &mut rf);
            preargs.extend(svl.words);
        } else {
            preargs.push(w);
        }
    }

    /// zsh's `execcmd_exec`.
    #[expect(clippy::too_many_lines, reason = "zsh's execcmd_exec")]
    pub(crate) fn execcmd_exec(
        &mut self,
        cmd: &Command,
        input: i32,
        output: i32,
        how: i32,
        last1: i32,
        close_if_forked: i32,
    ) {
        let mut how = how;
        let mut last1 = last1;
        let typ = wc_type(cmd);
        let (varspc, words, postassigns): CmdParts<'_> = match &cmd.kind {
            CmdKind::Simple { assigns, words } => {
                (assigns, (!words.is_empty()).then(|| words.clone()), None)
            }
            CmdKind::Typeset {
                assigns,
                words,
                args,
            } => (assigns, Some(words.clone()), Some(args)),
            _ => (&[], None, None),
        };
        let htok = words
            .as_ref()
            .is_some_and(|w| w.iter().any(|x| has_token(x)));
        let mut args = words;
        let mut redir: Vec<XRedir> = xredirs(&cmd.redirs);
        let mut save = [-2i32; 10];
        let mut mfds: Mfds = Default::default();
        let mut text: Option<Vec<u8>> = None;
        let mut hn: Option<BuiltinOrFunc> = None;
        let (mut is_shfunc, mut is_builtin, mut is_exec, mut use_defpath) =
            (false, false, false, false);
        let (mut cflags, mut orig_cflags, mut checked) = (0u32, 0u32, false);
        let mut oautocont = -1;
        let mut do_exec = false;
        let mut nullexec = 0;
        let mut magic_assign = false;
        let mut forked = false;
        let oxtrerr = self.xtrerr;
        let mut newxtrerr = -1;
        let mut redir_err = false;
        let mut filelist_forked = false;
        self.doneps4 = false;
        let old_lastval = self.lastval;
        if args.is_none() && !varspc.is_empty() {
            self.lastval = if self.errflag() {
                self.errflag.get()
            } else {
                self.cmdoutval
            };
        }
        self.use_cmdoutval = args.is_none();

        if (typ == WC_SIMPLE || typ == WC_TYPESET)
            && let Some(a) = args.as_mut()
            && a.first().is_some_and(|w| w.first() == Some(&b'%'))
        {
            if how & Z_DISOWN != 0 {
                oautocont = i32::from(self.opts[AUTOCONTINUE]);
                self.opts[AUTOCONTINUE] = true;
            }
            let word: &[u8] = if how & Z_DISOWN != 0 {
                b"disown"
            } else if how & Z_ASYNC != 0 {
                b"bg"
            } else {
                b"fg"
            };
            a.insert(0, word.to_vec());
            how = Z_SYNC;
        }
        if self.isset(AUTORESUME)
            && typ == WC_SIMPLE
            && how & Z_SYNC != 0
            && args.as_ref().is_some_and(|a| a.len() == 1)
            && redir.is_empty()
            && input == 0
        {
            if self.unset_opt(NOTIFY) {
                self.scanjobs();
            }
            let first = args
                .as_ref()
                .and_then(|a| a.first().cloned())
                .unwrap_or_default();
            if self.findjobnam(&first) != -1
                && let Some(a) = args.as_mut()
            {
                a.insert(0, b"fg".to_vec());
            }
        }
        if how & Z_ASYNC != 0
            || output != 0
            || (last1 == 2 && input != 0 && self.emulation_is(EMULATE_SH))
        {
            let t = self.getjobtext_cmd(cmd);
            text = Some(t);
            match self.execcmd_fork(
                how,
                typ,
                varspc,
                text.as_deref(),
                oautocont,
                close_if_forked,
            ) {
                -1 => {
                    self.execcmd_fatal(false, forked, cflags, orig_cflags, redir_err);
                    return;
                }
                0 => {}
                _ => return,
            }
            last1 = 1;
            forked = true;
            filelist_forked = true;
        }

        let mut preargs: Vec<Vec<u8>> = Vec::new();
        if (typ == WC_SIMPLE || typ == WC_TYPESET)
            && let Some(mut a) = args.take()
        {
            self.execcmd_getargs(&mut preargs, &mut a, htok);
            while let Some(cmdarg) = preargs.first().cloned() {
                checked = !has_token(&cmdarg);
                if !checked {
                    break;
                }
                let found: Option<Builtin>;
                if typ == WC_TYPESET && self.builtintab.get(&cmdarg).is_some() {
                    found = self.builtintab.get(&cmdarg).cloned();
                    checked = true;
                } else if self.isset(POSIXBUILTINS) && cflags & BINF_EXEC != 0 {
                    break;
                } else {
                    if cflags & (BINF_BUILTIN | BINF_COMMAND) == 0
                        && self.getshfunc(&cmdarg).is_some()
                    {
                        hn = Some(BuiltinOrFunc::Func(cmdarg.clone()));
                        is_shfunc = true;
                        break;
                    }
                    found = self
                        .builtintab
                        .get(&cmdarg)
                        .filter(|b| b.flags & crate::tables::DISABLED == 0)
                        .cloned();
                    if found.is_none() {
                        checked = cflags & BINF_BUILTIN == 0;
                        break;
                    }
                }
                let Some(b) = found else { break };
                orig_cflags |= cflags;
                cflags &= !BINF_BUILTIN & !BINF_COMMAND;
                cflags |= b.flags;
                if b.flags & BINF_PREFIX == 0 {
                    is_builtin = true;
                    if typ != WC_TYPESET {
                        magic_assign = b.flags & BINF_MAGICEQUALS != 0;
                    }
                    hn = Some(BuiltinOrFunc::Builtin(cmdarg.clone(), b));
                    break;
                }
                checked = false;
                let _ = preargs.remove(0);
                if preargs.is_empty() {
                    self.execcmd_getargs(&mut preargs, &mut a, htok);
                    if preargs.is_empty() {
                        break;
                    }
                }
                if cflags & BINF_COMMAND != 0 {
                    let (mut has_p, mut has_vv, mut has_other) = (false, false, false);
                    let mut idx = 0usize;
                    let mut pidx: Option<usize> = None;
                    while let Some(argdata) = preargs.get(idx).cloned() {
                        if !argdata.first().is_some_and(|&c| is_dash(c)) {
                            break;
                        }
                        if argdata.len() == 1
                            || (argdata.len() == 2 && argdata.get(1).is_some_and(|&c| is_dash(c)))
                        {
                            break;
                        }
                        for &c in argdata.get(1..).unwrap_or(&[]) {
                            match c {
                                b'p' => {
                                    has_p = true;
                                    pidx = Some(idx);
                                }
                                b'v' | b'V' => has_vv = true,
                                _ => has_other = true,
                            }
                        }
                        if has_other {
                            has_p = false;
                            has_vv = false;
                            break;
                        }
                        idx += 1;
                        if idx >= preargs.len() {
                            self.execcmd_getargs(&mut preargs, &mut a, htok);
                            if idx >= preargs.len() {
                                break;
                            }
                        }
                    }
                    if has_vv {
                        preargs.insert(0, b"command".to_vec());
                        hn = Some(BuiltinOrFunc::CommandWhence);
                        is_builtin = true;
                        break;
                    } else if has_p {
                        use_defpath = true;
                        if let Some(p) = pidx {
                            let _ = preargs.remove(p);
                            idx = idx.saturating_sub(1);
                        }
                    }
                    if preargs
                        .get(idx)
                        .is_some_and(|d| d.len() == 2 && d.iter().all(|&c| is_dash(c)))
                    {
                        let _ = preargs.remove(idx);
                    }
                } else if cflags & BINF_EXEC != 0 {
                    let mut exec_argv0: Option<Vec<u8>> = None;
                    while let Some(argdata) = preargs.first().cloned() {
                        if !(argdata.first().is_some_and(|&c| is_dash(c)) && argdata.len() >= 2) {
                            break;
                        }
                        if preargs.len() < 2 {
                            self.execcmd_getargs(&mut preargs, &mut a, htok);
                        }
                        if preargs.len() < 2 {
                            self.zerr("exec requires a command to execute");
                            self.lastval = 1;
                            self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
                            self.execcmd_done(
                                forked,
                                cflags,
                                orig_cflags,
                                redir_err,
                                newxtrerr,
                                oxtrerr,
                                oautocont,
                            );
                            return;
                        }
                        let _ = preargs.remove(0);
                        if argdata.len() == 2 && argdata.iter().all(|&c| is_dash(c)) {
                            break;
                        }
                        let mut ci = 1usize;
                        while let Some(&c) = argdata.get(ci) {
                            match c {
                                b'a' => {
                                    if ci + 1 < argdata.len() {
                                        exec_argv0 =
                                            Some(argdata.get(ci + 1..).unwrap_or(&[]).to_vec());
                                        ci = argdata.len();
                                        continue;
                                    }
                                    if preargs.len() < 2 {
                                        self.execcmd_getargs(&mut preargs, &mut a, htok);
                                    }
                                    if preargs.len() < 2 {
                                        self.zerr("exec flag -a requires a parameter");
                                        self.lastval = 1;
                                        self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
                                        self.execcmd_done(
                                            forked,
                                            cflags,
                                            orig_cflags,
                                            redir_err,
                                            newxtrerr,
                                            oxtrerr,
                                            oautocont,
                                        );
                                        return;
                                    }
                                    exec_argv0 = Some(preargs.remove(0));
                                }
                                b'c' => cflags |= BINF_CLEARENV,
                                b'l' => cflags |= BINF_DASH,
                                other => {
                                    self.zerr(&format!("unknown exec flag -{}", char::from(other)));
                                    self.lastval = 1;
                                    self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
                                    if forked {
                                        self._realexit();
                                    }
                                    return;
                                }
                            }
                            ci += 1;
                        }
                    }
                    if let Some(mut a0) = exec_argv0 {
                        crate::utils::remnulargs(&mut a0);
                        tok::untokenize(&mut a0);
                        let mut s = b"ARGV0=".to_vec();
                        s.extend(a0);
                        self.zputenv(&s);
                    }
                }
                hn = None;
                if cflags & BINF_COMMAND != 0 && self.unset_opt(POSIXBUILTINS) {
                    break;
                }
                if preargs.is_empty() {
                    self.execcmd_getargs(&mut preargs, &mut a, htok);
                }
            }
            args = Some(a);
        }

        if self.noerrexit & crate::signals::NOERREXIT_UNTIL_EXEC != 0 {
            self.noerrexit = 0;
        }
        self.esprefork = if magic_assign || (self.isset(MAGICEQUALSUBST) && typ != WC_TYPESET) {
            PREFORK_TYPESET
        } else {
            0
        };
        let mut args: Option<Vec<Vec<u8>>> = match args {
            Some(a) => {
                let mut wl = WordList { words: a, flags: 0 };
                if htok {
                    let mut rf = 0;
                    let ep = self.esprefork;
                    self.prefork(&mut wl, ep, &mut rf);
                }
                let mut joined = preargs;
                joined.extend(wl.words);
                Some(joined)
            }
            None if !preargs.is_empty() => Some(preargs),
            None => None,
        };

        if typ == WC_SIMPLE || typ == WC_TYPESET {
            let mut unglobbed = false;
            loop {
                if cflags & BINF_NOGLOB == 0 {
                    while !checked
                        && !self.errflag()
                        && args
                            .as_ref()
                            .is_some_and(|a| a.first().is_some_and(|w| has_token(w)))
                    {
                        if let Some(a) = args.as_mut() {
                            let _ = self.zglob(a, 0, false);
                        }
                    }
                } else if !unglobbed {
                    if let Some(a) = args.as_mut() {
                        for w in a.iter_mut() {
                            tok::untokenize(w);
                        }
                    }
                    unglobbed = true;
                }
                if cflags & BINF_EXEC != 0 && last1 != 0 {
                    do_exec = true;
                }
                if args.as_ref().is_none_or(Vec::is_empty) {
                    if !redir.is_empty() {
                        if do_exec {
                            nullexec = 1;
                            break;
                        } else if !varspc.is_empty() {
                            nullexec = 2;
                            break;
                        } else if self.nullcmd.as_ref().is_none_or(Vec::is_empty)
                            || self.opts[CSHNULLCMD]
                            || cflags & BINF_PREFIX != 0
                        {
                            self.zerr("redirection with no command");
                            self.lastval = 1;
                            self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
                            if forked {
                                self._realexit();
                            }
                            return;
                        } else if self.opts[SHNULLCMD] {
                            args.get_or_insert_with(Vec::new).push(b":".to_vec());
                        } else if self.readnullcmd.as_ref().is_some_and(|r| !r.is_empty())
                            && redir.len() == 1
                            && redir.first().is_some_and(|r| r.typ == REDIR_READ)
                        {
                            let r = self.readnullcmd.clone().unwrap_or_default();
                            args.get_or_insert_with(Vec::new).push(r);
                        } else {
                            let n = self.nullcmd.clone().unwrap_or_default();
                            args.get_or_insert_with(Vec::new).push(n);
                        }
                    } else if cflags & BINF_PREFIX != 0 && cflags & BINF_COMMAND != 0 {
                        self.lastval = 0;
                        if forked {
                            self._realexit();
                        }
                        return;
                    } else {
                        if self.badcshglob == 1 {
                            self.zerr("no match");
                            self.lastval = 1;
                            if forked {
                                self._realexit();
                            }
                            return;
                        }
                        self.cmdoutval = if self.use_cmdoutval { self.lastval } else { 0 };
                        if !varspc.is_empty() {
                            self.lastval = old_lastval;
                            self.addvars(varspc, 0);
                        }
                        self.lastval = if self.errflag() { 1 } else { self.cmdoutval };
                        if self.isset(XTRACE) {
                            write_fd(self.xtrerr_fd(), b"\n");
                        }
                        if forked {
                            self._realexit();
                        }
                        return;
                    }
                } else if self.isset(RESTRICTED) && cflags & BINF_EXEC != 0 && do_exec {
                    let first = args
                        .as_ref()
                        .and_then(|a| a.first().cloned())
                        .unwrap_or_default();
                    self.zerrnam("exec", &format!("{}: restricted", lossy(&first)));
                    self.lastval = 1;
                    if forked {
                        self._realexit();
                    }
                    return;
                }
                let stop = if self.isset(POSIXBUILTINS) {
                    cflags & BINF_EXEC != 0
                } else {
                    cflags & BINF_COMMAND != 0
                };
                if self.errflag() || checked || is_builtin || stop {
                    break;
                }
                let cmdarg = args
                    .as_ref()
                    .and_then(|a| a.first().cloned())
                    .unwrap_or_default();
                if cflags & (BINF_BUILTIN | BINF_COMMAND) == 0 && self.getshfunc(&cmdarg).is_some()
                {
                    hn = Some(BuiltinOrFunc::Func(cmdarg));
                    is_shfunc = true;
                    break;
                }
                let Some(b) = self
                    .builtintab
                    .get(&cmdarg)
                    .filter(|b| b.flags & crate::tables::DISABLED == 0)
                    .cloned()
                else {
                    if cflags & BINF_BUILTIN != 0 {
                        self.zwarn(&format!("no such builtin: {}", lossy(&cmdarg)));
                        self.lastval = 1;
                        if oautocont >= 0 {
                            self.opts[AUTOCONTINUE] = oautocont != 0;
                        }
                        if forked {
                            self._realexit();
                        }
                        return;
                    }
                    break;
                };
                if b.flags & BINF_PREFIX == 0 {
                    is_builtin = true;
                    hn = Some(BuiltinOrFunc::Builtin(cmdarg, b));
                    break;
                }
                cflags &= !BINF_BUILTIN & !BINF_COMMAND;
                cflags |= b.flags;
                if let Some(a) = args.as_mut() {
                    let _ = a.remove(0);
                }
                hn = None;
            }
        }

        if self.errflag() {
            if self.lastval == 0 {
                self.lastval = 1;
            }
            if oautocont >= 0 {
                self.opts[AUTOCONTINUE] = oautocont != 0;
            }
            if forked {
                self._realexit();
            }
            return;
        }
        if text.is_none() && self.sfcontext == 0 && (self.jobbing() || how & Z_TIMED != 0) {
            text = Some(self.getjobtext_cmd(cmd));
        }
        if typ != WC_FUNCDEF {
            let last = args
                .as_ref()
                .and_then(|a| a.last().cloned())
                .unwrap_or_default();
            self.setunderscore(&last);
        }
        if typ == WC_SIMPLE
            && self.interact()
            && self.unset_opt(RMSTARSILENT)
            && self.isset(SHINSTDIN)
            && args
                .as_ref()
                .is_some_and(|a| a.len() > 1 && a.first().is_some_and(|w| w.as_slice() == b"rm"))
        {
            let rest: Vec<Vec<u8>> = args
                .as_ref()
                .map(|a| a.iter().skip(1).cloned().collect())
                .unwrap_or_default();
            for s in rest {
                if self.errflag() {
                    break;
                }
                if s.as_slice() == [tok::STAR] {
                    let pwd = self.pwd.clone();
                    if !self.checkrmall(&pwd) {
                        self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
                        break;
                    }
                } else if s.len() >= 2 && s.ends_with(&[b'/', tok::STAR]) {
                    let dir = if s.len() == 2 {
                        b"/".to_vec()
                    } else {
                        s.get(..s.len() - 2).unwrap_or(&[]).to_vec()
                    };
                    if !self.checkrmall(&dir) {
                        self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
                        break;
                    }
                }
            }
        }
        if let CmdKind::FuncDef { names, .. } = &cmd.kind {
            if !names.is_empty() {
                redir.clear();
            }
        } else if let Some(BuiltinOrFunc::Func(fname)) = &hn
            && is_shfunc
            && let Some(r) = self.getshfunc(fname).and_then(|f| f.redir)
        {
            redir.extend(xredirs(&r));
        }
        if self.errflag() {
            self.lastval = 1;
            if oautocont >= 0 {
                self.opts[AUTOCONTINUE] = oautocont != 0;
            }
            if forked {
                self._realexit();
            }
            return;
        }
        if (typ == WC_SIMPLE || typ == WC_TYPESET) && nullexec == 0 {
            let trycd = self.isset(AUTOCD)
                && self.isset(SHINSTDIN)
                && redir.is_empty()
                && args
                    .as_ref()
                    .is_some_and(|a| a.len() == 1 && a.first().is_some_and(|w| !w.is_empty()));
            if hn.is_none() {
                let cmdarg = args
                    .as_ref()
                    .and_then(|a| a.first().cloned())
                    .unwrap_or_default();
                let mut found = self.cmdnamtab.get(&cmdarg).cloned();
                let mut checkfrom = self.pathchecked;
                let mut dohashcmd = self.isset(HASHCMDS);
                if let Some(cn) = &found
                    && trycd
                    && !self.isreallycom(&cmdarg, cn)
                {
                    if cn.flags & crate::tables::HASHED == 0 {
                        checkfrom = 0;
                        dohashcmd = true;
                    }
                    let _ = self.cmdnamtab.remove(&cmdarg);
                    found = None;
                }
                if found.is_none()
                    && dohashcmd
                    && cmdarg.as_slice() != b".."
                    && !cmdarg.contains(&b'/')
                {
                    let n = self.arrvar(ArrVar::Path).len();
                    found = self.hashcmd(&cmdarg, checkfrom, n);
                }
                if found.is_some() {
                    hn = Some(BuiltinOrFunc::External);
                }
            }
            if hn.is_none()
                && trycd
                && let Some(first) = args.as_ref().and_then(|a| a.first().cloned())
                && let Some(s) = self.cancd(&first)
                && let Some(a) = args.as_mut()
            {
                if let Some(f) = a.first_mut() {
                    *f = s;
                }
                a.insert(0, b"--".to_vec());
                a.insert(0, b"cd".to_vec());
                if let Some(b) = self.builtintab.get(b"cd").cloned() {
                    hn = Some(BuiltinOrFunc::Builtin(b"cd".to_vec(), b));
                    is_builtin = true;
                }
            }
        }
        let is_cursh = is_builtin || is_shfunc || nullexec != 0 || typ >= WC_CURSH;
        if !forked {
            if !do_exec
                && (((is_builtin || is_shfunc) && output != 0)
                    || (!is_cursh
                        && (last1 != 1
                            || self.nsigtrapped != 0
                            || self.havefiles()
                            || self.fdtable_flocks != 0)))
            {
                match self.execcmd_fork(
                    how,
                    typ,
                    varspc,
                    text.as_deref(),
                    oautocont,
                    close_if_forked,
                ) {
                    -1 => {
                        self.execcmd_fatal(false, forked, cflags, orig_cflags, redir_err);
                        return;
                    }
                    0 => {}
                    _ => return,
                }
                forked = true;
                filelist_forked = true;
            } else if is_cursh {
                let tj = self.thisjob;
                if let Some(j) = self.job_mut(tj) {
                    j.stat |= STAT_CURSH;
                    if j.procs.is_empty() {
                        j.stat |= STAT_NOPRINT;
                    }
                    if is_builtin {
                        j.stat |= STAT_BUILTIN;
                    }
                }
            } else {
                is_exec = true;
                if typ == WC_SUBSH {
                    forked = true;
                }
            }
        }
        let _ = filelist_forked;
        self.esglob = cflags & BINF_NOGLOB == 0;
        if self.esglob
            && htok
            && let Some(a) = args.take()
        {
            let mut wl = WordList { words: a, flags: 0 };
            self.globlist(&mut wl, 0);
            args = Some(wl.words);
        }
        if self.errflag() {
            self.lastval = 1;
            self.execcmd_err(forked, &save);
            self.execcmd_done(
                forked,
                cflags,
                orig_cflags,
                redir_err,
                newxtrerr,
                oxtrerr,
                oautocont,
            );
            return;
        }
        if self.isset(XTRACE) && self.xtrerr < 0 && (typ < WC_SUBSH || typ == WC_TIMED) {
            // SAFETY: dup has no memory preconditions.
            let d = unsafe { libc::dup(2) };
            newxtrerr = self.movefd(d);
            if newxtrerr >= 0 {
                self.xtrerr = newxtrerr;
                self.fdtable_mark(newxtrerr, FDT_XTRACE);
            }
        }
        if input != 0 {
            self.addfd(forked, &mut save, &mut mfds, 0, input, 0, None);
        }
        if output != 0 {
            self.addfd(forked, &mut save, &mut mfds, 1, output, 1, None);
        }
        if !self.do_redirections(&mut redir, forked, &mut save, &mut mfds, nullexec) {
            if forked {
                crate::shell::exit_now(1);
            }
            redir_err = true;
            self.lastval = 1;
            self.execcmd_done(
                forked,
                cflags,
                orig_cflags,
                redir_err,
                newxtrerr,
                oxtrerr,
                oautocont,
            );
            return;
        }
        if nullexec != 0 {
            if !varspc.is_empty() {
                let saved = if !self.isset(POSIXBUILTINS) && nullexec != 2 {
                    Some(self.save_params(varspc))
                } else {
                    None
                };
                self.addvars(varspc, 0);
                if let Some((r, m)) = saved {
                    self.restore_params(r, m);
                }
            }
            self.lastval = if self.errflag() {
                self.errflag.get()
            } else {
                self.cmdoutval
            };
            if nullexec == 1 {
                for &s in &save {
                    if s != -2 {
                        let _ = self.zclose(s);
                    }
                }
                let tj = self.thisjob;
                if let Some(j) = self.job_mut(tj) {
                    j.stat |= STAT_DONE;
                }
                self.execcmd_done(
                    forked,
                    cflags,
                    orig_cflags,
                    redir_err,
                    newxtrerr,
                    oxtrerr,
                    oautocont,
                );
                return;
            }
            if self.isset(XTRACE) {
                write_fd(self.xtrerr_fd(), b"\n");
            }
        } else if self.isset(EXECOPT) && !self.errflag() {
            let q = self.queue_signal_level();
            if is_exec {
                let mut flags =
                    (if how & Z_ASYNC != 0 { ESUB_ASYNC } else { 0 }) | ESUB_PGRP | ESUB_FAKE;
                if typ != WC_SUBSH {
                    flags |= ESUB_KEEPTRAP;
                }
                if (do_exec || (typ >= WC_CURSH && last1 == 1)) && !forked {
                    flags |= ESUB_REVERTPGRP;
                }
                let _ = self.entersubsh(flags);
            }
            if typ == WC_FUNCDEF {
                let redir_prog = if !cmd.redirs.is_empty() {
                    Some(std::rc::Rc::new(cmd.redirs.clone()))
                } else {
                    None
                };
                self.dont_queue_signals();
                self.lastval = self.execfuncdef(cmd, redir_prog);
                self.restore_queue_signals(q);
            } else if typ >= WC_CURSH {
                if last1 == 1 {
                    do_exec = true;
                }
                self.dont_queue_signals();
                self.lastval = self.execconstruct(cmd, do_exec);
                self.restore_queue_signals(q);
            } else if is_builtin || is_shfunc {
                let mut saved: Option<(Vec<Saved>, Vec<Vec<u8>>)> = None;
                let mut do_save = false;
                if !forked {
                    let pspecial = match &hn {
                        Some(BuiltinOrFunc::Builtin(_, b)) => {
                            b.flags & (BINF_PSPECIAL | BINF_ASSIGN) != 0
                        }
                        _ => false,
                    };
                    if self.isset(POSIXBUILTINS) {
                        do_save = if is_shfunc || pspecial {
                            orig_cflags & BINF_COMMAND != 0
                        } else {
                            true
                        };
                    } else if cflags & (BINF_COMMAND | BINF_ASSIGN) != 0 || !magic_assign {
                        do_save = true;
                    }
                    if do_save && !varspc.is_empty() {
                        saved = Some(self.save_params(varspc));
                    }
                }
                if !varspc.is_empty() {
                    let mut flags = 0;
                    if is_shfunc {
                        flags |= ADDVAR_EXPORT;
                    }
                    if saved.is_some() {
                        flags |= ADDVAR_RESTORE;
                    }
                    self.addvars(varspc, flags);
                    if self.errflag() {
                        if let Some((r, m)) = saved.take() {
                            self.restore_params(r, m);
                        }
                        self.lastval = 1;
                        self.fixfds(&save);
                        self.execcmd_done(
                            forked,
                            cflags,
                            orig_cflags,
                            redir_err,
                            newxtrerr,
                            oxtrerr,
                            oautocont,
                        );
                        return;
                    }
                }
                let mut argv = args.take().unwrap_or_default();
                match &hn {
                    Some(BuiltinOrFunc::Func(name)) => {
                        if let Some(shf) = self.getshfunc(name) {
                            self.execshfunc(&shf, argv);
                        }
                    }
                    Some(BuiltinOrFunc::Builtin(name, b)) => {
                        if forked {
                            self.closem(FDT_INTERNAL, false);
                        }
                        let mut assigns = match postassigns {
                            Some(pa) => self.expand_postassigns(pa),
                            None => Vec::new(),
                        };
                        self.dont_queue_signals();
                        if !self.errflag() {
                            let ret = self.execbuiltin(name, &mut argv, &mut assigns, b);
                            if self.errflag.get() & ERRFLAG_INT == 0 {
                                self.lastval = ret;
                            }
                        }
                        if do_save && orig_cflags & BINF_COMMAND != 0 {
                            self.errflag.set(self.errflag.get() & !ERRFLAG_ERROR);
                        }
                        self.restore_queue_signals(q);
                    }
                    Some(BuiltinOrFunc::CommandWhence) => {
                        self.dont_queue_signals();
                        if !self.errflag() {
                            let ret = self.bin_command_whence(&mut argv);
                            if self.errflag.get() & ERRFLAG_INT == 0 {
                                self.lastval = ret;
                            }
                        }
                        self.restore_queue_signals(q);
                    }
                    _ => {}
                }
                if self.isset(PRINTEXITVALUE)
                    && self.isset(SHINSTDIN)
                    && self.lastval != 0
                    && !self.subsh
                {
                    write_fd(2, format!("zsh: exit {}\n", self.lastval).as_bytes());
                }
                if do_exec {
                    if self.subsh {
                        self._realexit();
                    }
                    if self.isset(RCS) && self.interact() && !self.nohistsave {
                        self.savehistfile(None, true, crate::hist::HFILE_USE_OPTIONS);
                    }
                    self.realexit();
                }
                if let Some((r, m)) = saved {
                    self.restore_params(r, m);
                }
            } else {
                if !self.subsh {
                    if !forked {
                        self.shlvl -= 1;
                        let v = self.shlvl;
                        let _ = self.setiparam(b"SHLVL", v);
                    }
                    if do_exec && self.isset(RCS) && self.interact() && !self.nohistsave {
                        self.savehistfile(None, true, crate::hist::HFILE_USE_OPTIONS);
                    }
                }
                if typ == WC_SIMPLE || typ == WC_TYPESET {
                    if !varspc.is_empty() {
                        let mut addflags = ADDVAR_EXPORT | ADDVAR_RESTRICT;
                        if forked {
                            addflags |= ADDVAR_RESTORE;
                        }
                        self.addvars(varspc, addflags);
                        if self.errflag() {
                            crate::shell::exit_now(1);
                        }
                    }
                    self.closem(FDT_INTERNAL, false);
                    if self.coprocin != -1 {
                        let c = self.coprocin;
                        let _ = self.zclose(c);
                        self.coprocin = -1;
                    }
                    if self.coprocout != -1 {
                        let c = self.coprocout;
                        let _ = self.zclose(c);
                        self.coprocout = -1;
                    }
                    if !forked {
                        self.setlimits(None);
                    }
                    if how & Z_ASYNC != 0 {
                        self.sttyval = None;
                    }
                    let mut argv = args.take().unwrap_or_default();
                    self.execute(&mut argv, cflags, use_defpath);
                } else {
                    self.list_pipe = false;
                    if let CmdKind::Subsh(l) = &cmd.kind {
                        self.execlist(l, false, true);
                    }
                }
            }
        }
        self.execcmd_err(forked, &save);
        self.execcmd_done(
            forked,
            cflags,
            orig_cflags,
            redir_err,
            newxtrerr,
            oxtrerr,
            oautocont,
        );
    }

    /// The `err:` label of `execcmd_exec`.
    fn execcmd_err(&mut self, forked: bool, save: &[i32; 10]) {
        if forked {
            for i in 0..10 {
                if self.fdtable_get(i) != FDT_UNUSED {
                    // SAFETY: closing a descriptor number.
                    unsafe {
                        libc::close(i);
                    }
                }
            }
            self.closem(FDT_UNUSED, true);
            if self.thisjob != -1 {
                self.waitjobs();
            }
            self._realexit();
        }
        self.fixfds(save);
    }

    /// The `done:` label of `execcmd_exec`.
    #[expect(
        clippy::too_many_arguments,
        reason = "the locals zsh's done: label reads"
    )]
    fn execcmd_done(
        &mut self,
        forked: bool,
        cflags: u32,
        orig_cflags: u32,
        redir_err: bool,
        newxtrerr: i32,
        oxtrerr: i32,
        oautocont: i32,
    ) {
        if self.isset(POSIXBUILTINS)
            && cflags & (BINF_PSPECIAL | BINF_EXEC) != 0
            && orig_cflags & BINF_COMMAND == 0
        {
            let forked = forked || self.zsh_subshell != 0;
            self.execcmd_fatal(true, forked, cflags, orig_cflags, redir_err);
        }
        if newxtrerr >= 0 {
            let _ = self.zclose(newxtrerr);
            self.xtrerr = oxtrerr;
        }
        self.sttyval = None;
        if oautocont >= 0 {
            self.opts[AUTOCONTINUE] = oautocont != 0;
        }
    }

    /// The `fatal:` label of `execcmd_exec`.
    fn execcmd_fatal(
        &mut self,
        _from_done: bool,
        forked: bool,
        _cflags: u32,
        _orig: u32,
        redir_err: bool,
    ) {
        if redir_err || self.errflag() {
            if !self.isset(INTERACTIVE) {
                if forked {
                    crate::shell::exit_now(1);
                } else {
                    crate::shell::flush_and_exit(1);
                }
            }
            self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
        }
    }

    /// Expand a typeset command's assignments into `Asgment`s.
    fn expand_postassigns(&mut self, pa: &[Assign]) -> Vec<Asgment> {
        let mut assigns = Vec::new();
        for a in pa {
            let mut name = a.name.clone();
            let htok_name = has_token(&name);
            if htok_name && matches!(a.value, AssignValue::None) {
                let mut svl = WordList::one(name);
                let mut rf = 0;
                self.prefork(&mut svl, PREFORK_TYPESET, &mut rf);
                if self.errflag() {
                    break;
                }
                self.globlist(&mut svl, 0);
                if self.errflag() {
                    break;
                }
                for data in svl.words {
                    match data.iter().position(|&c| c == b'=') {
                        Some(p) => assigns.push(Asgment {
                            name: data.get(..p).unwrap_or(&[]).to_vec(),
                            flags: 0,
                            scalar: Some(data.get(p + 1..).unwrap_or(&[]).to_vec()),
                            array: Vec::new(),
                        }),
                        None => assigns.push(Asgment {
                            name: data,
                            flags: 0,
                            scalar: None,
                            array: Vec::new(),
                        }),
                    }
                }
                continue;
            }
            if htok_name {
                let mut svl = WordList::one(name);
                let mut rf = 0;
                self.prefork(&mut svl, PREFORK_SINGLE, &mut rf);
                name = svl.words.into_iter().next().unwrap_or_default();
            }
            tok::untokenize(&mut name);
            match &a.value {
                AssignValue::None => assigns.push(Asgment {
                    name,
                    flags: 0,
                    scalar: None,
                    array: Vec::new(),
                }),
                AssignValue::Scalar(v) => {
                    let mut val = v.clone();
                    if has_token(&val) {
                        let mut svl = WordList::one(val);
                        let mut rf = 0;
                        self.prefork(&mut svl, PREFORK_SINGLE | PREFORK_ASSIGN, &mut rf);
                        if self.errflag() {
                            break;
                        }
                        val = svl.words.into_iter().next().unwrap_or_default();
                    }
                    tok::untokenize(&mut val);
                    assigns.push(Asgment {
                        name,
                        flags: 0,
                        scalar: Some(val),
                        array: Vec::new(),
                    });
                }
                AssignValue::Array(ws) => {
                    let mut flags = ASG_ARRAY;
                    let mut wl = WordList {
                        words: ws.clone(),
                        flags: 0,
                    };
                    if !self.errflag() {
                        let mut rf = 0;
                        self.prefork(&mut wl, PREFORK_ASSIGN, &mut rf);
                        if self.errflag() {
                            break;
                        }
                        if rf & PREFORK_KEY_VALUE != 0 {
                            flags |= ASG_KEY_VALUE;
                        }
                        self.globlist(&mut wl, rf);
                    }
                    if self.errflag() {
                        break;
                    }
                    assigns.push(Asgment {
                        name,
                        flags,
                        scalar: None,
                        array: wl.words,
                    });
                }
            }
        }
        assigns
    }

    /// zsh's `getoutput`: the words `$(cmd)` produces; `None` on a parse
    /// error.
    pub(crate) fn getoutput(&mut self, cmd: &[u8], qt: bool) -> Option<Vec<Vec<u8>>> {
        let prog = self.parse_string(cmd, false)?;
        if let Some(mut s) = simple_redir_name(&prog, REDIR_READ) {
            s = self.singsub(&s);
            if self.errflag() {
                return None;
            }
            tok::untokenize(&mut s);
            let Ok(c) = std::ffi::CString::new(tok::unmetafy(&s)) else {
                return Some(Vec::new());
            };
            // SAFETY: c is NUL-terminated.
            let stream = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_NOCTTY) };
            if stream == -1 {
                self.zwarn(&format!("{}: {}", errmsg(errno()), lossy(&s)));
                self.lastval = 1;
                self.cmdoutval = 1;
                return Some(Vec::new());
            }
            let (retval, readerror) = self.readoutput(stream, qt);
            if readerror != 0 {
                self.zwarn(&format!(
                    "error when reading {}: {}",
                    lossy(&s),
                    errmsg(readerror)
                ));
                self.lastval = 1;
                self.cmdoutval = 1;
            }
            return Some(retval);
        }
        let mut pipes = [0i32; 2];
        if self.mpipe(&mut pipes) < 0 {
            self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
            self.cmdoutpid = 0;
            return None;
        }
        child_block();
        self.cmdoutval = 0;
        let pid = self.zfork(None);
        self.cmdoutpid = pid;
        if pid == -1 {
            let _ = self.zclose(pipes[0]);
            let _ = self.zclose(pipes[1]);
            self.errflag.set(self.errflag.get() | ERRFLAG_ERROR);
            self.cmdoutpid = 0;
            child_unblock();
            return None;
        } else if pid != 0 {
            let _ = self.zclose(pipes[1]);
            let (retval, _) = self.readoutput(pipes[0], qt);
            self.fdtable_mark(pipes[0], FDT_UNUSED);
            let _ = self.waitforpid(pid, false);
            self.lastval = self.cmdoutval;
            return Some(retval);
        }
        child_unblock();
        let _ = self.zclose(pipes[0]);
        let _ = self.redup(pipes[1], 1);
        let _ = self.entersubsh(ESUB_PGRP | ESUB_NOMONITOR);
        self.execode(&prog, false, true, "cmdsubst");
        // SAFETY: closing stdout before exiting.
        unsafe {
            libc::close(1);
        }
        self._realexit();
    }

    /// zsh's `readoutput`: the output read from `input`, and the read error.
    pub(crate) fn readoutput(&mut self, input: i32, qt: bool) -> (Vec<Vec<u8>>, i32) {
        let q = self.queue_signal_level();
        self.dont_queue_signals();
        child_unblock();
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        let readret = loop {
            // SAFETY: buf is writable for its length.
            let n = unsafe { libc::read(input, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                if n < 0 && errno() == libc::EINTR {
                    self.check_signals();
                    continue;
                }
                break n;
            }
            raw.extend_from_slice(buf.get(..usize::try_from(n).unwrap_or(0)).unwrap_or(&[]));
        };
        child_block();
        self.restore_queue_signals(q);
        let readerror = if readret < 0 { errno() } else { 0 };
        // SAFETY: closing the read end.
        unsafe {
            libc::close(input);
        }
        while raw.last() == Some(&b'\n') {
            let _ = raw.pop();
        }
        let mut s = tok::metafy(&raw);
        if qt {
            if s.is_empty() {
                s.push(tok::NULARG);
            }
            return (vec![s], readerror);
        }
        let mut words = self.spacesplit(&s, false, true);
        if self.isset(GLOBSUBST) {
            for w in &mut words {
                crate::pattern::shtokenize(w, self.isset(SHGLOB));
            }
        }
        (words, readerror)
    }

    /// zsh's `parsecmd`: the program inside `<(...)` at `cmd[start..]`, and
    /// where the text after `)` begins.
    fn parsecmd(&mut self, cmd: &[u8], start: usize) -> Option<(crate::tables::Eprog, usize)> {
        let at = |i: usize| cmd.get(i).copied().unwrap_or(0);
        let mut end = start + 2;
        while end < cmd.len() && at(end) != tok::OUTPAR {
            end += 1;
        }
        if end >= cmd.len() || at(start + 1) != tok::INPAR {
            let mut errstr = cmd.get(start..start + 2).unwrap_or(&[]).to_vec();
            tok::untokenize(&mut errstr);
            self.zerr(&format!("unterminated `{}...)'", lossy(&errstr)));
            return None;
        }
        let body = cmd.get(start + 2..end).unwrap_or(&[]).to_vec();
        let Some(prog) = self.parse_string(&body, false) else {
            self.zerr("parse error in process substitution");
            return None;
        };
        Some((prog, end + 1))
    }

    /// zsh's `getoutputfile`: `=(cmd)` at `s[start..]`.
    pub(crate) fn getoutputfile(&mut self, s: &[u8], start: usize) -> (Option<Vec<u8>>, usize) {
        if self.thisjob == -1 {
            self.zerr(&format!(
                "process substitution {} cannot be used here",
                lossy(s)
            ));
            return (None, s.len());
        }
        let Some((prog, rest)) = self.parsecmd(s, start) else {
            return (None, s.len());
        };
        let Some(nam) = self.gettempname(None) else {
            return (None, rest);
        };
        let mut herestr = simple_redir_name(&prog, REDIR_HERESTR);
        if let Some(h) = herestr.take() {
            let mut v = self.singsub(&h);
            if !self.errflag() {
                tok::untokenize(&mut v);
                v.push(b'\n');
                herestr = Some(v);
            }
        }
        if herestr.is_none() {
            child_block();
        }
        let Ok(c) = std::ffi::CString::new(nam.clone()) else {
            return (None, rest);
        };
        // SAFETY: c is NUL-terminated.
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOCTTY,
                0o600,
            )
        };
        if fd < 0 {
            self.zerr(&format!("process substitution failed: {}", errmsg(errno())));
            if herestr.is_none() {
                child_unblock();
            }
            return (None, rest);
        }
        let mut nam = nam;
        if let Some(suffix) = self.getsparam(b"TMPSUFFIX")
            && !suffix.is_empty()
            && !suffix.contains(&b'/')
        {
            let mut with = nam.clone();
            with.extend(tok::unmetafy(&suffix));
            if let Ok(cw) = std::ffi::CString::new(with.clone())
                // SAFETY: both names are NUL-terminated.
                && unsafe { libc::link(c.as_ptr(), cw.as_ptr()) } == 0
            {
                self.addfilelist(Some(&nam), 0);
                nam = with;
            }
        }
        self.addfilelist(Some(&nam), 0);
        let mnam = tok::metafy(&nam);
        if let Some(h) = herestr {
            let _ = self.write_loop(fd, &tok::unmetafy(&h));
            // SAFETY: closing the file just written.
            unsafe {
                libc::close(fd);
            }
            return (Some(mnam), rest);
        }
        let pid = self.zfork(None);
        self.cmdoutpid = pid;
        if pid == -1 {
            // SAFETY: closing the file.
            unsafe {
                libc::close(fd);
            }
            child_unblock();
            return (Some(mnam), rest);
        } else if pid != 0 {
            // SAFETY: closing the file in the parent.
            unsafe {
                libc::close(fd);
            }
            let _ = self.waitforpid(pid, false);
            self.cmdoutval = 0;
            return (Some(mnam), rest);
        }
        self.closem(FDT_UNUSED, false);
        let _ = self.redup(fd, 1);
        let _ = self.entersubsh(ESUB_PGRP | ESUB_NOMONITOR);
        self.execode(&prog, false, true, "equalsubst");
        // SAFETY: closing stdout before exiting.
        unsafe {
            libc::close(1);
        }
        self._realexit();
    }

    /// zsh's `getproc` (with `PATH_DEV_FD`): `<(cmd)` or `>(cmd)` at
    /// `s[start..]`.
    pub(crate) fn getproc(&mut self, s: &[u8], start: usize) -> (Option<Vec<u8>>, usize) {
        let out = s.get(start) == Some(&tok::INANG);
        if self.thisjob == -1 {
            self.zerr(&format!(
                "process substitution {} cannot be used here",
                lossy(s)
            ));
            return (None, s.len());
        }
        let Some((prog, rest)) = self.parsecmd(s, start) else {
            return (None, s.len());
        };
        let mut pipes = [0i32; 2];
        if self.mpipe(&mut pipes) < 0 {
            return (None, rest);
        }
        let mut bgtime = (0, 0);
        let pid = self.zfork(Some(&mut bgtime));
        let (keep, give) = if out {
            (pipes[0], pipes[1])
        } else {
            (pipes[1], pipes[0])
        };
        if pid != 0 {
            let _ = self.zclose(give);
            if pid == -1 {
                let _ = self.zclose(keep);
                return (None, rest);
            }
            self.fdtable_mark(keep, FDT_PROC_SUBST);
            self.addfilelist(None, keep);
            if !out {
                self.addproc(pid, None, true, bgtime, -1, -1);
            }
            self.procsubstpid = pid;
            return (Some(format!("/dev/fd/{keep}").into_bytes()), rest);
        }
        let _ = self.entersubsh(ESUB_ASYNC | ESUB_PGRP);
        let _ = self.redup(give, i32::from(out));
        self.closem(FDT_UNUSED, false);
        self.execode(&prog, false, true, if out { "outsubst" } else { "insubst" });
        let _ = self.zclose(i32::from(out));
        self._realexit();
    }

    /// zsh's `getpipe`: `> >(cmd)` or `< <(cmd)` as a redirection.
    pub(crate) fn getpipe(&mut self, cmd: &[u8], nullexec: bool) -> i32 {
        let out = cmd.first() == Some(&tok::INANG);
        let Some((prog, rest)) = self.parsecmd(cmd, 0) else {
            return -1;
        };
        if rest < cmd.len() {
            self.zerr("invalid syntax for process substitution in redirection");
            return -1;
        }
        let mut pipes = [0i32; 2];
        if self.mpipe(&mut pipes) < 0 {
            return -1;
        }
        let (keep, give) = if out {
            (pipes[0], pipes[1])
        } else {
            (pipes[1], pipes[0])
        };
        let mut bgtime = (0, 0);
        let pid = self.zfork(Some(&mut bgtime));
        if pid != 0 {
            let _ = self.zclose(give);
            if pid == -1 {
                let _ = self.zclose(keep);
                return -1;
            }
            if !nullexec {
                self.addproc(pid, None, true, bgtime, -1, -1);
            }
            self.procsubstpid = pid;
            return keep;
        }
        let _ = self.entersubsh(ESUB_ASYNC | ESUB_PGRP);
        let _ = self.redup(give, i32::from(out));
        self.closem(FDT_UNUSED, false);
        self.execode(&prog, false, true, if out { "outsubst" } else { "insubst" });
        self._realexit();
    }
}

/// A command's leading assignments, its words, and a typeset's assignments.
type CmdParts<'a> = (&'a [Assign], Option<Vec<Vec<u8>>>, Option<&'a [Assign]>);

/// What a command name resolved to in `execcmd_exec`.
#[derive(Debug, Clone)]
enum BuiltinOrFunc {
    Builtin(Vec<u8>, Builtin),
    Func(Vec<u8>),
    /// `command -v`/`-V`: `whence` with `command`'s rules.
    CommandWhence,
    External,
}

/// zsh's `simple_redir_name`: `prog` is exactly one redirection of `typ`
/// with no command; its word.
fn simple_redir_name(prog: &crate::tables::Eprog, typ: i32) -> Option<Vec<u8>> {
    let [item] = prog.list.items.as_slice() else {
        return None;
    };
    if item.mode != crate::ast::ListMode::Sync || !item.sublist.rest.is_empty() {
        return None;
    }
    let s2 = &item.sublist.first;
    if s2.not || s2.coproc {
        return None;
    }
    let [cmd] = s2.pipeline.as_ref()?.cmds.as_slice() else {
        return None;
    };
    let CmdKind::Simple { assigns, words } = &cmd.kind else {
        return None;
    };
    if !assigns.is_empty() || !words.is_empty() {
        return None;
    }
    let [r] = cmd.redirs.as_slice() else {
        return None;
    };
    let x = xredirs(std::slice::from_ref(r));
    let x = x.first()?;
    (x.typ == typ && x.varid.is_none() && x.flags == 0).then(|| x.name.clone())
}
