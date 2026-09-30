//! glibc's names for its 64-bit time functions on ARMv7-A, for a program
//! built against glibc with `_TIME_BITS=64`, which calls `__time64` where its
//! source says `time`, and `__stat64_time64` where it says `stat`.
//!
//! This library's own functions already take a 64-bit `time_t`, so most of
//! those names are the function itself, a branch as in `arm_names`. musl's
//! names for the same thing are there, and a name the two C libraries share
//! (`__time64`, `__clock_gettime64`, ...) is answered once, there.
//!
//! The rest pass glibc's structures, which differ from musl's:
//!
//! * glibc's `struct stat` is 112 bytes, with the inode second and the
//!   times after the block count; musl's is the kernel's `stat64` with the
//!   64-bit inode and times after it, 152 bytes. The four `stat` calls fill
//!   one and copy the fields across.
//! * glibc's SysV IPC `*_ds` structures put each time as one 64-bit field
//!   where the kernel has it, in two halves, and musl keeps the halves and
//!   adds the `time_t`s at the end. `semctl`, `shmctl` and `msgctl` convert
//!   for the commands that write one, and `msgctl`'s `IPC_SET`, which reads
//!   `msg_qbytes` from a place the two do not share.
//! * glibc's `struct timex` for 64-bit time is the kernel's own, so
//!   `adjtimex` is the system call with it, no conversion at all.
//!
//! Names whose glibc structure this library does not convert -- the `glob`,
//! `ftw` and `fts` families, which hand glibc's `struct stat` to the
//! program's callbacks -- are not defined, and `tools/glibc-versions` keeps
//! the plain names they replace out of `libc.so.6`, so a program that needs
//! one fails to load rather than misreading a structure.

use core::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use core::mem::{offset_of, size_of};

use crate::errno;
use crate::stat::Stat;
use crate::syscall::{self, nr};
use crate::time::Timespec;

// Each glibc name that is this library's function under another name.
#[cfg(not(test))]
core::arch::global_asm!(
    ".pushsection .text.ferrousli_glibc_time64,\"ax\",%progbits",
    ".p2align 2",
    ".arm",
    ".globl __epoll_pwait2_time64",
    ".type __epoll_pwait2_time64, %function",
    "__epoll_pwait2_time64:",
    "b epoll_pwait2",
    ".globl __fcntl_time64",
    ".type __fcntl_time64, %function",
    "__fcntl_time64:",
    "b fcntl",
    ".globl __futimens64",
    ".type __futimens64, %function",
    "__futimens64:",
    "b futimens",
    ".globl __getitimer64",
    ".type __getitimer64, %function",
    "__getitimer64:",
    "b getitimer",
    ".globl __getrusage64",
    ".type __getrusage64, %function",
    "__getrusage64:",
    "b getrusage",
    ".globl __getsockopt64",
    ".type __getsockopt64, %function",
    "__getsockopt64:",
    "b getsockopt",
    ".globl __gettimeofday64",
    ".type __gettimeofday64, %function",
    "__gettimeofday64:",
    "b gettimeofday",
    ".globl __ioctl_time64",
    ".type __ioctl_time64, %function",
    "__ioctl_time64:",
    "b ioctl",
    ".globl __lutimes64",
    ".type __lutimes64, %function",
    "__lutimes64:",
    "b lutimes",
    ".globl __mtx_timedlock64",
    ".type __mtx_timedlock64, %function",
    "__mtx_timedlock64:",
    "b mtx_timedlock",
    ".globl __cnd_timedwait64",
    ".type __cnd_timedwait64, %function",
    "__cnd_timedwait64:",
    "b cnd_timedwait",
    ".globl __nanosleep64",
    ".type __nanosleep64, %function",
    "__nanosleep64:",
    "b nanosleep",
    ".globl __ppoll64",
    ".type __ppoll64, %function",
    "__ppoll64:",
    "b ppoll",
    ".globl __prctl_time64",
    ".type __prctl_time64, %function",
    "__prctl_time64:",
    "b prctl",
    ".globl __pselect64",
    ".type __pselect64, %function",
    "__pselect64:",
    "b pselect",
    ".globl __pthread_cond_timedwait64",
    ".type __pthread_cond_timedwait64, %function",
    "__pthread_cond_timedwait64:",
    "b pthread_cond_timedwait",
    ".globl __pthread_mutex_timedlock64",
    ".type __pthread_mutex_timedlock64, %function",
    "__pthread_mutex_timedlock64:",
    "b pthread_mutex_timedlock",
    ".globl __pthread_rwlock_timedrdlock64",
    ".type __pthread_rwlock_timedrdlock64, %function",
    "__pthread_rwlock_timedrdlock64:",
    "b pthread_rwlock_timedrdlock",
    ".globl __pthread_rwlock_timedwrlock64",
    ".type __pthread_rwlock_timedwrlock64, %function",
    "__pthread_rwlock_timedwrlock64:",
    "b pthread_rwlock_timedwrlock",
    ".globl __pthread_timedjoin_np64",
    ".type __pthread_timedjoin_np64, %function",
    "__pthread_timedjoin_np64:",
    "b pthread_timedjoin_np",
    ".globl __recvmsg64",
    ".type __recvmsg64, %function",
    "__recvmsg64:",
    "b recvmsg",
    ".globl __sched_rr_get_interval64",
    ".type __sched_rr_get_interval64, %function",
    "__sched_rr_get_interval64:",
    "b sched_rr_get_interval",
    ".globl __select64",
    ".type __select64, %function",
    "__select64:",
    "b select",
    ".globl __sem_clockwait64",
    ".type __sem_clockwait64, %function",
    "__sem_clockwait64:",
    "b sem_clockwait",
    ".globl __sem_timedwait64",
    ".type __sem_timedwait64, %function",
    "__sem_timedwait64:",
    "b sem_timedwait",
    ".globl __semtimedop64",
    ".type __semtimedop64, %function",
    "__semtimedop64:",
    "b semtimedop",
    ".globl __sendmsg64",
    ".type __sendmsg64, %function",
    "__sendmsg64:",
    "b sendmsg",
    ".globl __setitimer64",
    ".type __setitimer64, %function",
    "__setitimer64:",
    "b setitimer",
    ".globl __setsockopt64",
    ".type __setsockopt64, %function",
    "__setsockopt64:",
    "b setsockopt",
    ".globl __settimeofday64",
    ".type __settimeofday64, %function",
    "__settimeofday64:",
    "b settimeofday",
    ".globl __sigtimedwait64",
    ".type __sigtimedwait64, %function",
    "__sigtimedwait64:",
    "b sigtimedwait",
    ".globl __thrd_sleep64",
    ".type __thrd_sleep64, %function",
    "__thrd_sleep64:",
    "b thrd_sleep",
    ".globl __timegm64",
    ".type __timegm64, %function",
    "__timegm64:",
    "b timegm",
    ".globl __utimensat64",
    ".type __utimensat64, %function",
    "__utimensat64:",
    "b utimensat",
    ".globl __utimes64",
    ".type __utimes64, %function",
    "__utimes64:",
    "b utimes",
    ".popsection",
);

/// glibc's `struct stat` on ARMv7-A for a program built with 64-bit time:
/// `struct __stat64_t64`, whose sizes and offsets were read from glibc's
/// armhf headers by compiling a probe with `_TIME_BITS=64`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GlibcStat {
    /// The device holding the file.
    pub st_dev: u64,
    /// The inode number.
    pub st_ino: u64,
    /// The type and permissions.
    pub st_mode: c_uint,
    /// The link count.
    pub st_nlink: c_uint,
    /// The owner.
    pub st_uid: c_uint,
    /// The group.
    pub st_gid: c_uint,
    /// The device a special file stands for.
    pub st_rdev: u64,
    /// The size in bytes.
    pub st_size: i64,
    /// The preferred block size for I/O.
    pub st_blksize: c_int,
    /// Padding before the 64-bit count.
    pub __pad: c_int,
    /// The 512-byte blocks allocated.
    pub st_blocks: i64,
    /// The last access.
    pub st_atim: Timespec,
    /// The last modification.
    pub st_mtim: Timespec,
    /// The last status change.
    pub st_ctim: Timespec,
}

const _: () = assert!(size_of::<GlibcStat>() == 112);
const _: () = assert!(offset_of!(GlibcStat, st_ino) == 8);
const _: () = assert!(offset_of!(GlibcStat, st_rdev) == 32);
const _: () = assert!(offset_of!(GlibcStat, st_size) == 40);
const _: () = assert!(offset_of!(GlibcStat, st_blksize) == 48);
const _: () = assert!(offset_of!(GlibcStat, st_blocks) == 56);
const _: () = assert!(offset_of!(GlibcStat, st_atim) == 64);
const _: () = assert!(offset_of!(GlibcStat, st_mtim) == 80);
const _: () = assert!(offset_of!(GlibcStat, st_ctim) == 96);

impl GlibcStat {
    /// glibc's form of this library's `struct stat`.
    fn of(s: &Stat) -> Self {
        Self {
            st_dev: s.st_dev,
            st_ino: s.st_ino,
            st_mode: s.st_mode,
            st_nlink: s.st_nlink,
            st_uid: s.st_uid,
            st_gid: s.st_gid,
            st_rdev: s.st_rdev,
            st_size: s.st_size,
            st_blksize: s.st_blksize,
            __pad: 0,
            st_blocks: s.st_blocks,
            st_atim: s.st_atim,
            st_mtim: s.st_mtim,
            st_ctim: s.st_ctim,
        }
    }
}

/// After one of the `stat` calls returned `ret` into `ours`, writes glibc's
/// form of it to `buf` if it succeeded, and returns `ret`.
///
/// # Safety
///
/// `buf` must be valid for a write of glibc's `struct stat`.
unsafe fn finish(ret: c_int, ours: &Stat, buf: *mut GlibcStat) -> c_int {
    if ret == 0 {
        // SAFETY: the caller vouches for `buf`.
        unsafe { buf.write(GlibcStat::of(ours)) };
    }
    ret
}

/// glibc's `stat` for 64-bit time.
///
/// # Safety
///
/// `path` must be a NUL-terminated string and `buf` valid for a write of
/// glibc's `struct stat`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __stat64_time64(path: *const c_char, buf: *mut GlibcStat) -> c_int {
    let mut ours = Stat::default();
    // SAFETY: the caller vouches for `path`; `ours` is a live local.
    let ret = unsafe { crate::stat::stat(path, &raw mut ours) };
    // SAFETY: the caller vouches for `buf`.
    unsafe { finish(ret, &ours, buf) }
}

/// glibc's `lstat` for 64-bit time.
///
/// # Safety
///
/// As [`__stat64_time64`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __lstat64_time64(path: *const c_char, buf: *mut GlibcStat) -> c_int {
    let mut ours = Stat::default();
    // SAFETY: as in `__stat64_time64`.
    let ret = unsafe { crate::stat::lstat(path, &raw mut ours) };
    // SAFETY: as above.
    unsafe { finish(ret, &ours, buf) }
}

/// glibc's `fstat` for 64-bit time.
///
/// # Safety
///
/// `buf` must be valid for a write of glibc's `struct stat`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __fstat64_time64(fd: c_int, buf: *mut GlibcStat) -> c_int {
    let mut ours = Stat::default();
    // SAFETY: `ours` is a live local.
    let ret = unsafe { crate::stat::fstat(fd, &raw mut ours) };
    // SAFETY: the caller vouches for `buf`.
    unsafe { finish(ret, &ours, buf) }
}

/// glibc's `fstatat` for 64-bit time.
///
/// # Safety
///
/// As [`__stat64_time64`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __fstatat64_time64(
    dirfd: c_int,
    path: *const c_char,
    buf: *mut GlibcStat,
    flags: c_int,
) -> c_int {
    let mut ours = Stat::default();
    // SAFETY: as in `__stat64_time64`.
    let ret = unsafe { crate::stat::fstatat(dirfd, path, &raw mut ours, flags) };
    // SAFETY: as above.
    unsafe { finish(ret, &ours, buf) }
}

/// `CLOCK_REALTIME`.
const CLOCK_REALTIME: usize = 0;

/// glibc's `adjtimex` for 64-bit time, whose `struct timex` is the kernel's
/// `__kernel_timex`: `clock_adjtime64` on the real-time clock, unconverted.
///
/// # Safety
///
/// `buf` must be valid for reads and writes of the kernel's `struct timex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ___adjtimex64(buf: *mut c_void) -> c_int {
    // SAFETY: the kernel reads and writes the caller's structure.
    let ret = unsafe { syscall::syscall2(nr::CLOCK_ADJTIME64, CLOCK_REALTIME, buf.addr()) };
    errno::from_syscall(ret) as c_int
}

/// `IPC_SET`.
const IPC_SET: c_int = 1;
/// `IPC_STAT`, as glibc's programs pass it: without the flag musl's headers
/// add.
const IPC_STAT: c_int = 2;
/// The flag musl's ARMv7-A headers add to the commands that write times,
/// which [`crate::ipc`] turns into joining them.
const IPC_TIME64: c_int = 0x100;

/// The size of glibc's and the kernel's `struct ipc_perm`, at the start of
/// every `*_ds`, the same in both.
const PERM: usize = 36;

/// Copies `from[at..at + len]` to `to[to_at..]`, where both have room: the
/// offsets here are constants inside each structure.
fn move_bytes(to: &mut [u8], to_at: usize, from: &[u8], at: usize, len: usize) {
    if let (Some(to), Some(from)) = (to.get_mut(to_at..to_at + len), from.get(at..at + len)) {
        to.copy_from_slice(from);
    }
}

/// glibc's `semctl` for 64-bit time. Its `struct semid_ds` has `sem_otime`
/// at 40, `sem_ctime` at 48 and `sem_nsems` at 56, 72 bytes in all.
///
/// # Safety
///
/// As `semctl`, with glibc's structure behind `arg` for `IPC_STAT` and
/// `SEM_STAT`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __semctl64(id: c_int, num: c_int, cmd: c_int, arg: c_ulong) -> c_int {
    /// `SEM_STAT` and `SEM_STAT_ANY`.
    const SEM_STAT: c_int = 18;
    const SEM_STAT_ANY: c_int = 20;
    if !matches!(cmd, IPC_STAT | SEM_STAT | SEM_STAT_ANY) {
        // SAFETY: no structure with times: the caller's contract is
        // `semctl`'s.
        return unsafe { crate::ipc::semctl(id, num, cmd, arg) };
    }
    // musl's structure: the kernel's 64 bytes, then the two `time_t`s.
    let mut ours = [0_u8; 80];
    let at = (&raw mut ours).expose_provenance() as c_ulong;
    // SAFETY: `ours` is 80 bytes, musl's `struct semid_ds`.
    let ret = unsafe { crate::ipc::semctl(id, num, cmd | IPC_TIME64, at) };
    if ret >= 0 {
        let mut theirs = [0_u8; 72];
        move_bytes(&mut theirs, 0, &ours, 0, PERM);
        move_bytes(&mut theirs, 40, &ours, 64, 16);
        // `sem_nsems`: musl's 16 bits, glibc's `unsigned long`, both little
        // endian with zeros above.
        move_bytes(&mut theirs, 56, &ours, 52, 2);
        let to = core::ptr::with_exposed_provenance_mut::<[u8; 72]>(arg as usize);
        // SAFETY: the caller vouches for glibc's 72 bytes at `arg`.
        unsafe { to.write_unaligned(theirs) };
    }
    ret
}

/// glibc's `shmctl` for 64-bit time. Its `struct shmid_ds` has `shm_atime`,
/// `shm_dtime` and `shm_ctime` at 40, 48 and 56, and the rest where musl's
/// has them, in 88 bytes.
///
/// # Safety
///
/// As `shmctl`, with glibc's structure at `buf` for `IPC_STAT` and
/// `SHM_STAT`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __shmctl64(id: c_int, cmd: c_int, buf: *mut c_void) -> c_int {
    /// `SHM_STAT` and `SHM_STAT_ANY`.
    const SHM_STAT: c_int = 13;
    const SHM_STAT_ANY: c_int = 15;
    if !matches!(cmd, IPC_STAT | SHM_STAT | SHM_STAT_ANY) {
        // SAFETY: the caller's contract is `shmctl`'s.
        return unsafe { crate::ipc::shmctl(id, cmd, buf) };
    }
    let mut ours = [0_u8; 112];
    // SAFETY: `ours` is 112 bytes, musl's `struct shmid_ds`.
    let ret = unsafe { crate::ipc::shmctl(id, cmd | IPC_TIME64, (&raw mut ours).cast()) };
    if ret >= 0 {
        let mut theirs = [0_u8; 88];
        // The permissions and `shm_segsz`.
        move_bytes(&mut theirs, 0, &ours, 0, PERM + 4);
        move_bytes(&mut theirs, 40, &ours, 88, 24);
        // `shm_cpid`, `shm_lpid` and `shm_nattch`.
        move_bytes(&mut theirs, 64, &ours, 64, 12);
        // SAFETY: the caller vouches for glibc's 88 bytes at `buf`.
        unsafe { buf.cast::<[u8; 88]>().write_unaligned(theirs) };
    }
    ret
}

/// glibc's `msgctl` for 64-bit time. Its `struct msqid_ds` has
/// `msg_stime`, `msg_rtime` and `msg_ctime` at 40, 48 and 56, then
/// `__msg_cbytes`, `msg_qnum`, `msg_qbytes`, `msg_lspid` and `msg_lrpid`
/// from 64, four bytes further on than musl's, in 96 bytes.
///
/// # Safety
///
/// As `msgctl`, with glibc's structure at `buf` for `IPC_STAT`, `MSG_STAT`
/// and `IPC_SET`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __msgctl64(id: c_int, cmd: c_int, buf: *mut c_void) -> c_int {
    /// `MSG_STAT` and `MSG_STAT_ANY`.
    const MSG_STAT: c_int = 11;
    const MSG_STAT_ANY: c_int = 13;
    let mut ours = [0_u8; 112];
    if cmd == IPC_SET {
        // SAFETY: the caller vouches for glibc's structure at `buf`.
        let theirs = unsafe { buf.cast::<[u8; 96]>().read_unaligned() };
        // The kernel reads the permissions and `msg_qbytes`, which is at 68
        // in its structure and at 72 in glibc's.
        move_bytes(&mut ours, 0, &theirs, 0, PERM);
        move_bytes(&mut ours, 68, &theirs, 72, 4);
        // SAFETY: `ours` is musl's structure, filled for `IPC_SET`.
        return unsafe { crate::ipc::msgctl(id, cmd, (&raw mut ours).cast()) };
    }
    if !matches!(cmd, IPC_STAT | MSG_STAT | MSG_STAT_ANY) {
        // SAFETY: the caller's contract is `msgctl`'s.
        return unsafe { crate::ipc::msgctl(id, cmd, buf) };
    }
    // SAFETY: `ours` is 112 bytes, musl's `struct msqid_ds`.
    let ret = unsafe { crate::ipc::msgctl(id, cmd | IPC_TIME64, (&raw mut ours).cast()) };
    if ret >= 0 {
        let mut theirs = [0_u8; 96];
        move_bytes(&mut theirs, 0, &ours, 0, PERM);
        move_bytes(&mut theirs, 40, &ours, 88, 24);
        // `__msg_cbytes` to `msg_lrpid`.
        move_bytes(&mut theirs, 64, &ours, 60, 20);
        // SAFETY: the caller vouches for glibc's 96 bytes at `buf`.
        unsafe { buf.cast::<[u8; 96]>().write_unaligned(theirs) };
    }
    ret
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_move_where_both_have_room_and_nowhere_else() {
        let mut to = [0_u8; 8];
        move_bytes(&mut to, 2, &[1, 2, 3, 4], 1, 3);
        assert_eq!(to, [0, 0, 2, 3, 4, 0, 0, 0]);
        move_bytes(&mut to, 6, &[9, 9, 9], 0, 3);
        assert_eq!(to, [0, 0, 2, 3, 4, 0, 0, 0]);
    }
}
