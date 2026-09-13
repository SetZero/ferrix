//! Stage 8's self-checks for the root the kernel built.
//!
//! Two properties the host tests of `libs/vfs` cannot establish, because both
//! are about this machine rather than the logic. That the archive the loader
//! handed over is the one the build wrote, unpacked intact through the direct
//! map — hard link and symbolic link included. And that tmpfs over VMO pages
//! stores what it is given, gives zeros where nothing was written, and gives
//! every frame back once the file is gone.
//!
//! And, in [`run_calls`], a third: that pipes, a FIFO and the calls about
//! filesystems answer as a program meets them. Those waits and wake-ups, the
//! user-memory byte layouts and the descriptors left behind are the kernel's,
//! not the library's, so they are driven through the system call handlers
//! against a process built for the check. Last, `mount -t proc` and `mount -t
//! devtmpfs` go in by syscall number, as an init script's do, and are read
//! through, listed in `/proc/mounts` and unmounted again.

use alloc::vec;
use alloc::vec::Vec;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_linux_abi::nr::Syscall;
use ferrix_linux_abi::types::{
    AT_FDCWD, AT_REMOVEDIR, F_GETFD, FALLOC_FL_KEEP_SIZE, FALLOC_FL_PUNCH_HOLE, FD_CLOEXEC,
    MAP_ANONYMOUS, MAP_PRIVATE, MS_NODEV, MS_NOEXEC, MS_NOSUID, MS_RELATIME, O_APPEND, O_CLOEXEC,
    O_CREAT, O_NONBLOCK, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY, PROT_READ, PROT_WRITE, SEEK_CUR,
};
use ferrix_vfs::pipe::PIPEFS_MAGIC;
use ferrix_vfs::tmpfs::TMPFS_MAGIC;
use ferrix_vfs::{Errno, FileType, Namespace, NewNode, OpenFlags, RenameMode};

use crate::fs::{self, Report as Built};
use crate::mm;
use crate::syscall::check as syscall_check;
use crate::syscall::memory::{self, MmapRequest, OffsetUnit};
use crate::syscall::process::{self, Process};
use crate::syscall::{fd, file, fsctl, pipe, uaccess};

/// The marker `xtask/src/initramfs.rs` writes, byte for byte.
const MARKER: &[u8] = b"unpacked by the kernel from a cpio archive the loader handed it\n";

/// Where the marker is unpacked.
const MARKER_PATH: &[u8] = b"/etc/ferrix/initramfs";

/// What the checks measured, for the boot log.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Report {
    /// Whether the archive's marker, link and symbolic link were checked.
    pub(crate) initramfs_verified: bool,
    /// Pages a file under `/tmp` committed while the check wrote it.
    pub(crate) pages: u64,
    /// Frames the tmpfs check cost once the file was gone. Zero, or the page
    /// store is leaking.
    pub(crate) leaked: i64,
}

/// Run them. `Err` names the first thing that was not true.
pub(crate) fn run(built: &Built) -> Result<Report, &'static str> {
    let initramfs_verified = match built.unpacked {
        Some(_) => {
            check_the_archive_unpacked_intact()?;
            true
        }
        None => false,
    };
    check_tmp_is_its_own_mount()?;

    // Twice, measured on the second, for the reason `syscall::check` gives:
    // the heap keeps the last page of a size class it has used, and a single
    // run cannot tell that apart from a leak.
    let _warm = check_tmpfs_stores_pages()?;
    let before = mm::free_frames();
    let pages = check_tmpfs_stores_pages()?;
    let leaked = i64::try_from(before).unwrap_or(i64::MAX)
        - i64::try_from(mm::free_frames()).unwrap_or(i64::MAX);
    // Checked, not only printed: a count nothing tests would boot green
    // through the very leak it exists to show.
    if leaked != 0 {
        return Err("the tmpfs check did not give every frame back");
    }

    Ok(Report {
        initramfs_verified,
        pages,
        leaked,
    })
}

/// Read a whole file through the namespace.
fn read_all(ns: &Namespace, path: &[u8]) -> Result<Vec<u8>, &'static str> {
    let ctx = ns.context();
    let read = OpenFlags {
        read: true,
        ..OpenFlags::default()
    };
    let file = ns
        .open(&ctx, None, path, &read, 0)
        .map_err(|_| "a file the check needs would not open")?;
    let mut contents = Vec::new();
    let mut chunk = [0_u8; 97];
    loop {
        let count = file
            .read(&mut chunk)
            .map_err(|_| "a file the check opened would not read")?;
        if count == 0 {
            return Ok(contents);
        }
        contents.extend_from_slice(chunk.get(..count).ok_or("a read overran its buffer")?);
    }
}

/// The marker reads back exactly, and both of its other names reach it.
fn check_the_archive_unpacked_intact() -> Result<(), &'static str> {
    let ns = fs::namespace();
    let ctx = ns.context();
    if read_all(ns, MARKER_PATH)? != MARKER {
        return Err("the initramfs marker does not hold what the build wrote");
    }

    let marker = ns
        .resolve(&ctx, None, MARKER_PATH, false)
        .and_then(|at| ns.stat(&at))
        .map_err(|_| "the initramfs marker would not stat")?;
    let link = ns
        .resolve(&ctx, None, b"/etc/ferrix/initramfs.link", false)
        .and_then(|at| ns.stat(&at))
        .map_err(|_| "the initramfs hard link is missing")?;
    if link.metadata.ino != marker.metadata.ino || marker.metadata.nlink != 2 {
        return Err("the initramfs hard link is a copy, not a second name");
    }

    let symlink = b"/etc/ferrix/initramfs.symlink";
    let kind = ns
        .resolve(&ctx, None, symlink, false)
        .and_then(|at| ns.stat(&at))
        .map_err(|_| "the initramfs symbolic link is missing")?
        .metadata
        .kind;
    if kind != FileType::Symlink || read_all(ns, symlink)? != MARKER {
        return Err("the initramfs symbolic link does not lead to the marker");
    }

    // The whole-file read that loading a program goes through: through the
    // link, as `execve` of `/bin/sh` will be, and refusing what is not a file.
    let whole = fs::read_file(&ctx, None, symlink);
    if whole.as_deref() != Ok(MARKER) {
        return Err("reading a whole file through a symbolic link did not give the marker");
    }
    if fs::read_file(&ctx, None, b"/etc/ferrix") != Err(Errno::EISDIR) {
        return Err("reading a directory as a whole file was not refused");
    }
    Ok(())
}

/// `/tmp` is a separate filesystem from the root, sticky and world-writable.
fn check_tmp_is_its_own_mount() -> Result<(), &'static str> {
    let ns = fs::namespace();
    let ctx = ns.context();
    let root = ns.stat(&ctx.root).map_err(|_| "the root would not stat")?;
    let tmp = ns
        .resolve(&ctx, None, b"/tmp", true)
        .and_then(|at| ns.stat(&at))
        .map_err(|_| "/tmp is missing")?;
    if tmp.dev == root.dev {
        return Err("/tmp is not a filesystem of its own");
    }
    if tmp.metadata.permissions != 0o1777 {
        return Err("/tmp is not sticky and world-writable");
    }
    Ok(())
}

/// A byte of the pattern the tmpfs check writes, by its offset.
fn pattern(at: usize) -> u8 {
    (at.wrapping_mul(31) ^ (at >> 12)) as u8
}

/// Write across pages, read back, truncate into a page, grow again, rename,
/// and remove. Returns the pages the file had committed.
fn check_tmpfs_stores_pages() -> Result<u64, &'static str> {
    let ns = fs::namespace();
    let ctx = ns.context();
    let page = usize::try_from(PAGE_SIZE).map_err(|_| "the page size does not fit")?;
    let len = page * 3 + 123;
    let data: Vec<u8> = (0..len).map(pattern).collect();

    let create = OpenFlags {
        read: true,
        write: true,
        create: true,
        exclusive: true,
        ..OpenFlags::default()
    };
    let file = ns
        .open(&ctx, None, b"/tmp/stage8-check", &create, 0o600)
        .map_err(|_| "a file could not be created under /tmp")?;
    for piece in data.chunks(1000) {
        if file.write(piece) != Ok(piece.len()) {
            return Err("a write to a tmpfs file came back short");
        }
    }
    let stat = ns
        .stat(file.location())
        .map_err(|_| "the file would not stat")?;
    let pages = stat.metadata.blocks * 512 / PAGE_SIZE;
    if stat.metadata.size != len as u64 || pages != 4 {
        return Err("the file's size or committed pages are not what was written");
    }
    if read_all(ns, b"/tmp/stage8-check")? != data {
        return Err("a tmpfs file read back different bytes");
    }

    // Into the second page, then out past where the data used to end: the
    // old bytes must not reappear.
    let keep = page + 10;
    file.set_len(keep as u64)
        .and_then(|()| file.set_len(len as u64))
        .map_err(|_| "a tmpfs file would not truncate and grow")?;
    let mut back = vec![0_u8; len];
    if file.read_at(0, &mut back) != Ok(len) {
        return Err("a regrown tmpfs file read back short");
    }
    let head_intact = back.get(..keep) == data.get(..keep);
    let tail_zero = back
        .get(keep..)
        .is_some_and(|tail| tail.iter().all(|&b| b == 0));
    if !head_intact || !tail_zero {
        return Err("truncating a tmpfs file did not discard what it cut off");
    }

    ns.rename(
        &ctx,
        (None, b"/tmp/stage8-check"),
        (None, b"/tmp/stage8-check.moved"),
        RenameMode::NoReplace,
    )
    .map_err(|_| "a tmpfs file would not rename")?;
    if read_all(ns, b"/tmp/stage8-check.moved")?.len() != len {
        return Err("a renamed tmpfs file lost its contents");
    }
    ns.unlink(&ctx, None, b"/tmp/stage8-check.moved")
        .map_err(|_| "a tmpfs file would not unlink")?;
    drop(file);
    Ok(pages)
}

// ---------------------------------------------------------------------------
// Pipes and the calls about filesystems
//
// Every path and buffer lives in the check process's own memory, so what is
// checked is what a program meets: the flag words, the byte layouts, the
// errors, and which descriptors are left behind. No step waits: every read has
// something to read or is non-blocking, and every FIFO writer opens after its
// reader. The boot task has no process to be woken for.
// ---------------------------------------------------------------------------

/// What the pipe and filesystem call checks measured, for the boot log.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CallsReport {
    /// Bytes the checks moved through a pipe, a FIFO and `sendfile`.
    pub(crate) bytes: u64,
    /// Frames the second run cost. Zero, or a pipe, a FIFO's pipe or a file
    /// outlives its last descriptor.
    pub(crate) leaked: i64,
}

/// Where the checks keep their paths and buffers in the process's page.
const AT_FDS: u64 = 0;
/// `statfs`'s path.
const AT_TMP: u64 = 16;
/// The FIFO's path.
const AT_FIFO: u64 = 32;
/// The file `truncate`, `fallocate` and `sendfile` work on.
const AT_FILE: u64 = 64;
/// Where `sendfile` copies it to.
const AT_COPY: u64 = 96;
/// The bytes written into pipes.
const AT_DATA: u64 = 128;
/// Where reads land.
const AT_BACK: u64 = 256;
/// Where `statfs` writes, room for the widest layout.
const AT_STATFS: u64 = 512;
/// `sendfile`'s offset.
const AT_OFFSET: u64 = 768;
/// The directory a second procfs is mounted on.
const AT_PROC_DIR: u64 = 800;
/// The directory a second devtmpfs is mounted on.
const AT_DEV_DIR: u64 = 832;
/// `proc`, as a type and as a source.
const AT_PROC_TYPE: u64 = 864;
/// `devtmpfs`, as a type and as a source.
const AT_DEVTMPFS_TYPE: u64 = 880;
/// `sysfs`, which is not a type here.
const AT_SYSFS_TYPE: u64 = 896;
/// An options string procfs has on Linux and does not read here.
const AT_OPTIONS: u64 = 912;
/// `self` in the second procfs.
const AT_PROC_SELF: u64 = 944;
/// `/proc/self`, to compare it with.
const AT_SELF: u64 = 976;
/// `zero` in the second devtmpfs.
const AT_DEV_ZERO: u64 = 1000;
/// `/proc/mounts`.
const AT_MOUNTS: u64 = 1024;
/// The check process's own `stat` in the second procfs, written at run time:
/// room for the longest pid.
const AT_STAT: u64 = 1040;
/// Where the link `self` in the second procfs is read to.
const AT_LINK: u64 = 1104;
/// Where `/proc/self` is read to.
const AT_LINK_PROC: u64 = 1136;
/// Where zeros are read to.
const AT_ZEROS: u64 = 1168;
/// Where `/proc/mounts` is read to, to the end of the page.
const AT_LISTING: u64 = 1536;

const TMP: &[u8] = b"/tmp\0";
const FIFO: &[u8] = b"/tmp/stage8-fifo\0";
const FILE: &[u8] = b"/tmp/stage8-calls\0";
const COPY: &[u8] = b"/tmp/stage8-calls.copy\0";
const DATA: &[u8] = b"through a pipe\n";
const PROC_DIR: &[u8] = b"/tmp/stage8-proc\0";
const DEV_DIR: &[u8] = b"/tmp/stage8-dev\0";
const PROC_TYPE: &[u8] = b"proc\0";
const DEVTMPFS_TYPE: &[u8] = b"devtmpfs\0";
const SYSFS_TYPE: &[u8] = b"sysfs\0";
const OPTIONS: &[u8] = b"hidepid=invisible\0";
const PROC_SELF: &[u8] = b"/tmp/stage8-proc/self\0";
const SELF: &[u8] = b"/proc/self\0";
const DEV_ZERO: &[u8] = b"/tmp/stage8-dev/zero\0";
const MOUNTS: &[u8] = b"/proc/mounts\0";

/// The flags an init script mounts `/proc` with.
const PROC_FLAGS: u32 = MS_NOSUID | MS_NODEV | MS_NOEXEC | MS_RELATIME;
/// The flags an init script mounts `/dev` with.
const DEVTMPFS_FLAGS: u32 = MS_NOSUID | MS_RELATIME;

/// `AT_FDCWD`, as a register carries it.
const CWD: u64 = AT_FDCWD as i64 as u64;

/// An address no program may write, for the check that `pipe2` hands back
/// nothing it could not deliver.
const KERNEL_ADDRESS: u64 = u64::MAX - 0xFFF;

/// Run the pipe and filesystem call checks: twice, measured on the second,
/// for the reason [`run`] gives.
pub(crate) fn run_calls() -> Result<CallsReport, &'static str> {
    let process = process::new_for_check()
        .map_err(|_| "could not make a process for the file system calls")?;
    let _warm = check_the_calls(&process)?;
    let cached = fs::namespace().cached();
    let before = mm::free_frames();
    let bytes = check_the_calls(&process)?;
    // The run mounts a fresh procfs and devtmpfs and walks into them. A dentry
    // of either that outlives its unmount is heap the frame count only notices
    // when the growth crosses a page, which is how this check once failed one
    // boot in many; so the cache is required not to grow at all.
    let cache_growth = i64::try_from(fs::namespace().cached()).unwrap_or(i64::MAX)
        - i64::try_from(cached).unwrap_or(i64::MAX);
    if cache_growth != 0 {
        crate::console::println!("  pipes    dentry cache {cache_growth:+} across the second run");
        return Err("the pipe and filesystem call checks left dentries behind in the cache");
    }
    let leaked = i64::try_from(before).unwrap_or(i64::MAX)
        - i64::try_from(mm::free_frames()).unwrap_or(i64::MAX);
    // Checked, not only printed: a count nothing tests would boot green
    // through the very leak it exists to show.
    if leaked != 0 {
        return Err("the pipe and filesystem call checks did not give every frame back");
    }
    Ok(CallsReport { bytes, leaked })
}

/// A path without the terminator a program's copy carries.
fn name(path: &[u8]) -> &[u8] {
    path.strip_suffix(b"\0").unwrap_or(path)
}

/// One run: stage the page, check, and clean up whatever happened.
fn check_the_calls(process: &Process) -> Result<u64, &'static str> {
    let page = memory::sys_mmap(
        process,
        &MmapRequest {
            addr: 0,
            len: PAGE_SIZE,
            prot: PROT_READ | PROT_WRITE,
            flags: MAP_ANONYMOUS | MAP_PRIVATE,
            fd: -1,
            offset: 0,
            unit: OffsetUnit::Bytes,
        },
    )
    .map_err(|_| "a page for the file system call checks was refused")?;
    let page = u64::try_from(page).map_err(|_| "mmap returned an impossible address")?;
    for (offset, bytes) in [
        (AT_TMP, TMP),
        (AT_FIFO, FIFO),
        (AT_FILE, FILE),
        (AT_COPY, COPY),
        (AT_DATA, DATA),
        (AT_PROC_DIR, PROC_DIR),
        (AT_DEV_DIR, DEV_DIR),
        (AT_PROC_TYPE, PROC_TYPE),
        (AT_DEVTMPFS_TYPE, DEVTMPFS_TYPE),
        (AT_SYSFS_TYPE, SYSFS_TYPE),
        (AT_OPTIONS, OPTIONS),
        (AT_PROC_SELF, PROC_SELF),
        (AT_SELF, SELF),
        (AT_DEV_ZERO, DEV_ZERO),
        (AT_MOUNTS, MOUNTS),
    ] {
        uaccess::copy_to_user(process.space(), page + offset, bytes)
            .map_err(|_| "could not stage the file system call checks")?;
    }

    let outcome = check_a_pipe_carries_bytes_and_then_ends(process, page)
        .and_then(|piped| check_a_pipe_refuses_as_linux_does(process, page).map(|()| piped))
        .and_then(|piped| check_a_fifo_is_one_pipe(process, page).map(|fifo| piped + fifo))
        .and_then(|bytes| check_statfs_says_tmp_is_tmpfs(process, page).map(|()| bytes))
        .and_then(|bytes| check_truncate_and_fallocate_grow(process, page).map(|()| bytes))
        .and_then(|bytes| check_sendfile_copies_a_file(process, page).map(|sent| bytes + sent))
        .and_then(|bytes| check_proc_and_devtmpfs_mount(process, page).map(|()| bytes));

    // Cleaned up whatever happened, so a failure reports itself and not also
    // leaked frames.
    for fd in 3..32 {
        let _ = fd::sys_close(process, fd);
    }
    for dir in [AT_PROC_DIR, AT_DEV_DIR] {
        let _ = by_number(process, Syscall::Umount2, [page + dir, 0, 0, 0, 0, 0]);
        let _ = by_number(
            process,
            Syscall::Unlinkat,
            [CWD, page + dir, u64::from(AT_REMOVEDIR), 0, 0, 0],
        );
    }
    let ns = fs::namespace();
    let ctx = ns.context();
    for path in [FIFO, FILE, COPY] {
        let _ = ns.unlink(&ctx, None, name(path));
    }
    let _ = memory::sys_munmap(process, page, PAGE_SIZE);
    outcome
}

/// Require a handler to have answered `want`.
fn answers(got: Result<usize, Errno>, want: usize, what: &'static str) -> Result<(), &'static str> {
    if got == Ok(want) { Ok(()) } else { Err(what) }
}

/// Require a handler to have refused with `errno`.
fn refuses(
    got: Result<usize, Errno>,
    errno: Errno,
    what: &'static str,
) -> Result<(), &'static str> {
    if got == Err(errno) { Ok(()) } else { Err(what) }
}

/// A descriptor a handler returned.
fn descriptor(got: Result<usize, Errno>, what: &'static str) -> Result<i32, &'static str> {
    got.ok().and_then(|fd| i32::try_from(fd).ok()).ok_or(what)
}

/// `len` bytes of the process's memory at `at`.
fn read_back(process: &Process, at: u64, len: usize) -> Result<Vec<u8>, &'static str> {
    let mut out = vec![0_u8; len];
    uaccess::copy_from_user(process.space(), at, &mut out)
        .map_err(|_| "the check could not read its own page back")?;
    Ok(out)
}

/// The two descriptors `pipe2` wrote.
fn pair(process: &Process, page: u64) -> Result<(i32, i32), &'static str> {
    let bytes = read_back(process, page + AT_FDS, 8)?;
    let [a, b, c, d, e, f, g, h] = <[u8; 8]>::try_from(bytes).map_err(|_| "a short pair")?;
    Ok((
        i32::from_le_bytes([a, b, c, d]),
        i32::from_le_bytes([e, f, g, h]),
    ))
}

/// The filesystem magic number `statfs` wrote: the first four bytes in every
/// layout, because `f_type` comes first and the magic numbers fit in 32 bits.
fn magic(process: &Process, page: u64) -> Result<u64, &'static str> {
    let bytes = read_back(process, page + AT_STATFS, 4)?;
    let word = <[u8; 4]>::try_from(bytes).map_err(|_| "a short magic number")?;
    Ok(u64::from(u32::from_le_bytes(word)))
}

/// The size of the file at `path`, as `stat` reports it.
fn size_is(path: &[u8], want: u64, what: &'static str) -> Result<(), &'static str> {
    let ns = fs::namespace();
    let size = ns
        .resolve(&ns.context(), None, name(path), true)
        .and_then(|at| ns.stat(&at))
        .map_err(|_| "a file the check made would not stat")?
        .metadata
        .size;
    if size == want { Ok(()) } else { Err(what) }
}

/// Bytes written into a pipe come out of it; a pipe says it is on pipefs and
/// cannot be sought or synced; and once its writer is closed its reader reads
/// end of file.
fn check_a_pipe_carries_bytes_and_then_ends(
    process: &Process,
    page: u64,
) -> Result<u64, &'static str> {
    answers(
        pipe::sys_pipe2(process, page + AT_FDS, 0),
        0,
        "pipe2 was refused",
    )?;
    let (reader, writer) = pair(process, page)?;
    let len = DATA.len();
    answers(
        file::sys_write(process, writer, page + AT_DATA, len as u64),
        len,
        "a write into a pipe came back short",
    )?;
    answers(
        file::sys_read(process, reader, page + AT_BACK, 64),
        len,
        "a read from a pipe did not return what was queued",
    )?;
    if read_back(process, page + AT_BACK, len)? != DATA {
        return Err("a pipe gave back different bytes than were written into it");
    }

    answers(
        fsctl::sys_fstatfs(process, reader, page + AT_STATFS),
        0,
        "fstatfs on a pipe was refused",
    )?;
    if magic(process, page)? != PIPEFS_MAGIC {
        return Err("fstatfs on a pipe did not report PIPEFS_MAGIC");
    }
    refuses(
        fd::sys_lseek(process, reader, 0, SEEK_CUR),
        Errno::ESPIPE,
        "a pipe could be sought",
    )?;
    refuses(
        fsctl::sys_fsync(process, writer),
        Errno::EINVAL,
        "fsync on a pipe was not EINVAL",
    )?;

    answers(
        fd::sys_close(process, writer),
        0,
        "a pipe's write end would not close",
    )?;
    answers(
        file::sys_read(process, reader, page + AT_BACK, 64),
        0,
        "a pipe with no writer left did not read end of file",
    )?;
    answers(
        fd::sys_close(process, reader),
        0,
        "a pipe's read end would not close",
    )?;
    Ok(len as u64)
}

/// An empty non-blocking pipe answers `EAGAIN`; a write with no reader left is
/// `EPIPE`; `pipe2` takes only its two flags; and a pair it cannot hand back
/// is closed again.
fn check_a_pipe_refuses_as_linux_does(process: &Process, page: u64) -> Result<(), &'static str> {
    answers(
        pipe::sys_pipe2(process, page + AT_FDS, O_NONBLOCK | O_CLOEXEC),
        0,
        "pipe2 with O_NONBLOCK and O_CLOEXEC was refused",
    )?;
    let (reader, writer) = pair(process, page)?;
    refuses(
        file::sys_read(process, reader, page + AT_BACK, 1),
        Errno::EAGAIN,
        "an empty non-blocking pipe did not answer EAGAIN",
    )?;
    answers(
        fd::sys_fcntl(process, writer, F_GETFD, 0),
        FD_CLOEXEC as usize,
        "pipe2's O_CLOEXEC did not reach the descriptor",
    )?;
    answers(
        fd::sys_close(process, reader),
        0,
        "a pipe's read end would not close",
    )?;
    refuses(
        file::sys_write(process, writer, page + AT_DATA, 1),
        Errno::EPIPE,
        "a write with no reader left was not EPIPE",
    )?;
    answers(
        fd::sys_close(process, writer),
        0,
        "a pipe's write end would not close",
    )?;

    refuses(
        pipe::sys_pipe2(process, page + AT_FDS, O_APPEND),
        Errno::EINVAL,
        "pipe2 accepted a flag it does not take",
    )?;
    refuses(
        pipe::sys_pipe2(process, KERNEL_ADDRESS, 0),
        Errno::EFAULT,
        "pipe2 into memory the program cannot write was not EFAULT",
    )?;
    refuses(
        fd::sys_close(process, 3),
        Errno::EBADF,
        "a pipe2 that failed left a descriptor behind",
    )
}

/// A FIFO under /tmp is one pipe for every opener: a non-blocking writer with
/// no reader is `ENXIO`, and once a reader is open what one descriptor writes
/// the other reads.
fn check_a_fifo_is_one_pipe(process: &Process, page: u64) -> Result<u64, &'static str> {
    let ns = fs::namespace();
    ns.mknod(&ns.context(), None, name(FIFO), NewNode::Fifo, 0o600)
        .map_err(|_| "a FIFO could not be made under /tmp")?;
    refuses(
        fd::sys_openat(process, AT_FDCWD, page + AT_FIFO, O_WRONLY | O_NONBLOCK, 0),
        Errno::ENXIO,
        "a non-blocking open of a FIFO for writing, with no reader, was not ENXIO",
    )?;
    let reader = descriptor(
        fd::sys_openat(process, AT_FDCWD, page + AT_FIFO, O_RDONLY | O_NONBLOCK, 0),
        "a FIFO would not open for reading",
    )?;
    // A blocking open, which does not wait: a reader is already there.
    let writer = descriptor(
        fd::sys_openat(process, AT_FDCWD, page + AT_FIFO, O_WRONLY, 0),
        "a FIFO would not open for writing",
    )?;
    let len = DATA.len();
    answers(
        file::sys_write(process, writer, page + AT_DATA, len as u64),
        len,
        "a write into a FIFO came back short",
    )?;
    answers(
        file::sys_read(process, reader, page + AT_BACK, 64),
        len,
        "a second open of a FIFO did not read what the first wrote",
    )?;
    if read_back(process, page + AT_BACK, len)? != DATA {
        return Err("a FIFO gave back different bytes than were written into it");
    }
    refuses(
        file::sys_read(process, reader, page + AT_BACK, 64),
        Errno::EAGAIN,
        "a drained FIFO with a writer still open did not answer EAGAIN",
    )?;
    answers(
        fd::sys_close(process, writer),
        0,
        "a FIFO's writer would not close",
    )?;
    answers(
        fd::sys_close(process, reader),
        0,
        "a FIFO's reader would not close",
    )?;
    Ok(len as u64)
}

/// `statfs` of /tmp reports tmpfs in this word size's layout, and in
/// ARMv7-A's packed `statfs64`, whose size argument takes the kernel's 84 and
/// musl's 88 and nothing else.
fn check_statfs_says_tmp_is_tmpfs(process: &Process, page: u64) -> Result<(), &'static str> {
    answers(
        fsctl::sys_statfs(process, page + AT_TMP, page + AT_STATFS),
        0,
        "statfs of /tmp was refused",
    )?;
    if magic(process, page)? != TMPFS_MAGIC {
        return Err("statfs of /tmp did not report TMPFS_MAGIC");
    }
    for size in [84, 88] {
        answers(
            fsctl::sys_statfs64(process, page + AT_TMP, size, page + AT_STATFS),
            0,
            "statfs64 refused the size the kernel or musl passes",
        )?;
    }
    // TMPFS_MAGIC, then a block size of 4096, both 32 bits little-endian.
    if read_back(process, page + AT_STATFS, 8)? != [0x94, 0x19, 0x02, 0x01, 0, 0x10, 0, 0] {
        return Err("statfs64 did not pack the magic number and block size first");
    }
    refuses(
        fsctl::sys_statfs64(process, page + AT_TMP, 120, page + AT_STATFS),
        Errno::EINVAL,
        "statfs64 accepted a size that is neither structure's",
    )
}

/// `truncate` by path and `fallocate` on a descriptor both grow a file under
/// /tmp; `fallocate` never shrinks one, keeps its size when asked to, and
/// refuses a mode it does not have.
fn check_truncate_and_fallocate_grow(process: &Process, page: u64) -> Result<(), &'static str> {
    let made = descriptor(
        fd::sys_openat(
            process,
            AT_FDCWD,
            page + AT_FILE,
            O_RDWR | O_CREAT | O_TRUNC,
            0o644,
        ),
        "a file could not be created under /tmp",
    )?;
    let len = DATA.len();
    answers(
        file::sys_write(process, made, page + AT_DATA, len as u64),
        len,
        "a write to a file under /tmp came back short",
    )?;
    answers(
        fsctl::sys_truncate(process, page + AT_FILE, 5000),
        0,
        "truncate to a larger size was refused",
    )?;
    size_is(FILE, 5000, "truncate did not grow the file")?;
    answers(
        fsctl::sys_fallocate(process, made, 0, 0, 9000),
        0,
        "fallocate was refused",
    )?;
    size_is(FILE, 9000, "fallocate did not grow the file")?;
    answers(
        fsctl::sys_fallocate(process, made, 0, 0, 100),
        0,
        "fallocate of a range inside the file was refused",
    )?;
    answers(
        fsctl::sys_fallocate(process, made, FALLOC_FL_KEEP_SIZE, 0, 20_000),
        0,
        "fallocate with FALLOC_FL_KEEP_SIZE was refused",
    )?;
    size_is(
        FILE,
        9000,
        "fallocate shrank the file, or grew it despite KEEP_SIZE",
    )?;
    refuses(
        fsctl::sys_fallocate(
            process,
            made,
            FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE,
            0,
            1,
        ),
        Errno::EOPNOTSUPP,
        "fallocate accepted a mode it does not have",
    )?;
    refuses(
        fsctl::sys_truncate(process, page + AT_FILE, -1),
        Errno::EINVAL,
        "truncate to a negative length was not EINVAL",
    )?;
    refuses(
        fsctl::sys_truncate(process, page + AT_TMP, 0),
        Errno::EISDIR,
        "truncate of a directory was not EISDIR",
    )?;
    answers(
        fsctl::sys_fsync(process, made),
        0,
        "fsync of a file was refused",
    )?;
    answers(fd::sys_close(process, made), 0, "a file would not close")
}

/// `sendfile` copies a file: from an offset it reads there, moves the offset
/// and leaves the file position alone; without one it moves the position; and
/// an input not open for reading is `EBADF`.
fn check_sendfile_copies_a_file(process: &Process, page: u64) -> Result<u64, &'static str> {
    let source = descriptor(
        fd::sys_openat(process, AT_FDCWD, page + AT_FILE, O_RDONLY, 0),
        "the file to send would not open",
    )?;
    let copy = descriptor(
        fd::sys_openat(
            process,
            AT_FDCWD,
            page + AT_COPY,
            O_WRONLY | O_CREAT | O_TRUNC,
            0o644,
        ),
        "a file to send into could not be created",
    )?;
    uaccess::copy_to_user(process.space(), page + AT_OFFSET, &10_u64.to_le_bytes())
        .map_err(|_| "could not stage sendfile's offset")?;
    answers(
        pipe::sys_sendfile(process, copy, source, page + AT_OFFSET, 1 << 20),
        8990,
        "sendfile from an offset did not send the rest of the file",
    )?;
    if read_back(process, page + AT_OFFSET, 8)? != 9000_u64.to_le_bytes() {
        return Err("sendfile did not move its offset past what it sent");
    }
    answers(
        fd::sys_lseek(process, source, 0, SEEK_CUR),
        0,
        "sendfile with an offset moved the file position",
    )?;
    answers(
        pipe::sys_sendfile(process, copy, source, 0, 1 << 20),
        9000,
        "sendfile from the file position did not send the whole file",
    )?;
    answers(
        fd::sys_lseek(process, source, 0, SEEK_CUR),
        9000,
        "sendfile without an offset did not move the file position",
    )?;
    refuses(
        pipe::sys_sendfile(process, source, copy, 0, 1),
        Errno::EBADF,
        "sendfile read from a descriptor open only for writing",
    )?;
    answers(fd::sys_close(process, copy), 0, "the copy would not close")?;
    answers(
        fd::sys_close(process, source),
        0,
        "the source would not close",
    )?;

    // The copy is the file from byte 10, then the whole file again: its first
    // bytes are the data's from byte 10 on.
    let ns = fs::namespace();
    let read = OpenFlags {
        read: true,
        ..OpenFlags::default()
    };
    let copied = ns
        .open(&ns.context(), None, name(COPY), &read, 0)
        .map_err(|_| "the copy sendfile made would not open")?;
    let mut head = [0_u8; 5];
    let expected = DATA.get(10..15).unwrap_or_default();
    if copied.read_at(0, &mut head) != Ok(5) || head.as_slice() != expected {
        return Err("sendfile copied different bytes than the file holds");
    }
    size_is(
        COPY,
        17_990,
        "sendfile's copy is not as long as what it sent",
    )?;
    Ok(17_990)
}

/// Make `call` by its number, as a program on this architecture would.
fn by_number(process: &Process, call: Syscall, args: [u64; 6]) -> Result<usize, Errno> {
    syscall_check::call_by_number(process, call, args)
}

/// A descriptor, as a register carries it.
fn register(fd: i32) -> u64 {
    u64::from(fd.unsigned_abs())
}

/// `mount -t proc` and `mount -t devtmpfs`, by number, as an init script makes
/// them: each on a directory under /tmp, with the flags scripts pass and, for
/// proc, an option it does not read. Through the second procfs the check
/// process finds itself, and `self` answers as `/proc/self` does; from the
/// second devtmpfs `zero` reads zeros; `/proc/mounts` lists both; both unmount;
/// and `sysfs`, which does not exist, is still `ENODEV`.
fn check_proc_and_devtmpfs_mount(process: &Process, page: u64) -> Result<(), &'static str> {
    for dir in [AT_PROC_DIR, AT_DEV_DIR] {
        answers(
            by_number(process, Syscall::Mkdirat, [CWD, page + dir, 0o755, 0, 0, 0]),
            0,
            "a directory to mount on could not be made under /tmp",
        )?;
    }
    check_a_second_procfs(process, page)?;
    check_a_second_devtmpfs(process, page)?;
    check_both_are_listed_and_unmount(process, page)
}

/// `mount -t proc` with the flags and an option: `self` in it answers as
/// `/proc/self` does, and the check process is found in it by its pid.
fn check_a_second_procfs(process: &Process, page: u64) -> Result<(), &'static str> {
    answers(
        by_number(
            process,
            Syscall::Mount,
            [
                page + AT_PROC_TYPE,
                page + AT_PROC_DIR,
                page + AT_PROC_TYPE,
                u64::from(PROC_FLAGS),
                page + AT_OPTIONS,
                0,
            ],
        ),
        0,
        "mount -t proc with nosuid, nodev, noexec, relatime and hidepid= was refused",
    )?;
    // `self` is the reader's pid, and `ENOENT` to a task with no process,
    // which the boot task is. What matters is that a second procfs answers
    // exactly as the first.
    let through_mount = by_number(
        process,
        Syscall::Readlinkat,
        [CWD, page + AT_PROC_SELF, page + AT_LINK, 32, 0, 0],
    );
    let through_proc = by_number(
        process,
        Syscall::Readlinkat,
        [CWD, page + AT_SELF, page + AT_LINK_PROC, 32, 0, 0],
    );
    let same = match (through_mount, through_proc) {
        (Ok(a), Ok(b)) => {
            a == b
                && read_back(process, page + AT_LINK, a)?
                    == read_back(process, page + AT_LINK_PROC, b)?
        }
        (Err(a), Err(b)) => a == b,
        _ => false,
    };
    if !same {
        return Err("self in a second procfs does not answer as /proc/self does");
    }
    // The same processes: the check's own, found by its pid.
    let pid = process.pid();
    let stat = alloc::format!("/tmp/stage8-proc/{pid}/stat\0");
    uaccess::copy_to_user(process.space(), page + AT_STAT, stat.as_bytes())
        .map_err(|_| "could not stage a path into a second procfs")?;
    let own = descriptor(
        by_number(
            process,
            Syscall::Openat,
            [CWD, page + AT_STAT, u64::from(O_RDONLY), 0, 0, 0],
        ),
        "the check process's stat would not open through a second procfs",
    )?;
    let read = by_number(
        process,
        Syscall::Read,
        [register(own), page + AT_LINK, 32, 0, 0, 0],
    )
    .map_err(|_| "the check process's stat would not read through a second procfs")?;
    answers(
        by_number(process, Syscall::Close, [register(own), 0, 0, 0, 0, 0]),
        0,
        "a file in a second procfs would not close",
    )?;
    let prefix = alloc::format!("{pid} (");
    if !read_back(process, page + AT_LINK, read)?.starts_with(prefix.as_bytes()) {
        return Err("a second procfs does not show the check process as /proc does");
    }
    Ok(())
}

/// `mount -t devtmpfs` with the flags: `zero` in it reads zeros.
fn check_a_second_devtmpfs(process: &Process, page: u64) -> Result<(), &'static str> {
    answers(
        by_number(
            process,
            Syscall::Mount,
            [
                page + AT_DEVTMPFS_TYPE,
                page + AT_DEV_DIR,
                page + AT_DEVTMPFS_TYPE,
                u64::from(DEVTMPFS_FLAGS),
                0,
                0,
            ],
        ),
        0,
        "mount -t devtmpfs with nosuid and relatime was refused",
    )?;
    uaccess::copy_to_user(process.space(), page + AT_ZEROS, &[0xA5; 16])
        .map_err(|_| "could not stage a buffer for zeros")?;
    let zero = descriptor(
        by_number(
            process,
            Syscall::Openat,
            [CWD, page + AT_DEV_ZERO, u64::from(O_RDONLY), 0, 0, 0],
        ),
        "zero in a second devtmpfs would not open",
    )?;
    answers(
        by_number(
            process,
            Syscall::Read,
            [register(zero), page + AT_ZEROS, 16, 0, 0, 0],
        ),
        16,
        "zero in a second devtmpfs did not fill a read",
    )?;
    answers(
        by_number(process, Syscall::Close, [register(zero), 0, 0, 0, 0, 0]),
        0,
        "zero in a second devtmpfs would not close",
    )?;
    if read_back(process, page + AT_ZEROS, 16)? != [0; 16] {
        return Err("zero in a second devtmpfs read something other than zeros");
    }
    Ok(())
}

/// `/proc/mounts` lists both mounts as Linux prints them; both unmount and
/// their directories can be removed; and `sysfs` is still `ENODEV`.
fn check_both_are_listed_and_unmount(process: &Process, page: u64) -> Result<(), &'static str> {
    let listing = read_mounts(process, page)?;
    for line in [
        &b"proc /tmp/stage8-proc proc rw 0 0\n"[..],
        b"devtmpfs /tmp/stage8-dev devtmpfs rw 0 0\n",
    ] {
        if !listing.windows(line.len()).any(|window| window == line) {
            return Err("/proc/mounts does not list a proc or devtmpfs mount as Linux prints it");
        }
    }

    for dir in [AT_PROC_DIR, AT_DEV_DIR] {
        answers(
            by_number(process, Syscall::Umount2, [page + dir, 0, 0, 0, 0, 0]),
            0,
            "umount2 of a proc or devtmpfs mount was refused",
        )?;
    }
    refuses(
        by_number(
            process,
            Syscall::Umount2,
            [page + AT_PROC_DIR, 0, 0, 0, 0, 0],
        ),
        Errno::EINVAL,
        "umount2 of a directory nothing is mounted on any more was not EINVAL",
    )?;
    refuses(
        by_number(
            process,
            Syscall::Mount,
            [
                page + AT_SYSFS_TYPE,
                page + AT_PROC_DIR,
                page + AT_SYSFS_TYPE,
                0,
                0,
                0,
            ],
        ),
        Errno::ENODEV,
        "mount -t sysfs, a type that does not exist, was not ENODEV",
    )?;
    for dir in [AT_PROC_DIR, AT_DEV_DIR] {
        answers(
            by_number(
                process,
                Syscall::Unlinkat,
                [CWD, page + dir, u64::from(AT_REMOVEDIR), 0, 0, 0],
            ),
            0,
            "a directory unmounted from could not be removed",
        )?;
    }
    Ok(())
}

/// The whole of `/proc/mounts`, read by number in pieces.
fn read_mounts(process: &Process, page: u64) -> Result<Vec<u8>, &'static str> {
    let mounts = descriptor(
        by_number(
            process,
            Syscall::Openat,
            [CWD, page + AT_MOUNTS, u64::from(O_RDONLY), 0, 0, 0],
        ),
        "/proc/mounts would not open",
    )?;
    let room = PAGE_SIZE - AT_LISTING;
    let mut listing = Vec::new();
    let outcome = loop {
        match by_number(
            process,
            Syscall::Read,
            [register(mounts), page + AT_LISTING, room, 0, 0, 0],
        ) {
            Ok(0) => break Ok(()),
            Ok(count) if listing.len() < 4 * PAGE_SIZE as usize => {
                listing.extend_from_slice(&read_back(process, page + AT_LISTING, count)?);
            }
            Ok(_) => break Err("/proc/mounts did not end"),
            Err(_) => break Err("/proc/mounts would not read"),
        }
    };
    let _ = by_number(process, Syscall::Close, [register(mounts), 0, 0, 0, 0, 0]);
    outcome.map(|()| listing)
}
