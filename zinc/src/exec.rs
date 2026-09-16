//! Running the tree: lists, pipelines, commands, redirections, functions,
//! subshells and command substitution (zsh's `exec.c`, the core of it).

use std::ffi::CString;
use std::rc::Rc;

use crate::ast::{
    AndOr, Assign, AssignValue, CaseTerm, CmdKind, Command, List, ListMode, Pipeline,
};
use crate::ast::{Redir, RedirKind, Sublist, Sublist2};
use crate::expand::{expand_pattern, expand_single, expand_words};
use crate::lex::{AliasDef, Lexer};
use crate::parse::Parser;
use crate::pattern::Pattern;
use crate::shell::{Flow, Function, Shell, Value};
use crate::tok;

/// Leave the process now, without running destructors or flushing Rust's
/// buffers (all output is written unbuffered).
pub(crate) fn exit_now(status: i32) -> ! {
    // SAFETY: _exit has no preconditions and does not return.
    unsafe { libc::_exit(status & 0xff) }
}

/// Write all of `bytes` to `fd`, retrying on interruption.
pub(crate) fn write_fd(fd: i32, bytes: &[u8]) -> bool {
    let mut done = 0;
    while done < bytes.len() {
        let rest = bytes.get(done..).unwrap_or(&[]);
        // SAFETY: rest is a valid slice for its length.
        let n = unsafe { libc::write(fd, rest.as_ptr().cast(), rest.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return false;
        }
        done += usize::try_from(n).unwrap_or(0);
    }
    true
}

fn fork() -> i32 {
    // SAFETY: the shell is single-threaded, so the child may run any code.
    unsafe { libc::fork() }
}

/// Wait for `pid` and turn its wait status into a shell status.
pub(crate) fn wait_pid(pid: i32) -> i32 {
    let mut st = 0;
    loop {
        // SAFETY: st is a valid out-pointer.
        let r = unsafe { libc::waitpid(pid, &raw mut st, 0) };
        if r < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return 127;
        }
        break;
    }
    if libc::WIFEXITED(st) {
        libc::WEXITSTATUS(st)
    } else if libc::WIFSIGNALED(st) {
        128 + libc::WTERMSIG(st)
    } else {
        0
    }
}

/// Parse and run `text` (metafied), event by event.
pub(crate) fn run_string(sh: &mut Shell, text: &[u8]) {
    let mut lx = Lexer::new(text.to_vec(), sh.lex_opts());
    loop {
        lx.opts = sh.lex_opts();
        let parsed = {
            let mut p = Parser::new(&mut lx, &*sh);
            p.parse_event()
        };
        match parsed {
            Ok(Some(list)) => run_list(sh, &list),
            Ok(None) => return,
            Err(e) => {
                sh.lineno = e.lineno;
                sh.error(&e.msg);
                sh.status = 1;
                return;
            }
        }
        if sh.flow != Flow::Normal {
            return;
        }
    }
}

/// Run a command substitution and return its output.
pub(crate) fn capture(sh: &mut Shell, cmd: &[u8]) -> Vec<u8> {
    let mut fds = [0i32; 2];
    // SAFETY: fds is a two-element array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        sh.error("pipe failed");
        return Vec::new();
    }
    let [r, w] = fds;
    let pid = fork();
    if pid == 0 {
        close(r);
        dup2(w, 1);
        close(w);
        sh.subshell = true;
        run_string(sh, cmd);
        exit_now(sh.status);
    }
    close(w);
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        // SAFETY: buf is writable for its length.
        let n = unsafe { libc::read(r, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        if n <= 0 {
            break;
        }
        out.extend_from_slice(buf.get(..usize::try_from(n).unwrap_or(0)).unwrap_or(&[]));
    }
    close(r);
    if pid > 0 {
        let status = wait_pid(pid);
        sh.status = status;
        sh.subst_status = Some(status);
    }
    out
}

fn close(fd: i32) {
    // SAFETY: closing a descriptor has no memory-safety preconditions.
    let _r = unsafe { libc::close(fd) };
}

fn dup2(from: i32, to: i32) {
    if from != to {
        // SAFETY: dup2 has no memory-safety preconditions.
        let _r = unsafe { libc::dup2(from, to) };
    }
}

/// Run a list.
pub(crate) fn run_list(sh: &mut Shell, list: &List) {
    for item in &list.items {
        if sh.flow != Flow::Normal {
            return;
        }
        match item.mode {
            ListMode::Sync => run_sublist(sh, &item.sublist),
            ListMode::Async | ListMode::Disown => {
                let pid = fork();
                if pid == 0 {
                    sh.subshell = true;
                    run_sublist(sh, &item.sublist);
                    exit_now(sh.status);
                }
                sh.last_bg = pid;
                if item.mode == ListMode::Async {
                    sh.jobs.push(pid);
                }
                sh.status = 0;
            }
        }
    }
}

fn run_sublist(sh: &mut Shell, sl: &Sublist) {
    run_sublist2(sh, &sl.first);
    for (op, next) in &sl.rest {
        if sh.flow != Flow::Normal {
            return;
        }
        let go = match op {
            AndOr::And => sh.status == 0,
            AndOr::Or => sh.status != 0,
        };
        if go {
            run_sublist2(sh, next);
        }
    }
}

pub(crate) fn run_sublist2(sh: &mut Shell, s: &Sublist2) {
    if let Some(p) = &s.pipeline {
        run_pipeline(sh, p);
    }
    if s.not {
        sh.status = i32::from(sh.status == 0);
    }
}

fn run_pipeline(sh: &mut Shell, p: &Pipeline) {
    let n = p.cmds.len();
    if n <= 1 {
        if let Some(cmd) = p.cmds.first() {
            run_command(sh, cmd, false);
        }
        return;
    }
    let mut prev: i32 = -1;
    let mut pids = Vec::new();
    for (i, cmd) in p.cmds.iter().enumerate() {
        if i + 1 == n {
            // The last element runs in the shell, as in zsh: `x | read v`.
            let saved = if prev >= 0 { save_fd(0) } else { -2 };
            if prev >= 0 {
                dup2(prev, 0);
                close(prev);
            }
            run_command(sh, cmd, false);
            if saved != -2 {
                restore_fd(0, saved);
            }
            break;
        }
        let mut fds = [0i32; 2];
        // SAFETY: fds is a two-element array.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            sh.error("pipe failed");
            sh.status = 1;
            return;
        }
        let [r, w] = fds;
        let pid = fork();
        if pid == 0 {
            close(r);
            if prev >= 0 {
                dup2(prev, 0);
                close(prev);
            }
            dup2(w, 1);
            close(w);
            sh.subshell = true;
            run_command(sh, cmd, true);
            exit_now(sh.status);
        }
        pids.push(pid);
        close(w);
        if prev >= 0 {
            close(prev);
        }
        prev = r;
    }
    let status = sh.status;
    for pid in pids {
        let _s = wait_pid(pid);
    }
    sh.status = status;
}

/// Duplicate `fd` above the user's range so it can be restored; -1 if closed.
fn save_fd(fd: i32) -> i32 {
    // SAFETY: fcntl F_DUPFD_CLOEXEC has no memory-safety preconditions.
    unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) }
}

fn restore_fd(fd: i32, saved: i32) {
    if saved >= 0 {
        dup2(saved, fd);
        close(saved);
    } else {
        close(fd);
    }
}

/// What zsh prints for an errno: the system's own words, with a capital first
/// letter lowered unless a second capital follows, which is `zerrmsg`'s `%e`.
pub(crate) fn errmsg(err: i32) -> String {
    // SAFETY: strerror returns a pointer to a string that outlives the call.
    let p = unsafe { libc::strerror(err) };
    if p.is_null() {
        return format!("error {err}");
    }
    // SAFETY: p is a valid NUL-terminated string.
    let s = unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy();
    let mut bytes = s.into_owned().into_bytes();
    let second_is_upper = bytes.get(1).is_some_and(u8::is_ascii_uppercase);
    if let Some(first) = bytes.first_mut()
        && first.is_ascii_uppercase()
        && !second_is_upper
    {
        *first = first.to_ascii_lowercase();
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn open_file(path: &[u8], flags: i32) -> Result<i32, String> {
    let c = CString::new(tok::unmetafy(path)).map_err(|_| "bad file name".to_owned())?;
    // SAFETY: c is a valid NUL-terminated string.
    let fd = unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC, 0o666) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        let msg = match e.raw_os_error() {
            Some(libc::ENOENT) => "no such file or directory".to_owned(),
            Some(libc::EACCES) => "permission denied".to_owned(),
            Some(libc::EISDIR) => "is a directory".to_owned(),
            _ => e.to_string().to_lowercase(),
        };
        return Err(format!(
            "{msg}: {}",
            String::from_utf8_lossy(&tok::unmetafy(path))
        ));
    }
    Ok(fd)
}

/// Put `data` behind a readable descriptor.
fn data_fd(data: &[u8]) -> Result<i32, String> {
    let mut fds = [0i32; 2];
    // SAFETY: fds is a two-element array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err("pipe failed".to_owned());
    }
    let [r, w] = fds;
    if data.len() < 32768 {
        let _ok = write_fd(w, data);
        close(w);
        return Ok(r);
    }
    let pid = fork();
    if pid == 0 {
        close(r);
        let _ok = write_fd(w, data);
        exit_now(0);
    }
    close(w);
    Ok(r)
}

/// Apply redirections, returning what to restore.
pub(crate) fn apply_redirs(sh: &mut Shell, redirs: &[Redir]) -> Result<Vec<(i32, i32)>, String> {
    let mut saved: Vec<(i32, i32)> = Vec::new();
    for r in redirs {
        let (fds, src): (Vec<i32>, i32) = match r.kind {
            RedirKind::HereDoc | RedirKind::HereDocDash => {
                let body = match &r.heredoc {
                    Some(slot) => {
                        let d = slot.borrow();
                        if d.quoted {
                            d.body.clone()
                        } else {
                            expand_single(sh, &d.body)?
                        }
                    }
                    None => Vec::new(),
                };
                // The body already ends with its last line's newline.
                let body = tok::unmetafy(&body);
                (vec![r.fd], data_fd(&body)?)
            }
            RedirKind::HereStr => {
                let mut s = tok::unmetafy(&expand_single(sh, &r.target)?);
                s.push(b'\n');
                (vec![r.fd], data_fd(&s)?)
            }
            RedirKind::MergeIn | RedirKind::MergeOut => {
                let t = expand_single(sh, &r.target)?;
                if t == b"-" {
                    saved.push((r.fd, save_fd(r.fd)));
                    close(r.fd);
                    continue;
                }
                if t == b"p" {
                    continue;
                }
                match std::str::from_utf8(&t)
                    .ok()
                    .and_then(|s| s.parse::<i32>().ok())
                {
                    Some(n) => {
                        // SAFETY: fcntl F_GETFD has no memory-safety preconditions.
                        if unsafe { libc::fcntl(n, libc::F_GETFD) } < 0 {
                            return Err(format!("{n}: bad file descriptor"));
                        }
                        saved.push((r.fd, save_fd(r.fd)));
                        dup2(n, r.fd);
                        continue;
                    }
                    None if r.kind == RedirKind::MergeOut => {
                        let fd = open_file(&t, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC)?;
                        (vec![1, 2], fd)
                    }
                    None => return Err("file number expected".to_string()),
                }
            }
            RedirKind::InPipe | RedirKind::OutPipe => {
                return Err("process substitution is not supported yet".to_owned());
            }
            kind => {
                let t = expand_single(sh, &r.target)?;
                let flags = match kind {
                    RedirKind::Read => libc::O_RDONLY,
                    RedirKind::ReadWrite => libc::O_RDWR | libc::O_CREAT,
                    RedirKind::App
                    | RedirKind::AppNow
                    | RedirKind::ErrApp
                    | RedirKind::ErrAppNow => libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                    _ => libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                };
                let fd = open_file(&t, flags)?;
                let both = matches!(
                    kind,
                    RedirKind::ErrWrite
                        | RedirKind::ErrWriteNow
                        | RedirKind::ErrApp
                        | RedirKind::ErrAppNow
                );
                (if both { vec![1, 2] } else { vec![r.fd] }, fd)
            }
        };
        if let Some(var) = &r.varid {
            // SAFETY: fcntl F_DUPFD_CLOEXEC has no memory-safety preconditions.
            let high = unsafe { libc::fcntl(src, libc::F_DUPFD, 10) };
            close(src);
            sh.set_scalar(var, high.to_string().into_bytes());
            continue;
        }
        for fd in fds {
            saved.push((fd, save_fd(fd)));
            dup2(src, fd);
        }
        close(src);
    }
    Ok(saved)
}

pub(crate) fn restore_redirs(saved: Vec<(i32, i32)>) {
    for (fd, s) in saved.into_iter().rev() {
        restore_fd(fd, s);
    }
}

fn with_redirs(sh: &mut Shell, redirs: &[Redir], f: impl FnOnce(&mut Shell)) {
    match apply_redirs(sh, redirs) {
        Ok(saved) => {
            f(sh);
            restore_redirs(saved);
        }
        Err(e) => {
            sh.error(&e);
            sh.status = 1;
        }
    }
}

/// Run one command; `in_child` means the shell is a forked pipeline element
/// that may exec in place.
pub(crate) fn run_command(sh: &mut Shell, cmd: &Command, in_child: bool) {
    sh.lineno = cmd.lineno;
    match &cmd.kind {
        CmdKind::Simple { assigns, words } => run_simple(sh, assigns, words, &cmd.redirs, in_child),
        CmdKind::Typeset {
            assigns,
            words,
            args,
        } => {
            let (a, w, r) = (assigns.clone(), words.clone(), args.clone());
            with_redirs(sh, &cmd.redirs, |sh| {
                for asg in &a {
                    if let Err(e) = assign(sh, asg, false) {
                        sh.error(&e);
                    }
                }
                crate::builtins::typeset(sh, &w, &r);
            });
        }
        kind => {
            let kind = kind.clone();
            with_redirs(sh, &cmd.redirs, |sh| run_compound(sh, &kind));
        }
    }
}

/// Set a variable from an assignment. `local` makes it local first.
pub(crate) fn assign(sh: &mut Shell, a: &Assign, local: bool) -> Result<(), String> {
    let (name, sub) = match a.name.iter().position(|&c| c == tok::INBRACK) {
        Some(p) => (
            a.name.get(..p).unwrap_or(&[]).to_vec(),
            Some(
                a.name
                    .get(p + 1..a.name.len().saturating_sub(1))
                    .unwrap_or(&[])
                    .to_vec(),
            ),
        ),
        None => (a.name.clone(), None),
    };
    if let Some(v) = sh.vars.get(&name)
        && v.readonly
    {
        return Err(format!(
            "read-only variable: {}",
            String::from_utf8_lossy(&name)
        ));
    }
    if local {
        sh.make_local(&name);
    }
    match &a.value {
        AssignValue::None => {
            if sh.get(&name).is_none() || local {
                sh.set_scalar(&name, Vec::new());
            }
        }
        AssignValue::Scalar(w) => {
            let mut val = expand_single(sh, w)?;
            if sh.vars.get(&name).is_some_and(|v| v.integer) {
                val = crate::arith::eval(sh, &val)?.to_string().into_bytes();
            }
            if let Some(sub) = sub {
                return assign_element(sh, &name, &sub, val, a.append);
            }
            let new = match (a.append, sh.get(&name)) {
                (true, Some(Value::Array(mut arr))) => {
                    arr.push(val);
                    Value::Array(arr)
                }
                (true, Some(old)) => {
                    let mut s = old.joined();
                    s.extend_from_slice(&val);
                    Value::Scalar(s)
                }
                _ => Value::Scalar(val),
            };
            sh.set_value(&name, new);
        }
        AssignValue::Array(words) => {
            let vals = expand_words(sh, words)?;
            let is_assoc = matches!(sh.get(&name), Some(Value::Assoc(_)));
            let new = if is_assoc && !a.append {
                Value::Assoc(
                    vals.chunks(2)
                        .map(|c| {
                            (
                                c.first().cloned().unwrap_or_default(),
                                c.get(1).cloned().unwrap_or_default(),
                            )
                        })
                        .collect(),
                )
            } else {
                match (a.append, sh.get(&name)) {
                    (true, Some(Value::Array(mut arr))) => {
                        arr.extend(vals);
                        Value::Array(arr)
                    }
                    (true, Some(Value::Scalar(s))) => {
                        let mut arr = vec![s];
                        arr.extend(vals);
                        Value::Array(arr)
                    }
                    _ => Value::Array(vals),
                }
            };
            sh.set_value(&name, new);
        }
    }
    Ok(())
}

fn assign_element(
    sh: &mut Shell,
    name: &[u8],
    sub: &[u8],
    val: Vec<u8>,
    append: bool,
) -> Result<(), String> {
    if matches!(name, b"aliases" | b"galiases") {
        let key = expand_single(sh, sub)?;
        let global = name == b"galiases";
        let text = if append {
            let mut old = sh
                .aliases
                .get(&key)
                .map(|definition| definition.text.clone())
                .unwrap_or_default();
            old.extend_from_slice(&val);
            old
        } else {
            val
        };
        let _old = sh.aliases.insert(key, AliasDef { text, global });
        return Ok(());
    }
    match sh.get(name) {
        Some(Value::Assoc(mut pairs)) => {
            let key = expand_single(sh, sub)?;
            match pairs.iter_mut().find(|(k, _)| *k == key) {
                Some((_, v)) => {
                    if append {
                        v.extend_from_slice(&val);
                    } else {
                        *v = val;
                    }
                }
                None => pairs.push((key, val)),
            }
            sh.set_value(name, Value::Assoc(pairs));
        }
        other => {
            let mut arr = match other {
                Some(Value::Array(a)) => a,
                Some(Value::Scalar(s)) if !s.is_empty() => vec![s],
                _ => Vec::new(),
            };
            let text = expand_single(sh, sub)?;
            let idx = crate::arith::eval(sh, &text)?;
            let len = i64::try_from(arr.len()).unwrap_or(0);
            let idx = if idx < 0 { len + idx + 1 } else { idx };
            let Ok(k) = usize::try_from(idx - 1) else {
                return Err("assignment to invalid subscript range".to_owned());
            };
            while arr.len() <= k {
                arr.push(Vec::new());
            }
            if let Some(slot) = arr.get_mut(k) {
                if append {
                    slot.extend_from_slice(&val);
                } else {
                    *slot = val;
                }
            }
            sh.set_value(name, Value::Array(arr));
        }
    }
    Ok(())
}

fn word_text(w: &[u8]) -> Vec<u8> {
    tok::remove_nulls(w)
}

#[expect(
    clippy::too_many_lines,
    reason = "precommand modifiers, functions, builtins and programs"
)]
fn run_simple(
    sh: &mut Shell,
    assigns: &[Assign],
    words: &[Vec<u8>],
    redirs: &[Redir],
    in_child: bool,
) {
    let noglob = words.first().is_some_and(|w| word_text(w) == b"noglob");
    let glob_was = sh.opt("glob");
    if noglob {
        let _ok = sh.set_option(b"glob", false);
    }
    sh.subst_status = None;
    let expanded = expand_words(sh, words);
    if noglob {
        let _ok = sh.set_option(b"glob", glob_was);
    }
    let mut args = match expanded {
        Ok(a) => a,
        Err(e) => {
            sh.error(&e);
            sh.status = 1;
            sh.flow = Flow::Abort;
            return;
        }
    };
    if args.is_empty() {
        for a in assigns {
            if let Err(e) = assign(sh, a, false) {
                sh.error(&e);
                sh.status = 1;
                return;
            }
        }
        match apply_redirs(sh, redirs) {
            Ok(saved) => restore_redirs(saved),
            Err(e) => {
                sh.error(&e);
                sh.status = 1;
                return;
            }
        }
        // An assignment's status is its last command substitution's, or 0.
        sh.status = sh.subst_status.take().unwrap_or(0);
        return;
    }
    let (mut use_functions, mut force_builtin, mut exec) = (true, false, false);
    loop {
        match args.first().map(Vec::as_slice) {
            Some(b"noglob" | b"nocorrect" | b"-") => {
                let _m = args.remove(0);
            }
            Some(b"builtin") => {
                let _m = args.remove(0);
                force_builtin = true;
            }
            Some(b"command") if args.get(1).is_none_or(|a| a.first() != Some(&b'-')) => {
                let _m = args.remove(0);
                use_functions = false;
            }
            Some(b"exec") if args.get(1).is_none_or(|a| a.first() != Some(&b'-')) => {
                let _m = args.remove(0);
                exec = true;
            }
            _ => break,
        }
    }
    let Some(name) = args.first().cloned() else {
        if exec {
            // `exec >file`: redirections stay.
            if let Err(e) = apply_redirs(sh, redirs) {
                sh.error(&e);
                sh.status = 1;
            }
        }
        return;
    };
    if use_functions && !force_builtin && !sh.functions.contains_key(&name) {
        let _loaded = crate::builtins::load_autoload(sh, &name);
    }
    let is_function = use_functions && !force_builtin && sh.functions.contains_key(&name);
    let is_builtin = crate::builtins::is_builtin(&name);
    if force_builtin && !is_builtin {
        sh.error(&format!(
            "no such builtin: {}",
            String::from_utf8_lossy(&name)
        ));
        sh.status = 1;
        return;
    }
    if (is_function || is_builtin) && !exec {
        // Prefix assignments last for the command only.
        let saved: Vec<(Vec<u8>, Option<crate::shell::Var>)> = assigns
            .iter()
            .map(|a| (a.name.clone(), sh.vars.get(&a.name).cloned()))
            .collect();
        for a in assigns {
            if let Err(e) = assign(sh, a, false) {
                sh.error(&e);
            }
            if let Some(v) = sh.vars.get_mut(&a.name) {
                v.export = true;
            }
        }
        match apply_redirs(sh, redirs) {
            Ok(red) => {
                if is_function {
                    let rest = args.get(1..).unwrap_or(&[]).to_vec();
                    sh.status = call_function(sh, &name, rest);
                } else {
                    sh.status = crate::builtins::run(sh, &args);
                }
                restore_redirs(red);
            }
            Err(e) => {
                sh.error(&e);
                sh.status = 1;
            }
        }
        for (n, old) in saved {
            match old {
                Some(v) => {
                    let _p = sh.vars.insert(n, v);
                }
                None => sh.unset(&n),
            }
        }
        return;
    }
    let child = |sh: &mut Shell| -> ! {
        for a in assigns {
            if let Err(e) = assign(sh, a, false) {
                sh.error(&e);
            }
            if let Some(v) = sh.vars.get_mut(&a.name) {
                v.export = true;
            }
        }
        if let Err(e) = apply_redirs(sh, redirs) {
            sh.error(&e);
            exit_now(1);
        }
        exec_program(sh, &args)
    };
    if in_child || exec {
        child(sh);
    }
    let pid = fork();
    if pid == 0 {
        sh.subshell = true;
        reset_signals();
        child(sh);
    }
    if pid < 0 {
        sh.error("fork failed");
        sh.status = 1;
        return;
    }
    sh.status = wait_pid(pid);
}

/// Restore default signal dispositions in a child about to exec.
pub(crate) fn reset_signals() {
    for sig in [
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTSTP,
        libc::SIGTTIN,
        libc::SIGTTOU,
    ] {
        // SAFETY: SIG_DFL is a valid disposition for these signals.
        let _old = unsafe { libc::signal(sig, libc::SIG_DFL) };
    }
}

/// True if `path` (unmetafied) is a regular file this process may execute.
pub(crate) fn is_executable(path: &[u8]) -> bool {
    let Ok(c) = CString::new(path) else {
        return false;
    };
    // SAFETY: c is NUL-terminated.
    if unsafe { libc::access(c.as_ptr(), libc::X_OK) } != 0 {
        return false;
    }
    use std::os::unix::ffi::OsStrExt;
    std::fs::metadata(std::ffi::OsStr::from_bytes(path)).is_ok_and(|m| m.is_file())
}

/// Find `name` on `$PATH`.
pub(crate) fn find_program(sh: &Shell, name: &[u8]) -> Option<Vec<u8>> {
    if name.contains(&b'/') {
        return Some(name.to_vec());
    }
    let path = sh
        .get(b"PATH")
        .map_or_else(|| b"/bin:/usr/bin".to_vec(), |v| v.joined());
    for dir in path.split(|&c| c == b':') {
        let mut p = if dir.is_empty() {
            b".".to_vec()
        } else {
            dir.to_vec()
        };
        p.push(b'/');
        p.extend_from_slice(name);
        let Ok(c) = CString::new(tok::unmetafy(&p)) else {
            continue;
        };
        // SAFETY: c is NUL-terminated.
        if unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0 {
            let Ok(meta) = std::fs::metadata(String::from_utf8_lossy(&tok::unmetafy(&p)).as_ref())
            else {
                continue;
            };
            if meta.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Replace the process with a program; never returns.
fn exec_program(sh: &mut Shell, args: &[Vec<u8>]) -> ! {
    let name = args.first().cloned().unwrap_or_default();
    let Some(path) = find_program(sh, &name) else {
        sh.error(&format!(
            "command not found: {}",
            String::from_utf8_lossy(&tok::unmetafy(&name))
        ));
        exit_now(127);
    };
    let argv: Vec<CString> = args
        .iter()
        .filter_map(|a| CString::new(tok::unmetafy(a)).ok())
        .collect();
    let envp: Vec<CString> = sh
        .environ()
        .into_iter()
        .filter_map(|e| CString::new(e).ok())
        .collect();
    let mut argv_p: Vec<*const libc::c_char> = argv.iter().map(|c| c.as_ptr()).collect();
    argv_p.push(std::ptr::null());
    let mut envp_p: Vec<*const libc::c_char> = envp.iter().map(|c| c.as_ptr()).collect();
    envp_p.push(std::ptr::null());
    let Ok(cpath) = CString::new(tok::unmetafy(&path)) else {
        exit_now(127)
    };
    // SAFETY: all three arguments are NUL-terminated arrays of valid strings.
    let _r = unsafe { libc::execve(cpath.as_ptr(), argv_p.as_ptr(), envp_p.as_ptr()) };
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::ENOEXEC) => {
            // A script without `#!`: run it with this shell.
            let text = std::fs::read(String::from_utf8_lossy(&tok::unmetafy(&path)).as_ref())
                .unwrap_or_default();
            sh.argzero = path;
            sh.positional = args.get(1..).unwrap_or(&[]).to_vec();
            run_string(sh, &tok::metafy(&text));
            exit_now(sh.status);
        }
        Some(libc::EACCES) => {
            sh.error(&format!(
                "permission denied: {}",
                String::from_utf8_lossy(&tok::unmetafy(&name))
            ));
            exit_now(126);
        }
        Some(libc::ENOENT) => {
            sh.error(&format!(
                "no such file or directory: {}",
                String::from_utf8_lossy(&tok::unmetafy(&name))
            ));
            exit_now(127);
        }
        _ => {
            sh.error(&format!(
                "{}: {}",
                err.to_string().to_lowercase(),
                String::from_utf8_lossy(&tok::unmetafy(&name))
            ));
            exit_now(126);
        }
    }
}

/// Call a shell function with `args` as its positional parameters.
pub(crate) fn call_function(sh: &mut Shell, name: &[u8], args: Vec<Vec<u8>>) -> i32 {
    let Some(f) = sh.functions.get(name).cloned() else {
        return 127;
    };
    if sh.locals.len() > 1000 {
        sh.error("maximum nested function level reached; increase FUNCNEST?");
        return 1;
    }
    let saved_pos = std::mem::replace(&mut sh.positional, args);
    let saved_zero = std::mem::replace(&mut sh.argzero, name.to_vec());
    let saved_loops = std::mem::replace(&mut sh.loop_depth, 0);
    // An error in the body names the function and counts its line from the
    // definition, the way zsh reports one.
    let saved_script = std::mem::replace(&mut sh.script, name.to_vec());
    let saved_base = std::mem::replace(&mut sh.line_base, f.line);
    sh.push_scope();
    run_list(sh, &f.body);
    sh.pop_scope();
    sh.line_base = saved_base;
    sh.script = saved_script;
    sh.loop_depth = saved_loops;
    sh.positional = saved_pos;
    sh.argzero = saved_zero;
    if sh.flow == Flow::Return {
        sh.flow = Flow::Normal;
    }
    sh.status
}

/// Handle `break`/`continue` at the end of one loop iteration. Returns true
/// if the loop must stop.
fn loop_flow(sh: &mut Shell) -> bool {
    match sh.flow {
        Flow::Break(n) => {
            sh.flow = if n > 1 {
                Flow::Break(n - 1)
            } else {
                Flow::Normal
            };
            true
        }
        Flow::Continue(n) => {
            if n > 1 {
                sh.flow = Flow::Continue(n - 1);
                true
            } else {
                sh.flow = Flow::Normal;
                false
            }
        }
        Flow::Normal => false,
        _ => true,
    }
}

fn arith_word(sh: &mut Shell, w: &[u8]) -> Result<i64, String> {
    let text = expand_single(sh, w)?;
    crate::arith::eval(sh, &text)
}

#[expect(clippy::too_many_lines, reason = "one arm per compound command")]
fn run_compound(sh: &mut Shell, kind: &CmdKind) {
    macro_rules! try_or_status {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(msg) => {
                    sh.error(&msg);
                    sh.status = 1;
                    return;
                }
            }
        };
    }
    match kind {
        CmdKind::Simple { .. } | CmdKind::Typeset { .. } => {}
        CmdKind::Subsh(list) => {
            let pid = fork();
            if pid == 0 {
                sh.subshell = true;
                run_list(sh, list);
                exit_now(sh.status);
            }
            sh.status = wait_pid(pid);
        }
        CmdKind::Cursh(list) => run_list(sh, list),
        CmdKind::Try { body, always } => {
            run_list(sh, body);
            let (flow, status) = (sh.flow, sh.status);
            sh.flow = Flow::Normal;
            run_list(sh, always);
            if sh.flow == Flow::Normal {
                sh.flow = flow;
                sh.status = status;
            }
        }
        CmdKind::For { vars, words, body } => {
            let values = match words {
                Some(w) => try_or_status!(expand_words(sh, w)),
                None => sh.positional.clone(),
            };
            sh.status = 0;
            sh.loop_depth += 1;
            for chunk in values.chunks(vars.len().max(1)) {
                for (k, var) in vars.iter().enumerate() {
                    let name = word_text(var);
                    sh.set_scalar(&name, chunk.get(k).cloned().unwrap_or_default());
                }
                run_list(sh, body);
                if loop_flow(sh) {
                    break;
                }
            }
            sh.loop_depth -= 1;
        }
        CmdKind::ForArith {
            init,
            cond,
            step,
            body,
        } => {
            let _i = try_or_status!(arith_word(sh, init));
            sh.loop_depth += 1;
            sh.status = 0;
            loop {
                let text = try_or_status!(expand_single(sh, cond));
                if !text.iter().all(u8::is_ascii_whitespace)
                    && try_or_status!(crate::arith::eval(sh, &text)) == 0
                {
                    break;
                }
                run_list(sh, body);
                if loop_flow(sh) {
                    break;
                }
                let _s = try_or_status!(arith_word(sh, step));
            }
            sh.loop_depth -= 1;
        }
        CmdKind::Select { .. } => {
            sh.error("select is not supported yet");
            sh.status = 1;
        }
        CmdKind::Case { word, arms } => {
            let subject = tok::unmetafy(&try_or_status!(expand_single(sh, word)));
            sh.status = 0;
            let mut k = 0;
            let mut fall = false;
            while let Some(arm) = arms.get(k) {
                k += 1;
                let mut hit = fall;
                if !hit {
                    for p in &arm.patterns {
                        let pat = try_or_status!(expand_pattern(sh, p));
                        if Pattern::compile(&pat, sh.opt("extendedglob")).matches(&subject) {
                            hit = true;
                            break;
                        }
                    }
                }
                if !hit {
                    continue;
                }
                run_list(sh, &arm.body);
                match arm.term {
                    CaseTerm::Break => break,
                    CaseTerm::Fallthrough => fall = true,
                    CaseTerm::TestNext => fall = false,
                }
                if sh.flow != Flow::Normal {
                    break;
                }
            }
        }
        CmdKind::If {
            branches,
            otherwise,
        } => {
            for (cond, body) in branches {
                run_list(sh, cond);
                if sh.flow != Flow::Normal {
                    return;
                }
                if sh.status == 0 {
                    run_list(sh, body);
                    return;
                }
            }
            match otherwise {
                Some(list) => run_list(sh, list),
                None => sh.status = 0,
            }
        }
        CmdKind::While { until, cond, body } => {
            sh.loop_depth += 1;
            let mut last = 0;
            loop {
                run_list(sh, cond);
                if sh.flow != Flow::Normal {
                    let _stop = loop_flow(sh);
                    break;
                }
                if (sh.status == 0) == *until {
                    break;
                }
                run_list(sh, body);
                last = sh.status;
                if loop_flow(sh) {
                    break;
                }
            }
            sh.loop_depth -= 1;
            sh.status = last;
        }
        CmdKind::Repeat { count, body } => {
            let n = try_or_status!(arith_word(sh, count));
            sh.loop_depth += 1;
            sh.status = 0;
            for _ in 0..n.max(0) {
                run_list(sh, body);
                if loop_flow(sh) {
                    break;
                }
            }
            sh.loop_depth -= 1;
        }
        CmdKind::FuncDef {
            names, body, args, ..
        } => {
            if names.is_empty() {
                let args = try_or_status!(expand_words(sh, args));
                let _ = sh.functions.insert(
                    b"(anon)".to_vec(),
                    Function {
                        body: Rc::clone(body),
                        line: sh.lineno,
                    },
                );
                sh.status = call_function(sh, b"(anon)", args);
                return;
            }
            for n in names {
                let name = try_or_status!(expand_single(sh, n));
                let _old = sh.functions.insert(
                    name,
                    Function {
                        body: Rc::clone(body),
                        line: sh.lineno,
                    },
                );
            }
            sh.status = 0;
        }
        CmdKind::Time(inner) => {
            if let Some(s) = inner {
                run_sublist2(sh, s);
            }
        }
        CmdKind::Cond(c) => match crate::cond::eval_cond(sh, c) {
            Ok(b) => sh.status = i32::from(!b),
            Err(e) => {
                sh.error(&e);
                sh.status = 2;
            }
        },
        CmdKind::Arith(expr) => match arith_word(sh, expr) {
            Ok(v) => sh.status = i32::from(v == 0),
            Err(e) => {
                sh.error(&e);
                sh.status = 2;
            }
        },
    }
}
