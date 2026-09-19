//! Job control: process groups, the terminal's foreground group, and the
//! table of jobs `jobs`, `fg` and `bg` name (zsh's `jobs.c` and `signals.c`,
//! the parts a session needs).
//!
//! # What job control is, in three rules
//!
//! 1. **Every pipeline is a process group.** The first process forked for it
//!    becomes the group's leader and the rest join it, so one signal reaches
//!    the whole pipeline: Ctrl-C at the terminal interrupts `a | b | c`
//!    entire, not whichever part the terminal happened to be talking to.
//! 2. **The terminal has one foreground group at a time.** A foreground job
//!    is handed the terminal with `tcsetpgrp` before it runs and the shell
//!    takes it back when the job ends or stops; a background group that reads
//!    the terminal is sent `SIGTTIN` by the line discipline rather than
//!    stealing the user's keystrokes.
//! 3. **A stopped job is not a finished one.** The shell waits with
//!    `WUNTRACED`, so a job that takes `SIGTSTP` comes back as *suspended*
//!    and stays in the table until `fg` or `bg` moves it, or it is killed.
//!
//! The kernel side of all three is Ferrix's: `setpgid`, `setsid`,
//! `TIOCSPGRP`/`TIOCGPGRP`, `TIOCSCTTY`, the `SIGTSTP`/`SIGTTIN`/`SIGTTOU`
//! the console's line discipline raises, and `wait4` with `WUNTRACED` and
//! `WCONTINUED`. This module is the half that uses them.
//!
//! # Why the shell steals the terminal before it starts
//!
//! Because a shell may itself be started in the background -- by another
//! shell, or by an init that does not hand over the terminal. A shell that
//! began writing prompts there would be stopped by `SIGTTOU` at its first
//! one, with nothing on the screen to say why. So [`Jobs::take_terminal`]
//! stops the shell's own group until the terminal is the shell's, exactly as
//! the glibc manual's job-control shell does, and only then takes a group of
//! its own.
//!
//! # What is not here
//!
//! No `SIGCHLD` handler: a child that ends while the user is typing is
//! noticed at the next prompt, which is when [`Jobs::notify`] reports it.
//! zsh notices it at once under `NOTIFY`, and printing over a line being
//! edited is the reason it is an option there; here it is simply not done.

use crate::exec::write_fd;

/// The terminal settings a stopped job left behind, restored when `fg`
/// resumes it, so that an editor comes back to the raw mode it set.
///
/// A type of its own because `libc::termios` has no `Debug` without the
/// `extra_traits` feature, and everything in the shell derives one.
#[derive(Clone, Copy)]
pub(crate) struct Modes(libc::termios);

impl std::fmt::Debug for Modes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Modes")
    }
}

/// Read `fd`'s terminal settings, or nothing if it is not a terminal.
fn modes_of(fd: i32) -> Option<Modes> {
    // SAFETY: an all-zero termios is a valid value for tcgetattr to fill.
    let mut settings: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: settings is a valid out-pointer for the call's lifetime.
    if unsafe { libc::tcgetattr(fd, &raw mut settings) } != 0 {
        return None;
    }
    Some(Modes(settings))
}

/// Put `modes` back on `fd`, after the output already written has drained.
fn set_modes(fd: i32, modes: Modes) {
    let settings = modes.0;
    // SAFETY: settings is a valid termios read from a terminal.
    let _r = unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &raw const settings) };
}

/// Where a process of a job has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcState {
    /// Started, and not known to have stopped or finished.
    Running,
    /// Stopped by the signal named.
    Stopped(i32),
    /// Exited with the status named.
    Exited(i32),
    /// Killed by the signal named.
    Signalled(i32),
}

/// One process of a job: a pipeline element, or the one command of a job
/// that is not a pipeline.
#[derive(Debug, Clone)]
pub(crate) struct Proc {
    /// Its pid, which is also the group's when it is the leader.
    pub(crate) pid: i32,
    /// Where it has got to.
    pub(crate) state: ProcState,
}

/// What a whole job is doing, taken from its processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobState {
    /// At least one process is still running.
    Running,
    /// Nothing runs and at least one process is stopped.
    Stopped,
    /// Every process has finished.
    Done,
}

/// A pipeline the shell started, and everything `jobs` says about it.
#[derive(Debug)]
pub(crate) struct Job {
    /// The number in brackets, unique among the jobs that exist now.
    pub(crate) id: usize,
    /// The process group every one of its processes is in.
    pub(crate) pgid: i32,
    /// Its processes, in pipeline order; the last one's status is the job's.
    pub(crate) procs: Vec<Proc>,
    /// What to print for it: the commands, joined by `|`.
    pub(crate) text: Vec<u8>,
    /// Started with `&`, or moved to the background by `bg`.
    pub(crate) background: bool,
    /// Its present state has been reported to the user already.
    notified: bool,
    /// The terminal as the job left it when it stopped.
    tmodes: Option<Modes>,
}

impl Job {
    /// What the job as a whole is doing.
    pub(crate) fn state(&self) -> JobState {
        if self.procs.iter().any(|p| p.state == ProcState::Running) {
            JobState::Running
        } else if self
            .procs
            .iter()
            .any(|p| matches!(p.state, ProcState::Stopped(_)))
        {
            JobState::Stopped
        } else {
            JobState::Done
        }
    }

    /// The status the shell takes from it: the last process's, as zsh takes
    /// a pipeline's status from its last element.
    fn status(&self) -> i32 {
        match self.procs.last().map(|p| p.state) {
            Some(ProcState::Exited(status)) => status,
            Some(ProcState::Signalled(sig) | ProcState::Stopped(sig)) => 128 + sig,
            Some(ProcState::Running) | None => 0,
        }
    }

    /// The word `jobs` prints for its state: zsh's vocabulary.
    fn word(&self) -> String {
        match self.state() {
            JobState::Running => "running".to_owned(),
            JobState::Stopped => match self.procs.iter().find_map(|p| match p.state {
                ProcState::Stopped(sig) => Some(sig),
                _ => None,
            }) {
                Some(libc::SIGTSTP) => "suspended".to_owned(),
                Some(libc::SIGTTIN) => "suspended (tty input)".to_owned(),
                Some(libc::SIGTTOU) => "suspended (tty output)".to_owned(),
                Some(sig) => format!("suspended ({})", signal_word(sig)),
                None => "suspended".to_owned(),
            },
            JobState::Done => match self.procs.last().map(|p| p.state) {
                Some(ProcState::Exited(0)) | None => "done".to_owned(),
                Some(ProcState::Exited(status)) => format!("exit {status}"),
                Some(ProcState::Signalled(sig)) => signal_word(sig),
                Some(_) => "done".to_owned(),
            },
        }
    }
}

/// What a signal is called in a job's line, as zsh names the ones a job dies
/// of. An unnamed signal is printed by number rather than guessed at.
fn signal_word(sig: i32) -> String {
    match sig {
        libc::SIGHUP => "hangup".to_owned(),
        libc::SIGINT => "interrupt".to_owned(),
        libc::SIGQUIT => "quit".to_owned(),
        libc::SIGILL => "illegal hardware instruction".to_owned(),
        libc::SIGABRT => "abort".to_owned(),
        libc::SIGFPE => "floating point exception".to_owned(),
        libc::SIGKILL => "killed".to_owned(),
        libc::SIGSEGV => "segmentation fault".to_owned(),
        libc::SIGPIPE => "broken pipe".to_owned(),
        libc::SIGALRM => "alarm".to_owned(),
        libc::SIGTERM => "terminated".to_owned(),
        other => format!("signal {other}"),
    }
}

/// The job table, and the shell's side of the terminal.
#[derive(Debug)]
pub(crate) struct Jobs {
    /// The jobs that exist, in the order they were started.
    table: Vec<Job>,
    /// `%+`: the job `fg` takes with no argument.
    current: Option<usize>,
    /// `%-`: the one before it.
    previous: Option<usize>,
    /// Job control is on: an interactive shell that owns a terminal. A
    /// script, a subshell and a shell with no terminal all run without it,
    /// and then every child stays in the shell's own process group, as it
    /// did before this module existed.
    enabled: bool,
    /// The shell's own process group, which the terminal comes back to.
    shell_pgid: i32,
    /// A descriptor on the terminal, kept above the range a redirection uses
    /// and closed on exec, so that `fg </dev/null` still finds the terminal.
    tty: i32,
    /// The terminal as the shell likes it, restored after a job has had it.
    shell_tmodes: Option<Modes>,
}

impl Default for Jobs {
    fn default() -> Jobs {
        Jobs::new()
    }
}

impl Jobs {
    /// An empty table, with job control off.
    pub(crate) fn new() -> Jobs {
        Jobs {
            table: Vec::new(),
            current: None,
            previous: None,
            enabled: false,
            // SAFETY: getpgrp has no preconditions.
            shell_pgid: unsafe { libc::getpgrp() },
            tty: -1,
            shell_tmodes: None,
        }
    }

    /// Whether children are put into groups of their own and handed the
    /// terminal.
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// The descriptor the terminal is held on, or -1.
    pub(crate) fn tty(&self) -> i32 {
        self.tty
    }

    /// Turn job control off: what a forked subshell does, since its children
    /// are the parent shell's job and not a table of their own.
    pub(crate) fn disable(&mut self) {
        self.enabled = false;
        self.table.clear();
        self.current = None;
        self.previous = None;
    }

    /// Take the terminal and a process group of the shell's own, which is
    /// what job control needs before the first job is started.
    ///
    /// Returns whether job control could be turned on. It cannot when fd 0
    /// is not a terminal, or when the terminal belongs to a session this
    /// shell is not in and will not be given -- a shell down a pipe, say.
    /// Then jobs are still tracked, but nothing is moved between groups.
    pub(crate) fn take_terminal(&mut self) -> bool {
        let Some(tty) = keep_terminal() else {
            return false;
        };
        self.tty = tty;
        // SAFETY: getpid has no preconditions.
        let pid = unsafe { libc::getpid() };
        // Wait until the terminal is this shell's, by stopping the shell's
        // group whenever it is not: a shell started in the background comes
        // back here when somebody brings it to the foreground. Bounded,
        // because a terminal whose foreground group cannot be read at all --
        // no controlling terminal, and none to be had -- would otherwise be
        // an endless stop-and-signal loop with nothing on the screen.
        for _ in 0..16 {
            // SAFETY: tcgetpgrp has no memory-safety preconditions.
            let owner = unsafe { libc::tcgetpgrp(tty) };
            // SAFETY: getpgrp has no preconditions.
            let ours = unsafe { libc::getpgrp() };
            if owner < 0 {
                // No controlling terminal. A session leader is given one by
                // asking for it; a process that is not one asks for a
                // session first, which makes it a leader with no terminal.
                // SAFETY: getsid has no memory-safety preconditions.
                if unsafe { libc::getsid(0) } != pid {
                    // SAFETY: setsid has no preconditions.
                    let _s = unsafe { libc::setsid() };
                }
                // SAFETY: TIOCSCTTY takes an integer argument, not a pointer.
                let taken = unsafe { libc::ioctl(tty, libc::TIOCSCTTY, 0) };
                if taken != 0 {
                    return false;
                }
                continue;
            }
            if owner == ours {
                break;
            }
            // SAFETY: kill has no memory-safety preconditions.
            let _k = unsafe { libc::kill(-ours, libc::SIGTTIN) };
        }
        // A group of the shell's own, so that a job's group is never the
        // shell's. A session leader already leads its group and is told
        // EPERM, which is not a failure.
        // SAFETY: setpgid has no memory-safety preconditions.
        let _p = unsafe { libc::setpgid(0, 0) };
        // SAFETY: getpgrp has no preconditions.
        self.shell_pgid = unsafe { libc::getpgrp() };
        // SAFETY: tcsetpgrp has no memory-safety preconditions.
        if unsafe { libc::tcsetpgrp(tty, self.shell_pgid) } != 0 {
            return false;
        }
        self.shell_tmodes = modes_of(tty);
        self.enabled = true;
        true
    }

    /// The shell's process group, which a child joins when job control is
    /// off and a foreground job's terminal comes back to.
    pub(crate) fn shell_pgid(&self) -> i32 {
        self.shell_pgid
    }

    /// Put `job` in the table and give it the lowest free number.
    pub(crate) fn add(&mut self, build: JobBuild) -> usize {
        let mut id = 1;
        while self.table.iter().any(|job| job.id == id) {
            id += 1;
        }
        self.table.push(Job {
            id,
            pgid: build.pgid,
            procs: build.procs,
            text: build.text,
            background: !build.foreground,
            notified: false,
            tmodes: None,
        });
        self.make_current(id);
        id
    }

    /// Make `id` the `%+` job and the one it replaces `%-`.
    fn make_current(&mut self, id: usize) {
        if self.current == Some(id) {
            return;
        }
        self.previous = self.current;
        self.current = Some(id);
    }

    /// Find a job by its number.
    fn job(&self, id: usize) -> Option<&Job> {
        self.table.iter().find(|job| job.id == id)
    }

    /// Find a job by its number, to change it.
    fn job_mut(&mut self, id: usize) -> Option<&mut Job> {
        self.table.iter_mut().find(|job| job.id == id)
    }

    /// Its process group, for `kill %1`.
    pub(crate) fn pgid(&self, id: usize) -> Option<i32> {
        self.job(id).map(|job| job.pgid)
    }

    /// A job's command, for the line `fg` prints when it resumes one.
    pub(crate) fn text_of(&self, id: usize) -> Vec<u8> {
        self.job(id).map(|job| job.text.clone()).unwrap_or_default()
    }

    /// The numbers of every job in the table, oldest first.
    pub(crate) fn ids(&self) -> Vec<usize> {
        self.table.iter().map(|job| job.id).collect()
    }

    /// Whether any job is stopped, which is what makes a first `exit` refuse.
    pub(crate) fn any_stopped(&self) -> bool {
        self.table
            .iter()
            .any(|job| job.state() == JobState::Stopped)
    }

    /// The job a job specification names: `%1`, `%+`, `%%`, `%-`, `%name`,
    /// `%?text`, or a bare number, which zsh takes as `%number`.
    pub(crate) fn find(&self, spec: &[u8]) -> Option<usize> {
        let rest = spec.strip_prefix(b"%").unwrap_or(spec);
        match rest {
            b"" | b"%" | b"+" => return self.current,
            b"-" => return self.previous,
            _ => {}
        }
        if let Ok(text) = std::str::from_utf8(rest)
            && let Ok(number) = text.parse::<usize>()
        {
            return self.job(number).map(|job| job.id);
        }
        if let Some(text) = rest.strip_prefix(b"?") {
            return self
                .table
                .iter()
                .find(|job| contains(&job.text, text))
                .map(|job| job.id);
        }
        self.table
            .iter()
            .find(|job| job.text.starts_with(rest))
            .map(|job| job.id)
    }

    /// Record what `status` says about `pid`, wherever it is in the table.
    fn mark(&mut self, pid: i32, status: i32) {
        let mut stopped = None;
        for job in &mut self.table {
            let Some(proc) = job.procs.iter_mut().find(|proc| proc.pid == pid) else {
                continue;
            };
            proc.state = if libc::WIFSTOPPED(status) {
                ProcState::Stopped(libc::WSTOPSIG(status))
            } else if libc::WIFSIGNALED(status) {
                ProcState::Signalled(libc::WTERMSIG(status))
            } else if libc::WIFCONTINUED(status) {
                ProcState::Running
            } else {
                ProcState::Exited(libc::WEXITSTATUS(status))
            };
            if matches!(proc.state, ProcState::Stopped(_)) {
                job.notified = false;
                stopped = Some(job.id);
            } else if libc::WIFCONTINUED(status) {
                job.notified = false;
            }
            break;
        }
        // A job that stops becomes the current one, as it does in zsh: `fg`
        // with no argument resumes what was just suspended.
        if let Some(id) = stopped {
            self.make_current(id);
        }
    }

    /// Collect what the kernel has to say about this shell's children.
    ///
    /// With `block`, waits for one event and reports whether it got one; an
    /// answer of `ECHILD` -- no children at all -- is false, and is what
    /// stops a wait for a job whose processes have already been reaped.
    /// Without it, takes everything ready and returns whether anything was.
    pub(crate) fn update(&mut self, block: bool) -> bool {
        let mut flags = libc::WUNTRACED | libc::WCONTINUED;
        if !block {
            flags |= libc::WNOHANG;
        }
        let mut seen = false;
        loop {
            let mut status = 0;
            // SAFETY: status is a valid out-pointer.
            let pid = unsafe { libc::waitpid(-1, &raw mut status, flags) };
            if pid < 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return seen;
            }
            if pid == 0 {
                return seen;
            }
            self.mark(pid, status);
            seen = true;
            if block {
                return true;
            }
        }
    }

    /// Wait for a foreground job to finish or stop, and give the terminal
    /// back to the shell. The status is the job's last process's.
    pub(crate) fn wait_foreground(&mut self, id: usize) -> i32 {
        while self.job(id).map(Job::state) == Some(JobState::Running) {
            if !self.update(true) {
                break;
            }
        }
        let state = self.job(id).map(Job::state);
        if state == Some(JobState::Stopped)
            && self.tty >= 0
            && let Some(modes) = modes_of(self.tty)
            && let Some(job) = self.job_mut(id)
        {
            job.tmodes = Some(modes);
        }
        self.reclaim_terminal();
        let status = self.job(id).map_or(0, Job::status);
        match state {
            Some(JobState::Stopped) => {
                let line = self.job(id).map(|job| line_for(job, self.mark_of(id)));
                if let Some(line) = line {
                    let _ok = write_fd(2, &line);
                }
                if let Some(job) = self.job_mut(id) {
                    job.notified = true;
                    job.background = true;
                }
            }
            // A foreground job that finished is not announced: its output is
            // the announcement, and its status is `$?`.
            _ => self.remove(id),
        }
        status
    }

    /// Take the terminal back and put the shell's settings on it.
    fn reclaim_terminal(&mut self) {
        if !self.enabled || self.tty < 0 {
            return;
        }
        // SAFETY: tcsetpgrp has no memory-safety preconditions.
        let _t = unsafe { libc::tcsetpgrp(self.tty, self.shell_pgid) };
        if let Some(modes) = self.shell_tmodes {
            set_modes(self.tty, modes);
        }
    }

    /// Hand the terminal to a job's group, which the shell does just before
    /// it waits for a foreground job.
    fn hand_terminal(&self, pgid: i32) {
        if !self.enabled || self.tty < 0 {
            return;
        }
        // SAFETY: tcsetpgrp has no memory-safety preconditions.
        let _t = unsafe { libc::tcsetpgrp(self.tty, pgid) };
    }

    /// Run a job in the foreground: this is `fg`, and the path a foreground
    /// pipeline takes as soon as its processes are forked.
    ///
    /// `resume` sends `SIGCONT` and puts the terminal back as the job left
    /// it, which a job that was never stopped does not need.
    pub(crate) fn foreground(&mut self, id: usize, resume: bool) -> i32 {
        let Some(job) = self.job(id) else {
            return 127;
        };
        let (pgid, tmodes) = (job.pgid, job.tmodes);
        if resume {
            if let Some(modes) = tmodes
                && self.tty >= 0
            {
                set_modes(self.tty, modes);
            }
            self.hand_terminal(pgid);
            self.continue_group(id);
        } else {
            self.hand_terminal(pgid);
        }
        if let Some(job) = self.job_mut(id) {
            job.background = false;
        }
        self.make_current(id);
        self.wait_foreground(id)
    }

    /// Run a stopped job in the background: `bg`.
    pub(crate) fn background(&mut self, id: usize) -> i32 {
        if self.job(id).is_none() {
            return 127;
        }
        self.continue_group(id);
        if let Some(job) = self.job_mut(id) {
            job.background = true;
            job.notified = true;
        }
        self.make_current(id);
        let line = self.job(id).map(|job| {
            let mut line = format!("[{}]  {} continued  ", job.id, self.mark_of(id)).into_bytes();
            line.extend_from_slice(&job.text);
            line.push(b'\n');
            line
        });
        if let Some(line) = line {
            let _ok = write_fd(2, &line);
        }
        0
    }

    /// Send `SIGCONT` to a job's group and count its stopped processes as
    /// running again. The signal goes to the group, so a pipeline resumes
    /// whole.
    fn continue_group(&mut self, id: usize) {
        let Some(job) = self.job_mut(id) else {
            return;
        };
        for proc in &mut job.procs {
            if matches!(proc.state, ProcState::Stopped(_)) {
                proc.state = ProcState::Running;
            }
        }
        let pgid = job.pgid;
        // SAFETY: kill has no memory-safety preconditions.
        let _k = unsafe { libc::kill(-pgid, libc::SIGCONT) };
    }

    /// Wait for a background job, without giving it the terminal: `wait %1`.
    pub(crate) fn wait_for(&mut self, id: usize) -> i32 {
        while self.job(id).map(Job::state) == Some(JobState::Running) {
            if !self.update(true) {
                break;
            }
        }
        let status = self.job(id).map_or(0, Job::status);
        if self.job(id).map(Job::state) == Some(JobState::Done) {
            self.remove(id);
        }
        status
    }

    /// Take a job out of the table, and out of `%+` and `%-`.
    pub(crate) fn remove(&mut self, id: usize) {
        self.table.retain(|job| job.id != id);
        if self.current == Some(id) {
            self.current = self.previous.filter(|&p| p != id);
            self.previous = None;
        }
        if self.previous == Some(id) {
            self.previous = None;
        }
        if self.current.is_none() {
            self.current = self.table.last().map(|job| job.id);
        }
    }

    /// `+`, `-` or a space: which of `%+` and `%-` a job is.
    fn mark_of(&self, id: usize) -> char {
        if self.current == Some(id) {
            '+'
        } else if self.previous == Some(id) {
            '-'
        } else {
            ' '
        }
    }

    /// What `jobs` prints: one line per job, oldest first.
    pub(crate) fn list(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for job in &self.table {
            out.extend(line_for(job, self.mark_of(job.id)));
        }
        out
    }

    /// Report the jobs whose state the user has not been told about, and
    /// forget the ones that have finished. Called at the prompt, which is
    /// where zsh reports them without `NOTIFY`.
    pub(crate) fn notify(&mut self) {
        self.update(false);
        let mut out = Vec::new();
        let mut finished = Vec::new();
        for job in &self.table {
            let state = job.state();
            if state == JobState::Done {
                finished.push(job.id);
            }
            if job.notified || (state == JobState::Running) {
                continue;
            }
            out.extend(line_for(job, self.mark_of(job.id)));
        }
        for job in &mut self.table {
            job.notified = true;
        }
        if !out.is_empty() {
            let _ok = write_fd(2, &out);
        }
        for id in finished {
            self.remove(id);
        }
    }
}

/// One job's line, in zsh's layout: `[1]  + running    sleep 100`.
fn line_for(job: &Job, mark: char) -> Vec<u8> {
    let mut line = format!("[{}]  {} {:<10} ", job.id, mark, job.word()).into_bytes();
    line.extend_from_slice(&job.text);
    line.push(b'\n');
    line
}

/// Whether `text` holds `needle`, which `%?text` asks of a job's command.
fn contains(text: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    text.windows(needle.len()).any(|window| window == needle)
}

/// A descriptor on the terminal, above the range a redirection moves and
/// closed on exec, or nothing when fd 0 is not a terminal.
///
/// fd 0 rather than fd 2, because fd 0 is what `isatty` decided the shell was
/// interactive on, and a shell whose output is redirected still reads the
/// user's keystrokes from there.
fn keep_terminal() -> Option<i32> {
    // SAFETY: isatty has no memory-safety preconditions.
    if unsafe { libc::isatty(0) } != 1 {
        return None;
    }
    // SAFETY: F_DUPFD_CLOEXEC takes an integer argument, not a pointer.
    let fd = unsafe { libc::fcntl(0, libc::F_DUPFD_CLOEXEC, 10) };
    (fd >= 0).then_some(fd)
}

/// A job being built: the shell fills one in while it forks a pipeline's
/// processes, and hands it to [`Jobs::add`] when they are all started.
#[derive(Debug, Default)]
pub(crate) struct JobBuild {
    /// The group every process of it joins: the first one's pid, and 0 until
    /// that process exists.
    pub(crate) pgid: i32,
    /// The processes forked for it so far.
    pub(crate) procs: Vec<Proc>,
    /// The commands, joined by `|`, as `jobs` prints them.
    pub(crate) text: Vec<u8>,
    /// It runs in the foreground: it is handed the terminal and waited for.
    pub(crate) foreground: bool,
}

impl JobBuild {
    /// A job that will run in the foreground, or in the background for `&`.
    pub(crate) fn new(foreground: bool) -> JobBuild {
        JobBuild {
            pgid: 0,
            procs: Vec::new(),
            text: Vec::new(),
            foreground,
        }
    }

    /// Add a process's command to the text, as another pipeline element.
    pub(crate) fn add_text(&mut self, words: &[Vec<u8>]) {
        if self.text.is_empty() {
            self.text = join(words);
            return;
        }
        self.text.extend_from_slice(b" | ");
        self.text.extend(join(words));
    }

    /// Use `text` when nothing better is known: what a `&` on something that
    /// is not a simple command -- a loop, a subshell -- is called.
    pub(crate) fn text_if_empty(&mut self, text: &[u8]) {
        if self.text.is_empty() {
            self.text = text.to_vec();
        }
    }

    /// Record a process just forked into the job, and make it the leader if
    /// it is the first.
    pub(crate) fn started(&mut self, pid: i32) {
        if self.pgid == 0 {
            self.pgid = pid;
        }
        self.procs.push(Proc {
            pid,
            state: ProcState::Running,
        });
    }

    /// The last process started, which is what `$!` names.
    pub(crate) fn last_pid(&self) -> i32 {
        self.procs.last().map_or(0, |proc| proc.pid)
    }
}

/// The words of one command, separated by spaces, for a job's line.
///
/// Unmetafied: a job's line is read by a person, and the shell's internal
/// encoding of a byte above 127 is not what they typed.
fn join(words: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for (at, word) in words.iter().enumerate() {
        if at > 0 {
            out.push(b' ');
        }
        out.extend(crate::tok::unmetafy(word));
    }
    out
}
