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

pub(crate) mod check;
mod pages;

use alloc::sync::Arc;
use core::fmt;
use core::sync::atomic::{AtomicU32, Ordering};

use ferrix_bootinfo::BootView;
use ferrix_sync::Once;
use ferrix_vfs::initramfs::{self, UnpackError, Unpacked, makedev};
use ferrix_vfs::tmpfs::Tmpfs;
use ferrix_vfs::{Clock, Errno, Namespace, SetAttributes, Timespec};

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
    NAMESPACE.call_once(|| Namespace::new(new_tmpfs()))
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
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InitError::OutsideDirectMap => f.write_str("the initramfs is outside the direct map"),
            InitError::Unpack(why) => write!(f, "the initramfs did not unpack: {why}"),
            InitError::Tmp(errno) => write!(f, "/tmp could not be mounted: errno {}", errno.0),
        }
    }
}

/// Build the root: unpack the initramfs, if there is one, and mount `/tmp`.
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
