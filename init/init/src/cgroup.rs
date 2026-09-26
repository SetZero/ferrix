//! The cgroup backend (§5.1, §5.2): each unit's cgroup as a directory under
//! the cgroup2 mount, its limits as controller files, and its
//! `cgroup.events` watched for the flip of `populated`.
//!
//! # When a cgroup has emptied
//!
//! The manager waits for [`Event::Emptied`](ferrix_svc::event::Event) only
//! for a cgroup it believes populated: one it spawned into or moved
//! processes into, and has not yet heard is empty. This backend keeps the
//! same belief and reports `Emptied` exactly when a cgroup believed
//! populated reads `populated 0`. It reads every cgroup after every wake,
//! not only the one whose `cgroup.events` raised `EPOLLPRI`, because the
//! read is what clears that event (kernfs's rule, which Ferrix keeps), and
//! because a process that is spawned and gone between two waits raises no
//! event that says so afterwards.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::os::unix::fs::FileExt as _;
use std::path::{Path, PathBuf};

use ferrix_svc::event::{GroupPath, UnitId};
use ferrix_svc::limits::{Controller, CpuWeight, Limits, Memory, Tasks};

use crate::sys;

/// Where cgroup2 is mounted (§5.1).
pub(crate) const MOUNT: &str = "/sys/fs/cgroup";

/// The cgroup init moves itself into before it enables any controller,
/// since cgroup v2 keeps processes out of a cgroup whose controllers are
/// enabled for its children.
const INIT_SCOPE: &str = "init.scope";

/// The controllers init writes limits through, in `subtree_control`'s
/// order.
const CONTROLLERS: [Controller; 4] = [
    Controller::Cpu,
    Controller::Io,
    Controller::Memory,
    Controller::Pids,
];

/// One cgroup init made.
#[derive(Debug)]
struct Group {
    /// Where, below the mount.
    path: GroupPath,
    /// The directory, for `CLONE_INTO_CGROUP`.
    dir: File,
    /// Its `cgroup.events`, watched for `EPOLLPRI`.
    events: File,
    /// Whether the manager believes it has processes.
    populated: bool,
}

/// Every cgroup init made, by unit.
#[derive(Debug)]
pub(crate) struct Groups {
    /// The mount.
    root: PathBuf,
    /// The groups.
    groups: BTreeMap<UnitId, Group>,
}

impl Groups {
    /// Mount cgroup2 at [`MOUNT`], move init into `init.scope`, and enable
    /// in the root's `cgroup.subtree_control` every controller the kernel
    /// has. What is not there is said, and init goes on without it.
    pub(crate) fn mount(log: &mut dyn FnMut(String)) -> io::Result<Groups> {
        let root = PathBuf::from(MOUNT);
        fs::create_dir_all(&root)?;
        let source = c"cgroup2";
        let target = CString::new(MOUNT).map_err(io::Error::other)?;
        match sys::mount(source, &target, source, 0, None) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(libc::EBUSY) => {}
            Err(error) => return Err(error),
        }
        let scope = root.join(INIT_SCOPE);
        make_dir(&scope)?;
        write(&scope.join("cgroup.procs"), &std::process::id().to_string())?;
        let groups = Groups {
            root,
            groups: BTreeMap::new(),
        };
        groups.enable_controllers(&groups.root, log);
        Ok(groups)
    }

    /// Enable in `dir`'s `cgroup.subtree_control` every controller its
    /// `cgroup.controllers` offers.
    fn enable_controllers(&self, dir: &Path, log: &mut dyn FnMut(String)) {
        let offered = fs::read_to_string(dir.join("cgroup.controllers")).unwrap_or_default();
        let words: Vec<String> = CONTROLLERS
            .iter()
            .map(|controller| controller.name())
            .filter(|name| offered.split_whitespace().any(|word| word == *name))
            .map(|name| format!("+{name}"))
            .collect();
        if words.is_empty() {
            return;
        }
        if let Err(error) = write(&dir.join("cgroup.subtree_control"), &words.join(" ")) {
            log(format!(
                "{}: enabling {} failed: {error}",
                self.display(dir),
                words.join(" ")
            ));
        }
    }

    /// A path below the mount, as the log shows it.
    fn display(&self, dir: &Path) -> String {
        match dir.strip_prefix(&self.root) {
            Ok(path) => format!("/{}", path.display()),
            Err(_) => dir.display().to_string(),
        }
    }

    /// Make `unit`'s cgroup at `path` and write its limits; for a slice,
    /// enable its children's controllers. Returns the descriptor of its
    /// `cgroup.events` for the caller to watch.
    pub(crate) fn make(
        &mut self,
        unit: UnitId,
        path: &GroupPath,
        limits: &Limits,
        log: &mut dyn FnMut(String),
    ) -> io::Result<RawFd> {
        let dir = self.root.join(path.as_str());
        if !path.as_str().is_empty() {
            make_dir(&dir)?;
        }
        if path.as_str().is_empty() || path.as_str().ends_with(".slice") {
            self.enable_controllers(&dir, log);
        }
        self.write_limits(&dir, limits, log);
        let group = Group {
            path: path.clone(),
            dir: OpenOptions::new()
                .read(true)
                .custom_flags_directory()
                .open(&dir)?,
            events: File::open(dir.join("cgroup.events"))?,
            populated: false,
        };
        let fd = group.events.as_raw_fd();
        if let Some(old) = self.groups.insert(unit, group) {
            drop(old);
        }
        Ok(fd)
    }

    /// Write new limits to `unit`'s cgroup.
    pub(crate) fn set_limits(&self, unit: UnitId, limits: &Limits, log: &mut dyn FnMut(String)) {
        if let Some(group) = self.groups.get(&unit) {
            self.write_limits(&self.root.join(group.path.as_str()), limits, log);
        }
    }

    /// Write each limit that is set to its controller's file. A controller
    /// the cgroup does not have is a warning (§5.5).
    fn write_limits(&self, dir: &Path, limits: &Limits, log: &mut dyn FnMut(String)) {
        let offered = dir
            .parent()
            .map(|parent| fs::read_to_string(parent.join("cgroup.subtree_control")))
            .and_then(Result::ok)
            .unwrap_or_default();
        let mut files: Vec<(Controller, &str, String)> = Vec::new();
        if let Some(memory) = limits.memory_max {
            files.push((Controller::Memory, "memory.max", memory_value(memory)));
        }
        if let Some(memory) = limits.memory_high {
            files.push((Controller::Memory, "memory.high", memory_value(memory)));
        }
        if let Some(tasks) = limits.tasks_max {
            files.push((Controller::Pids, "pids.max", tasks_value(tasks)));
        }
        match limits.cpu_weight {
            Some(CpuWeight::Weight(weight)) => {
                files.push((Controller::Cpu, "cpu.weight", weight.to_string()));
            }
            Some(CpuWeight::Idle) => files.push((Controller::Cpu, "cpu.idle", "1".to_owned())),
            None => {}
        }
        if let Some(quota) = limits.cpu_quota {
            // Hundredths of a percent of one CPU, per 100 ms.
            let micros = quota.saturating_mul(10);
            files.push((Controller::Cpu, "cpu.max", format!("{micros} 100000")));
        }
        if let Some(weight) = limits.io_weight {
            files.push((Controller::Io, "io.weight", format!("default {weight}")));
        }
        for (controller, file, value) in files {
            let name = controller.name();
            if !offered.split_whitespace().any(|word| word == name) {
                log(format!(
                    "{}: no {name} controller, so {file} is not set",
                    self.display(dir)
                ));
                continue;
            }
            if let Err(error) = write(&dir.join(file), &value) {
                log(format!(
                    "{}: writing {value} to {file} failed: {error}",
                    self.display(dir)
                ));
            }
        }
    }

    /// Remove `unit`'s cgroup, returning its `cgroup.events` descriptor so
    /// the caller can stop watching it first.
    pub(crate) fn remove(&mut self, unit: UnitId) -> Option<(RawFd, io::Result<()>)> {
        let group = self.groups.remove(&unit)?;
        let fd = group.events.as_raw_fd();
        let dir = self.root.join(group.path.as_str());
        Some((fd, fs::remove_dir(dir)))
    }

    /// The events descriptor of `unit`'s cgroup, to stop watching.
    pub(crate) fn events_fd(&self, unit: UnitId) -> Option<RawFd> {
        self.groups.get(&unit).map(|group| group.events.as_raw_fd())
    }

    /// The directory of the cgroup at `path`, for `CLONE_INTO_CGROUP`.
    pub(crate) fn dir(&self, path: &GroupPath) -> Option<BorrowedFd<'_>> {
        self.groups
            .values()
            .find(|group| group.path == *path)
            .map(|group| group.dir.as_fd())
    }

    /// Record that processes were put in the cgroup at `path`.
    pub(crate) fn filled(&mut self, path: &GroupPath) {
        if let Some(group) = self.groups.values_mut().find(|group| group.path == *path) {
            group.populated = true;
        }
    }

    /// The pids in `unit`'s cgroup.
    pub(crate) fn procs(&self, unit: UnitId) -> Vec<u32> {
        let Some(group) = self.groups.get(&unit) else {
            return Vec::new();
        };
        let path = self.root.join(group.path.as_str()).join("cgroup.procs");
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect()
    }

    /// Move `pids` into `unit`'s cgroup, returning what could not be moved.
    pub(crate) fn move_in(&mut self, unit: UnitId, pids: &[u32]) -> Vec<(u32, io::Error)> {
        let Some(group) = self.groups.get_mut(&unit) else {
            return Vec::new();
        };
        group.populated = true;
        let path = self.root.join(group.path.as_str()).join("cgroup.procs");
        pids.iter()
            .filter_map(|&pid| write(&path, &pid.to_string()).err().map(|e| (pid, e)))
            .collect()
    }

    /// Write `cgroup.kill` in `unit`'s cgroup.
    pub(crate) fn kill(&self, unit: UnitId) -> io::Result<()> {
        let Some(group) = self.groups.get(&unit) else {
            return Ok(());
        };
        write(
            &self.root.join(group.path.as_str()).join("cgroup.kill"),
            "1",
        )
    }

    /// Read every cgroup's `cgroup.events`, clearing its `EPOLLPRI`, and
    /// return the units whose cgroup was believed populated and is empty.
    pub(crate) fn emptied(&mut self) -> Vec<UnitId> {
        let mut emptied = Vec::new();
        for (&unit, group) in &mut self.groups {
            let mut text = [0_u8; 128];
            let Ok(count) = group.events.read_at(&mut text, 0) else {
                continue;
            };
            let text = String::from_utf8_lossy(text.get(..count).unwrap_or_default());
            let empty = text.lines().any(|line| line.trim() == "populated 0");
            if empty && group.populated {
                group.populated = false;
                emptied.push(unit);
            }
        }
        emptied
    }
}

/// `mkdir`, where one that is there already is fine.
fn make_dir(dir: &Path) -> io::Result<()> {
    match fs::create_dir(dir) {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => Err(error),
        _ => Ok(()),
    }
}

/// Write `value` to a cgroup file in one `write`, as the kernel wants it.
fn write(path: &Path, value: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.write_all(value.as_bytes())
}

/// A memory limit as `memory.max` takes it.
fn memory_value(memory: Memory) -> String {
    match memory {
        Memory::Bytes(bytes) => bytes.to_string(),
        Memory::Share(share) => {
            let total = meminfo_total().unwrap_or(0);
            (u128::from(total) * u128::from(share) / 10_000).to_string()
        }
        Memory::Infinity => "max".to_owned(),
    }
}

/// A task limit as `pids.max` takes it.
fn tasks_value(tasks: Tasks) -> String {
    match tasks {
        Tasks::Count(count) => count.to_string(),
        Tasks::Share(share) => {
            let most: u64 = fs::read_to_string("/proc/sys/kernel/pid_max")
                .ok()
                .and_then(|text| text.trim().parse().ok())
                .unwrap_or(32_768);
            (u128::from(most) * u128::from(share) / 10_000).to_string()
        }
        Tasks::Infinity => "max".to_owned(),
    }
}

/// `MemTotal` from `/proc/meminfo`, in bytes.
fn meminfo_total() -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

/// `O_DIRECTORY` on an [`OpenOptions`].
trait DirectoryFlag {
    /// Open a directory, and nothing else.
    fn custom_flags_directory(&mut self) -> &mut Self;
}

impl DirectoryFlag for OpenOptions {
    fn custom_flags_directory(&mut self) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt as _;
        self.custom_flags(libc::O_DIRECTORY)
    }
}
