//! Running other programs: `unistd.h`'s `execl`, `execle` and `execlp`,
//! `stdlib.h`'s `system`, `stdio.h`'s `popen` and `pclose`, and `daemon`.
//!
//! musl starts `system`'s and `popen`'s shell with `posix_spawn`, which this
//! library does not have yet. Here they `fork`, and the child does only what is
//! safe between `fork` and `execve` in a program that may have threads: system
//! calls, and reading memory the parent prepared before forking. The effects
//! are musl's. `system` ignores `SIGINT` and `SIGQUIT` and blocks `SIGCHLD`
//! while it waits, and its shell starts with the caller's mask and those two
//! signals at their defaults unless the caller ignored them. A `popen` child
//! closes every stream earlier `popen` calls left open, as POSIX requires:
//! `popen` forks while holding the stream list's lock, so the child's copy of
//! the list is whole. The shell is `/bin/sh`, and a child that cannot run it
//! exits with status 127.
//!
//! The `execl` functions copy their arguments into an array from `malloc` and
//! call `execv`, `execve` or `execvp`, and free the array if that returns.

use core::ffi::{c_char, c_int, c_ulong};
use core::mem::size_of;
use core::ptr::{null, null_mut};
use core::sync::atomic::Ordering;

use crate::errno;
use crate::fcntl::{fcntl, open};
use crate::malloc::{free, malloc};
use crate::process::{execv, execve, execvp, fork, setsid};
use crate::sigaction::{SIG_IGN, Sigaction, sigaction};
use crate::sigset::{SIG_BLOCK, SIG_SETMASK, SigSet, sigaddset, sigprocmask};
use crate::stdio::file::{self, File};
use crate::stdio::open::{fclose, fdopen};
use crate::stdlib::environ;
use crate::string::strlen;
use crate::unistd::{_exit, chdir, close, dup2, pipe2};
use crate::va::{self, VaList, VaListTag};
use crate::wait::waitpid;

/// `SIGINT`, from `asm/signal.h`.
const SIGINT: c_int = 2;
/// `SIGQUIT`, from `asm/signal.h`.
const SIGQUIT: c_int = 3;
/// `SIGCHLD`, from `asm/signal.h`.
const SIGCHLD: c_int = 17;
/// `O_RDWR`, from `asm-generic/fcntl.h`.
const O_RDWR: c_int = 0o2;
/// `O_CLOEXEC`, from `asm-generic/fcntl.h`.
const O_CLOEXEC: c_int = 0o2_000_000;
/// `F_SETFD`, from `asm-generic/fcntl.h`.
const F_SETFD: c_int = 2;
/// The status of a child that could not run the shell.
const NOT_RUN: c_int = 127;

/// The calling thread's `errno`.
fn last_errno() -> c_int {
    // SAFETY: the pointer is this thread's errno.
    unsafe { errno::__errno_location().read() }
}

/// Waits for child `pid` through interruptions, and returns its status, or -1
/// with `errno` set.
fn wait_for(pid: c_int) -> c_int {
    let mut status = 0;
    loop {
        // SAFETY: `status` is a live local.
        if unsafe { waitpid(pid, &raw mut status, 0) } >= 0 {
            return status;
        }
        if last_errno() != errno::EINTR {
            return -1;
        }
    }
}

/// Copies `argv0` and the arguments after it, up to the null pointer that ends
/// them, into a null-terminated array from `malloc`, and leaves `args` just
/// past that null. Null, with `errno` set, if there is no memory.
///
/// # Safety
///
/// `args` must hold pointers ending in a null pointer.
unsafe fn collect(argv0: *const c_char, args: &mut VaList<'_>) -> *mut *const c_char {
    let mut tag = args.copy();
    let mut counter = VaList::from_tag(&mut tag);
    let mut count = 1_usize;
    // SAFETY: the caller's list ends in a null pointer.
    while !unsafe { counter.next_ptr::<c_char>() }.is_null() {
        count += 1;
    }
    let Some(bytes) = count
        .checked_add(1)
        .and_then(|slots| slots.checked_mul(size_of::<*const c_char>()))
    else {
        errno::set(errno::ENOMEM);
        return null_mut();
    };
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "malloc aligns to 16, more than a pointer needs"
    )]
    let argv = malloc(bytes).cast::<*const c_char>();
    if argv.is_null() {
        return argv;
    }
    // SAFETY: the array holds `count + 1` pointers.
    unsafe { argv.write(argv0) };
    for index in 1..=count {
        // SAFETY: the list holds `count - 1` more strings, then the null,
        // which lands in the last slot.
        let arg = unsafe { args.next_ptr::<c_char>() };
        // SAFETY: `index` is at most `count`.
        unsafe { argv.wrapping_add(index).write(arg.cast_const()) };
    }
    argv
}

/// `execl`'s body: the arguments after `argv0` are the list, ending in a null
/// pointer.
///
/// # Safety
///
/// `path` must be a NUL-terminated string, and `ap` a list of NUL-terminated
/// strings ending in a null pointer.
pub unsafe extern "C" fn execl_list(
    path: *const c_char,
    argv0: *const c_char,
    ap: *mut VaListTag,
) -> c_int {
    // SAFETY: the thunk passes its own list.
    let mut args = unsafe { VaList::from_raw(ap) };
    // SAFETY: the caller ends the list with a null pointer.
    let argv = unsafe { collect(argv0, &mut args) };
    if argv.is_null() {
        return -1;
    }
    // SAFETY: `argv` is a null-terminated array of the caller's strings.
    let _ = unsafe { execv(path, argv.cast_const()) };
    // SAFETY: the array came from `malloc`.
    unsafe { free(argv.cast()) };
    -1
}

/// `execle`'s body: the list ends in a null pointer, followed by the
/// environment.
///
/// # Safety
///
/// As `execl_list`, with a null-terminated array of NUL-terminated strings
/// after the list's null.
pub unsafe extern "C" fn execle_list(
    path: *const c_char,
    argv0: *const c_char,
    ap: *mut VaListTag,
) -> c_int {
    // SAFETY: the thunk passes its own list.
    let mut args = unsafe { VaList::from_raw(ap) };
    // SAFETY: the caller ends the list with a null pointer.
    let argv = unsafe { collect(argv0, &mut args) };
    if argv.is_null() {
        return -1;
    }
    // SAFETY: the environment follows the list's null.
    let envp = unsafe { args.next_ptr::<*const c_char>() };
    // SAFETY: `argv` and `envp` are null-terminated arrays of strings.
    let _ = unsafe { execve(path, argv.cast_const(), envp.cast_const()) };
    // SAFETY: the array came from `malloc`.
    unsafe { free(argv.cast()) };
    -1
}

/// `execlp`'s body: `execvp` with the list.
///
/// # Safety
///
/// As `execl_list`, with `file` for `path`.
pub unsafe extern "C" fn execlp_list(
    file: *const c_char,
    argv0: *const c_char,
    ap: *mut VaListTag,
) -> c_int {
    // SAFETY: the thunk passes its own list.
    let mut args = unsafe { VaList::from_raw(ap) };
    // SAFETY: the caller ends the list with a null pointer.
    let argv = unsafe { collect(argv0, &mut args) };
    if argv.is_null() {
        return -1;
    }
    // SAFETY: `argv` is a null-terminated array of the caller's strings.
    let _ = unsafe { execvp(file, argv.cast_const()) };
    // SAFETY: the array came from `malloc`.
    unsafe { free(argv.cast()) };
    -1
}

va::variadic!(execl, 2, execl_list);
va::variadic!(execle, 2, execle_list);
va::variadic!(execlp, 2, execlp_list);

/// Runs `/bin/sh -c command` with the environment and waits for it, in the
/// child that `fork` just returned 0 in. Never returns.
///
/// # Safety
///
/// `command` must be a NUL-terminated string.
unsafe fn run_shell(command: *const c_char) -> ! {
    let argv: [*const c_char; 4] = [c"sh".as_ptr(), c"-c".as_ptr(), command, null()];
    let environment = environ.load(Ordering::Relaxed);
    // SAFETY: `argv` is a null-terminated array of strings, and the environment
    // the program's.
    let _ = unsafe {
        execve(
            c"/bin/sh".as_ptr(),
            argv.as_ptr(),
            environment.cast_const().cast(),
        )
    };
    _exit(NOT_RUN)
}

/// Runs `command` with `/bin/sh -c`, waits for it, and returns its wait
/// status, or -1 with `errno` set if it could not be started or waited for.
/// With a null `command`, returns 1: there is a shell.
///
/// # Safety
///
/// `command` must be null or a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn system(command: *const c_char) -> c_int {
    if command.is_null() {
        return 1;
    }
    let ignore = Sigaction {
        handler: SIG_IGN,
        ..Sigaction::DEFAULT
    };
    let default = Sigaction::DEFAULT;
    let mut old_int = Sigaction::DEFAULT;
    let mut old_quit = Sigaction::DEFAULT;
    // SAFETY: the actions are live locals.
    let _ = unsafe { sigaction(SIGINT, &raw const ignore, &raw mut old_int) };
    // SAFETY: as above.
    let _ = unsafe { sigaction(SIGQUIT, &raw const ignore, &raw mut old_quit) };
    let mut block = SigSet::EMPTY;
    // SAFETY: `block` is a live local.
    let _ = unsafe { sigaddset(&raw mut block, SIGCHLD) };
    let mut old_mask = SigSet::EMPTY;
    // SAFETY: the sets are live locals.
    let _ = unsafe { sigprocmask(SIG_BLOCK, &raw const block, &raw mut old_mask) };

    let pid = fork();
    if pid == 0 {
        if old_int.handler != SIG_IGN {
            // SAFETY: `default` is a live local.
            let _ = unsafe { sigaction(SIGINT, &raw const default, null_mut()) };
        }
        if old_quit.handler != SIG_IGN {
            // SAFETY: as above.
            let _ = unsafe { sigaction(SIGQUIT, &raw const default, null_mut()) };
        }
        // SAFETY: `old_mask` is a live local.
        let _ = unsafe { sigprocmask(SIG_SETMASK, &raw const old_mask, null_mut()) };
        // SAFETY: the caller passes a NUL-terminated command.
        unsafe { run_shell(command) }
    }
    let spawn_error = if pid < 0 { last_errno() } else { 0 };
    let status = if pid > 0 { wait_for(pid) } else { -1 };
    let wait_error = last_errno();

    // SAFETY: the old actions and mask are live locals.
    let _ = unsafe { sigaction(SIGINT, &raw const old_int, null_mut()) };
    // SAFETY: as above.
    let _ = unsafe { sigaction(SIGQUIT, &raw const old_quit, null_mut()) };
    // SAFETY: as above.
    let _ = unsafe { sigprocmask(SIG_SETMASK, &raw const old_mask, null_mut()) };
    if pid < 0 {
        errno::set(spawn_error);
    } else if status == -1 {
        errno::set(wait_error);
    }
    status
}

/// Starts `/bin/sh -c command` with a pipe to its standard output, for mode
/// `r`, or from its standard input, for mode `w`, and returns a stream on the
/// pipe's other end. With `e` in the mode, that end closes on `exec`. Null,
/// with `errno` set, if the mode is neither or the shell could not be started.
///
/// # Safety
///
/// `command` and `mode` must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn popen(command: *const c_char, mode: *const c_char) -> *mut File {
    // SAFETY: the caller passes a NUL-terminated mode.
    let reading = match unsafe { mode.read() } as u8 {
        b'r' => true,
        b'w' => false,
        _ => {
            errno::set(errno::EINVAL);
            return null_mut();
        }
    };
    // SAFETY: as above.
    let mode_len = unsafe { strlen(mode) };
    let close_on_exec = (0..mode_len).any(|offset| {
        // SAFETY: the offset is inside the mode.
        (unsafe { mode.wrapping_add(offset).read() }) as u8 == b'e'
    });
    let mut fds: [c_int; 2] = [-1; 2];
    // SAFETY: `fds` holds two ints.
    if unsafe { pipe2(fds.as_mut_ptr(), O_CLOEXEC) } != 0 {
        return null_mut();
    }
    let [read_end, write_end] = fds;
    // The child writes its standard output into the pipe, or reads its
    // standard input from it.
    let (ours, theirs, target) = if reading {
        (read_end, write_end, 1)
    } else {
        (write_end, read_end, 0)
    };
    // SAFETY: the caller passes a NUL-terminated mode.
    let stream = unsafe { fdopen(ours, mode) };
    if stream.is_null() {
        let error = last_errno();
        let _ = close(ours);
        let _ = close(theirs);
        errno::set(error);
        return null_mut();
    }

    file::lock_list();
    let pid = fork();
    if pid == 0 {
        // SAFETY: this is the child of a fork made while the list's lock was
        // held, and it has run nothing else.
        unsafe { file::close_popen_descriptors() };
        if theirs == target {
            // SAFETY: `fcntl` reads no memory for `F_SETFD`.
            let _ = unsafe { fcntl(theirs, F_SETFD, 0) };
        } else if dup2(theirs, target) < 0 {
            _exit(NOT_RUN);
        }
        // SAFETY: the caller passes a NUL-terminated command.
        unsafe { run_shell(command) }
    }
    if pid > 0 {
        // SAFETY: `stream` is the live stream just made.
        unsafe { file::locked(stream, |inner| inner.pipe_pid = pid) };
    }
    file::unlock_list();

    let _ = close(theirs);
    if pid < 0 {
        let error = last_errno();
        // SAFETY: `stream` is live, and not used again.
        let _ = unsafe { fclose(stream) };
        errno::set(error);
        return null_mut();
    }
    if !close_on_exec {
        // SAFETY: `fcntl` reads no memory for `F_SETFD`.
        let _ = unsafe { fcntl(ours, F_SETFD, 0 as c_ulong) };
    }
    stream
}

/// Closes a stream `popen` returned, waits for its shell, and returns the
/// shell's wait status, or -1 with `errno` set. A stream `popen` did not
/// return fails with `ECHILD`, after it is closed.
///
/// # Safety
///
/// `stream` must be a live stream, not used again.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pclose(stream: *mut File) -> c_int {
    // SAFETY: the caller passes a live stream.
    let pid = unsafe { file::locked(stream, |inner| inner.pipe_pid) };
    // SAFETY: as above; the caller does not use it again.
    let _ = unsafe { fclose(stream) };
    if pid <= 0 {
        errno::set(errno::ECHILD);
        return -1;
    }
    wait_for(pid)
}

/// Detaches the process from its terminal and session, as a daemon: it
/// changes to `/` unless `nochdir`, points its standard streams at
/// `/dev/null` unless `noclose`, then forks, starts a new session, and forks
/// again, the parents exiting. Returns 0 in the final child, or -1 with
/// `errno` set.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn daemon(nochdir: c_int, noclose: c_int) -> c_int {
    // SAFETY: the path is NUL-terminated.
    if nochdir == 0 && unsafe { chdir(c"/".as_ptr()) } != 0 {
        return -1;
    }
    if noclose == 0 {
        // SAFETY: the path is NUL-terminated.
        let fd = unsafe { open(c"/dev/null".as_ptr(), O_RDWR, 0) };
        if fd < 0 {
            return -1;
        }
        let failed = dup2(fd, 0) < 0 || dup2(fd, 1) < 0 || dup2(fd, 2) < 0;
        if fd > 2 {
            let _ = close(fd);
        }
        if failed {
            return -1;
        }
    }
    match fork() {
        0 => {}
        -1 => return -1,
        _ => _exit(0),
    }
    if setsid() < 0 {
        return -1;
    }
    match fork() {
        0 => 0,
        -1 => -1,
        _ => _exit(0),
    }
}
