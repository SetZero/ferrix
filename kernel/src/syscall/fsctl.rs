//! The calls about a filesystem as a whole rather than one file in it:
//! `statfs`, `sync` and its kin, `truncate`, `fallocate`, `chroot`, `mount`,
//! `umount2`, `pivot_root` and the extended attributes. [`dispatch`] also
//! routes `pipe`, `pipe2`, `sendfile`, `splice` and `copy_file_range` to
//! `crate::syscall::pipe`, so that
//! stage 8's calls hang off `with_process` by one line.
//!
//! # Writing out
//!
//! Every filesystem but btrfs is memory, and has nothing for `sync`,
//! `syncfs`, `fsync` or `fdatasync` to write out: each is done the moment it
//! is asked, which is a true answer rather than a pretence. On btrfs each
//! commits: `fsync` writes the file back and commits the transaction,
//! `syncfs` the filesystem the descriptor is on, and `sync` every mount in
//! the namespace. What they check is what Linux checks -- a descriptor that
//! names nothing is `EBADF`, and an object with no storage to sync, a pipe
//! or a terminal, is `EINVAL`.
//!
//! # Mounting what exists
//!
//! `mount` makes a new filesystem on a directory: a `tmpfs`, a `proc` or a
//! `devtmpfs`, the three an init script mounts first, or a `btrfs` on a disk.
//! Each `proc` mount is a new procfs instance over the one kernel, so every
//! one of them shows the same processes, as on Linux; each `devtmpfs` mount
//! is a new devfs over the one device table. Mounting on a directory that is
//! already a mount's root stacks the new one on top, so `mount -t proc proc
//! /proc` over the boot's `/proc` works, and unmounting it uncovers the old
//! one.
//!
//! `btrfs` is the one type with a source: the path of a block node in `/dev`,
//! whose number names a disk a ring-3 driver registered. The source is
//! resolved last, after the target and the flags, as Linux resolves it inside
//! the filesystem's own mount; a source that is not a block node is
//! `ENOTBLK`, and a number no disk answers to is `ENXIO`. A mount that asks
//! for `MS_RDONLY` gets stage 11's reader; one that does not gets stage
//! 12's writer, which refuses with `EROFS` — the answer a program gets from
//! a read-only medium — when the disk takes no writes or the volume is one
//! it will not maintain, a snapshot or a quota-enabled volume among them.
//!
//! The types that do not exist yet -- `sysfs`, `devpts`, `cgroup2` and every
//! other -- are `ENODEV`, Linux's answer for a type the kernel was built
//! without; they are added to [`filesystem_named`] as they arrive.
//!
//! The per-mount flags `MS_NOSUID`, `MS_NODEV`, `MS_NOEXEC`, `MS_RELATIME`
//! and the rest of the access-time ones are accepted for every type and do
//! nothing, because there is nothing yet for any of them to switch off: no
//! program runs set-user-ID, and no access time is kept apart from the others.
//! `/proc/mounts` does not print them, for the same reason. The options string
//! -- tmpfs's `size=`, procfs's `hidepid=` -- is not read for any type, so an
//! option is never refused and never has an effect. Changing an existing mount --
//! `MS_REMOUNT`, `MS_BIND`, `MS_MOVE` and the propagation flags -- is `EINVAL`,
//! because the mount table has no operation that does it.
//!
//! # No extended attributes
//!
//! tmpfs keeps none. So every file answers that it has none: `getxattr` is
//! `ENODATA`, a list is empty, and setting or removing one is `EOPNOTSUPP`. The
//! path or the descriptor is resolved first, so a missing file is still
//! `ENOENT` and a closed descriptor still `EBADF`.

use alloc::sync::Arc;
use core::mem::size_of;

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::nr::Syscall;
use ferrix_linux_abi::types::{
    ARM_STATFS64_UNPACKED_SIZE, AT_FDCWD, AT_SYMLINK_NOFOLLOW, FALLOC_FL_KEEP_SIZE, MNT_DETACH,
    MNT_EXPIRE, MNT_FORCE, MS_BIND, MS_MGC_MSK, MS_MGC_VAL, MS_MOVE, MS_PRIVATE, MS_RDONLY,
    MS_REMOUNT, MS_SHARED, MS_SLAVE, MS_UNBINDABLE, UMOUNT_NOFOLLOW,
};
use ferrix_vfs::access::{MAY_EXEC, MAY_WRITE};
use ferrix_vfs::statfs::StatfsLayout;
use ferrix_vfs::{FileSystem, FileType};

use crate::fs;
use crate::fs::devfs::Devfs;
use crate::fs::procfs::Procfs;
use crate::syscall::credentials;
use crate::syscall::path::{self, Target};
use crate::syscall::process::Process;
use crate::syscall::{fd, pipe, uaccess};

/// The `mount` flags that ask to change a mount rather than make one. See the
/// module documentation.
///
/// `MS_RDONLY` is judged per filesystem rather than here: a program that
/// mounts read-only relies on writes failing, so the memory filesystems, which
/// cannot refuse a write, refuse the flag with `EINVAL`, while btrfs, which
/// cannot accept one, requires it. The other per-mount flags -- `nosuid`,
/// `nodev`, `noexec`, the access-time ones, `MS_SILENT` -- are accepted,
/// because there is nothing yet for any of them to switch off.
const REFUSED_MOUNT_FLAGS: u32 =
    MS_REMOUNT | MS_BIND | MS_MOVE | MS_UNBINDABLE | MS_PRIVATE | MS_SLAVE | MS_SHARED;

/// The calls this module answers, or `None` for one it does not.
pub(crate) fn dispatch(
    call: Syscall,
    a: &[u64; 6],
    process: &Process,
) -> Option<Result<usize, Errno>> {
    let fd = fd::arg(a[0]);
    let answer = match call {
        Syscall::Pipe => pipe::sys_pipe2(process, a[0], 0),
        Syscall::Pipe2 => pipe::sys_pipe2(process, a[0], super::truncate(a[1])),
        Syscall::Sendfile | Syscall::Sendfile64 => {
            pipe::sys_sendfile(process, fd, fd::arg(a[1]), a[2], a[3])
        }
        Syscall::Splice => {
            let flags = super::truncate(a[5]);
            pipe::sys_splice(process, fd, a[1], fd::arg(a[2]), a[3], a[4], flags)
        }
        Syscall::CopyFileRange => {
            let flags = super::truncate(a[5]);
            pipe::sys_copy_file_range(process, fd, a[1], fd::arg(a[2]), a[3], a[4], flags)
        }
        Syscall::Statfs => sys_statfs(process, a[0], a[1]),
        Syscall::Fstatfs => sys_fstatfs(process, fd, a[1]),
        Syscall::Statfs64 => sys_statfs64(process, a[0], a[1], a[2]),
        Syscall::Fstatfs64 => sys_fstatfs64(process, fd, a[1], a[2]),
        Syscall::Sync => sys_sync(),
        Syscall::Syncfs => sys_syncfs(process, fd),
        Syscall::Fsync => sys_fsync(process, fd, false),
        Syscall::Fdatasync => sys_fsync(process, fd, true),
        Syscall::Readahead => sys_readahead(process, fd, readahead_count(a)),
        Syscall::Truncate => sys_truncate(process, a[0], super::native_signed(a[1])),
        Syscall::Truncate64 => sys_truncate(process, a[0], super::wide(a, 1)),
        // `fallocate(fd, mode, offset, len)`: on ARMv7-A the offset is in the
        // register pair from 2 and the length in the pair from 4, which
        // `wide` reaches from slot 3. QEMU's linux-user reads the same six
        // registers, and busybox loads r4 and r5 before the call.
        Syscall::Fallocate => sys_fallocate(
            process,
            fd,
            super::truncate(a[1]),
            super::wide(a, 2),
            super::wide(a, 3),
        ),
        Syscall::Chroot => sys_chroot(process, a[0]),
        Syscall::Mount => sys_mount(process, a[0], a[1], a[2], super::truncate(a[3])),
        Syscall::Umount2 => sys_umount2(process, a[0], super::truncate(a[1])),
        // `pivot_root` moves the root mount aside and puts another in its
        // place. The namespace's root is fixed at its creation and has no
        // parent to move under, which is the case Linux answers `EINVAL` for.
        Syscall::PivotRoot => Err(Errno::EINVAL),
        Syscall::Getxattr | Syscall::Listxattr | Syscall::Setxattr | Syscall::Removexattr => {
            xattr_at(process, a[0], 0, call)
        }
        Syscall::Lgetxattr | Syscall::Llistxattr | Syscall::Lsetxattr | Syscall::Lremovexattr => {
            xattr_at(process, a[0], AT_SYMLINK_NOFOLLOW, call)
        }
        Syscall::Fgetxattr | Syscall::Flistxattr | Syscall::Fsetxattr | Syscall::Fremovexattr => {
            xattr_of(process, fd, call)
        }
        _ => return None,
    };
    Some(answer)
}

// ---------------------------------------------------------------------------
// statfs
// ---------------------------------------------------------------------------

/// The layout plain `statfs` and `fstatfs` fill, which the word size decides.
fn native_layout() -> StatfsLayout {
    StatfsLayout::native(size_of::<usize>())
}

/// Put what `target`'s filesystem says into the program's buffer.
fn write_statfs(
    process: &Process,
    buf: u64,
    target: &Target,
    layout: StatfsLayout,
) -> Result<usize, Errno> {
    let record = layout.encode(&fs::namespace().statfs(target.location()))?;
    uaccess::copy_to_user(process.space(), buf, &record).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}

/// `statfs64`'s size argument: the kernel's packed structure, or musl's
/// unpacked one, which Linux's ARM entry code takes as the same thing. Checked
/// before the path is looked at, as on Linux.
fn statfs64_size(size: u64) -> Result<(), Errno> {
    let size = usize::try_from(size).map_err(|_| Errno::EINVAL)?;
    if size == StatfsLayout::Packed64.size() || size == ARM_STATFS64_UNPACKED_SIZE {
        Ok(())
    } else {
        Err(Errno::EINVAL)
    }
}

/// `statfs`: the filesystem a path is on, following a final link.
pub(crate) fn sys_statfs(process: &Process, at: u64, buf: u64) -> Result<usize, Errno> {
    let target = path::target(process, AT_FDCWD, at, 0)?;
    write_statfs(process, buf, &target, native_layout())
}

/// `fstatfs`: the filesystem an open file is on. An `O_PATH` descriptor is
/// enough, as on Linux.
pub(crate) fn sys_fstatfs(process: &Process, fd: i32, buf: u64) -> Result<usize, Errno> {
    let target = Target::Open(fd::file(process, fd)?);
    write_statfs(process, buf, &target, native_layout())
}

/// `statfs64`, ARMv7-A's, into the packed `struct statfs64`.
pub(crate) fn sys_statfs64(
    process: &Process,
    at: u64,
    size: u64,
    buf: u64,
) -> Result<usize, Errno> {
    statfs64_size(size)?;
    let target = path::target(process, AT_FDCWD, at, 0)?;
    write_statfs(process, buf, &target, StatfsLayout::Packed64)
}

/// `fstatfs64`, ARMv7-A's.
pub(crate) fn sys_fstatfs64(
    process: &Process,
    fd: i32,
    size: u64,
    buf: u64,
) -> Result<usize, Errno> {
    statfs64_size(size)?;
    let target = Target::Open(fd::file(process, fd)?);
    write_statfs(process, buf, &target, StatfsLayout::Packed64)
}

// ---------------------------------------------------------------------------
// Syncing, and lengths
// ---------------------------------------------------------------------------

/// An open file a call that acts through a descriptor may use: `O_PATH` names
/// a file without opening it, and is `EBADF` to these as to `read`.
fn usable(process: &Process, fd: i32) -> Result<Arc<ferrix_vfs::OpenFile>, Errno> {
    let file = fd::file(process, fd)?;
    if file.is_path() {
        return Err(Errno::EBADF);
    }
    Ok(file)
}

/// `syncfs`: write out the filesystem the descriptor's file is on. On a
/// filesystem that keeps nothing back — everything in memory — that is
/// nothing; on btrfs it is a transaction commit.
fn sys_syncfs(process: &Process, fd: i32) -> Result<usize, Errno> {
    let file = fd::file(process, fd)?;
    file.location().mount.filesystem().sync()?;
    Ok(0)
}

/// `sync`: write out every filesystem in the namespace, and answer nothing,
/// as Linux does — `sync(2)` has no error to give.
fn sys_sync() -> Result<usize, Errno> {
    for mount in fs::namespace().mounts() {
        let _ = mount.filesystem().sync();
    }
    Ok(0)
}

/// `fsync` and `fdatasync`: write this file out and wait for it. `EINVAL` for
/// an object with no storage to sync -- a pipe, a terminal -- as Linux
/// answers for a file with no `fsync` operation.
pub(crate) fn sys_fsync(process: &Process, fd: i32, data_only: bool) -> Result<usize, Errno> {
    let file = usable(process, fd)?;
    match file.kind() {
        FileType::Regular | FileType::Directory => {
            file.location().inode()?.fsync(data_only)?;
            Ok(0)
        }
        _ => Err(Errno::EINVAL),
    }
}

/// `readahead`: done as soon as it is asked, because there is no page cache
/// to fill -- every file is in memory already.
///
/// What is still checked is what Linux's `ksys_readahead` checks, in its
/// order: a descriptor not open for reading, `O_PATH` included, is `EBADF`;
/// anything but a regular file is `EINVAL`; and a count too large to be a
/// `loff_t` is `EINVAL`, from `generic_fadvise`. The offset is never looked
/// at, as Linux does not look at it either before the page cache does.
pub(crate) fn sys_readahead(process: &Process, fd: i32, count: u64) -> Result<usize, Errno> {
    let file = fd::file(process, fd)?;
    if !file.readable() {
        return Err(Errno::EBADF);
    }
    if file.kind() != FileType::Regular || i64::try_from(count).is_err() {
        return Err(Errno::EINVAL);
    }
    Ok(0)
}

/// `readahead`'s count register.
///
/// `readahead(fd, offset, count)` puts a 64-bit `loff_t` second. One register
/// on a 64-bit architecture, so the count is the third. On ARMv7-A the EABI
/// starts the offset at the next even register, r2 and r3, which leaves r1
/// empty and the 32-bit count in r4 -- what `regpairs_aligned` makes QEMU's
/// linux-user read, and what `super::wide` assumes for the offset.
fn readahead_count(a: &[u64; 6]) -> u64 {
    if size_of::<usize>() == 8 {
        a[2]
    } else {
        a[4] & 0xFFFF_FFFF
    }
}

/// `truncate` and `truncate64`: set a file's length by path, following a
/// final link. A negative length is refused before the path is read.
pub(crate) fn sys_truncate(process: &Process, at: u64, length: i64) -> Result<usize, Errno> {
    let length = u64::try_from(length).map_err(|_| Errno::EINVAL)?;
    let target = path::target(process, AT_FDCWD, at, 0)?;
    let metadata = target.stat()?.metadata;
    if metadata.kind == FileType::Regular {
        path::context(process).who.require(&metadata, MAY_WRITE)?;
    }
    fs::namespace().truncate(target.location(), length)?;
    Ok(0)
}

/// `fallocate`, in the order Linux's `vfs_fallocate` checks.
///
/// Mode 0 grows the file to cover the range if it does not already, and never
/// shrinks it. `FALLOC_FL_KEEP_SIZE` is accepted and does nothing: what it
/// asks is that later writes into the range cannot fail for space, and pages
/// here are committed when first written, so there is nothing to reserve --
/// the same is true of the range mode 0 grows over, which reads as zeros and
/// costs nothing until written. Every other mode punches, zeroes, collapses or
/// inserts ranges, which tmpfs cannot, and is `EOPNOTSUPP`.
pub(crate) fn sys_fallocate(
    process: &Process,
    fd: i32,
    mode: u32,
    offset: i64,
    len: i64,
) -> Result<usize, Errno> {
    let file = usable(process, fd)?;
    if offset < 0 || len <= 0 {
        return Err(Errno::EINVAL);
    }
    if mode & !FALLOC_FL_KEEP_SIZE != 0 {
        return Err(Errno::EOPNOTSUPP);
    }
    if !file.writable() {
        return Err(Errno::EBADF);
    }
    match file.kind() {
        FileType::Regular => {}
        FileType::Fifo => return Err(Errno::ESPIPE),
        FileType::Directory => return Err(Errno::EISDIR),
        _ => return Err(Errno::ENODEV),
    }
    let end = offset
        .checked_add(len)
        .and_then(|end| u64::try_from(end).ok())
        .ok_or(Errno::EFBIG)?;
    if mode & FALLOC_FL_KEEP_SIZE == 0 {
        file.grow_to(end)?;
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// The tree: chroot, mount, umount2
// ---------------------------------------------------------------------------

/// `chroot`: make a directory the process's `/`.
///
/// The new root must be a directory the caller may search, as Linux's
/// `path_permission` requires, and the caller root, for `CAP_SYS_CHROOT`. The
/// working directory stays where it was, as on Linux.
pub(crate) fn sys_chroot(process: &Process, at: u64) -> Result<usize, Errno> {
    let place = path::target(process, AT_FDCWD, at, 0)?.location().clone();
    let metadata = fs::namespace().stat(&place)?.metadata;
    if metadata.kind != FileType::Directory {
        return Err(Errno::ENOTDIR);
    }
    path::context(process).who.require(&metadata, MAY_EXEC)?;
    credentials::require_privilege(process)?;
    process.fs_context().lock().root = place;
    Ok(0)
}

/// The filesystem a `mount` type names, new: on nothing for the memory
/// filesystems, on the disk `source` names for btrfs.
///
/// The names are the ones Linux registers, and the ones `/proc/filesystems`
/// lists. `sysfs` and `devpts` join this match when they exist; until then
/// they fall to `ENODEV` with every name Linux would not know either.
/// `cgroup2` mounts read-only as well as writable, as on Linux, where a
/// read-only mount is how a container is shown the tree it may not change;
/// here the flag is not yet enforced on it.
fn filesystem_named(
    process: &Process,
    name: &[u8],
    source: u64,
    read_only: bool,
) -> Result<Arc<dyn FileSystem>, Errno> {
    match name {
        b"tmpfs" | b"proc" | b"devtmpfs" if read_only => Err(Errno::EINVAL),
        b"tmpfs" => Ok(fs::new_tmpfs()),
        b"proc" => Ok(Arc::new(Procfs::new())),
        b"devtmpfs" => Ok(Arc::new(Devfs::new())),
        b"cgroup2" => Ok(Arc::new(fs::cgroupfs::Cgroupfs::new())),
        b"btrfs" => {
            if source == 0 {
                return Err(Errno::EINVAL);
            }
            let node = path::target(process, AT_FDCWD, source, 0)?;
            let meta = node.location().inode()?.metadata();
            if meta.kind != FileType::BlockDevice {
                return Err(Errno::ENOTBLK);
            }
            if read_only {
                fs::btrfs::mount(meta.rdev)
            } else {
                fs::btrfs::mount_rw(meta.rdev)
            }
        }
        _ => Err(Errno::ENODEV),
    }
}

/// `mount(source, target, type, flags, data)`, for the one kind of mount there
/// is: a new filesystem on a directory. The options string means nothing to
/// any filesystem here and is not read, and the source only to btrfs; see the
/// module documentation.
///
/// In Linux's order: the type is copied in before the target is looked up,
/// the flags are judged after, and the type is only looked for last, with
/// the source resolved inside it.
pub(crate) fn sys_mount(
    process: &Process,
    source: u64,
    target: u64,
    kind: u64,
    flags: u32,
) -> Result<usize, Errno> {
    let kind = if kind == 0 {
        None
    } else {
        Some(fd::user_path(process, kind)?)
    };
    let place = path::target(process, AT_FDCWD, target, 0)?
        .location()
        .clone();
    // The magic number programs from before Linux 2.4 put in the top half,
    // which Linux still strips.
    let flags = if flags & MS_MGC_MSK == MS_MGC_VAL {
        flags & !MS_MGC_MSK
    } else {
        flags
    };
    if flags & REFUSED_MOUNT_FLAGS != 0 {
        return Err(Errno::EINVAL);
    }
    // `may_mount`: `CAP_SYS_ADMIN`.
    credentials::require_privilege(process)?;
    let read_only = flags & MS_RDONLY != 0;
    let filesystem = filesystem_named(process, &kind.ok_or(Errno::EINVAL)?, source, read_only)?;
    let _ = fs::namespace().mount(filesystem, &place)?;
    Ok(0)
}

/// `umount2`: unmount the mount whose root `target` names.
///
/// Every unmount here is what `MNT_DETACH` asks for: the mount leaves the tree
/// at once and goes when the last open file on it closes. So a busy mount is
/// not `EBUSY`, and `MNT_FORCE`, which exists to make a busy one go, has
/// nothing further to do. `MNT_EXPIRE` marks a mount for a later call to
/// remove if nobody used it in between, and there is no use count to decide
/// that by, so it is `EINVAL`.
pub(crate) fn sys_umount2(process: &Process, target: u64, flags: u32) -> Result<usize, Errno> {
    if flags & !(MNT_FORCE | MNT_DETACH | MNT_EXPIRE | UMOUNT_NOFOLLOW) != 0
        || flags & MNT_EXPIRE != 0
    {
        return Err(Errno::EINVAL);
    }
    // `may_mount`, before the target is looked up, as `ksys_umount` does.
    credentials::require_privilege(process)?;
    let follow = if flags & UMOUNT_NOFOLLOW != 0 {
        AT_SYMLINK_NOFOLLOW
    } else {
        0
    };
    let place = path::target(process, AT_FDCWD, target, follow)?
        .location()
        .clone();
    fs::namespace().unmount(&place)?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// Extended attributes
// ---------------------------------------------------------------------------

/// What a file with no extended attributes answers each of the calls.
fn no_attributes(call: Syscall) -> Result<usize, Errno> {
    match call {
        // An empty list, which is a length of zero.
        Syscall::Listxattr | Syscall::Llistxattr | Syscall::Flistxattr => Ok(0),
        Syscall::Getxattr | Syscall::Lgetxattr | Syscall::Fgetxattr => Err(Errno::ENODATA),
        _ => Err(Errno::EOPNOTSUPP),
    }
}

/// The path forms, resolved first so a missing file is `ENOENT`.
fn xattr_at(process: &Process, at: u64, flags: u32, call: Syscall) -> Result<usize, Errno> {
    let _ = path::target(process, AT_FDCWD, at, flags)?;
    no_attributes(call)
}

/// The descriptor forms, checked first so a closed descriptor is `EBADF`.
fn xattr_of(process: &Process, fd: i32, call: Syscall) -> Result<usize, Errno> {
    let _ = usable(process, fd)?;
    no_attributes(call)
}
