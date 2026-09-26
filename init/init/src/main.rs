//! `/sbin/init`: pid 1, around `libs/svc`'s manager (`docs/INIT.md`).
//!
//! The manager is a pure state machine: [`Manager::step`] takes what
//! happened and returns what to do. This program is everything around it
//! (§9): it boots the machine far enough for the manager to run (§8.1), then
//! waits in one `epoll_wait` for a signal, a child's exec report or a
//! cgroup's `cgroup.events`, turns what it finds into events, and carries
//! out the actions each step returns, until one of them is
//! [`Action::Power`].
//!
//! # Boot (§8.1)
//!
//! 1. Ignore every signal but the three it acts on, which it blocks and
//!    reads through a signalfd: `SIGCHLD` to reap, `SIGTERM` and `SIGINT` to
//!    power off (§8.2). Become a subreaper (§5.7).
//! 2. Mount a tmpfs on `/run`, then cgroup2 on `/sys/fs/cgroup`, move into
//!    `init.scope`, and enable the controllers the kernel has (§5.1). The
//!    kernel mounts `/proc`, `/dev`, `/sys` and `/tmp` itself.
//! 3. Run each generator in `/lib/ferrix/generators` with
//!    `/run/ferrix/units` as its argument, each for up to five seconds.
//! 4. Read the three unit directories and hand the manager
//!    [`Event::Boot`].
//!
//! If cgroup2 cannot be mounted there is nothing to run a service in, and
//! init says why and becomes a shell on the console, so the machine can be
//! looked at.

mod cgroup;
mod probe;
mod spawn;
mod sys;
mod units;

use std::collections::{BTreeMap, VecDeque};
use std::ffi::CString;
use std::fs;
use std::io::{self, Write as _};
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use ferrix_svc::event::{
    Action, ClientId, Event, Exit, MountSpec, Pid, PowerAction, Request, UnitId, Whom,
};
use ferrix_svc::{Instant, Manager, Options};

use crate::cgroup::Groups;
use crate::probe::Machine;
use crate::spawn::Report;

/// The unit directory generators write to (§8.1).
const RUNTIME_UNITS: &str = "/run/ferrix/units";

/// Where generators are.
const GENERATORS: &str = "/lib/ferrix/generators";

/// How long one generator may take, in polls of 10 ms.
const GENERATOR_PATIENCE: u32 = 500;

/// The client a signal to pid 1 asks for as: a power-off with nobody to
/// answer.
const SIGNALLED: ClientId = ClientId(0);

/// The signals init reads, blocked everywhere else.
const HANDLED: [libc::c_int; 3] = [libc::SIGCHLD, libc::SIGTERM, libc::SIGINT];

/// The signals init ignores. `SIGCHLD` stays at its default, since ignoring
/// it would reap children before init could see how they ended.
const IGNORED: [libc::c_int; 9] = [
    libc::SIGHUP,
    libc::SIGQUIT,
    libc::SIGPIPE,
    libc::SIGUSR1,
    libc::SIGUSR2,
    libc::SIGALRM,
    libc::SIGTSTP,
    libc::SIGTTIN,
    libc::SIGTTOU,
];

/// What an epoll token stands for: its kind in the top 32 bits, a number
/// below them.
mod token {
    /// The signalfd.
    pub(crate) const SIGNALS: u64 = 1 << 32;
    /// A cgroup's `cgroup.events`; the unit's number below.
    pub(crate) const GROUP: u64 = 2 << 32;
    /// A child's exec report pipe; its pid below.
    pub(crate) const REPORT: u64 = 3 << 32;
    /// The kind of `token`.
    pub(crate) fn kind(token: u64) -> u64 {
        token & !0xffff_ffff
    }
    /// The number in `token`.
    pub(crate) fn number(token: u64) -> u32 {
        u32::try_from(token & 0xffff_ffff).unwrap_or(0)
    }
}

/// One line on the console, in the `  init     …` form the kernel's own
/// lines about init take and `xtask` reads.
fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "  init     {line}");
    let _ = out.flush();
}

/// The whole of init's state.
#[derive(Debug)]
struct Init {
    manager: Manager,
    epoll: OwnedFd,
    signals: OwnedFd,
    groups: Groups,
    /// The report pipe of each child that has not yet exec'd or failed.
    reports: BTreeMap<u32, (UnitId, OwnedFd)>,
    /// Where each mounted unit is, for its unmount.
    mounts: BTreeMap<UnitId, String>,
    /// Events waiting to be stepped, in order.
    queue: VecDeque<Event>,
    /// The `TERM` a service on a terminal gets.
    terminal: String,
}

fn main() {
    let mut init = match Init::boot() {
        Ok(init) => init,
        Err(why) => emergency(&why),
    };
    init.run();
}

/// Say why init cannot go on, and become a shell on the console.
fn emergency(why: &str) -> ! {
    say(&format!("{why}; starting a shell on the console"));
    let _ = sys::unblock_all();
    let error = sys::execve(
        c"/bin/sh",
        &[c"sh".as_ptr(), c"-i".as_ptr(), std::ptr::null()],
        &[c"PATH=/bin:/sbin".as_ptr(), std::ptr::null()],
    );
    say(&format!("/bin/sh could not be started: {error}"));
    sys::exit_now(1)
}

/// Now, on the manager's clock.
fn now() -> Instant {
    Instant::from_nanos(sys::monotonic())
}

impl Init {
    /// §8.1's steps 1 to 4, up to the first event.
    fn boot() -> Result<Init, String> {
        for signal in IGNORED {
            sys::disposition(signal, libc::SIG_IGN);
        }
        let set = sys::block(&HANDLED).map_err(|e| format!("blocking signals failed: {e}"))?;
        let signals = sys::signalfd(&set).map_err(|e| format!("signalfd failed: {e}"))?;
        if let Err(error) = sys::subreaper() {
            say(&format!("PR_SET_CHILD_SUBREAPER failed: {error}"));
        }

        mount_run();
        let mut log = |line: String| say(&line);
        let groups =
            Groups::mount(&mut log).map_err(|e| format!("cgroup2 at {}: {e}", cgroup::MOUNT))?;
        run_generators();

        let command_line = fs::read_to_string("/proc/cmdline").unwrap_or_default();
        let options = Options {
            target: command_line
                .split_whitespace()
                .find_map(|word| word.strip_prefix("ferrix.target="))
                .map(str::to_owned),
        };
        let source = units::read(&mut log);
        let machine = Machine { command_line };
        let manager = Manager::new(source, Box::new(machine), options);

        let epoll = sys::epoll().map_err(|e| format!("epoll_create1 failed: {e}"))?;
        sys::watch(
            epoll.as_fd(),
            signals.as_raw_fd(),
            libc::EPOLLIN as u32,
            token::SIGNALS,
        )
        .map_err(|e| format!("watching the signalfd failed: {e}"))?;
        let terminal = std::env::var("TERM").unwrap_or_else(|_| "vt220".to_owned());
        Ok(Init {
            manager,
            epoll,
            signals,
            groups,
            reports: BTreeMap::new(),
            mounts: BTreeMap::new(),
            queue: VecDeque::from([Event::Boot]),
            terminal,
        })
    }

    /// Step, act and wait, for as long as the machine runs.
    fn run(&mut self) -> ! {
        loop {
            while let Some(event) = self.queue.pop_front() {
                let actions = self.manager.step(event, now());
                for action in actions {
                    self.perform(action);
                }
            }
            let timeout = match self.manager.deadline() {
                None => -1,
                Some(deadline) => {
                    let wait = deadline.saturating_since(now());
                    // Rounded up, so a timer is never looked at early.
                    let millis = wait.as_nanos().div_ceil(1_000_000);
                    i32::try_from(millis).unwrap_or(i32::MAX)
                }
            };
            let ready = match sys::wait(self.epoll.as_fd(), timeout) {
                Ok(ready) => ready,
                Err(error) => {
                    say(&format!("epoll_wait failed: {error}"));
                    std::thread::sleep(Duration::from_millis(100));
                    Vec::new()
                }
            };
            self.gather(&ready);
        }
    }

    /// Turn what woke init into events, in the order the manager wants
    /// them: exec reports before the exits of the same children, exits
    /// before the cgroups they leave empty, and the timer last.
    fn gather(&mut self, ready: &[(u32, u64)]) {
        for &(_, token) in ready {
            if token::kind(token) == token::REPORT {
                self.report(token::number(token));
            }
        }
        loop {
            match sys::next_signal(self.signals.as_fd()) {
                Ok(Some(signal))
                    if signal == libc::SIGTERM as u32 || signal == libc::SIGINT as u32 =>
                {
                    self.queue.push_back(Event::Request {
                        client: SIGNALLED,
                        request: Request::Poweroff,
                    });
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(error) => {
                    say(&format!("reading the signalfd failed: {error}"));
                    break;
                }
            }
        }
        self.reap();
        for unit in self.groups.emptied() {
            self.queue.push_back(Event::Emptied { unit });
        }
        if self.manager.deadline().is_some_and(|at| at <= now()) {
            self.queue.push_back(Event::Timer);
        }
    }

    /// Reap every child that has ended; a pid the manager does not know is
    /// an orphan, which it ignores (§5.7).
    fn reap(&mut self) {
        loop {
            match sys::reap() {
                Ok(Some((pid, how))) => {
                    if self.reports.contains_key(&pid) {
                        self.report(pid);
                    }
                    self.queue.push_back(Event::Exited { pid: Pid(pid), how });
                }
                Ok(None) => return,
                Err(error) => {
                    say(&format!("waitpid failed: {error}"));
                    return;
                }
            }
        }
    }

    /// Read a child's exec report, which is ready: its pipe has closed.
    fn report(&mut self, pid: u32) {
        let Some((unit, pipe)) = self.reports.remove(&pid) else {
            return;
        };
        sys::unwatch(self.epoll.as_fd(), pipe.as_raw_fd());
        match spawn::read_report(&pipe) {
            Report::Execed => self.queue.push_back(Event::Execed {
                unit,
                pid: Pid(pid),
            }),
            Report::Failed(step, error) => {
                let name = self.display(unit);
                say(&format!("{name}: {} failed: {error}", step.name()));
            }
        }
    }

    /// A unit's name, for the log.
    fn display(&self, unit: UnitId) -> String {
        self.manager.name(unit).map_or_else(
            || format!("unit {}", unit.0),
            |name| name.as_str().to_owned(),
        )
    }

    /// Carry out one action.
    fn perform(&mut self, action: Action) {
        match action {
            Action::Log { line, .. } => say(&line),
            Action::MakeGroup { unit, path, limits } => {
                let mut log = |line: String| say(&line);
                match self.groups.make(unit, &path, &limits, &mut log) {
                    Ok(fd) => self.watch(fd, token::GROUP | u64::from(unit.0)),
                    Err(error) => say(&format!("making the cgroup {path}: {error}")),
                }
            }
            Action::SetLimits { unit, limits } => {
                let mut log = |line: String| say(&line);
                self.groups.set_limits(unit, &limits, &mut log);
            }
            Action::RemoveGroup { unit } => {
                if let Some(fd) = self.groups.events_fd(unit) {
                    sys::unwatch(self.epoll.as_fd(), fd);
                }
                if let Some((_, Err(error))) = self.groups.remove(unit) {
                    let name = self.display(unit);
                    say(&format!("{name}: removing its cgroup: {error}"));
                }
            }
            Action::Spawn { unit, spec } => self.spawn(unit, &spec),
            Action::Move { unit, pids } => {
                let pids: Vec<u32> = pids.iter().map(|pid| pid.0).collect();
                for (pid, error) in self.groups.move_in(unit, &pids) {
                    let name = self.display(unit);
                    say(&format!("{name}: moving {pid} in: {error}"));
                }
            }
            Action::Signal { unit, signal, whom } => {
                let signal = libc::c_int::from(signal.0);
                let pids = match whom {
                    Whom::Process(pid) => vec![pid.0],
                    Whom::Group => self.groups.procs(unit),
                };
                for pid in pids {
                    let _ = sys::kill(pid, signal);
                }
            }
            Action::KillGroup { unit } => {
                if let Err(error) = self.groups.kill(unit) {
                    let name = self.display(unit);
                    say(&format!("{name}: writing cgroup.kill: {error}"));
                }
            }
            Action::Mount { unit, spec } => {
                let result = mount(&spec);
                if result.is_ok() {
                    let _ = self.mounts.insert(unit, spec.r#where.clone());
                }
                self.queue.push_back(Event::Mounted { unit, result });
            }
            Action::Unmount { unit } => {
                let result = match self.mounts.remove(&unit) {
                    Some(target) => unmount(&target),
                    None => Ok(()),
                };
                self.queue.push_back(Event::Unmounted { unit, result });
            }
            Action::Route { to, name, .. } | Action::Refuse { to, name, .. } => {
                let unit = self.display(to);
                say(&format!("{unit}: the directory ({}) comes with L8", name.0));
            }
            // No control socket yet (L6): the one client is a signal to
            // pid 1, which waits for no answer.
            Action::Reply { .. } => {}
            Action::Power(action) => power(action),
        }
    }

    /// Watch `fd` for `EPOLLPRI`, as `cgroup.events` reports a change.
    fn watch(&self, fd: RawFd, token: u64) {
        if let Err(error) = sys::watch(self.epoll.as_fd(), fd, libc::EPOLLPRI as u32, token) {
            say(&format!("watching a cgroup failed: {error}"));
        }
    }

    /// Start a process for `unit`.
    fn spawn(&mut self, unit: UnitId, spec: &ferrix_svc::event::SpawnSpec) {
        let failed = |init: &mut Init, errno: i32, why: &str| {
            let name = init.display(unit);
            say(&format!("{name}: {why}"));
            init.queue.push_back(Event::SpawnFailed {
                unit,
                error: ferrix_svc::event::Errno(errno),
            });
        };
        let prepared = match spawn::prepare(spec, &self.terminal) {
            Ok(prepared) => prepared,
            Err(unprepared) => return failed(self, unprepared.errno, &unprepared.why),
        };
        let Some(cgroup) = self.groups.dir(&spec.group) else {
            let why = format!("its cgroup {} was never made", spec.group);
            return failed(self, libc::ENOENT, &why);
        };
        match spawn::start(&prepared, cgroup) {
            Ok((pid, report)) => {
                self.groups.filled(&spec.group);
                let fd = report.as_raw_fd();
                let _ = self.reports.insert(pid, (unit, report));
                if let Err(error) = sys::watch(
                    self.epoll.as_fd(),
                    fd,
                    libc::EPOLLIN as u32,
                    token::REPORT | u64::from(pid),
                ) {
                    say(&format!("watching an exec report failed: {error}"));
                }
                self.queue.push_back(Event::Spawned {
                    unit,
                    pid: Pid(pid),
                });
            }
            Err(error) => {
                let why = format!("clone3 failed: {error}");
                failed(self, error.raw_os_error().unwrap_or(libc::EIO), &why);
            }
        }
    }
}

/// A tmpfs on `/run`, and the runtime unit directory in it.
fn mount_run() {
    let _ = fs::create_dir("/run");
    match sys::mount(
        c"tmpfs",
        c"/run",
        c"tmpfs",
        libc::MS_NOSUID | libc::MS_NODEV,
        Some(c"mode=755"),
    ) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(libc::EBUSY) => {}
        Err(error) => say(&format!("mounting a tmpfs on /run failed: {error}")),
    }
    if let Err(error) = fs::create_dir_all(RUNTIME_UNITS) {
        say(&format!("making {RUNTIME_UNITS} failed: {error}"));
    }
}

/// Run every generator, in name order, each to its end or its deadline.
fn run_generators() {
    let Ok(entries) = fs::read_dir(GENERATORS) else {
        return;
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            fs::metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .collect();
    paths.sort();
    for path in paths {
        let name = path.display().to_string();
        match run_generator(&path) {
            Ok(Exit::Code(0)) => {}
            Ok(how) => say(&format!("generator {name} ended: {how:?}")),
            Err(why) => say(&format!("generator {name}: {why}")),
        }
    }
}

/// Run one generator with the runtime directory as its argument.
fn run_generator(path: &Path) -> Result<Exit, String> {
    let program = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
    let argument = CString::new(RUNTIME_UNITS).map_err(|e| e.to_string())?;
    let argv = [program.as_ptr(), argument.as_ptr(), std::ptr::null()];
    let envp = [c"PATH=/bin:/sbin".as_ptr(), std::ptr::null()];
    match sys::fork().map_err(|e| format!("fork failed: {e}"))? {
        sys::Forked::Child => {
            let _ = sys::unblock_all();
            let error = sys::execve(&program, &argv, &envp);
            say(&format!("{}: execve failed: {error}", path.display()));
            sys::exit_now(127)
        }
        sys::Forked::Parent(pid) => match sys::wait_for(pid, GENERATOR_PATIENCE) {
            Some(how) => Ok(how),
            None => {
                let _ = sys::kill(pid, libc::SIGKILL);
                let _ = sys::wait_for(pid, GENERATOR_PATIENCE);
                Err("did not finish within 5 s, and was killed".to_owned())
            }
        },
    }
}

/// Mount a `.mount` unit. A mount the kernel has made already is a
/// success (§8.1).
fn mount(spec: &MountSpec) -> Result<(), ferrix_svc::event::Errno> {
    let errno =
        |error: io::Error| ferrix_svc::event::Errno(error.raw_os_error().unwrap_or(libc::EIO));
    if mounted(&spec.r#where) {
        return Ok(());
    }
    let _ = fs::create_dir_all(&spec.r#where);
    let (flags, data) = mount_options(&spec.options);
    let c = |text: &str| CString::new(text).map_err(|_| ferrix_svc::event::Errno(libc::EINVAL));
    let what = c(&spec.what)?;
    let target = c(&spec.r#where)?;
    let fs_type = c(spec.fs_type.as_deref().unwrap_or("auto"))?;
    let data = if data.is_empty() {
        None
    } else {
        Some(c(&data)?)
    };
    match sys::mount(&what, &target, &fs_type, flags, data.as_deref()) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::EBUSY) => Ok(()),
        Err(error) => Err(errno(error)),
    }
}

/// Unmount what a `.mount` unit mounted.
fn unmount(target: &str) -> Result<(), ferrix_svc::event::Errno> {
    let target = CString::new(target).map_err(|_| ferrix_svc::event::Errno(libc::EINVAL))?;
    sys::unmount(&target)
        .map_err(|error| ferrix_svc::event::Errno(error.raw_os_error().unwrap_or(libc::EIO)))
}

/// Whether something is mounted at `target`, by `/proc/self/mounts`.
fn mounted(target: &str) -> bool {
    fs::read_to_string("/proc/self/mounts")
        .unwrap_or_default()
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some(target))
}

/// `Options=`, split into the flags `mount(2)` takes and the rest, which
/// goes to the filesystem as its data.
fn mount_options(options: &str) -> (libc::c_ulong, String) {
    let mut flags = 0;
    let mut data = Vec::new();
    for option in options.split(',').filter(|option| !option.is_empty()) {
        match option {
            "ro" => flags |= libc::MS_RDONLY,
            "rw" | "defaults" => {}
            "nosuid" => flags |= libc::MS_NOSUID,
            "nodev" => flags |= libc::MS_NODEV,
            "noexec" => flags |= libc::MS_NOEXEC,
            "noatime" => flags |= libc::MS_NOATIME,
            "relatime" => flags |= libc::MS_RELATIME,
            other => data.push(other),
        }
    }
    (flags, data.join(","))
}

/// §8.2's steps 2 and 3: `sync`, `/` and `/data` read-only, the rest
/// unmounted in reverse order, and `reboot(2)`. A remount the kernel
/// refuses is said and passed over: `reboot(2)` commits `/` and `/data`
/// itself (K7).
fn power(action: PowerAction) -> ! {
    sys::sync();
    let table = fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let targets: Vec<&str> = table
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .collect();
    for target in ["/", "/data"] {
        if !targets.contains(&target) {
            continue;
        }
        let Ok(path) = CString::new(target) else {
            continue;
        };
        let flags = libc::MS_REMOUNT | libc::MS_RDONLY;
        if let Err(error) = sys::mount(c"none", &path, c"none", flags, None) {
            say(&format!("remounting {target} read-only: {error}"));
        }
    }
    let mut kept = Vec::new();
    for target in targets.iter().rev() {
        if matches!(*target, "/" | "/data") {
            continue;
        }
        let Ok(path) = CString::new(*target) else {
            continue;
        };
        if sys::unmount(&path).is_err() {
            kept.push(*target);
        }
    }
    if !kept.is_empty() {
        say(&format!("still mounted, and left so: {}", kept.join(" ")));
    }
    sys::sync();
    let command = match action {
        PowerAction::Poweroff => libc::RB_POWER_OFF,
        PowerAction::Reboot => libc::RB_AUTOBOOT,
    };
    let error = sys::reboot(command);
    say(&format!("reboot(2) failed: {error}"));
    sys::exit_now(1)
}
