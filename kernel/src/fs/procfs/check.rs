//! Stage 8's self-checks for `/dev` and `/proc`.
//!
//! The host tests in `libs/procfs` hold the byte formats against lines a real
//! Linux printed. What they cannot hold is the kernel's side: that a device
//! node opened through the namespace reaches the device, that every name a
//! `/proc` listing reports can be walked to — the property `busybox ls -R
//! /proc` fails on, by looping or by printing an error per name — and that
//! `/proc/self/maps` read by a process describes that process's address
//! space, one line per region, with the heap and stack named.
//!
//! # Why half of it runs in a task
//!
//! `/proc/self` means the process of the task that is running. The boot task
//! has none, so the `/proc` half runs in a task spawned into a process of its
//! own: a kernel-mode entry that never goes to user mode, with that process
//! current exactly as it is for a program's system call. It hands its result
//! back through a static and ends its process, which is what the boot task's
//! wait is waiting for.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_procfs::maps;
use ferrix_sync::SpinLock;
use ferrix_vfs::initramfs::makedev;
use ferrix_vfs::{Context, Errno, FileType, Namespace, OpenFile, OpenFlags, Whence};
use ferrix_vma::VmaFlags;

use crate::fs;
use crate::sched;
use crate::syscall::process::{self, Process, Startup};
use crate::syscall::registry;

/// What the checks measured, for the boot log.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Report {
    /// Device nodes whose numbers were checked against Linux's.
    pub(crate) devices: u32,
    /// Names under `/proc` listed and walked back to, recursively.
    pub(crate) listed: u32,
    /// Lines of `/proc/self/maps` parsed.
    pub(crate) maps_lines: u32,
    /// Of those, the ones naming `[heap]` or `[stack]`.
    pub(crate) named: u32,
}

/// Every node in `/dev`, with the number Linux gives it.
const NUMBERS: [(&[u8], u32, u32); 7] = [
    (b"/dev/null", 1, 3),
    (b"/dev/zero", 1, 5),
    (b"/dev/full", 1, 7),
    (b"/dev/random", 1, 8),
    (b"/dev/urandom", 1, 9),
    (b"/dev/tty", 5, 0),
    (b"/dev/console", 5, 1),
];

/// More names than any listing here can honestly have. A walk that reaches it
/// is a walk that does not terminate.
const LISTING_LIMIT: u32 = 4096;

/// Where the check process's image region goes: low, and a user address on
/// all three architectures.
const IMAGE: u64 = 0x40_0000;

/// How long the boot task waits for the check task before calling it lost.
/// Generous, for the reason `syscall::check`'s patience is: TCG under load.
const PATIENCE_NANOS: u64 = 120_000_000_000;

/// How long the boot task gives the check task to finish switching away once
/// its process has ended, before reaping its stack.
const SETTLE_NANOS: u64 = 20_000_000;

/// The status the check task ends its process with. It only says the task got
/// as far as reporting; what it found is in [`OUTCOME`].
const REPORTED: i32 = 0;

/// What the check task found: names listed, maps lines, named lines.
type Found = Result<(u32, u32, u32), &'static str>;

/// A directory's entries: name, kind and inode number.
type Listing = Vec<(Vec<u8>, FileType, u64)>;

/// The check task's result, handed back to the boot task. A static because a
/// task's entry takes a `usize` and nothing else; one check runs, at boot.
static OUTCOME: SpinLock<Option<Found>> = SpinLock::new(None);

/// The layout the boot task gave the check process, for the task to check
/// the maps against.
static LAYOUT: SpinLock<Option<Layout>> = SpinLock::new(None);

/// Run them. `Err` names the first thing that was not true.
pub(crate) fn run() -> Result<Report, &'static str> {
    let devices = check_devices()?;

    let process = process::new_for_check().map_err(|_| "no address space for the /proc check")?;
    if process.pid() == 0 || registry::find(process.pid()).is_none() {
        return Err("a new process was not findable by its pid");
    }
    *LAYOUT.lock() = Some(lay_out(&process)?);
    *OUTCOME.lock() = None;

    let task = sched::spawn_user(
        "procfs-check",
        in_the_process,
        Arc::clone(&process),
        None,
        None,
    )
    .map_err(|_| "no task for the /proc check")?;
    let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
    if process.wait_for_exit(deadline) != Some(REPORTED) {
        return Err("the /proc check's task never reported");
    }
    let found = OUTCOME
        .lock()
        .take()
        .ok_or("the /proc check's task ended without a result");

    // Leave no stack behind for a later check's frame count to find: the
    // stage 7 and stage 9 checks measure frames around their own work, and a
    // stack reaped inside that window moves the count.
    drop(task);
    drop(process);
    sched::sleep_for(SETTLE_NANOS);
    let _ = sched::reap();

    let (listed, maps_lines, named) = found??;
    Ok(Report {
        devices,
        listed,
        maps_lines,
        named,
    })
}

/// The check task's entry: run the `/proc` checks as the process, hand the
/// result back, and end the process so that the boot task's wait returns.
fn in_the_process(_argument: usize) {
    let layout = LAYOUT.lock().take();
    let found = match (process::current(), layout) {
        (Some(me), Some(layout)) => check_proc(&me, &layout),
        (None, _) => Err("the /proc check's task has no process"),
        (_, None) => Err("the /proc check's task was given no layout"),
    };
    *OUTCOME.lock() = Some(found);
    // Every process reference taken above has been dropped by here, which
    // matters: `exit_current` does not return, so nothing after it would.
    process::exit_current(REPORTED)
}

/// Open-for-reading-and-writing flags.
fn read_write() -> OpenFlags {
    OpenFlags {
        read: true,
        write: true,
        ..OpenFlags::default()
    }
}

/// Open-for-reading flags.
fn read_only() -> OpenFlags {
    OpenFlags {
        read: true,
        ..OpenFlags::default()
    }
}

/// `/dev`: the numbers, and the memory devices doing what they are for.
fn check_devices() -> Result<u32, &'static str> {
    let ns = fs::namespace();
    let ctx = ns.context();
    let open = |path: &[u8]| {
        ns.open(&ctx, None, path, &read_write(), 0)
            .map_err(|_| "a /dev node did not open")
    };

    let zero = open(b"/dev/zero")?;
    let mut buf = [0xA5_u8; 64];
    if zero.read(&mut buf) != Ok(buf.len()) || buf.iter().any(|&byte| byte != 0) {
        return Err("/dev/zero did not read as zeros");
    }

    let null = open(b"/dev/null")?;
    if null.write(b"swallowed") != Ok(9) {
        return Err("/dev/null refused a write");
    }
    if null.read(&mut buf) != Ok(0) {
        return Err("/dev/null did not read as end of file");
    }
    if null.seek(0, Whence::Set) != Err(Errno::ESPIPE) {
        return Err("/dev/null could be seeked, but devices here are streams");
    }

    if open(b"/dev/full")?.write(b"x") != Err(Errno::ENOSPC) {
        return Err("/dev/full accepted a write");
    }

    // Thirty-two zero bytes from the generator would be a one in 2^256 event,
    // so seeing them means the node is not reaching it.
    let mut noise = [0_u8; 32];
    if open(b"/dev/urandom")?.read(&mut noise) != Ok(noise.len())
        || noise.iter().all(|&byte| byte == 0)
    {
        return Err("/dev/urandom did not read from the generator");
    }

    for (path, major, minor) in NUMBERS {
        let at = ns
            .resolve(&ctx, None, path, true)
            .map_err(|_| "a /dev node Linux has is missing")?;
        let stat = ns.stat(&at).map_err(|_| "a /dev node cannot be stat'ed")?;
        if stat.metadata.kind != FileType::CharDevice || stat.metadata.rdev != makedev(major, minor)
        {
            return Err("a /dev node does not have the number Linux gives it");
        }
    }
    Ok(NUMBERS.len() as u32)
}

/// What the check process's address space was given.
#[derive(Debug, Clone, Copy)]
struct Layout {
    /// The heap `brk` made.
    heap: (u64, u64),
    /// The region the stack pointer is in.
    stack: (u64, u64),
}

/// Give the check process an image, a heap, a stack and a guard, and say how
/// it was started. Done by the boot task, before the check task exists:
/// nothing here needs the space installed.
fn lay_out(process: &Process) -> Result<Layout, &'static str> {
    let space = process.space();
    let refused = |_| "the check process's address space refused a region";
    let _ = space
        .map_anonymous(IMAGE, 2 * PAGE_SIZE, VmaFlags::READ_EXECUTE)
        .map_err(refused)?;
    process.set_heap_base(IMAGE + 2 * PAGE_SIZE);
    let (start, _) = process
        .heap_range()
        .ok_or("the check process has no heap after placing one")?;
    let want = start + 3 * PAGE_SIZE;
    if process.set_break(want) != want {
        return Err("brk did not grow the check process's heap");
    }
    let heap = process.heap_range().ok_or("the heap went away")?;

    let stack = space
        .map_anywhere(None, 8 * PAGE_SIZE, VmaFlags::READ_WRITE)
        .map_err(refused)?;
    let _ = space
        .map_anywhere(None, PAGE_SIZE, VmaFlags::NONE)
        .map_err(refused)?;
    let top = stack + 8 * PAGE_SIZE;
    // The stack pointer a program would have been handed is what names
    // `[stack]`. Nothing ever enters at `IMAGE`: the check task's entry is a
    // kernel function.
    process.set_startup(Startup {
        entry: IMAGE,
        stack: top - 64,
    });
    process.record_exec(b"/sbin/procfs-check", &[b"procfs-check", b"--self-test"]);
    Ok(Layout {
        heap,
        stack: (stack, top),
    })
}

/// `/proc`, read by the process the running task belongs to.
fn check_proc(process: &Arc<Process>, layout: &Layout) -> Found {
    let ns = fs::namespace();
    let ctx = ns.context();

    let pid = alloc::format!("{}", process.pid()).into_bytes();
    if ns.read_link(&ctx, None, b"/proc/self") != Ok(pid.clone()) {
        return Err("/proc/self is not a link to the current process's pid");
    }

    let descriptors = check_descriptors(ns, &ctx, process)?;
    let listed = check_listing(ns, &ctx, &pid)?;
    let (lines, named) = check_maps(ns, &ctx, process, layout)?;
    drop(descriptors);

    if read_all(ns, &ctx, b"/proc/self/cmdline", 5)? != b"procfs-check\0--self-test\0" {
        return Err("/proc/self/cmdline is not the arguments, each ended by a NUL");
    }
    if ns.read_link(&ctx, None, b"/proc/self/exe") != Ok(b"/sbin/procfs-check".to_vec()) {
        return Err("/proc/self/exe does not name the path the process was started from");
    }
    if read_all(ns, &ctx, b"/proc/self/comm", 64)? != b"procfs-check\n" {
        return Err("/proc/self/comm is not the last component of that path");
    }
    Ok((listed, lines, named))
}

/// `/proc/<pid>/fd`: a descriptor's link names where it was opened, and says
/// ` (deleted)` once that name is gone. Returns the descriptors, which the
/// caller keeps open until the listing has walked them.
fn check_descriptors(
    ns: &Namespace,
    ctx: &Context,
    process: &Process,
) -> Result<Vec<i32>, &'static str> {
    let null = ns
        .open(ctx, None, b"/dev/null", &read_only(), 0)
        .map_err(|_| "/dev/null did not open for the descriptor check")?;
    let create = OpenFlags {
        create: true,
        exclusive: true,
        ..read_write()
    };
    let doomed = ns
        .open(ctx, None, b"/tmp/procfs-check", &create, 0o600)
        .map_err(|_| "a file under /tmp could not be made")?;
    let (null_fd, doomed_fd) = {
        let mut files = process.files().lock();
        let null_fd = files.insert(null, false);
        let doomed_fd = files.insert(doomed, false);
        (null_fd, doomed_fd)
    };
    let (Ok(null_fd), Ok(doomed_fd)) = (null_fd, doomed_fd) else {
        return Err("the check process's descriptor table refused a file");
    };
    ns.unlink(ctx, None, b"/tmp/procfs-check")
        .map_err(|_| "the file under /tmp could not be removed")?;

    let link = |fd: i32| {
        let path = alloc::format!("/proc/self/fd/{fd}").into_bytes();
        ns.read_link(ctx, None, &path)
    };
    if link(null_fd) != Ok(b"/dev/null".to_vec()) {
        return Err("/proc/self/fd names a descriptor's path wrongly");
    }
    if link(doomed_fd) != Ok(b"/tmp/procfs-check (deleted)".to_vec()) {
        return Err("/proc/self/fd does not say a removed file was deleted");
    }
    if link(doomed_fd.saturating_add(100)).is_ok() {
        return Err("/proc/self/fd has a link for a descriptor that is not open");
    }
    Ok(vec![null_fd, doomed_fd])
}

/// One directory's entries: name, kind and inode number, `.` and `..` left
/// out.
fn list(ns: &Namespace, ctx: &Context, path: &[u8]) -> Result<Listing, &'static str> {
    let flags = OpenFlags {
        directory: true,
        ..read_only()
    };
    let dir = ns
        .open(ctx, None, path, &flags, 0)
        .map_err(|_| "a directory /proc listed did not open")?;
    let mut entries = Vec::new();
    dir.read_dir(&mut |entry| {
        if entry.name != b"." && entry.name != b".." {
            entries.push((entry.name.to_vec(), entry.kind, entry.ino));
        }
        true
    })
    .map_err(|_| "a directory /proc listed could not be read")?;
    Ok(entries)
}

/// `ls -R /proc`: list every directory without following links, walk to
/// every name listed, and require the walk to end.
fn check_listing(ns: &Namespace, ctx: &Context, pid: &[u8]) -> Result<u32, &'static str> {
    let mut pending: Vec<Vec<u8>> = vec![b"/proc".to_vec()];
    let mut listed = 0_u32;
    let mut seen_self = false;
    let mut seen_pid = false;
    while let Some(dir) = pending.pop() {
        let top = dir == b"/proc";
        for (name, kind, ino) in list(ns, ctx, &dir)? {
            let mut path = dir.clone();
            path.push(b'/');
            path.extend_from_slice(&name);
            walk_back(ns, ctx, &path, kind, ino)?;
            listed += 1;
            if listed > LISTING_LIMIT {
                return Err("listing /proc recursively does not end");
            }
            seen_self |= top && name == b"self" && kind == FileType::Symlink;
            seen_pid |= top && name == pid && kind == FileType::Directory;
            if kind == FileType::Directory {
                pending.push(path);
            }
        }
    }
    if !seen_self {
        return Err("/proc does not list self as a symbolic link");
    }
    if !seen_pid {
        return Err("/proc does not list the current process's directory");
    }
    Ok(listed)
}

/// A listed name must lead, without following a link, to what the listing
/// said it was.
fn walk_back(
    ns: &Namespace,
    ctx: &Context,
    path: &[u8],
    kind: FileType,
    ino: u64,
) -> Result<(), &'static str> {
    let at = ns
        .resolve(ctx, None, path, false)
        .map_err(|_| "a name /proc listed cannot be walked to")?;
    let stat = ns
        .stat(&at)
        .map_err(|_| "a name /proc listed cannot be stat'ed")?;
    if stat.metadata.kind != kind || stat.metadata.ino != ino {
        return Err("a /proc listing and stat disagree about a name");
    }
    Ok(())
}

/// Read a whole file `piece` bytes at a time.
fn read_all(
    ns: &Namespace,
    ctx: &Context,
    path: &[u8],
    piece: usize,
) -> Result<Vec<u8>, &'static str> {
    let file = ns
        .open(ctx, None, path, &read_only(), 0)
        .map_err(|_| "a /proc file did not open")?;
    read_rest(&file, piece, Vec::new())
}

/// Read the rest of an open file onto `out`, `piece` bytes at a time.
fn read_rest(file: &OpenFile, piece: usize, mut out: Vec<u8>) -> Result<Vec<u8>, &'static str> {
    let mut buf = vec![0_u8; piece];
    loop {
        let count = file
            .read(&mut buf)
            .map_err(|_| "a /proc file could not be read")?;
        let Some(got) = buf.get(..count).filter(|got| !got.is_empty()) else {
            return Ok(out);
        };
        out.extend_from_slice(got);
    }
}

/// `/proc/self/maps`: read seven bytes at a time with the map changing
/// underneath, it is still the map as it was at open, a line per region, with
/// the heap and the stack named where the process put them.
fn check_maps(
    ns: &Namespace,
    ctx: &Context,
    process: &Process,
    layout: &Layout,
) -> Result<(u32, u32), &'static str> {
    let regions = process.space().regions().len();
    let file = ns
        .open(ctx, None, b"/proc/self/maps", &read_only(), 0)
        .map_err(|_| "/proc/self/maps did not open")?;
    let mut first = [0_u8; 7];
    let count = file
        .read(&mut first)
        .map_err(|_| "/proc/self/maps could not be read")?;
    let _ = process
        .space()
        .map_anywhere(None, PAGE_SIZE, VmaFlags::READ)
        .map_err(|_| "the check process's address space refused a region")?;
    let text = read_rest(&file, 7, first.get(..count).unwrap_or_default().to_vec())?;

    let mut lines = 0_u32;
    let mut named = 0_u32;
    let (mut heap, mut stack) = (false, false);
    for line in text
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let mapping = maps::parse(line).ok_or("a /proc/self/maps line does not parse")?;
        lines += 1;
        let inside = |(start, end): (u64, u64)| mapping.start >= start && mapping.end <= end;
        match mapping.name {
            None => {}
            Some(b"[heap]") if inside(layout.heap) && mapping.write => heap = true,
            Some(b"[stack]") if inside(layout.stack) && mapping.write => stack = true,
            Some(_) => return Err("/proc/self/maps names a region it should not"),
        }
        named += u32::from(mapping.name.is_some());
    }
    if lines as usize != regions {
        return Err("/proc/self/maps is not one line per region as they were at open");
    }
    if !heap || !stack {
        return Err("/proc/self/maps does not name the heap and the stack");
    }
    Ok((lines, named))
}
