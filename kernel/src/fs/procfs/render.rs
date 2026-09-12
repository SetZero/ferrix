//! What each file under `/proc` says.
//!
//! The kernel's half of every file: gathering the numbers. The arranging of
//! them is `libs/procfs`'s. Wherever a field has nothing true to put in it,
//! the comment at the place it is filled says so and says why, because a
//! plausible number with no source is the one kind of wrong answer nobody
//! goes looking for.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use ferrix_bootinfo::{Arch, PAGE_SIZE};
use ferrix_procfs::maps::{self, Mapping, Width};
use ferrix_procfs::meminfo::{self, Meminfo};
use ferrix_procfs::mounts::{self, Mount};
use ferrix_procfs::stat::{self, Stat};
use ferrix_procfs::status::{self, State, Status};
use ferrix_vfs::{Errno, Location, Result};

use super::Kernel;
use crate::arch;
use crate::fs;
use crate::mm;
use crate::smp;
use crate::syscall::process::{self, Process};
use crate::syscall::system::{RELEASE, VERSION};
use crate::syscall::time;
use crate::user::space::Region;

/// Nanoseconds in a second.
const NANOS: u64 = 1_000_000_000;

/// Clock ticks a second, as `AT_CLKTCK` tells every program; `stat`'s times
/// are counted in these.
const CLOCK_TICKS: u64 = 100;

/// The signal a process's parent is sent when it exits: `SIGCHLD`.
const SIGCHLD: i32 = 17;

/// Append formatted text. Formatting into a vector cannot fail.
fn put(out: &mut Vec<u8>, arguments: fmt::Arguments<'_>) {
    struct Sink<'a>(&'a mut Vec<u8>);
    impl fmt::Write for Sink<'_> {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            self.0.extend_from_slice(text.as_bytes());
            Ok(())
        }
    }
    let _ = fmt::Write::write_fmt(&mut Sink(out), arguments);
}

// -- The top level ------------------------------------------------------------

/// `/proc/self`: the caller's pid. `ENOENT` from a kernel thread, which has
/// no process to name, as on Linux.
pub(super) fn self_link(_: &Kernel) -> Result<Vec<u8>> {
    let process = process::current().ok_or(Errno::ENOENT)?;
    let mut target = Vec::new();
    put(&mut target, format_args!("{}", process.pid()));
    Ok(target)
}

/// `/proc/cpuinfo`: a block per online processor.
///
/// Only what the kernel knows. Linux's x86-64 blocks carry the vendor, model
/// and a line of feature flags from `CPUID`, and its Arm blocks the `MIDR`
/// fields and `HWCAP` names; nothing here has read those, so nothing here
/// claims them. What is left is the number of blocks — which is what a C
/// library's `sysconf(_SC_NPROCESSORS_ONLN)` fallback counts — and each
/// processor's hardware identifier under the name Linux gives it.
pub(super) fn cpuinfo(_: &Kernel) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let Some(topology) = smp::topology() else {
        return Ok(out);
    };
    for cpu in topology.cpus().iter().filter(|cpu| cpu.is_online()) {
        put(&mut out, format_args!("processor\t: {}\n", cpu.logical));
        match arch::ARCH {
            Arch::X86_64 => put(
                &mut out,
                format_args!(
                    "apicid\t\t: {id}\ninitial apicid\t: {id}\n",
                    id = cpu.hardware_id
                ),
            ),
            Arch::AArch64 => out.extend_from_slice(b"CPU architecture: 8\n"),
            Arch::Armv7a => out.extend_from_slice(b"CPU architecture: 7\n"),
        }
        out.push(b'\n');
    }
    Ok(out)
}

/// `/proc/filesystems`: the filesystem types this kernel has, none of them
/// needing a block device.
pub(super) fn filesystems(_: &Kernel) -> Result<Vec<u8>> {
    Ok(b"nodev\ttmpfs\nnodev\tproc\nnodev\tdevfs\n".to_vec())
}

/// `/proc/meminfo`.
///
/// Free memory is honestly all that is available, because nothing the kernel
/// holds can be reclaimed on demand: there is no page cache to shrink and no
/// swap. tmpfs file pages, which Linux reports under `Cached` and `Shmem`,
/// are held by VMOs here and are simply not free, so `Cached` is zero rather
/// than a figure that could be given back. `Slab` is the kernel heap's pages,
/// which is the same thing by another allocator.
pub(super) fn meminfo(_: &Kernel) -> Result<Vec<u8>> {
    let kib = |frames: u64| frames.saturating_mul(PAGE_SIZE / 1024);
    let free = kib(mm::free_frames());
    let info = Meminfo {
        total: kib(mm::managed_frames()),
        free,
        available: free,
        buffers: 0,
        cached: 0,
        swap_cached: 0,
        swap_total: 0,
        swap_free: 0,
        slab: kib(mm::heap_pages() as u64),
    };
    let mut out = Vec::new();
    meminfo::render(&mut out, &info);
    Ok(out)
}

/// `/proc/mounts`, with each mount point as the reader's root sees it.
///
/// The source is the filesystem's own name, which is what Linux shows for a
/// filesystem with no device. The options are `rw` alone: nothing here is
/// mounted read-only, and nothing enforces `nosuid`, `nodev` or `noexec`, so
/// printing them would promise what the kernel does not do.
pub(super) fn mounts(_: &Kernel) -> Result<Vec<u8>> {
    let ns = fs::namespace();
    let root = process::current().map_or_else(
        || ns.root(),
        |caller| caller.fs_context().lock().root.clone(),
    );
    let mut out = Vec::new();
    for mount in ns.mounts() {
        let at = Location {
            dentry: Arc::clone(mount.root()),
            mount: Arc::clone(&mount),
        };
        let point = ns.path_of(&at, &root);
        let name = mount.filesystem().name().as_bytes();
        mounts::render(
            &mut out,
            &Mount {
                source: name,
                point: &point,
                fstype: name,
                options: b"rw",
            },
        );
    }
    Ok(out)
}

/// `/proc/uptime`: seconds on the counter, and the idle time summed over
/// processors.
///
/// The second number is zero because no processor's idle time is accounted
/// yet. It is the one field here that is knowingly short of the truth; it is
/// kept because the file has two fields and `uptime` reads the first.
pub(super) fn uptime(_: &Kernel) -> Result<Vec<u8>> {
    let nanos = time::now_nanos();
    let mut out = Vec::new();
    put(
        &mut out,
        format_args!(
            "{}.{:02} 0.00\n",
            nanos / NANOS,
            (nanos % NANOS) / (NANOS / 100)
        ),
    );
    Ok(out)
}

/// `/proc/version`: the release and version `uname` reports, in the sentence
/// Linux writes them in, with the compiler it names being the one that built
/// this kernel.
pub(super) fn version(_: &Kernel) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    put(
        &mut out,
        format_args!("Linux version {RELEASE} (ferrix@ferrix) (rustc) {VERSION}\n"),
    );
    Ok(out)
}

// -- A process ----------------------------------------------------------------

/// `/proc/<pid>/cmdline`: each argument followed by a NUL.
pub(super) fn cmdline(process: &Process) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for arg in process.args() {
        out.extend_from_slice(&arg);
        out.push(0);
    }
    Ok(out)
}

/// `/proc/<pid>/comm`.
pub(super) fn comm(process: &Process) -> Result<Vec<u8>> {
    let mut out = process.comm();
    out.push(b'\n');
    Ok(out)
}

/// `/proc/<pid>/exe`: the path it was started from, or `ENOENT` for a process
/// nothing was started in.
pub(super) fn exe(process: &Process) -> Result<Vec<u8>> {
    let exe = process.exe();
    if exe.is_empty() {
        return Err(Errno::ENOENT);
    }
    Ok(exe)
}

/// `/proc/<pid>/fd/<fd>`: the path the descriptor was opened at, from the
/// process's own root, with ` (deleted)` after a name that has since been
/// removed — which is how a program recovers an unlinked temporary file.
pub(super) fn descriptor(process: &Process, fd: i32) -> Result<Vec<u8>> {
    let file = process
        .files()
        .lock()
        .get(fd)
        .map(Arc::clone)
        .map_err(|_| Errno::ENOENT)?;
    let root = process.fs_context().lock().root.clone();
    let mut path = fs::namespace().path_of(file.location(), &root);
    if file.location().dentry.is_unhashed() {
        path.extend_from_slice(b" (deleted)");
    }
    Ok(path)
}

/// `/proc/<pid>/maps`.
///
/// Every region is anonymous memory, including the program's own image:
/// the loader copies an ELF into fresh pages rather than mapping the file, so
/// the offset, device and inode Linux would print for a file mapping would
/// describe a mapping that does not exist. The names are the two the process
/// knows — `[heap]` for the regions `brk` made, `[stack]` for the one holding
/// the stack pointer it was started with.
pub(super) fn maps(process: &Process) -> Result<Vec<u8>> {
    let heap = process.heap_range();
    let stack = start_stack(process);
    let mut out = Vec::new();
    for region in process.space().regions() {
        let name: Option<&[u8]> =
            if heap.is_some_and(|(start, end)| region.start < end && start < region.end) {
                Some(b"[heap]")
            } else if holds(&region, stack) {
                Some(b"[stack]")
            } else {
                None
            };
        let mapping = Mapping {
            start: region.start,
            end: region.end,
            read: region.flags.read,
            write: region.flags.write,
            execute: region.flags.execute,
            shared: region.flags.shared,
            offset: 0,
            major: 0,
            minor: 0,
            inode: 0,
            name,
        };
        maps::render(&mut out, &mapping, Width::native());
    }
    Ok(out)
}

/// The stack pointer the process's program was started with, or zero: what
/// names `[stack]`.
fn start_stack(process: &Process) -> u64 {
    process.startup().map_or(0, |startup| startup.stack)
}

/// Whether a region holds an address, zero never being one.
fn holds(region: &Region, address: u64) -> bool {
    address != 0 && region.start <= address && address < region.end
}

/// What `status` and `stat` say about a process's memory, in bytes.
struct Memory {
    /// Everything mapped.
    size: u64,
    /// The `mlock`ed part.
    locked: u64,
    /// Private writable memory that is not the stack.
    data: u64,
    /// The stack.
    stack: u64,
}

impl Memory {
    fn of(process: &Process) -> Memory {
        let stack_pointer = start_stack(process);
        let mut memory = Memory {
            size: 0,
            locked: 0,
            data: 0,
            stack: 0,
        };
        for region in process.space().regions() {
            let bytes = region.end.saturating_sub(region.start);
            memory.size = memory.size.saturating_add(bytes);
            if region.flags.locked {
                memory.locked = memory.locked.saturating_add(bytes);
            }
            if holds(&region, stack_pointer) {
                memory.stack = memory.stack.saturating_add(bytes);
            } else if region.flags.write && !region.flags.shared {
                memory.data = memory.data.saturating_add(bytes);
            }
        }
        memory
    }
}

/// Running if it is the process on this processor, and sleeping otherwise:
/// a process that is not running is waiting for its turn or for something.
fn state_of(process: &Process) -> State {
    match process::current() {
        Some(current) if current.pid() == process.pid() => State::Running,
        _ => State::Sleeping,
    }
}

/// Its parent's pid. No process has a parent yet: `getppid` answers 1 for
/// every caller, and this does too, except for pid 1 itself, whose parent
/// Linux reports as 0 — a process that was its own parent would send a tree
/// walker round in a circle.
fn parent_of(process: &Process) -> u32 {
    if process.pid() == 1 { 0 } else { 1 }
}

/// Processors online, which every process may run on.
fn online_cpus() -> u32 {
    smp::topology().map_or(1, |topology| {
        u32::try_from(topology.online()).unwrap_or(u32::MAX)
    })
}

/// `/proc/<pid>/status`.
///
/// `Umask` is the mask the process keeps and `umask` changes. Uid and gid are
/// zero because everything runs as root until credentials exist.
pub(super) fn status(process: &Process) -> Result<Vec<u8>> {
    let memory = Memory::of(process);
    let name = process.comm();
    // Slots in the table as Linux sizes one: a power of two, 64 at least.
    let highest = process.files().lock().iter().map(|(fd, _)| fd).last();
    let fd_size = highest
        .and_then(|fd| u32::try_from(fd).ok())
        .map_or(64, |fd| fd.saturating_add(1).next_power_of_two().max(64));
    let status = Status {
        name: &name,
        umask: process.umask(),
        state: state_of(process),
        pid: process.pid(),
        ppid: parent_of(process),
        uid: 0,
        gid: 0,
        fd_size,
        vm_size: memory.size / 1024,
        vm_locked: memory.locked / 1024,
        vm_data: memory.data / 1024,
        vm_stack: memory.stack / 1024,
        threads: 1,
        cpus: online_cpus(),
    };
    let mut out = Vec::new();
    status::render(&mut out, &status);
    Ok(out)
}

/// `/proc/<pid>/stat`.
pub(super) fn stat(process: &Process) -> Result<Vec<u8>> {
    let memory = Memory::of(process);
    let comm = process.comm();
    let pid = process.pid();
    let stat = Stat {
        pid,
        comm: &comm,
        state: state_of(process),
        ppid: parent_of(process),
        // No process has joined a group or a session other than its own,
        // because `setpgid` and `setsid` are not answered yet.
        pgrp: pid,
        session: pid,
        tty_nr: 0,
        tpgid: -1,
        // No `PF_*` flag applies: in particular `PF_RANDOMIZE` is clear,
        // because nothing randomizes the layout.
        flags: 0,
        // No CPU time is accounted per process yet.
        utime: 0,
        stime: 0,
        priority: 20,
        nice: 0,
        threads: 1,
        start_time: process.started() / (NANOS / CLOCK_TICKS),
        vsize: memory.size,
        rss: process.space().resident_pages(),
        // `RLIM_INFINITY`, as an `unsigned long`.
        rss_limit: usize::MAX as u64,
        // A copying loader keeps no record of where the text was put.
        start_code: 0,
        end_code: 0,
        start_stack: start_stack(process),
        // The signal state belongs to `syscall::signal`, which has no way to
        // read it from outside a handler yet; zero is "none", which is true
        // of pending signals and may not be of the rest.
        pending: 0,
        blocked: 0,
        ignored: 0,
        caught: 0,
        exit_signal: SIGCHLD,
        processor: smp::this_cpu().map_or(0, |cpu| u32::try_from(cpu.logical).unwrap_or(0)),
        start_brk: process.heap_range().map_or(0, |(start, _)| start),
        // The startup image's addresses are `libs/ustack`'s, and not kept.
        arg_start: 0,
        arg_end: 0,
        env_start: 0,
        env_end: 0,
    };
    let mut out = Vec::new();
    stat::render(&mut out, &stat);
    Ok(out)
}
