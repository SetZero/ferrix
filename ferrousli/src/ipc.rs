//! `sys/ipc.h`, `sys/shm.h`, `sys/sem.h` and `sys/msg.h`: System V shared
//! memory, semaphores and message queues.
//!
//! Each function is its system call, as in musl. On x86-64 and AArch64 C's
//! structures are the kernel's, so commands and structures pass through
//! unchanged. On ARMv7-A the kernel wants `IPC_64` in every command, or it
//! uses its oldest structures, and its structures split each time into two
//! 32-bit halves. C's structures there are the kernel's with a 64-bit
//! `time_t` for each time after them, and the headers mark the commands that
//! write one, such as `IPC_STAT`, with [`IPC_TIME64`]; after such a command
//! the halves are joined into the `time_t`s, as musl does. musl's conversion
//! for big-endian permission modes does not apply.
//!
//! `semctl` is variadic in C. Its fourth argument, a `union semun` of an `int`
//! and two pointers, travels in the register a fixed `unsigned long` would, so
//! it is defined with one; [`crate::fcntl`] says why that is sound. It is read
//! only for the commands that take it, as musl does.

use core::ffi::{c_int, c_long, c_ulong, c_void};

use crate::errno;
use crate::syscall::{self, nr};

/// `IPC_SET`, from `include/sys/ipc.h`.
const IPC_SET: c_int = 1;
/// `IPC_STAT`, from `include/bits/ipcstat.h`.
const IPC_STAT: c_int = 2;
/// `IPC_INFO`, from `include/sys/ipc.h`.
const IPC_INFO: c_int = 3;
/// `GETALL`, from `include/sys/sem.h`.
const GETALL: c_int = 13;
/// `SETVAL`, from `include/sys/sem.h`.
const SETVAL: c_int = 16;
/// `SETALL`, from `include/sys/sem.h`.
const SETALL: c_int = 17;
/// `SEM_STAT`, from `include/sys/sem.h`.
const SEM_STAT: c_int = 18;
/// `SEM_INFO`, from `include/sys/sem.h`.
const SEM_INFO: c_int = 19;
/// `SEM_STAT_ANY`, from `include/sys/sem.h`.
const SEM_STAT_ANY: c_int = 20;

/// The flag that asks the kernel for its current structures, which only
/// ARMv7-A's kernel needs; the others ignore it or have no other kind.
const IPC_64: c_int = if cfg!(target_arch = "arm") { 0x100 } else { 0 };

/// The flag ARMv7-A's headers set in `IPC_STAT` and each `*_STAT` command,
/// which write a structure with 64-bit times after the kernel's, and the
/// kernel never sees: `IPC_STAT & 0x100` in `include/bits/ipcstat.h`.
const IPC_TIME64: c_int = if cfg!(target_arch = "arm") { 0x100 } else { 0 };

/// The command the kernel is given for C's `cmd`.
fn kernel_command(cmd: c_int) -> usize {
    ((cmd & !IPC_TIME64) | IPC_64) as usize
}

/// After a command marked with [`IPC_TIME64`] succeeded, joins each time's
/// halves, the 32-bit words at the first offset of a pair, into the `time_t`
/// at the second, in the structure at `buf`.
///
/// # Safety
///
/// `buf` must be the structure the command wrote, which holds every offset.
#[cfg_attr(
    not(target_arch = "arm"),
    expect(unused_variables, reason = "only ARMv7-A splits the times")
)]
unsafe fn join_times(cmd: c_int, ret: isize, buf: *mut c_void, times: &[(usize, usize)]) {
    #[cfg(target_arch = "arm")]
    if ret >= 0 && cmd & IPC_TIME64 != 0 && !buf.is_null() {
        let base = buf.cast::<u8>();
        for &(halves, time) in times {
            // SAFETY: the caller vouches that the offsets lie inside the
            // structure, where the kernel just wrote the halves.
            let halves = unsafe {
                base.wrapping_add(halves)
                    .cast::<[u32; 2]>()
                    .read_unaligned()
            };
            let [low, high] = halves.map(u64::from);
            // SAFETY: as above, for the `time_t`.
            unsafe {
                base.wrapping_add(time)
                    .cast::<i64>()
                    .write_unaligned((high << 32 | low).cast_signed());
            }
        }
    }
}

/// Where the halves of `shm_atime`, `shm_dtime` and `shm_ctime` are in C's
/// `struct shmid_ds` on ARMv7-A, and where each `time_t` is.
const SHM_TIMES: [(usize, usize); 3] = [(40, 88), (48, 96), (56, 104)];
/// The same for `sem_otime` and `sem_ctime` in `struct semid_ds`.
const SEM_TIMES: [(usize, usize); 2] = [(36, 64), (44, 72)];
/// The same for `msg_stime`, `msg_rtime` and `msg_ctime` in `struct
/// msqid_ds`.
const MSG_TIMES: [(usize, usize); 3] = [(36, 88), (44, 96), (52, 104)];

/// Makes the IPC system call `number` with up to five arguments.
fn call(number: usize, args: [usize; 5]) -> isize {
    let [a0, a1, a2, a3, a4] = args;
    // SAFETY: each caller vouches for the memory its arguments point to.
    let ret = unsafe { syscall::syscall6(number, a0, a1, a2, a3, a4, 0) };
    errno::from_syscall(ret)
}

/// [`call`], as a cancellation point.
fn call_cp(number: usize, args: [usize; 5]) -> isize {
    let [a0, a1, a2, a3, a4] = args;
    // SAFETY: each caller vouches for the memory its arguments point to.
    let ret = unsafe { crate::cancel::syscall_cp(number, a0, a1, a2, a3, a4, 0) };
    errno::from_syscall(ret)
}

/// The size `shmget` asks the kernel for. Like musl, a size too large for
/// `ptrdiff_t` becomes the largest there is, which the kernel refuses rather
/// than reading as a small one.
fn segment_size(size: usize) -> usize {
    if size > isize::MAX as usize {
        usize::MAX
    } else {
        size
    }
}

/// A System V key made from the file `path` and the low byte of `id`, as
/// musl's `ftok` (MIT; see [`crate::math`] for the notice) and glibc's
/// make it: the low 16 bits of the inode, the low 8 of the device, then
/// `id`'s. -1 if `path` cannot be read.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ftok(path: *const core::ffi::c_char, id: c_int) -> c_int {
    let mut st = crate::stat::Stat::default();
    // SAFETY: the caller passes a C string, and `st` is a live local.
    if unsafe { crate::stat::stat(path, &raw mut st) } < 0 {
        return -1;
    }
    let key = (st.st_ino as u32 & 0xffff)
        | ((st.st_dev as u32 & 0xff) << 16)
        | ((id as u32 & 0xff) << 24);
    key as c_int
}

/// Returns the id of the shared memory segment with `key`, creating one of
/// `size` bytes if `flag` says so.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn shmget(key: c_int, size: usize, flag: c_int) -> c_int {
    call(
        nr::SHMGET,
        [key as usize, segment_size(size), flag as usize, 0, 0],
    ) as c_int
}

/// Attaches segment `id` at `addr`, or where the kernel chooses if it is null,
/// and returns the address, or `(void *)-1` with `errno` set.
///
/// # Safety
///
/// Attaching at a given `addr` replaces nothing only if the caller owns that
/// range, as with `mmap`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn shmat(id: c_int, addr: *const c_void, flag: c_int) -> *mut c_void {
    let ret = call(nr::SHMAT, [id as usize, addr.addr(), flag as usize, 0, 0]);
    core::ptr::with_exposed_provenance_mut(ret as usize)
}

/// Detaches the segment attached at `addr`.
///
/// # Safety
///
/// Nothing may use the segment's memory afterwards.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn shmdt(addr: *const c_void) -> c_int {
    call(nr::SHMDT, [addr.addr(), 0, 0, 0, 0]) as c_int
}

/// Performs command `cmd` on segment `id`, reading or writing `*buf` as the
/// command says.
///
/// # Safety
///
/// `buf` must be valid for what `cmd` reads or writes: a `struct shmid_ds`,
/// `struct shminfo` or `struct shm_info`, or nothing.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn shmctl(id: c_int, cmd: c_int, buf: *mut c_void) -> c_int {
    let ret = call(
        nr::SHMCTL,
        [id as usize, kernel_command(cmd), buf.addr(), 0, 0],
    );
    // SAFETY: a command that writes times wrote a `struct shmid_ds`.
    unsafe { join_times(cmd, ret, buf, &SHM_TIMES) };
    ret as c_int
}

/// Returns the id of the semaphore set with `key`, creating one of `count`
/// semaphores if `flag` says so. A count above `USHRT_MAX` fails with
/// `EINVAL`: the kernel keeps it in a wider field than C's `sem_nsems`, and
/// might not refuse it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn semget(key: c_int, count: c_int, flag: c_int) -> c_int {
    if count > c_int::from(u16::MAX) {
        errno::set(errno::EINVAL);
        return -1;
    }
    call(
        nr::SEMGET,
        [key as usize, count as usize, flag as usize, 0, 0],
    ) as c_int
}

/// Performs the `count` operations at `ops` on semaphore set `id`, atomically.
///
/// # Safety
///
/// `ops` must be valid for reads of `count` `struct sembuf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn semop(id: c_int, ops: *mut c_void, count: usize) -> c_int {
    call(nr::SEMOP, [id as usize, ops.addr(), count, 0, 0]) as c_int
}

/// `semop`, waiting no longer than `*timeout`, or without limit if it is
/// null.
///
/// # Safety
///
/// As `semop`, and `timeout` must be null or valid for a read of a `struct
/// timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn semtimedop(
    id: c_int,
    ops: *mut c_void,
    count: usize,
    timeout: *const c_void,
) -> c_int {
    call(
        nr::SEMTIMEDOP,
        [id as usize, ops.addr(), count, timeout.addr(), 0],
    ) as c_int
}

/// Performs command `cmd` on semaphore `num` of set `id`, with `arg` for the
/// commands that take one.
///
/// # Safety
///
/// For a command that takes a pointer in `arg`, it must be valid for what the
/// command reads or writes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn semctl(id: c_int, num: c_int, cmd: c_int, arg: c_ulong) -> c_int {
    let arg = match cmd & !IPC_TIME64 {
        SETVAL | GETALL | SETALL | IPC_SET | IPC_INFO | SEM_INFO | IPC_STAT | SEM_STAT
        | SEM_STAT_ANY => arg as usize,
        _ => 0,
    };
    let ret = call(
        nr::SEMCTL,
        [id as usize, num as usize, kernel_command(cmd), arg, 0],
    );
    // SAFETY: a command that writes times wrote a `struct semid_ds` at the
    // pointer in `arg`.
    unsafe {
        join_times(
            cmd,
            ret,
            core::ptr::with_exposed_provenance_mut(arg),
            &SEM_TIMES,
        )
    };
    ret as c_int
}

/// Returns the id of the message queue with `key`, creating it if `flag` says
/// so.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn msgget(key: c_int, flag: c_int) -> c_int {
    call(nr::MSGGET, [key as usize, flag as usize, 0, 0, 0]) as c_int
}

/// Sends the message at `msg`, a `long` type and `size` bytes of text, on
/// queue `id`.
///
/// # Safety
///
/// `msg` must be valid for reads of a `long` and `size` bytes after it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn msgsnd(id: c_int, msg: *const c_void, size: usize, flag: c_int) -> c_int {
    call_cp(
        nr::MSGSND,
        [id as usize, msg.addr(), size, flag as usize, 0],
    ) as c_int
}

/// Receives a message of type `r#type`, as `msgrcv` selects by it, from queue
/// `id` into `msg`, and returns the length of its text.
///
/// # Safety
///
/// `msg` must be valid for writes of a `long` and `size` bytes after it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn msgrcv(
    id: c_int,
    msg: *mut c_void,
    size: usize,
    r#type: c_long,
    flag: c_int,
) -> isize {
    call_cp(
        nr::MSGRCV,
        [
            id as usize,
            msg.addr(),
            size,
            r#type as usize,
            flag as usize,
        ],
    )
}

/// Performs command `cmd` on queue `id`, reading or writing `*buf` as the
/// command says.
///
/// # Safety
///
/// `buf` must be valid for what `cmd` reads or writes: a `struct msqid_ds` or
/// `struct msginfo`, or nothing.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn msgctl(id: c_int, cmd: c_int, buf: *mut c_void) -> c_int {
    let ret = call(
        nr::MSGCTL,
        [id as usize, kernel_command(cmd), buf.addr(), 0, 0],
    );
    // SAFETY: a command that writes times wrote a `struct msqid_ds`.
    unsafe { join_times(cmd, ret, buf, &MSG_TIMES) };
    ret as c_int
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_segment_size_beyond_ptrdiff_t_becomes_the_largest() {
        assert_eq!(segment_size(8192), 8192);
        assert_eq!(segment_size(isize::MAX as usize), isize::MAX as usize);
        assert_eq!(segment_size(isize::MAX as usize + 1), usize::MAX);
    }

    #[test]
    fn semget_refuses_a_count_c_cannot_hold() {
        // IPC_PRIVATE and IPC_CREAT | 0600, from include/sys/ipc.h.
        assert_eq!(semget(0, 70_000, 0o1600), -1);
        // SAFETY: the pointer is this thread's errno.
        assert_eq!(unsafe { errno::__errno_location().read() }, errno::EINVAL);
    }
}
