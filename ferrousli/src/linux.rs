//! Linux's own calls, from headers POSIX does not have: `sys/prctl.h`,
//! `capget` and `capset`, `sys/personality.h`, `sys/reboot.h`, `sys/klog.h`,
//! `sys/inotify.h`, `sys/sendfile.h`, `sys/sysinfo.h`, and `sys/file.h`'s
//! `flock`.
//!
//! Each is its system call, as in musl. `prctl` is variadic in C. Like
//! `fcntl`, it is defined with fixed parameters instead ([`crate::fcntl`] says
//! why that is sound), and it passes all four to the kernel whether the caller
//! gave them or not, as musl's does; the kernel reads only those the operation
//! takes. musl's `inotify_init1` falls back to `inotify_init` on kernels
//! without it, which every x86-64 kernel has, so there is no fallback here.

use core::ffi::{c_char, c_int, c_ulong, c_void};

use crate::errno;
use crate::syscall::{self, nr};

/// `LINUX_REBOOT_MAGIC1` and `LINUX_REBOOT_MAGIC2`, from `linux/reboot.h`.
const REBOOT_MAGIC: [usize; 2] = [0xfee1_dead, 672_274_793];

/// Makes system call `number` with up to five arguments.
fn call(number: usize, args: [usize; 5]) -> isize {
    let [a0, a1, a2, a3, a4] = args;
    // SAFETY: each caller vouches for the memory its arguments point to.
    let ret = unsafe { syscall::syscall6(number, a0, a1, a2, a3, a4, 0) };
    errno::from_syscall(ret)
}

/// Performs process operation `op`, with the arguments it takes.
///
/// # Safety
///
/// Any argument `op` reads as a pointer must be valid for what it reads or
/// writes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn prctl(op: c_int, a: c_ulong, b: c_ulong, c: c_ulong, d: c_ulong) -> c_int {
    call(
        nr::PRCTL,
        [op as usize, a as usize, b as usize, c as usize, d as usize],
    ) as c_int
}

/// Reads a thread's capabilities into `data`, as the version in `*header`
/// lays them out. With an unknown version, stores the kernel's in the header.
///
/// # Safety
///
/// `header` must be valid for a read and a write of a `struct
/// __user_cap_header_struct`, and `data` null or valid for writes of the
/// structures its version has.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn capget(header: *mut c_void, data: *mut c_void) -> c_int {
    call(nr::CAPGET, [header.addr(), data.addr(), 0, 0, 0]) as c_int
}

/// Sets a thread's capabilities from `data`, laid out as `*header` says.
///
/// # Safety
///
/// `header` must be valid for a read and a write of a `struct
/// __user_cap_header_struct`, and `data` for reads of the structures its
/// version has.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn capset(header: *mut c_void, data: *const c_void) -> c_int {
    call(nr::CAPSET, [header.addr(), data.addr(), 0, 0, 0]) as c_int
}

/// Sets the process's execution domain to `persona` and returns the one it
/// had. `0xffffffff` changes nothing.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn personality(persona: c_ulong) -> c_int {
    call(nr::PERSONALITY, [persona as usize, 0, 0, 0, 0]) as c_int
}

/// Reboots, halts or powers off the machine, or changes what Ctrl-Alt-Del
/// does, as `command` says.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn reboot(command: c_int) -> c_int {
    let [magic1, magic2] = REBOOT_MAGIC;
    call(nr::REBOOT, [magic1, magic2, command as usize, 0, 0]) as c_int
}

/// Performs kernel log operation `r#type` on the `len` bytes at `buf`.
///
/// # Safety
///
/// `buf` must be valid for what the operation reads or writes of its `len`
/// bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn klogctl(r#type: c_int, buf: *mut c_char, len: c_int) -> c_int {
    call(
        nr::SYSLOG,
        [r#type as usize, buf.addr(), len as usize, 0, 0],
    ) as c_int
}

/// Creates an inotify instance, returning its descriptor.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn inotify_init() -> c_int {
    inotify_init1(0)
}

/// Creates an inotify instance with `IN_CLOEXEC` and `IN_NONBLOCK` in
/// `flags`, returning its descriptor.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn inotify_init1(flags: c_int) -> c_int {
    call(nr::INOTIFY_INIT1, [flags as usize, 0, 0, 0, 0]) as c_int
}

/// Watches `path` for the events in `mask` on inotify instance `fd`, and
/// returns the watch's descriptor.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn inotify_add_watch(fd: c_int, path: *const c_char, mask: u32) -> c_int {
    call(
        nr::INOTIFY_ADD_WATCH,
        [fd as usize, path.addr(), mask as usize, 0, 0],
    ) as c_int
}

/// Removes watch `wd` from inotify instance `fd`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn inotify_rm_watch(fd: c_int, wd: c_int) -> c_int {
    call(nr::INOTIFY_RM_WATCH, [fd as usize, wd as usize, 0, 0, 0]) as c_int
}

/// Copies up to `count` bytes from `in_fd` to `out_fd` in the kernel, from
/// `*offset` if it is not null, updating it, and returns how many were copied.
///
/// # Safety
///
/// `offset` must be null or valid for a read and a write of an `off_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sendfile(
    out_fd: c_int,
    in_fd: c_int,
    offset: *mut i64,
    count: usize,
) -> isize {
    call(
        nr::SENDFILE,
        [out_fd as usize, in_fd as usize, offset.addr(), count, 0],
    )
}

/// `sendfile`, under glibc's large-file name.
///
/// # Safety
///
/// As `sendfile`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sendfile64(
    out_fd: c_int,
    in_fd: c_int,
    offset: *mut i64,
    count: usize,
) -> isize {
    // SAFETY: the caller's promises are `sendfile`'s.
    unsafe { sendfile(out_fd, in_fd, offset, count) }
}

/// Stores the system's uptime, load, memory and process count in `*info`.
///
/// # Safety
///
/// `info` must be valid for a write of a `struct sysinfo`, whose layout is the
/// kernel's.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sysinfo(info: *mut c_void) -> c_int {
    call(nr::SYSINFO, [info.addr(), 0, 0, 0, 0]) as c_int
}

/// Takes or releases an advisory lock on the open file `fd`, as `op` says.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn flock(fd: c_int, op: c_int) -> c_int {
    call(nr::FLOCK, [fd as usize, op as usize, 0, 0, 0]) as c_int
}

#[cfg(test)]
mod tests {
    use super::*;

    fn last_errno() -> c_int {
        // SAFETY: the pointer is this thread's errno.
        unsafe { errno::__errno_location().read() }
    }

    #[test]
    fn each_call_reaches_the_kernel_with_its_arguments_in_place() {
        assert!(personality(0xffff_ffff) >= 0);
        // PR_GET_DUMPABLE, from linux/prctl.h: 0, 1 or 2.
        // SAFETY: the operation reads no pointer.
        assert!((0..=2).contains(&unsafe { prctl(3, 0, 0, 0, 0) }));
        assert_eq!(flock(-1, 2), -1);
        assert_eq!(last_errno(), errno::EBADF);
        assert_eq!(inotify_rm_watch(-1, 1), -1);
        assert_eq!(last_errno(), errno::EBADF);
        // An unknown command is refused whether or not the caller may reboot.
        assert_eq!(reboot(0x1234_5678), -1);
        assert!([errno::EPERM, errno::EINVAL].contains(&last_errno()));
        let fd = inotify_init();
        assert!(fd >= 0);
        // SAFETY: `close` reads no memory.
        let _ = unsafe { syscall::syscall2(nr::CLOSE, fd as usize, 0) };
    }
}
