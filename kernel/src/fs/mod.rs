//! The kernel's filesystem: one namespace, and what `libs/vfs` needs from a
//! machine.
//!
//! Stage 8 of `docs/ROADMAP.md`. The VFS itself — dentries, mounts, the path
//! walk, tmpfs — is `libs/vfs`, host-tested and fuzzed. What only the kernel
//! can supply is here: memory for file contents, a clock, device numbers, the
//! archive the loader handed over, and the one namespace every process
//! resolves paths in until stage 13 gives them namespaces of their own.
//!
//! # The root, and where it comes from
//!
//! A tmpfs, with the initramfs unpacked into it. That is Linux's answer to the
//! cycle `docs/ARCHITECTURE.md` §7 describes — the root filesystem needs a
//! block driver that lives on the root filesystem — and it is Ferrix's for
//! the same reason. `/tmp` is a tmpfs of its own on top, so that what a
//! program writes there is one mount that can later be bounded or discarded
//! without touching what the archive put in place.
//!
//! # Why the namespace cannot be missing
//!
//! [`namespace`] is total: if nothing has built the root yet, the first caller
//! gets an empty tmpfs, and [`init`] unpacks into that same one. A system call
//! that raced boot, or a self-check run in an unusual order, finds an empty
//! tree and gets `ENOENT` — a real answer — rather than a kernel that has to
//! decide what to do about a filesystem that does not exist.

pub(crate) mod block;
pub(crate) mod check;
pub(crate) mod console;
pub(crate) mod devfs;
mod pages;
pub(crate) mod pipe;
pub(crate) mod procfs;
pub(crate) mod terminal;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicU32, Ordering};

use ferrix_bootinfo::BootView;
use ferrix_sync::Once;
use ferrix_vfs::initramfs::{self, UnpackError, Unpacked, makedev};
use ferrix_vfs::tmpfs::Tmpfs;
use ferrix_vfs::{
    Clock, Context, Errno, FileType, Location, Namespace, OpenFile, OpenFlags, SetAttributes,
    Timespec,
};

use crate::mm;
use crate::syscall::time;

/// The namespace every process resolves paths in.
static NAMESPACE: Once<Namespace> = Once::new();

/// The next anonymous device minor, for filesystems with no device behind
/// them. Linux gives these major 0; so does Ferrix.
static NEXT_ANONYMOUS: AtomicU32 = AtomicU32::new(1);

/// Nanoseconds in a second.
const NANOS: u64 = 1_000_000_000;

/// The namespace, built empty on first use if [`init`] has not run yet.
pub(crate) fn namespace() -> &'static Namespace {
    NAMESPACE.call_once(|| Namespace::new(new_tmpfs(), Arc::new(crate::sync::SchedParker)))
}

/// A device number no other filesystem has: `st_dev` for an in-memory one.
pub(crate) fn anonymous_device() -> u64 {
    makedev(0, NEXT_ANONYMOUS.fetch_add(1, Ordering::Relaxed))
}

/// The clock timestamps are taken from.
///
/// The counter since boot, read as time since 1970 — the same deliberately
/// wrong answer `clock_gettime(CLOCK_REALTIME)` gives, for the reason
/// `syscall::time` gives: nothing reads a real-time clock yet, and a file
/// dated 1970 is a curiosity where a file with no date is an error.
pub(crate) fn clock() -> Arc<dyn Clock> {
    Arc::new(CounterClock)
}

/// A new, empty tmpfs whose file contents are VMO pages.
pub(crate) fn new_tmpfs() -> Arc<Tmpfs> {
    Tmpfs::new(
        anonymous_device(),
        clock(),
        Arc::new(pages::VmoStorage),
        0o755,
    )
}

/// The most [`read_file`] will read: 64 MiB.
///
/// The whole file is built in kernel heap, which is what loading a program
/// needs today, and a program naming a file large enough to exhaust the heap
/// in one call should get `EFBIG` rather than take the kernel's memory with it.
/// A static busybox is two megabytes; a static `rustc` is well under this.
const READ_FILE_LIMIT: u64 = 64 * 1024 * 1024;

/// Read a whole regular file, resolving `path` from `start` -- or from the
/// context's working directory -- and following symbolic links.
///
/// What loading a program from a path needs: `execve`, and init starting its
/// first program. One function rather than a loop in each, so the two cannot
/// disagree about what a path names or which files may be read whole.
///
/// A file that shrinks while it is read comes back as the bytes that were
/// there; one that grows comes back at the size it had when it was opened.
///
/// # Errors
///
/// What the path walk refuses; `EISDIR` for a directory and `EACCES` for any
/// other file that is not a regular one, which is what `execve` reports;
/// `EFBIG` past [`READ_FILE_LIMIT`]; `ENOMEM` if the heap cannot hold it.
pub(crate) fn read_file(
    ctx: &Context,
    start: Option<&Location>,
    path: &[u8],
) -> Result<Vec<u8>, Errno> {
    let at = namespace().resolve(ctx, start, path, true)?;
    read_location(at)
}

/// [`read_file`] for a program: its contents, and the absolute path of the
/// file they were read from, symbolic links resolved.
///
/// The path is what `/proc/<pid>/exe` reports, and glibc's static startup
/// reads it back and asserts it is absolute. Taken from the location that was
/// read rather than by resolving the string again, so the two cannot name
/// different files if the tree changes in between.
///
/// # Errors
///
/// As [`read_file`].
pub(crate) fn read_program(
    ctx: &Context,
    start: Option<&Location>,
    path: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), Errno> {
    let ns = namespace();
    let at = ns.resolve(ctx, start, path, true)?;
    let exe = ns.path_of(&at, &ctx.root);
    Ok((read_location(at)?, exe))
}

/// The whole of the regular file at `at`: the half of [`read_file`] after the
/// walk, with every one of its refusals.
fn read_location(at: Location) -> Result<Vec<u8>, Errno> {
    let metadata = namespace().stat(&at)?.metadata;
    match metadata.kind {
        FileType::Regular => {}
        FileType::Directory => return Err(Errno::EISDIR),
        _ => return Err(Errno::EACCES),
    }
    if metadata.size > READ_FILE_LIMIT {
        return Err(Errno::EFBIG);
    }
    let len = usize::try_from(metadata.size).map_err(|_| Errno::EFBIG)?;

    let flags = OpenFlags {
        read: true,
        ..OpenFlags::default()
    };
    let file = OpenFile::new(at, &flags)?;
    let mut contents = Vec::new();
    contents.try_reserve_exact(len).map_err(|_| Errno::ENOMEM)?;
    contents.resize(len, 0);
    let mut done = 0;
    while done < len {
        let slot = contents.get_mut(done..).ok_or(Errno::EIO)?;
        let count = file.read(slot)?;
        if count == 0 {
            break;
        }
        done += count;
    }
    contents.truncate(done);
    Ok(contents)
}

/// See [`clock`].
#[derive(Debug)]
struct CounterClock;

impl Clock for CounterClock {
    fn now(&self) -> Timespec {
        let nanos = time::now_nanos();
        Timespec {
            tv_sec: i64::try_from(nanos / NANOS).unwrap_or(i64::MAX),
            tv_nsec: i64::try_from(nanos % NANOS).unwrap_or(0),
        }
    }
}

/// What building the root found.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Report {
    /// The archive's size, if the loader handed one over.
    pub(crate) initramfs_bytes: Option<u64>,
    /// What unpacking it made.
    pub(crate) unpacked: Option<Unpacked>,
}

/// Why the root could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InitError {
    /// The loader described an archive the direct map does not cover.
    OutsideDirectMap,
    /// The archive did not unpack.
    Unpack(UnpackError),
    /// `/tmp` could not be made or mounted.
    Tmp(Errno),
    /// A kernel filesystem could not be mounted on the directory named.
    Mount(&'static str, Errno),
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InitError::OutsideDirectMap => f.write_str("the initramfs is outside the direct map"),
            InitError::Unpack(why) => write!(f, "the initramfs did not unpack: {why}"),
            InitError::Tmp(errno) => write!(f, "/tmp could not be mounted: errno {}", errno.0),
            InitError::Mount(at, errno) => {
                write!(f, "{at} could not be mounted: errno {}", errno.0)
            }
        }
    }
}

/// Build the root: unpack the initramfs, if there is one, and mount `/tmp`
/// and the kernel's own filesystems.
///
/// Called once, from `kmain`, after the frame allocator and before anything
/// that opens a file.
///
/// # Errors
///
/// [`InitError`].
pub(crate) fn init(view: &BootView<'_>) -> Result<Report, InitError> {
    let ns = namespace();
    let ctx = ns.context();

    let mut report = Report {
        initramfs_bytes: None,
        unpacked: None,
    };
    if let Some((phys, len)) = view.initrd() {
        let archive = initrd(view, phys, len)?;
        report.initramfs_bytes = Some(len);
        report.unpacked = Some(initramfs::unpack(ns, &ctx, archive).map_err(InitError::Unpack)?);
    }

    match ns.mkdir(&ctx, None, b"/tmp", 0o1777) {
        Ok(()) | Err(Errno::EEXIST) => {}
        Err(errno) => return Err(InitError::Tmp(errno)),
    }
    let tmp = ns
        .resolve(&ctx, None, b"/tmp", true)
        .map_err(InitError::Tmp)?;
    let _ = ns.mount(new_tmpfs(), &tmp).map_err(InitError::Tmp)?;
    let mounted = ns
        .resolve(&ctx, None, b"/tmp", true)
        .map_err(InitError::Tmp)?;
    // Sticky and writable by everyone, as every Unix `/tmp` is: a program
    // that checks the mode before trusting the directory is right to.
    let sticky = SetAttributes {
        permissions: Some(0o1777),
        ..SetAttributes::default()
    };
    ns.set_attributes(&mounted, &sticky)
        .map_err(InitError::Tmp)?;

    devfs::mount().map_err(|errno| InitError::Mount("/dev", errno))?;
    procfs::mount().map_err(|errno| InitError::Mount("/proc", errno))?;
    Ok(report)
}

/// The archive, as bytes the kernel can read.
fn initrd(view: &BootView<'_>, phys: u64, len: u64) -> Result<&'static [u8], InitError> {
    let info = view.raw();
    let end = phys.checked_add(len).ok_or(InitError::OutsideDirectMap)?;
    let map_end = info
        .physmap_phys
        .checked_add(info.physmap_len)
        .ok_or(InitError::OutsideDirectMap)?;
    if phys < info.physmap_phys || end > map_end {
        return Err(InitError::OutsideDirectMap);
    }
    let len = usize::try_from(len).map_err(|_| InitError::OutsideDirectMap)?;
    // SAFETY: the loader read the archive into memory the map reports as
    // `Initrd`, which nothing reclaims and nothing writes after the hand-off,
    // and the check above puts all of it inside the direct map. So the bytes
    // stay valid and unaliased by a writer for the life of the system.
    Ok(unsafe { core::slice::from_raw_parts(mm::direct_map(phys) as *const u8, len) })
}
