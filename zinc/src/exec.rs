//! Running the tree: lists, pipelines, commands, redirections, functions,
//! subshells and command substitution (zsh's `exec.c`, the core of it).

use std::ffi::CString;
use std::rc::Rc;

use crate::ast::{
    AndOr, Assign, AssignValue, CaseTerm, CmdKind, Command, List, ListMode, Pipeline,
};
use crate::ast::{Redir, RedirKind, Sublist, Sublist2};
use crate::expand::{expand_pattern, expand_single, expand_words};
use crate::jobs::JobBuild;
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

/// What a forked child of the shell becomes before it runs anything: a
/// subshell, with no job table and no job being built.
///
/// A subshell's children are part of the job the parent shell started, not
/// jobs of their own, so it neither makes process groups nor touches the
/// terminal. This is also what stops a child from reaping, or reporting, the
/// processes its parent is waiting for.
fn enter_subshell(sh: &mut Shell) {
    sh.subshell = true;
    sh.building = None;
    sh.jobs.disable();
}

/// Fork a process of the job being built and run `child` in it.
///
/// The child joins the job's process group, and takes the terminal when the
/// job is a foreground one. Both sides set the group and both sides hand the
/// terminal over, because which of the two runs first is not ordered: a child
/// that reached `execve` before its parent had moved it would run in the
/// shell's own group, where a Ctrl-C meant for it would reach the shell.
fn spawn(sh: &mut Shell, child: impl FnOnce(&mut Shell) -> i32) -> i32 {
    let control = sh.jobs.enabled() && sh.building.is_some();
    let (pgid, foreground) = match &sh.building {
        Some(build) => (build.pgid, build.foreground),
        None => (0, false),
    };
    let tty = sh.jobs.tty();
    let pid = fork();
    if pid == 0 {
        if control {
            // SAFETY: setpgid has no memory-safety preconditions.
            let _p = unsafe { libc::setpgid(0, pgid) };
            if foreground && tty >= 0 {
                let group = if pgid == 0 {
                    // SAFETY: getpid has no preconditions.
                    unsafe { libc::getpid() }
                } else {
                    pgid
                };
                // SAFETY: tcsetpgrp has no memory-safety preconditions.
                let _t = unsafe { libc::tcsetpgrp(tty, group) };
            }
        }
        enter_subshell(sh);
        reset_signals();
        let status = child(sh);
        exit_now(status)
    }
    if pid > 0 {
        if control {
            let group = if pgid == 0 { pid } else { pgid };
            // SAFETY: setpgid has no memory-safety preconditions.
            let _p = unsafe { libc::setpgid(pid, group) };
        }
        if let Some(build) = &mut sh.building {
            build.started(pid);
        }
    }
    pid
}

/// Start the job a pipeline is, and give back the job that was being built,
/// for [`end_job`] to put back.
///
/// **A pipeline is a job of its own even when the shell reaches it while
/// building one.** `if`, `while`, `{ }` and a function's body run in the
/// shell rather than in a process of the enclosing job, so a command in one
/// of them belongs to the pipeline it is written in and not to the pipeline
/// the compound command is an element of. Adding it to the enclosing job
/// instead would mean nobody waited for it until that job ended: `mkdir d &&
/// cd d` inside a function would run `cd` while `mkdir` was still starting,
/// and take its status from a process that had not run.
///
/// A forked child has no job being built at all, since [`enter_subshell`]
/// clears it, so what this displaces is only ever a job of this shell's.
fn begin_job(sh: &mut Shell, foreground: bool) -> Option<JobBuild> {
    sh.building.replace(JobBuild::new(foreground))
}

/// Put back the job [`begin_job`] displaced, once this pipeline's own job has
/// been finished.
fn end_job(sh: &mut Shell, outer: Option<JobBuild>) {
    sh.building = outer;
}

/// Put the job that has just been started in the table, and wait for it if
/// it is a foreground one.
///
/// `elements` is how many commands the pipeline had. A job whose processes
/// number fewer than that ended in the shell itself -- `seq 3 | read line`,
/// whose last element is a builtin -- and then the status is already the
/// shell's and the job's last process does not decide it.
fn finish_job(sh: &mut Shell, elements: usize) {
    let Some(mut build) = sh.building.take() else {
        return;
    };
    if build.procs.is_empty() {
        return;
    }
    // A pipeline's parts are expanded in the processes that run them, so the
    // shell knows the words of a single command and not of a pipeline's
    // elements. What the user typed names the job instead, and a job started
    // where there is no such line -- a script's -- keeps the words it has.
    if elements > 1 && !sh.line_text.is_empty() {
        build.text = sh.line_text.clone();
    }
    build.text_if_empty(&sh.line_text);
    let complete = build.procs.len() == elements;
    let (foreground, last_pid) = (build.foreground, build.last_pid());
    let id = sh.jobs.add(build);
    if foreground {
        let status = sh.jobs.foreground(id, false);
        if complete {
            sh.status = status;
        }
        return;
    }
    sh.last_bg = last_pid;
    if sh.interactive {
        let line = format!("[{id}] {last_pid}\n");
        let _ok = write_fd(2, line.as_bytes());
    }
    sh.status = 0;
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
    if let Some(target) = simple_redir_name(sh, cmd) {
        return read_whole_file(sh, &target);
    }
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
        enter_subshell(sh);
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

/// `$(< word)`: a substitution that is one input redirection and nothing
/// else, which zsh's `getoutput` answers with the file's contents rather
/// than by running anything. Answers the redirection's word.
fn simple_redir_name(sh: &Shell, cmd: &[u8]) -> Option<Vec<u8>> {
    // Most substitutions are not this, and parsing them twice would cost.
    if cmd.iter().find(|c| !c.is_ascii_whitespace()) != Some(&b'<') {
        return None;
    }
    let mut lx = Lexer::new(cmd.to_vec(), sh.lex_opts());
    let list = Parser::new(&mut lx, sh).parse_all().ok()?;
    let [item] = list.items.as_slice() else {
        return None;
    };
    let first = &item.sublist.first;
    if item.mode != ListMode::Sync || !item.sublist.rest.is_empty() || first.not || first.coproc {
        return None;
    }
    let [command] = first.pipeline.as_ref()?.cmds.as_slice() else {
        return None;
    };
    let CmdKind::Simple { assigns, words } = &command.kind else {
        return None;
    };
    let [redir] = command.redirs.as_slice() else {
        return None;
    };
    (assigns.is_empty()
        && words.is_empty()
        && redir.kind == RedirKind::Read
        && redir.fd == 0
        && redir.varid.is_none())
    .then(|| redir.target.clone())
}

/// The contents of the file `$(< word)` names. oh-my-zsh reads its
/// completion dump that way to decide whether it may keep it; an empty
/// answer made it throw the dump away and build it again at every start.
fn read_whole_file(sh: &mut Shell, target: &[u8]) -> Vec<u8> {
    let read = expand_single(sh, target).and_then(|name| {
        let fd = open_file(&name, libc::O_RDONLY)?;
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            // SAFETY: buf is writable for its length.
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            if n < 0 {
                let error = std::io::Error::last_os_error().to_string().to_lowercase();
                close(fd);
                return Err(format!(
                    "error when reading {}: {error}",
                    String::from_utf8_lossy(&tok::unmetafy(&name))
                ));
            }
            if n == 0 {
                break;
            }
            out.extend_from_slice(buf.get(..usize::try_from(n).unwrap_or(0)).unwrap_or(&[]));
        }
        close(fd);
        Ok(out)
    });
    let (status, out) = match read {
        Ok(out) => (0, out),
        Err(error) => {
            sh.error(&error);
            (1, Vec::new())
        }
    };
    sh.status = status;
    sh.subst_status = Some(status);
    out
}

/// `<(cmd)` and `>(cmd)`: run `cmd` with one end of a pipe for its standard
/// output or input, and give back the name of the other end.
///
/// The command is not waited for -- that is the point of the construct, the
/// two run at once and the pipe joins them. The shell's end stays open, and
/// is closed when the command it was expanded for has finished.
pub(crate) fn proc_subst(sh: &mut Shell, cmd: &[u8], reading: bool) -> Option<Vec<u8>> {
    let mut fds = [0i32; 2];
    // SAFETY: fds is a two-element array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        sh.error("pipe failed");
        return None;
    }
    let [r, w] = fds;
    // `<(cmd)` is read by the shell and written by the command; `>(cmd)` is
    // the other way about.
    let (keep, give, child_fd) = if reading { (r, w, 1) } else { (w, r, 0) };
    let pid = fork();
    if pid == 0 {
        close(keep);
        dup2(give, child_fd);
        close(give);
        enter_subshell(sh);
        run_string(sh, cmd);
        exit_now(sh.status);
    }
    close(give);
    if pid < 0 {
        close(keep);
        return None;
    }
    sh.procsubs.push(keep);
    Some(format!("/proc/self/fd/{keep}").into_bytes())
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
                // The whole sublist runs in one process, which is the job's
                // group leader: `a && b &` is one job, and its own children
                // inherit the group, so one signal reaches all of it.
                let outer = begin_job(sh, false);
                let pid = spawn(sh, |sh| {
                    run_sublist(sh, &item.sublist);
                    sh.status
                });
                if pid < 0 {
                    end_job(sh, outer);
                    sh.error("fork failed");
                    sh.status = 1;
                    return;
                }
                if item.mode == ListMode::Disown {
                    // Disowned: started, and then not this shell's business.
                    sh.building = None;
                    sh.last_bg = pid;
                    sh.status = 0;
                } else {
                    finish_job(sh, 1);
                }
                end_job(sh, outer);
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

/// Run a pipeline, which is what a job is made of.
///
/// Every process forked here joins one process group, so the terminal's
/// signals reach the pipeline whole; `crate::jobs` says why that matters.
/// The shell waits for them together in [`finish_job`] rather than one at a
/// time, so that a Ctrl-Z in the middle of `a | b` suspends both.
fn run_pipeline(sh: &mut Shell, p: &Pipeline) {
    let n = p.cmds.len();
    if n == 0 {
        return;
    }
    let outer = begin_job(sh, true);
    if n == 1 {
        if let Some(cmd) = p.cmds.first() {
            run_command(sh, cmd, false);
        }
        finish_job(sh, 1);
        end_job(sh, outer);
        return;
    }
    let mut prev: i32 = -1;
    for (i, cmd) in p.cmds.iter().enumerate() {
        if i + 1 == n {
            // The last element runs in the shell, as in zsh: `x | read v`.
            // An external command forks from there and joins the job like
            // any other element; a builtin runs in the shell, which is not
            // in the job's group and so cannot be suspended with it.
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
            sh.building = None;
            end_job(sh, outer);
            return;
        }
        let [r, w] = fds;
        let pid = spawn(sh, move |sh| {
            close(r);
            if prev >= 0 {
                dup2(prev, 0);
                close(prev);
            }
            dup2(w, 1);
            close(w);
            run_command(sh, cmd, true);
            sh.status
        });
        close(w);
        if prev >= 0 {
            close(prev);
        }
        prev = r;
        if pid < 0 {
            sh.error("fork failed");
            sh.status = 1;
            break;
        }
    }
    finish_job(sh, n);
    end_job(sh, outer);
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
                    // `{d}>&-` closes the descriptor the parameter holds,
                    // which is the whole point of having asked the shell to
                    // pick one. Closing `r.fd` instead -- standard output,
                    // for an operator written with `>` -- is how zsh's own
                    // `compdump` used to take a shell's stdout away.
                    let fd = match &r.varid {
                        Some(var) => match varid_fd(sh, var) {
                            Some(fd) => fd,
                            None => return Err("bad file descriptor".to_owned()),
                        },
                        None => r.fd,
                    };
                    saved.push((fd, save_fd(fd)));
                    close(fd);
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
            // `< <(cmd)` and `> >(cmd)`: the same pipe as the word form, with
            // the descriptor put where the operator asks instead of its name
            // being passed along. Expanding the target is what starts the
            // command and gives back the name to open.
            RedirKind::InPipe | RedirKind::OutPipe => {
                let reading = r.kind == RedirKind::InPipe;
                let name = expand_single(sh, &r.target)?;
                let flags = if reading {
                    libc::O_RDONLY
                } else {
                    libc::O_WRONLY
                };
                (vec![r.fd], open_file(&name, flags)?)
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
        // The file was opened before the descriptors it is for were saved,
        // so it may have landed on one of them -- `exec 3>file` with 3
        // closed opens *as* 3. Saving that would save the file itself and
        // the `close` below would then undo the redirection, which is how
        // `exec 3>file` came to leave 3 closed.
        let mut keep = false;
        for fd in fds {
            if fd == src {
                saved.push((fd, -1));
                keep = true;
                continue;
            }
            saved.push((fd, save_fd(fd)));
            dup2(src, fd);
        }
        if keep {
            // `open_file` asks for O_CLOEXEC so a descriptor cannot leak
            // while it is being put in place; one the shell keeps is one a
            // program it runs inherits, so take the flag off again.
            // SAFETY: fcntl F_SETFD has no memory-safety preconditions.
            let _flags = unsafe { libc::fcntl(src, libc::F_SETFD, 0) };
        } else {
            close(src);
        }
    }
    Ok(saved)
}

/// The descriptor a `{name}>` parameter holds, for the `{name}>&-` that
/// closes it again.
fn varid_fd(sh: &Shell, var: &[u8]) -> Option<i32> {
    let value = sh.get(var)?.joined();
    std::str::from_utf8(&tok::unmetafy(&value))
        .ok()?
        .trim()
        .parse::<i32>()
        .ok()
        .filter(|&fd| fd >= 0)
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
            with_redirs(sh, &cmd.redirs, |sh| {
                for asg in assigns {
                    if let Err(e) = assign(sh, asg, false) {
                        sh.error(&e);
                    }
                }
                crate::builtins::typeset(sh, words, args);
            });
        }
        // The tree is borrowed, not copied: whatever owns it -- the parsed
        // input, or the `Rc` a running function holds -- outlives the run.
        // A copy here was a copy of every loop and `if` each time it ran,
        // with everything nested in it, and a fifth of compinit's time.
        kind => with_redirs(sh, &cmd.redirs, |sh| run_compound(sh, kind)),
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
            if !sh.is_set(&name) || local {
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
            if a.append
                && let Some(Value::Array(arr)) = plain_array(sh, &name)
            {
                arr.push(val);
                return Ok(());
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
            let is_assoc = match sh.stored(&name) {
                Some(value) => matches!(value, Value::Assoc(_)),
                None => matches!(sh.get(&name), Some(Value::Assoc(_))),
            };
            if a.append
                && !is_assoc
                && let Some(Value::Array(arr)) = plain_array(sh, &name)
            {
                arr.extend(vals);
                return Ok(());
            }
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

pub(crate) fn assign_element(
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
    // An element of an ordinary array or hash is set where the parameter is
    // kept. Copying the whole of it out, changing one element and storing
    // the copy back made every `_comps[$cmd]=$func` compinit runs cost the
    // size of the table: quadratic, over a thousand completion functions.
    // A special parameter, an integer, and `fpath`, whose every change is
    // mirrored into `FPATH`, still go the long way through `set_value`.
    let stored = crate::shell::prompt_name(name);
    let in_place = if Shell::is_special(name) || matches!(stored, b"fpath" | b"FPATH") {
        None
    } else {
        sh.vars
            .get(stored)
            .filter(|v| !v.integer)
            .map(|v| matches!(v.value, Value::Assoc(_)))
    };
    match in_place {
        Some(true) => {
            let key = expand_single(sh, sub)?;
            match sh.vars.get_mut(stored).map(|v| &mut v.value) {
                Some(Value::Assoc(pairs)) => set_pair(pairs, key, val, append),
                // Expanding the key unset the hash, or made it something
                // else: what is left to set is a hash of the one element.
                _ => sh.set_value(name, Value::Assoc(vec![(key, val)])),
            }
            return Ok(());
        }
        Some(false) => {
            let text = expand_single(sh, sub)?;
            let idx = crate::arith::eval(sh, &text)?;
            let Some(var) = sh.vars.get_mut(stored).filter(|v| !v.integer) else {
                let mut arr = Vec::new();
                set_index(&mut arr, idx, val, append)?;
                sh.set_value(name, Value::Array(arr));
                return Ok(());
            };
            // A scalar becomes an array of itself, as it does below; the
            // index is checked first, so a refused one changes nothing.
            if !matches!(var.value, Value::Array(_)) {
                let mut arr = match &var.value {
                    Value::Scalar(s) if !s.is_empty() => vec![s.clone()],
                    _ => Vec::new(),
                };
                set_index(&mut arr, idx, val, append)?;
                var.value = Value::Array(arr);
            } else if let Value::Array(arr) = &mut var.value {
                set_index(arr, idx, val, append)?;
            }
            return Ok(());
        }
        None => {}
    }
    match sh.get(name) {
        Some(Value::Assoc(mut pairs)) => {
            let key = expand_single(sh, sub)?;
            set_pair(&mut pairs, key, val, append);
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
            set_index(&mut arr, idx, val, append)?;
            sh.set_value(name, Value::Array(arr));
        }
    }
    Ok(())
}

/// The value of an ordinary parameter, to be changed where it is kept rather
/// than copied out and stored back: `None` for a special parameter, an
/// integer, and `fpath`, each of which `set_value` has more to do for.
fn plain_array<'a>(sh: &'a mut Shell, name: &[u8]) -> Option<&'a mut Value> {
    let stored = crate::shell::prompt_name(name);
    if Shell::is_special(name) || matches!(stored, b"fpath" | b"FPATH") {
        return None;
    }
    sh.vars
        .get_mut(stored)
        .filter(|v| !v.integer)
        .map(|v| &mut v.value)
}

/// `hash[key]=val`, or `+=` when `append`, on a hash's pairs.
fn set_pair(pairs: &mut Vec<(Vec<u8>, Vec<u8>)>, key: Vec<u8>, val: Vec<u8>, append: bool) {
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
}

/// `array[idx]=val`, or `+=` when `append`: a negative index counts from
/// the end, and an index past it grows the array with empty elements.
fn set_index(arr: &mut Vec<Vec<u8>>, idx: i64, val: Vec<u8>, append: bool) -> Result<(), String> {
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
    Ok(())
}

fn word_text(w: &[u8]) -> Vec<u8> {
    tok::remove_nulls(w)
}

/// A simple command, and then the descriptors any `<(...)` in it left open.
///
/// The shell holds its end of each process substitution's pipe so the
/// command can open `/proc/self/fd/N`; once the command has finished,
/// nothing will open it again. Closing here rather than at each of the many
/// ways out of the command below is what keeps a prompt that substitutes on
/// every draw from running the shell out of descriptors.
fn run_simple(
    sh: &mut Shell,
    assigns: &[Assign],
    words: &[Vec<u8>],
    redirs: &[Redir],
    in_child: bool,
) {
    let outer = std::mem::take(&mut sh.procsubs);
    run_simple_body(sh, assigns, words, redirs, in_child);
    for fd in std::mem::replace(&mut sh.procsubs, outer) {
        close(fd);
    }
}

fn run_simple_body(
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
    let child = |sh: &mut Shell| -> i32 {
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
        exit_now(child(sh));
    }
    if let Some(build) = &mut sh.building {
        build.add_text(&args);
    }
    let waiting = sh.building.is_none();
    let pid = spawn(sh, child);
    if pid < 0 {
        sh.error("fork failed");
        sh.status = 1;
        return;
    }
    // A process of a job is waited for with the job, by `finish_job`, so
    // that a pipeline is suspended and resumed as one thing.
    if waiting {
        sh.status = wait_pid(pid);
    }
}

/// Restore default signal dispositions in a child about to exec.
///
/// Every signal the interactive shell ignores is here, and `SIGTERM` is why
/// the list is not shorter: an ignored disposition survives `execve`, so a
/// child that kept the shell's would ignore `kill %1` -- and so would the
/// program it exec'd. The job would then be reported *done* thirty seconds
/// later when `sleep` ended on its own, with nothing to say the signal had
/// been dropped on the floor. Found by `cargo xtask test-jobs` on Ferrix.
pub(crate) fn reset_signals() {
    for sig in [
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTERM,
        libc::SIGHUP,
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
    // Each call parses its own options, as zsh's `doshfunc` has it: `getopts`
    // starts at the first argument and the caller's place is back afterwards,
    // unless POSIX_BUILTINS asks for one shared OPTIND. Without this the
    // first function to take an option left OPTIND past it for every later
    // one, and `compdef _git gco=git-checkout` shifted `_git` away.
    let saved_optind = (!sh.opt("posixbuiltins")).then(|| {
        let optind = sh.get(b"OPTIND").map(|v| v.joined());
        sh.set_scalar(b"OPTIND", b"1".to_vec());
        (optind, std::mem::replace(&mut sh.optpos, 1))
    });
    sh.push_scope();
    run_list(sh, &f.body);
    sh.pop_scope();
    if let Some((optind, optpos)) = saved_optind {
        if let Some(optind) = optind {
            sh.set_scalar(b"OPTIND", optind);
        }
        sh.optpos = optpos;
    }
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
                enter_subshell(sh);
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
