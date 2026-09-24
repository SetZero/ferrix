//! cgroupfs: the job tree, seen as cgroup v2 (`docs/CGROUPS.md` §3).
//!
//! Every directory is a job and every job beneath the root job is a
//! directory: one made by `mkdir` under its name, one native `job_create`
//! made as `job-<id>`. The job is the truth and this is a view of it, as
//! procfs is a view of processes, so nothing is stored here: each lookup asks
//! the job, and each file renders what the job says when it is opened.
//!
//! The text of every file, and how every write is read, is `ferrix-cgroupfs`'s,
//! where the host tests and the fuzzer reach it. What this module adds is the
//! kernel's half: which job, which process, which errno.
//!
//! # What G2 has, and what comes later
//!
//! The tree, `cgroup.procs` read and moves, `cgroup.kill`, `cgroup.events`
//! read and polled for `POLLPRI` (G3), the limits on depth and descendants, and
//! `cgroup.subtree_control` with no controller to enable yet. A move needs
//! write access to the target's `cgroup.procs`, which only root has until
//! delegation (G4) lets `chown` hand a subtree to a user. `cgroup.freeze`
//! reads `0` and refuses writes until F1.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

use ferrix_cgroupfs::controllers::{self, Set};
use ferrix_cgroupfs::files::{self, Kind};
use ferrix_cgroupfs::write::{self, Target};
use ferrix_cgroupfs::{Refusal, name, render};
use ferrix_linux_abi::errno::Errno;
use ferrix_vfs::{
    DirEntry, FIRST_CURSOR, FileSystem, FileType, Inode, Metadata, NewNode, Readiness, StatFs,
    Timespec,
};

use crate::fs::{self, procfs};
use crate::object::job::{self, Job, JobError};
use crate::sync::SpinLock;
use crate::syscall::process;
use crate::syscall::registry;

mod events_check;

/// The result every operation here returns.
type Result<T> = core::result::Result<T, Errno>;

/// `CGROUP2_SUPER_MAGIC`, from `include/uapi/linux/magic.h`: how systemd
/// tells a cgroup v2 mount from a v1 one before trusting it.
const CGROUP2_SUPER_MAGIC: u64 = 0x6367_7270;

/// The controllers this kernel has built. None yet: `pids` is landing P1.
const BUILT: Set = Set::EMPTY;

/// Where a directory's children begin in its cursor space, past its files.
const CHILD_CURSORS: u64 = 1 << 32;

/// A cgroupfs instance: a view of the root job's tree.
#[derive(Debug)]
pub(crate) struct Cgroupfs {
    /// What every node shares.
    shared: Arc<Shared>,
}

/// What every node of one instance shares.
#[derive(Debug)]
struct Shared {
    /// `st_dev`.
    device: u64,
    /// Every timestamp: when it was mounted.
    made: Timespec,
}

impl Cgroupfs {
    /// A cgroupfs over the root job, stamped with the time it was made.
    pub(crate) fn new() -> Cgroupfs {
        Cgroupfs {
            shared: Arc::new(Shared {
                device: fs::anonymous_device(),
                made: fs::clock().now(),
            }),
        }
    }
}

impl FileSystem for Cgroupfs {
    fn root(&self) -> Arc<dyn Inode> {
        Arc::new(Directory {
            job: Arc::clone(job::root()),
            shared: Arc::clone(&self.shared),
        })
    }

    fn name(&self) -> &'static str {
        "cgroup2"
    }

    fn device(&self) -> u64 {
        self.shared.device
    }

    fn statfs(&self) -> StatFs {
        StatFs {
            magic: CGROUP2_SUPER_MAGIC,
            block_size: 4096,
            name_max: 255,
            ..StatFs::default()
        }
    }
}

/// The errno Linux answers a refused write or name with.
fn errno(refusal: Refusal) -> Errno {
    match refusal {
        Refusal::Invalid => Errno::EINVAL,
        Refusal::Range => Errno::ERANGE,
        Refusal::NotSupported => Errno::EOPNOTSUPP,
        Refusal::TooLong => Errno::ENAMETOOLONG,
    }
}

/// The inode number of a job's directory; its files follow it. A job's id is
/// never reused, so neither is a number.
fn ino(job: &Job, slot: u64) -> u64 {
    job.id().saturating_mul(32).saturating_add(slot)
}

/// A job's directory.
#[derive(Debug)]
struct Directory {
    /// The job.
    job: Arc<Job>,
    /// The instance's device and timestamps.
    shared: Arc<Shared>,
}

impl Directory {
    /// Metadata for something in this directory's instance.
    fn metadata_for(&self, ino: u64, kind: FileType, permissions: u32) -> Metadata {
        Metadata {
            ino,
            kind,
            permissions,
            nlink: if kind == FileType::Directory { 2 } else { 1 },
            uid: 0,
            gid: 0,
            size: 0,
            rdev: 0,
            blocks: 0,
            block_size: 4096,
            atime: self.shared.made,
            mtime: self.shared.made,
            ctime: self.shared.made,
        }
    }

    /// The child job shown as `name`, if there is one.
    fn child(&self, name: &[u8]) -> Option<Arc<Job>> {
        self.job
            .children()
            .into_iter()
            .find(|child| child.display_name().as_bytes() == name)
    }

    /// Whether this is the root of the tree cgroupfs shows.
    fn is_root(&self) -> bool {
        self.job.is_root()
    }
}

impl Inode for Directory {
    fn metadata(&self) -> Metadata {
        self.metadata_for(ino(&self.job, 0), FileType::Directory, 0o755)
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    /// Asked afresh on every walk: a native `job_create` adds a directory,
    /// and a job going away takes one, without the VFS seeing either.
    fn caches_lookups(&self) -> bool {
        false
    }

    fn lookup(&self, name: &[u8]) -> Result<Arc<dyn Inode>> {
        if let Some(file) = files::named(name, self.is_root()) {
            return Ok(Arc::new(Interface {
                job: Arc::clone(&self.job),
                file,
                metadata: self.metadata_for(
                    ino(&self.job, 1 + file_slot(file)),
                    FileType::Regular,
                    u32::from(file.mode()),
                ),
            }));
        }
        let child = self.child(name).ok_or(Errno::ENOENT)?;
        Ok(Arc::new(Directory {
            job: child,
            shared: Arc::clone(&self.shared),
        }))
    }

    /// `mkdir`: a new named job. Anything else is refused, as kernfs refuses
    /// a file made in a cgroup directory.
    fn create(&self, name: &[u8], node: NewNode<'_>, _permissions: u32) -> Result<Arc<dyn Inode>> {
        if node != NewNode::Directory {
            return Err(Errno::EACCES);
        }
        name::check(name).map_err(errno)?;
        if files::named(name, self.is_root()).is_some() || self.child(name).is_some() {
            return Err(Errno::EEXIST);
        }
        let text = core::str::from_utf8(name).map_err(|_| Errno::EINVAL)?;
        let child = self
            .job
            .new_named_child(text)
            .map_err(|refused| match refused {
                JobError::Exists => Errno::EEXIST,
                JobError::Limited => Errno::EAGAIN,
                JobError::Killed | JobError::Missing | JobError::Busy => Errno::ENOENT,
            })?;
        Ok(Arc::new(Directory {
            job: child,
            shared: Arc::clone(&self.shared),
        }))
    }

    /// `rmdir`: only an empty named job. An anonymous one is held by the
    /// handles of whoever made it, and goes when they do.
    fn rmdir(&self, name: &[u8]) -> Result<()> {
        let text = core::str::from_utf8(name).map_err(|_| Errno::ENOENT)?;
        match self.job.remove_named_child(text) {
            Ok(_removed) => Ok(()),
            Err(JobError::Missing) if self.child(name).is_some() => Err(Errno::EBUSY),
            Err(JobError::Missing) => Err(Errno::ENOENT),
            Err(_) => Err(Errno::EBUSY),
        }
    }

    fn read_dir(&self, cursor: u64, emit: &mut dyn FnMut(DirEntry<'_>) -> bool) -> Result<()> {
        let root = self.is_root();
        let first = usize::try_from(cursor.saturating_sub(FIRST_CURSOR)).unwrap_or(usize::MAX);
        if cursor < CHILD_CURSORS {
            for (index, file) in files::of(root).enumerate().skip(first) {
                let accepted = emit(DirEntry {
                    ino: ino(&self.job, 1 + file_slot(file)),
                    kind: FileType::Regular,
                    name: file.name.as_bytes(),
                    next: FIRST_CURSOR.saturating_add(index as u64 + 1),
                });
                if !accepted {
                    return Ok(());
                }
            }
        }
        // Children in id order, resumed by id, so a child made or removed
        // between two reads neither repeats nor hides another.
        let from = cursor.saturating_sub(CHILD_CURSORS);
        let mut children = self.job.children();
        children.sort_unstable_by_key(|child| child.id());
        for child in children.iter().filter(|child| child.id() >= from) {
            let name = child.display_name();
            let accepted = emit(DirEntry {
                ino: ino(child, 0),
                kind: FileType::Directory,
                name: name.as_bytes(),
                next: CHILD_CURSORS.saturating_add(child.id()).saturating_add(1),
            });
            if !accepted {
                break;
            }
        }
        Ok(())
    }
}

/// A file's place in [`files::FILES`], which its inode number is made from.
fn file_slot(file: &files::File) -> u64 {
    files::FILES
        .iter()
        .position(|each| core::ptr::eq(each, file))
        .map_or(0, |at| at as u64)
}

/// One interface file of one job.
#[derive(Debug)]
struct Interface {
    /// The job it is a file of.
    job: Arc<Job>,
    /// Which file.
    file: &'static files::File,
    /// What `stat` reports.
    metadata: Metadata,
}

impl Inode for Interface {
    fn metadata(&self) -> Metadata {
        self.metadata
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    /// Accepted and ignored, as kernfs ignores the `O_TRUNC` a shell's `>`
    /// asks for.
    fn set_len(&self, _len: u64) -> Result<()> {
        Ok(())
    }

    /// The contents as they are now, and a writer for a file that takes
    /// writes. `cgroup.events` is the exception: it is rendered when read,
    /// and polled, so an open of it is an [`EventsFile`].
    fn open(&self) -> Result<Option<Arc<dyn Inode>>> {
        if self.file.kind == Kind::Events {
            return Ok(Some(Arc::new(EventsFile {
                job: Arc::clone(&self.job),
                metadata: self.metadata,
                rendered: SpinLock::new(Rendered::default()),
            })));
        }
        let bytes = contents(&self.job, self.file.kind);
        let job = Arc::clone(&self.job);
        let kind = self.file.kind;
        let writer: Option<procfs::Writer> = self
            .file
            .writable
            .then(|| Box::new(move |data: &[u8]| write_to(&job, kind, data)) as procfs::Writer);
        Ok(Some(procfs::snapshot(
            self.metadata,
            bytes,
            writer,
            Errno::EACCES,
        )))
    }
}

/// One open of a `cgroup.events` (`docs/CGROUPS.md` §4): what it says,
/// rendered afresh by every read from its start, and `POLLPRI` from a change
/// until it is read that way again.
///
/// That is kernfs's `kernfs_generic_poll`. The job's event queue is woken at
/// every flip of populated, and its wake count is the change counter: a read
/// from the start records the count it rendered at, and the file reports
/// priority (with `POLLERR`, as Linux does) while the count has moved since.
/// An open file not yet read reports it too, as on Linux, where the open
/// node's counter starts one ahead of a new open file's.
#[derive(Debug)]
struct EventsFile {
    /// The job it reports on.
    job: Arc<Job>,
    /// What `stat` reports, less the size, which is the last rendering's.
    metadata: Metadata,
    /// The last rendering.
    rendered: SpinLock<Rendered>,
}

/// What an [`EventsFile`] last rendered, and when.
#[derive(Debug, Default)]
struct Rendered {
    /// The text.
    bytes: Vec<u8>,
    /// The job's wake count read just before it was rendered, or `None`
    /// before the first read.
    seen: Option<u64>,
}

impl EventsFile {
    /// Whether the job has changed since the last rendering.
    fn changed(&self) -> bool {
        self.rendered.lock().seen != Some(self.job.events().wakes())
    }
}

impl Inode for EventsFile {
    fn metadata(&self) -> Metadata {
        Metadata {
            size: self.rendered.lock().bytes.len() as u64,
            ..self.metadata
        }
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    /// A read from the start renders the file again, as a `seq_file` does
    /// after `lseek` to 0, and that is what clears `POLLPRI`; a read further
    /// on continues the last rendering.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if offset == 0 {
            // The count before the text: a flip in between leaves the count
            // ahead of what was rendered, and the file still reporting.
            let seen = self.job.events().wakes();
            let bytes = contents(&self.job, Kind::Events);
            *self.rendered.lock() = Rendered {
                bytes,
                seen: Some(seen),
            };
        }
        let rendered = self.rendered.lock();
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let rest = rendered.bytes.get(start..).unwrap_or_default();
        let count = rest.len().min(buf.len());
        let (Some(to), Some(from)) = (buf.get_mut(..count), rest.get(..count)) else {
            return Ok(0);
        };
        to.copy_from_slice(from);
        Ok(count)
    }

    fn write_at(&self, _offset: u64, _data: &[u8], _append: bool) -> Result<(usize, u64)> {
        Err(Errno::EACCES)
    }

    /// Always readable, as kernfs's `DEFAULT_POLLMASK` is, and `POLLPRI`
    /// with `POLLERR` while the job has changed since the last rendering.
    fn poll(&self) -> Readiness {
        let changed = self.changed();
        Readiness {
            error: changed,
            priority: changed,
            ..Readiness::ALWAYS
        }
    }

    fn poll_changes(&self) -> Option<u64> {
        Some(self.job.events().wakes())
    }

    /// The job's event queue, which every flip wakes.
    fn poll_queues(&self, visit: &mut dyn FnMut(ferrix_vfs::WakeSource)) -> bool {
        visit(fs::wake::shared(self.job.events()));
        true
    }
}

/// What a file of `job` says now.
fn contents(job: &Arc<Job>, kind: Kind) -> Vec<u8> {
    let mut out = Vec::new();
    match kind {
        Kind::Type => out.extend_from_slice(b"domain\n"),
        Kind::Procs => render::ids(&mut out, &members(job)),
        Kind::Threads => {
            let mut tids: Vec<u32> = members_processes(job)
                .iter()
                .flat_map(|process| procfs::thread_ids(process))
                .collect();
            tids.sort_unstable();
            render::ids(&mut out, &tids);
        }
        // What the parent enables here; the root is offered what is built.
        Kind::Controllers | Kind::SubtreeControl => controllers::render(&mut out, BUILT),
        Kind::Events => render::events(&mut out, job.is_populated(), false),
        Kind::MaxDescendants => render::limit(&mut out, job.limits().1),
        Kind::MaxDepth => render::limit(&mut out, job.limits().0),
        Kind::Stat => render::stat(&mut out, job.descendants()),
        Kind::Freeze => out.extend_from_slice(b"0\n"),
        Kind::Kill => {}
    }
    out
}

/// The live processes directly in `job`, not beneath it, in pid order.
fn members_processes(job: &Arc<Job>) -> Vec<Arc<process::Process>> {
    registry::live()
        .into_iter()
        .filter(|process| Arc::ptr_eq(&process.job(), job) && !process.is_terminated())
        .collect()
}

/// Their pids.
fn members(job: &Arc<Job>) -> Vec<u32> {
    members_processes(job)
        .iter()
        .map(|process| process.pid())
        .collect()
}

/// A write of `data` to a file of `job`.
fn write_to(job: &Arc<Job>, kind: Kind, data: &[u8]) -> Result<usize> {
    match kind {
        Kind::Procs => {
            let process = match write::parse_procs(data).map_err(errno)? {
                Target::Writer => process::current().ok_or(Errno::ESRCH)?,
                Target::Pid(pid) => registry::find(pid).ok_or(Errno::ESRCH)?,
            };
            job.adopt(&process).map_err(|_| Errno::ENOENT)?;
        }
        Kind::Kill => {
            write::parse_kill(data).map_err(errno)?;
            let _ = job.kill_members();
        }
        Kind::SubtreeControl => {
            // Nothing is built, so nothing parses but an empty change, and an
            // empty change changes nothing.
            let _change = controllers::parse_change(data, BUILT).map_err(errno)?;
        }
        Kind::MaxDepth => job.set_max_depth(write::parse_limit(data).map_err(errno)?),
        Kind::MaxDescendants => job.set_max_descendants(write::parse_limit(data).map_err(errno)?),
        Kind::Type => write::parse_type(data).map_err(errno)?,
        Kind::Threads | Kind::Freeze => return Err(Errno::EOPNOTSUPP),
        Kind::Controllers | Kind::Events | Kind::Stat => return Err(Errno::EACCES),
    }
    Ok(data.len())
}

/// `/proc/<pid>/cgroup` for a process in `job`.
pub(crate) fn proc_cgroup(job: &Job) -> Vec<u8> {
    let names = job.path_names();
    let mut out = Vec::new();
    render::proc_cgroup(&mut out, names.iter().map(String::as_bytes));
    out
}

/// What [`check`] counted, for the boot line.
#[derive(Debug, Default)]
pub(crate) struct Report {
    /// Cgroups made and removed.
    pub(crate) made: u32,
    /// Writes and names refused as Linux refuses them.
    pub(crate) refusals: u32,
    /// Waits on `cgroup.events` that a release's wake ended.
    pub(crate) woken: u32,
}

/// Where [`check`] mounts its cgroupfs: under `/tmp`, and gone afterwards.
const CHECK_AT: &[u8] = b"/tmp/cgroup-check";

/// A check's failure, by name.
type Checked<T> = core::result::Result<T, &'static str>;

/// The check's way into its mount: paths beneath [`CHECK_AT`], driven
/// through the namespace as a program's calls would be.
struct Harness {
    /// The namespace.
    ns: &'static ferrix_vfs::Namespace,
    /// Root's view of it.
    ctx: ferrix_vfs::Context,
    /// What was counted.
    report: Report,
}

impl Harness {
    /// `tail` beneath the mount.
    fn path(tail: &[u8]) -> Vec<u8> {
        let mut path = Vec::from(CHECK_AT);
        path.extend_from_slice(tail);
        path
    }

    /// Everything in the file at `whole`, read to the end as a program reads
    /// it. `fs::read_file` sizes its buffer from `stat`, and a generated file
    /// -- here as in procfs -- says 0.
    fn read_path(&self, whole: &[u8]) -> Result<Vec<u8>> {
        let flags = ferrix_vfs::OpenFlags {
            read: true,
            ..ferrix_vfs::OpenFlags::default()
        };
        let file = self.ns.open(&self.ctx, None, whole, &flags, 0)?;
        Harness::read_to_end(&file)
    }

    /// Everything left in an open file, read to the end.
    fn read_to_end(file: &ferrix_vfs::OpenFile) -> Result<Vec<u8>> {
        let mut contents = Vec::new();
        let mut chunk = [0_u8; 256];
        loop {
            let count = file.read(&mut chunk)?;
            let Some(read) = chunk.get(..count).filter(|read| !read.is_empty()) else {
                return Ok(contents);
            };
            contents.extend_from_slice(read);
        }
    }

    /// The file at `tail` beneath the mount, opened for reading.
    fn open_read(&self, tail: &[u8]) -> Result<Arc<ferrix_vfs::OpenFile>> {
        let flags = ferrix_vfs::OpenFlags {
            read: true,
            ..ferrix_vfs::OpenFlags::default()
        };
        self.ns
            .open(&self.ctx, None, &Harness::path(tail), &flags, 0)
    }

    /// The file at `tail` beneath the mount, read whole.
    fn read(&self, tail: &[u8]) -> Result<Vec<u8>> {
        self.read_path(&Harness::path(tail))
    }

    /// Whether the file at `tail` reads exactly `expected`.
    fn reads(&self, tail: &[u8], expected: &[u8]) -> bool {
        self.read(tail).as_deref() == Ok(expected)
    }

    /// Write `data` to the file at `tail`, in one write.
    fn write(&self, tail: &[u8], data: &[u8]) -> Result<usize> {
        let flags = ferrix_vfs::OpenFlags {
            write: true,
            ..ferrix_vfs::OpenFlags::default()
        };
        self.ns
            .open(&self.ctx, None, &Harness::path(tail), &flags, 0)?
            .write(data)
    }

    /// `mkdir` at `tail`.
    fn mkdir(&self, tail: &[u8]) -> Result<()> {
        self.ns.mkdir(&self.ctx, None, &Harness::path(tail), 0o755)
    }

    /// `rmdir` at `tail`.
    fn rmdir(&self, tail: &[u8]) -> Result<()> {
        self.ns.rmdir(&self.ctx, None, &Harness::path(tail))
    }

    /// Whether something is at `tail`.
    fn exists(&self, tail: &[u8]) -> bool {
        self.ns
            .resolve(&self.ctx, None, &Harness::path(tail), true)
            .is_ok()
    }

    /// Require the error an operation answered, `got`, to be `errno`, and
    /// count it.
    fn refused(&mut self, got: Option<Errno>, errno: Errno, what: &'static str) -> Checked<()> {
        match got {
            Some(got) if got == errno => {
                self.report.refusals += 1;
                Ok(())
            }
            _ => Err(what),
        }
    }
}

/// Stage 13's cgroupfs check, landing G2: a mount of `cgroup2` shows the job
/// tree, `mkdir` makes a job and `rmdir` takes an empty one, a pid written to
/// `cgroup.procs` moves that process, `/proc/<pid>/cgroup` and
/// `cgroup.events` say where it is and that its cgroup is populated,
/// `cgroup.kill` ends it and leaves the cgroup to be removed, the limits on
/// descendants hold, and what Linux refuses is refused.
///
/// # Errors
///
/// The first thing that was not as Linux has it, by name.
pub(crate) fn check() -> Checked<Report> {
    let ns = fs::namespace();
    let mut harness = Harness {
        ns,
        ctx: ns.context(),
        report: Report::default(),
    };
    match ns.mkdir(&harness.ctx, None, CHECK_AT, 0o755) {
        Ok(()) | Err(Errno::EEXIST) => {}
        Err(_) => return Err("could not make the cgroupfs check's mount point"),
    }
    let at = ns
        .resolve(&harness.ctx, None, CHECK_AT, true)
        .map_err(|_| "the cgroupfs check's mount point did not resolve")?;
    let _mount = ns
        .mount(Arc::new(Cgroupfs::new()), &at)
        .map_err(|_| "cgroup2 did not mount")?;

    let process =
        process::new_for_check().map_err(|_| "could not make a process for the cgroupfs check")?;
    check_the_root(&harness, process.pid())?;
    check_a_move_and_a_kill(&mut harness, &process)?;
    check_the_limits(&mut harness)?;
    harness.report.woken = events_check::run(&mut harness)?;

    let root = ns
        .resolve(&harness.ctx, None, CHECK_AT, true)
        .map_err(|_| "the cgroupfs mount did not resolve")?;
    ns.unmount(&root).map_err(|_| "cgroup2 did not unmount")?;
    let _ = ns.rmdir(&harness.ctx, None, CHECK_AT);
    drop(process);
    Ok(harness.report)
}

/// The root lists every process not moved elsewhere, and has none of the
/// files Linux keeps off it.
fn check_the_root(harness: &Harness, pid: u32) -> Checked<()> {
    let listed = alloc::format!("{pid}");
    let procs = harness
        .read(b"/cgroup.procs")
        .map_err(|_| "the root's cgroup.procs did not read")?;
    if !procs
        .split(|&byte| byte == b'\n')
        .any(|line| line == listed.as_bytes())
    {
        return Err("a new process is not listed in the root's cgroup.procs");
    }
    if harness.exists(b"/cgroup.kill") {
        return Err("the root cgroup has a cgroup.kill");
    }
    Ok(())
}

/// A cgroup made, a process moved in by its pid and seen there, the cgroup
/// kept while populated, then emptied by `cgroup.kill` and removed.
fn check_a_move_and_a_kill(harness: &mut Harness, process: &process::Process) -> Checked<()> {
    const EMPTY: &[u8] = b"populated 0\nfrozen 0\n";
    const FULL: &[u8] = b"populated 1\nfrozen 0\n";
    let pid = process.pid();
    let listed = alloc::format!("{pid}\n");

    harness
        .mkdir(b"/check-a")
        .map_err(|_| "mkdir in cgroupfs failed")?;
    harness.report.made += 1;
    if !harness.reads(b"/check-a/cgroup.events", EMPTY) {
        return Err("a new cgroup's cgroup.events does not say it is empty");
    }
    let again = harness.mkdir(b"/check-a");
    harness.refused(
        again.err(),
        Errno::EEXIST,
        "mkdir over a cgroup was not EEXIST",
    )?;
    let over = harness.mkdir(b"/cgroup.procs");
    harness.refused(
        over.err(),
        Errno::EEXIST,
        "mkdir over an interface file was not EEXIST",
    )?;

    let _ = harness
        .write(b"/check-a/cgroup.procs", listed.as_bytes())
        .map_err(|_| "writing a pid to cgroup.procs failed")?;
    if process.job().name() != Some("check-a") {
        return Err("a pid written to cgroup.procs did not move its process");
    }
    if !harness.reads(b"/check-a/cgroup.procs", listed.as_bytes()) {
        return Err("a cgroup's cgroup.procs does not list the process moved into it");
    }
    let proc_path = alloc::format!("/proc/{pid}/cgroup");
    if harness.read_path(proc_path.as_bytes()).as_deref() != Ok(&b"0::/check-a\n"[..]) {
        return Err("/proc/<pid>/cgroup does not name the cgroup the process was moved to");
    }
    if !harness.reads(b"/check-a/cgroup.events", FULL) {
        return Err("a cgroup with a process in it does not say it is populated");
    }
    let busy = harness.rmdir(b"/check-a");
    harness.refused(
        busy.err(),
        Errno::EBUSY,
        "rmdir of a populated cgroup was not EBUSY",
    )?;

    let zero = harness.write(b"/check-a/cgroup.kill", b"0\n");
    harness.refused(
        zero.err(),
        Errno::ERANGE,
        "cgroup.kill took a number other than 1",
    )?;
    let _ = harness
        .write(b"/check-a/cgroup.kill", b"1\n")
        .map_err(|_| "writing 1 to cgroup.kill failed")?;
    if !process.is_terminated() {
        return Err("cgroup.kill did not end the process in its cgroup");
    }
    if !harness.reads(b"/check-a/cgroup.events", EMPTY) {
        return Err("a killed cgroup does not say it is empty");
    }
    harness
        .rmdir(b"/check-a")
        .map_err(|_| "rmdir of an emptied cgroup failed")?;
    if harness.exists(b"/check-a") {
        return Err("a removed cgroup can still be found");
    }
    Ok(())
}

/// `cgroup.max.descendants` holds and `cgroup.stat` counts, and what the
/// rest of the files refuse is refused.
fn check_the_limits(harness: &mut Harness) -> Checked<()> {
    harness
        .mkdir(b"/check-b")
        .map_err(|_| "mkdir of a second cgroup failed")?;
    let _ = harness
        .write(b"/check-b/cgroup.max.descendants", b"1\n")
        .map_err(|_| "writing cgroup.max.descendants failed")?;
    harness
        .mkdir(b"/check-b/c")
        .map_err(|_| "mkdir within cgroup.max.descendants failed")?;
    harness.report.made += 2;
    let past = harness.mkdir(b"/check-b/d");
    harness.refused(
        past.err(),
        Errno::EAGAIN,
        "mkdir past cgroup.max.descendants was not EAGAIN",
    )?;
    if !harness.reads(
        b"/check-b/cgroup.stat",
        b"nr_descendants 1\nnr_dying_descendants 0\n",
    ) {
        return Err("cgroup.stat does not count a cgroup's descendant");
    }
    for (file, data, errno, what) in [
        (
            &b"/check-b/cgroup.subtree_control"[..],
            &b"+memory\n"[..],
            Errno::EINVAL,
            "cgroup.subtree_control enabled a controller that is not built",
        ),
        (
            b"/check-b/cgroup.max.depth",
            b"-1\n",
            Errno::ERANGE,
            "cgroup.max.depth took a negative limit",
        ),
        (
            b"/check-b/cgroup.type",
            b"threaded\n",
            Errno::EOPNOTSUPP,
            "cgroup.type took threaded",
        ),
        (
            b"/check-b/cgroup.procs",
            b"-1\n",
            Errno::EINVAL,
            "cgroup.procs took a negative pid",
        ),
    ] {
        let outcome = harness.write(file, data);
        harness.refused(outcome.err(), errno, what)?;
    }
    harness
        .rmdir(b"/check-b/c")
        .map_err(|_| "rmdir of an empty nested cgroup failed")?;
    harness
        .rmdir(b"/check-b")
        .map_err(|_| "rmdir of an emptied cgroup failed")
}
