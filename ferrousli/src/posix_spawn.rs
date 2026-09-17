//! `spawn.h`: starting a program in one call.
//!
//! POSIX's answer to `fork` in a program with threads. `fork` copies one
//! thread into a new process holding every lock the others were holding, so
//! between the fork and the `exec` only system calls are safe. `posix_spawn`
//! takes what the caller wanted done in between — descriptors moved, a process
//! group set, signals reset — as data, and does it where it is safe to.
//!
//! # How it is done here
//!
//! `fork`, then the actions, then `exec`, with a close-on-exec pipe carrying
//! the failure back. The child writes the error number into the pipe and exits
//! 127 if anything up to and including the `exec` fails; the parent reads it
//! and returns it, so a program that could not be started is reported by
//! `posix_spawn` itself rather than as a child that died. When the `exec`
//! succeeds the pipe closes with nothing in it, which is how the parent knows.
//!
//! musl uses `clone` with `CLONE_VM | CLONE_VFORK` and a stack of its own,
//! which avoids copying the page tables and cannot fail for want of memory.
//! That is worth having and is not what this does yet: `docs/BACKLOG.md` has
//! the row. What this does is what POSIX describes, and the observable
//! behaviour is the same.
//!
//! The `pthread_atfork` handlers do **not** run, as they do not in glibc.
//! A program calls this rather than `fork` precisely to avoid what those
//! handlers work around, and one that took a lock another thread holds would
//! hang the spawn. [`crate::process::fork_bare`] is the fork used here.
//!
//! # The two structures
//!
//! `posix_spawnattr_t` and `posix_spawn_file_actions_t` are opaque: only the
//! functions here read them. Their layout is `include/spawn.h`'s, which is
//! musl's, and musl chose it so that every field glibc also has sits at
//! glibc's offset — `__flags` at 0, the process group at 4, the default set at
//! 8 and the mask at 136. A program built against either header therefore
//! hands this library a structure it reads correctly.

use core::ffi::{c_char, c_int, c_short, c_uint, c_ulong, c_void};
use core::mem::{align_of, offset_of, size_of};
use core::ptr::null_mut;

use crate::errno;
use crate::fcntl::fcntl;
use crate::malloc::{free, malloc};
use crate::process::{execve, execvpe, fork_bare, setpgid, setsid};
use crate::pthread_attr::SchedParam;
use crate::sched::{sched_setparam, sched_setscheduler};
use crate::sigaction::{SIG_DFL, Sigaction, sigaction};
use crate::sigset::{SIG_SETMASK, SigSet, sigismember, sigprocmask};
use crate::string::{memcpy, strlen};
use crate::unistd::{_exit, chdir, close, dup2, fchdir, pipe2, read, write};
use crate::wait::waitpid;

/// `POSIX_SPAWN_RESETIDS`, from `spawn.h`.
const RESETIDS: c_int = 1;
/// `POSIX_SPAWN_SETPGROUP`.
const SETPGROUP: c_int = 2;
/// `POSIX_SPAWN_SETSIGDEF`.
const SETSIGDEF: c_int = 4;
/// `POSIX_SPAWN_SETSIGMASK`.
const SETSIGMASK: c_int = 8;
/// `POSIX_SPAWN_SETSCHEDPARAM`.
const SETSCHEDPARAM: c_int = 16;
/// `POSIX_SPAWN_SETSCHEDULER`.
const SETSCHEDULER: c_int = 32;
/// `POSIX_SPAWN_SETSID`.
const SETSID: c_int = 128;
/// Every flag `spawn.h` defines; any other is rejected.
const KNOWN_FLAGS: c_int =
    RESETIDS | SETPGROUP | SETSIGDEF | SETSIGMASK | SETSCHEDPARAM | SETSCHEDULER | 64 | SETSID;

/// `O_CLOEXEC`, from `asm-generic/fcntl.h`.
const O_CLOEXEC: c_int = 0o2_000_000;
/// `F_DUPFD_CLOEXEC`, from `asm-generic/fcntl.h`.
const F_DUPFD_CLOEXEC: c_int = 1030;
/// The status of a child that could not start the program, as POSIX names it.
const NOT_STARTED: c_int = 127;
/// The highest signal Linux has.
const NSIG: c_int = 64;

/// `posix_spawnattr_t`, as `include/spawn.h` declares it.
#[repr(C)]
#[derive(Debug)]
pub struct SpawnAttr {
    /// `__flags`, the `POSIX_SPAWN_*` set.
    flags: c_int,
    /// `__pgrp`, the process group for `POSIX_SPAWN_SETPGROUP`.
    pgrp: c_int,
    /// `__def`, the signals to reset for `POSIX_SPAWN_SETSIGDEF`.
    def: SigSet,
    /// `__mask`, the mask for `POSIX_SPAWN_SETSIGMASK`.
    mask: SigSet,
    /// `__prio`, the priority for the two scheduling flags.
    prio: c_int,
    /// `__pol`, the policy for `POSIX_SPAWN_SETSCHEDULER`.
    pol: c_int,
    /// `__fn`, which musl reserves and nothing here uses.
    reserved: *mut c_void,
    /// `__pad`.
    pad: [u8; 56],
}

const _: () = assert!(size_of::<SpawnAttr>() == 336);
const _: () = assert!(offset_of!(SpawnAttr, pgrp) == 4);
const _: () = assert!(offset_of!(SpawnAttr, def) == 8);
const _: () = assert!(offset_of!(SpawnAttr, mask) == 136);
const _: () = assert!(offset_of!(SpawnAttr, prio) == 264);
const _: () = assert!(offset_of!(SpawnAttr, pol) == 268);

/// `posix_spawn_file_actions_t`, as `include/spawn.h` declares it. The actions
/// are a list off `actions`, oldest first, and `pad0` and `pad` are untouched.
#[repr(C)]
#[derive(Debug)]
pub struct FileActions {
    /// `__pad0`, which glibc uses for two counts and this does not.
    pad0: [c_int; 2],
    /// `__actions`: the first action added, or null.
    actions: *mut Action,
    /// `__pad`.
    pad: [c_int; 16],
}

const _: () = assert!(size_of::<FileActions>() == 80);
const _: () = assert!(offset_of!(FileActions, actions) == 8);

/// What one entry of a file-action list does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum Kind {
    Open = 1,
    Close = 2,
    Dup2 = 3,
    Chdir = 4,
    Fchdir = 5,
}

/// One file action. Allocated by the `add` functions and freed by
/// `posix_spawn_file_actions_destroy`, which owns `path` too.
#[repr(C)]
#[derive(Debug)]
struct Action {
    /// The action added after this one, or null.
    next: *mut Action,
    /// Which action this is.
    kind: Kind,
    /// The descriptor the action is about: the one opened into, closed,
    /// duplicated to, or changed directory to.
    fd: c_int,
    /// `Dup2`'s source descriptor.
    from: c_int,
    /// `Open`'s flags.
    oflag: c_int,
    /// `Open`'s mode.
    mode: c_uint,
    /// `Open`'s and `Chdir`'s path, a copy this owns, or null.
    path: *mut c_char,
}

// ---------------------------------------------------------------------------
// The attributes
// ---------------------------------------------------------------------------

/// Sets `*attr` to the defaults: no flags, and every other field zero.
///
/// # Safety
///
/// `attr` must be valid for a write of a `posix_spawnattr_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnattr_init(attr: *mut SpawnAttr) -> c_int {
    // SAFETY: the caller vouches for the structure, and every field of it is
    // valid as all-zero bytes.
    unsafe { attr.write_bytes(0, 1) };
    0
}

/// Releases what `*attr` holds, which is nothing: the structure owns no
/// memory. Returns 0.
///
/// # Safety
///
/// None: the pointer is not read.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnattr_destroy(_attr: *mut SpawnAttr) -> c_int {
    0
}

/// Sets the `POSIX_SPAWN_*` flags in `*attr`. Returns `EINVAL` for a bit
/// `spawn.h` does not define.
///
/// # Safety
///
/// `attr` must be valid for a read and a write of a `posix_spawnattr_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnattr_setflags(attr: *mut SpawnAttr, flags: c_short) -> c_int {
    let flags = c_int::from(flags);
    if flags & !KNOWN_FLAGS != 0 {
        return errno::EINVAL;
    }
    // SAFETY: the caller vouches for the structure.
    unsafe { (*attr).flags = flags };
    0
}

/// Stores `*attr`'s flags in `*flags`.
///
/// # Safety
///
/// `attr` must be valid for a read of a `posix_spawnattr_t` and `flags` for a
/// write of a `short`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnattr_getflags(
    attr: *const SpawnAttr,
    flags: *mut c_short,
) -> c_int {
    // SAFETY: the caller vouches for the structure.
    let value = unsafe { (*attr).flags };
    // SAFETY: the caller vouches for the pointer.
    unsafe { flags.write(value as c_short) };
    0
}

/// Sets the process group `POSIX_SPAWN_SETPGROUP` puts the child in.
///
/// # Safety
///
/// `attr` must be valid for a write of a `posix_spawnattr_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnattr_setpgroup(attr: *mut SpawnAttr, pgrp: c_int) -> c_int {
    // SAFETY: the caller vouches for the structure.
    unsafe { (*attr).pgrp = pgrp };
    0
}

/// Stores that process group in `*pgrp`.
///
/// # Safety
///
/// `attr` must be valid for a read of a `posix_spawnattr_t` and `pgrp` for a
/// write of a `pid_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnattr_getpgroup(
    attr: *const SpawnAttr,
    pgrp: *mut c_int,
) -> c_int {
    // SAFETY: the caller vouches for the structure.
    let value = unsafe { (*attr).pgrp };
    // SAFETY: the caller vouches for the pointer.
    unsafe { pgrp.write(value) };
    0
}

/// Sets the signals `POSIX_SPAWN_SETSIGDEF` puts back to their default
/// disposition in the child.
///
/// # Safety
///
/// `attr` must be valid for a write of a `posix_spawnattr_t` and `set` for a
/// read of a `sigset_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnattr_setsigdefault(
    attr: *mut SpawnAttr,
    set: *const SigSet,
) -> c_int {
    // SAFETY: the caller vouches for the structure.
    let slot = unsafe { &raw mut (*attr).def };
    // SAFETY: the caller vouches for the set, and the two do not overlap:
    // `set` is the caller's and `slot` is inside the structure.
    unsafe { slot.copy_from_nonoverlapping(set, 1) };
    0
}

/// Stores that set in `*set`.
///
/// # Safety
///
/// `attr` must be valid for a read of a `posix_spawnattr_t` and `set` for a
/// write of a `sigset_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnattr_getsigdefault(
    attr: *const SpawnAttr,
    set: *mut SigSet,
) -> c_int {
    // SAFETY: the caller vouches for the structure.
    let slot = unsafe { &raw const (*attr).def };
    // SAFETY: the caller vouches for the set, and the two do not overlap.
    unsafe { set.copy_from_nonoverlapping(slot, 1) };
    0
}

/// Sets the mask `POSIX_SPAWN_SETSIGMASK` gives the child.
///
/// # Safety
///
/// As [`posix_spawnattr_setsigdefault`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnattr_setsigmask(
    attr: *mut SpawnAttr,
    set: *const SigSet,
) -> c_int {
    // SAFETY: the caller vouches for the structure.
    let slot = unsafe { &raw mut (*attr).mask };
    // SAFETY: the caller vouches for the set, and the two do not overlap.
    unsafe { slot.copy_from_nonoverlapping(set, 1) };
    0
}

/// Stores that mask in `*set`.
///
/// # Safety
///
/// As [`posix_spawnattr_getsigdefault`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnattr_getsigmask(
    attr: *const SpawnAttr,
    set: *mut SigSet,
) -> c_int {
    // SAFETY: the caller vouches for the structure.
    let slot = unsafe { &raw const (*attr).mask };
    // SAFETY: the caller vouches for the set, and the two do not overlap.
    unsafe { set.copy_from_nonoverlapping(slot, 1) };
    0
}

/// Sets the scheduling parameters the two scheduling flags apply.
///
/// # Safety
///
/// `attr` must be valid for a write of a `posix_spawnattr_t` and `param` for a
/// read of a `struct sched_param`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnattr_setschedparam(
    attr: *mut SpawnAttr,
    param: *const SchedParam,
) -> c_int {
    // SAFETY: the caller vouches for the parameters.
    let priority = unsafe { (*param).sched_priority };
    // SAFETY: the caller vouches for the structure.
    unsafe { (*attr).prio = priority };
    0
}

/// Stores those parameters in `*param`.
///
/// # Safety
///
/// `attr` must be valid for a read of a `posix_spawnattr_t` and `param` for a
/// write of a `struct sched_param`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnattr_getschedparam(
    attr: *const SpawnAttr,
    param: *mut SchedParam,
) -> c_int {
    // SAFETY: the caller vouches for the structure.
    let priority = unsafe { (*attr).prio };
    // SAFETY: the caller vouches for the parameters, and every field of a
    // `sched_param` is valid as all-zero bytes.
    unsafe { param.write(SchedParam::with_priority(priority)) };
    0
}

/// Sets the scheduling policy `POSIX_SPAWN_SETSCHEDULER` applies.
///
/// # Safety
///
/// `attr` must be valid for a write of a `posix_spawnattr_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnattr_setschedpolicy(
    attr: *mut SpawnAttr,
    policy: c_int,
) -> c_int {
    // SAFETY: the caller vouches for the structure.
    unsafe { (*attr).pol = policy };
    0
}

/// Stores that policy in `*policy`.
///
/// # Safety
///
/// `attr` must be valid for a read of a `posix_spawnattr_t` and `policy` for a
/// write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnattr_getschedpolicy(
    attr: *const SpawnAttr,
    policy: *mut c_int,
) -> c_int {
    // SAFETY: the caller vouches for the structure.
    let value = unsafe { (*attr).pol };
    // SAFETY: the caller vouches for the pointer.
    unsafe { policy.write(value) };
    0
}

// ---------------------------------------------------------------------------
// The file actions
// ---------------------------------------------------------------------------

/// Sets `*acts` to an empty list of file actions.
///
/// # Safety
///
/// `acts` must be valid for a write of a `posix_spawn_file_actions_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawn_file_actions_init(acts: *mut FileActions) -> c_int {
    // SAFETY: the caller vouches for the structure, and every field of it is
    // valid as all-zero bytes.
    unsafe { acts.write_bytes(0, 1) };
    0
}

/// Frees the list `*acts` holds, and the paths in it. Returns 0.
///
/// # Safety
///
/// `acts` must be valid for a read and a write of a
/// `posix_spawn_file_actions_t` that `posix_spawn_file_actions_init` has been
/// called on, and no spawn using it may be in progress.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawn_file_actions_destroy(acts: *mut FileActions) -> c_int {
    // SAFETY: the caller vouches for the structure.
    let mut at = unsafe { (*acts).actions };
    while !at.is_null() {
        // SAFETY: every link is a node the `add` functions allocated and this
        // has not freed yet.
        let next = unsafe { (*at).next };
        // SAFETY: as above.
        let path = unsafe { (*at).path };
        if !path.is_null() {
            // SAFETY: the node owns its path, and nothing else refers to it.
            unsafe { free(path.cast()) };
        }
        // SAFETY: the node is this list's, and it has just been read out of.
        unsafe { free(at.cast()) };
        at = next;
    }
    // SAFETY: the caller vouches for the structure.
    unsafe { (*acts).actions = null_mut() };
    0
}

/// Adds `node` to the end of `*acts`, so that the actions run in the order
/// they were added.
///
/// # Safety
///
/// `acts` must be a list this module made, and `node` a fresh allocation
/// nothing else refers to.
unsafe fn append(acts: *mut FileActions, node: *mut Action) {
    // SAFETY: the caller vouches for the list.
    let mut at = unsafe { &raw mut (*acts).actions };
    loop {
        // SAFETY: `at` points at the head, or at a link inside a node this
        // module allocated and has not freed.
        let next = unsafe { *at };
        if next.is_null() {
            break;
        }
        // SAFETY: `next` is such a node.
        at = unsafe { &raw mut (*next).next };
    }
    // SAFETY: `at` is the null link at the end of the list.
    unsafe { *at = node };
}

/// Allocates an action, or returns null.
fn node(kind: Kind) -> *mut Action {
    let at = malloc(size_of::<Action>()).cast::<Action>();
    if at.is_null() {
        return at;
    }
    // SAFETY: `at` is a fresh allocation, large enough and aligned for an
    // `Action`, that nothing else refers to.
    unsafe {
        at.write(Action {
            next: null_mut(),
            kind,
            fd: -1,
            from: -1,
            oflag: 0,
            mode: 0,
            path: null_mut(),
        });
    }
    at
}

/// A copy of the NUL-terminated `path` from `malloc`, or null.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
unsafe fn dup_path(path: *const c_char) -> *mut c_char {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { strlen(path) };
    let copy = malloc(len.wrapping_add(1)).cast::<c_char>();
    if copy.is_null() {
        return copy;
    }
    // SAFETY: the copy holds `len + 1` bytes, and the string is `len` bytes
    // and a NUL.
    let _ = unsafe { memcpy(copy.cast(), path.cast(), len.wrapping_add(1)) };
    copy
}

/// Adds "open `path` with `oflag` and `mode` as descriptor `fd`" to `*acts`.
///
/// # Safety
///
/// `acts` must be a list this module made, and `path` a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawn_file_actions_addopen(
    acts: *mut FileActions,
    fd: c_int,
    path: *const c_char,
    oflag: c_int,
    mode: c_uint,
) -> c_int {
    if fd < 0 {
        return errno::EBADF;
    }
    let at = node(Kind::Open);
    if at.is_null() {
        return errno::ENOMEM;
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let path = unsafe { dup_path(path) };
    if path.is_null() {
        // SAFETY: `at` is this call's own allocation, which nothing else has
        // seen.
        unsafe { free(at.cast()) };
        return errno::ENOMEM;
    }
    // SAFETY: `at` is this call's own allocation, which nothing else refers
    // to yet, so the writes below are to memory only this thread can see.
    unsafe { (*at).fd = fd };
    // SAFETY: as above.
    unsafe { (*at).oflag = oflag };
    // SAFETY: as above.
    unsafe { (*at).mode = mode };
    // SAFETY: as above, and the node takes the path over.
    unsafe { (*at).path = path };
    // SAFETY: the caller vouches for the list, and `at` is a fresh node.
    unsafe { append(acts, at) };
    0
}

/// Adds "close `fd`" to `*acts`.
///
/// # Safety
///
/// `acts` must be a list this module made.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawn_file_actions_addclose(
    acts: *mut FileActions,
    fd: c_int,
) -> c_int {
    if fd < 0 {
        return errno::EBADF;
    }
    let at = node(Kind::Close);
    if at.is_null() {
        return errno::ENOMEM;
    }
    // SAFETY: `at` is this call's own allocation, which nothing else refers
    // to yet.
    unsafe { (*at).fd = fd };
    // SAFETY: the caller vouches for the list, and `at` is a fresh node.
    unsafe { append(acts, at) };
    0
}

/// Adds "make `newfd` a copy of `fd`" to `*acts`. As POSIX says, the two being
/// equal clears the descriptor's close-on-exec flag rather than duplicating.
///
/// # Safety
///
/// `acts` must be a list this module made.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawn_file_actions_adddup2(
    acts: *mut FileActions,
    fd: c_int,
    newfd: c_int,
) -> c_int {
    if fd < 0 || newfd < 0 {
        return errno::EBADF;
    }
    let at = node(Kind::Dup2);
    if at.is_null() {
        return errno::ENOMEM;
    }
    // SAFETY: `at` is this call's own allocation, which nothing else refers
    // to yet.
    unsafe { (*at).from = fd };
    // SAFETY: as above.
    unsafe { (*at).fd = newfd };
    // SAFETY: the caller vouches for the list, and `at` is a fresh node.
    unsafe { append(acts, at) };
    0
}

/// Adds "change directory to `path`" to `*acts`. glibc's extension.
///
/// # Safety
///
/// As [`posix_spawn_file_actions_addopen`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawn_file_actions_addchdir_np(
    acts: *mut FileActions,
    path: *const c_char,
) -> c_int {
    let at = node(Kind::Chdir);
    if at.is_null() {
        return errno::ENOMEM;
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let path = unsafe { dup_path(path) };
    if path.is_null() {
        // SAFETY: `at` is this call's own allocation, which nothing else has
        // seen.
        unsafe { free(at.cast()) };
        return errno::ENOMEM;
    }
    // SAFETY: `at` is this call's own allocation, and it takes the path over.
    unsafe { (*at).path = path };
    // SAFETY: the caller vouches for the list, and `at` is a fresh node.
    unsafe { append(acts, at) };
    0
}

/// Adds "change directory to what `fd` refers to" to `*acts`. glibc's
/// extension.
///
/// # Safety
///
/// `acts` must be a list this module made.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawn_file_actions_addfchdir_np(
    acts: *mut FileActions,
    fd: c_int,
) -> c_int {
    if fd < 0 {
        return errno::EBADF;
    }
    let at = node(Kind::Fchdir);
    if at.is_null() {
        return errno::ENOMEM;
    }
    // SAFETY: `at` is this call's own allocation, which nothing else refers
    // to yet.
    unsafe { (*at).fd = fd };
    // SAFETY: the caller vouches for the list, and `at` is a fresh node.
    unsafe { append(acts, at) };
    0
}

// ---------------------------------------------------------------------------
// The spawn
// ---------------------------------------------------------------------------

/// Starts `path` as a new process and stores its id in `*pid`.
///
/// Returns 0, or the error number — not -1 and `errno` — as POSIX says. An
/// error the child found on the way to `exec`, such as a file action that
/// failed or a program that is not there, is reported here.
///
/// # Safety
///
/// `path` must be a NUL-terminated string; `pid` null or valid for a write of
/// a `pid_t`; `acts` and `attr` null or structures this module made; and
/// `argv` and `envp` arrays of NUL-terminated strings ending in a null
/// pointer.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawn(
    pid: *mut c_int,
    path: *const c_char,
    acts: *const FileActions,
    attr: *const SpawnAttr,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    // SAFETY: the caller's contract is `spawn`'s.
    unsafe { spawn(pid, path, acts, attr, argv, envp, false) }
}

/// [`posix_spawn`], with `path` searched for in `PATH` when it holds no `/`.
///
/// # Safety
///
/// As [`posix_spawn`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_spawnp(
    pid: *mut c_int,
    path: *const c_char,
    acts: *const FileActions,
    attr: *const SpawnAttr,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    // SAFETY: the caller's contract is `spawn`'s.
    unsafe { spawn(pid, path, acts, attr, argv, envp, true) }
}

/// What [`posix_spawn`] and [`posix_spawnp`] share.
///
/// # Safety
///
/// As [`posix_spawn`].
unsafe fn spawn(
    pid: *mut c_int,
    path: *const c_char,
    acts: *const FileActions,
    attr: *const SpawnAttr,
    argv: *const *const c_char,
    envp: *const *const c_char,
    search: bool,
) -> c_int {
    let mut fds: [c_int; 2] = [-1, -1];
    // SAFETY: `fds` is a live local of two `int`.
    if unsafe { pipe2(fds.as_mut_ptr(), O_CLOEXEC) } != 0 {
        return errno::get();
    }
    let [reader, writer] = fds;

    let child = fork_bare();
    if child < 0 {
        let error = errno::get();
        let _ = close(reader);
        let _ = close(writer);
        return error;
    }
    if child == 0 {
        let _ = close(reader);
        // SAFETY: the caller vouches for every argument, and this process is
        // the fresh child, which does nothing but system calls from here.
        unsafe { child_of_spawn(writer, path, acts, attr, argv, envp, search) };
    }

    let _ = close(writer);
    let error = read_error(reader);
    let _ = close(reader);
    if error != 0 {
        // The child wrote why it could not start the program and exited. Reap
        // it here, so that a failed spawn leaves nothing behind; the caller
        // was never told a process id, so nobody else can.
        let mut status = 0;
        // SAFETY: `status` is a live local.
        while unsafe { waitpid(child, &raw mut status, 0) } < 0 && errno::get() == errno::EINTR {}
        return error;
    }
    if !pid.is_null() {
        // SAFETY: the caller vouches for the pointer.
        unsafe { pid.write(child) };
    }
    0
}

/// Reads the error number the child may have written, or 0 if it wrote none.
///
/// A short read is treated as none: the child writes the number in one call,
/// and a pipe write of four bytes is atomic.
fn read_error(reader: c_int) -> c_int {
    let mut buf = [0_u8; size_of::<c_int>()];
    loop {
        // SAFETY: `buf` is a live local of the length given.
        let got = unsafe { read(reader, buf.as_mut_ptr().cast(), buf.len()) };
        if got < 0 && errno::get() == errno::EINTR {
            continue;
        }
        if got != buf.len() as isize {
            return 0;
        }
        return c_int::from_ne_bytes(buf);
    }
}

/// The child: applies the attributes and the file actions and runs the
/// program. Never returns — it either `exec`s or writes why it could not and
/// exits 127.
///
/// Only system calls happen here. The memory it reads was prepared by the
/// parent before the fork.
///
/// # Safety
///
/// As [`posix_spawn`], and this must be the child of a fresh fork.
unsafe fn child_of_spawn(
    writer: c_int,
    path: *const c_char,
    acts: *const FileActions,
    attr: *const SpawnAttr,
    argv: *const *const c_char,
    envp: *const *const c_char,
    search: bool,
) -> ! {
    // SAFETY: the caller vouches for the attributes.
    let error = unsafe { apply_attr(attr) }
        // SAFETY: the caller vouches for the actions.
        .or_else(|| unsafe { apply_actions(acts, writer) })
        .unwrap_or_else(|| {
            // SAFETY: the caller vouches for the program, arguments and
            // environment.
            if search {
                // SAFETY: the caller vouches for the program, arguments and
                // environment.
                let _ = unsafe { execvpe(path, argv, envp) };
            } else {
                // SAFETY: as above.
                let _ = unsafe { execve(path, argv, envp) };
            }
            errno::get()
        });
    let bytes = error.to_ne_bytes();
    // SAFETY: `bytes` is a live local of the length given. Nothing can be done
    // about a write that fails: the parent then sees the child's exit status
    // instead, which is what a C library without a pipe would give it.
    let _ = unsafe { write(writer, bytes.as_ptr().cast(), bytes.len()) };
    _exit(NOT_STARTED)
}

/// Applies `*attr` in the child, in the order POSIX gives: session, process
/// group, scheduling, ids, signal dispositions, signal mask. Returns the error
/// number of the first step that failed.
///
/// The mask is set last on purpose. A signal reset to its default by
/// `POSIX_SPAWN_SETSIGDEF` must not be able to kill the child before the rest
/// of the setup has run, and the caller's mask is still in force until here.
///
/// # Safety
///
/// `attr` must be null or a structure this module made.
unsafe fn apply_attr(attr: *const SpawnAttr) -> Option<c_int> {
    if attr.is_null() {
        return None;
    }
    // SAFETY: the caller vouches for the structure, and it is not written to
    // while this runs: the child is the only thread in this process.
    let attr = unsafe { &*attr };
    let flags = attr.flags;

    if flags & SETSID != 0 && setsid() < 0 {
        return Some(errno::get());
    }
    if flags & SETPGROUP != 0 && setpgid(0, attr.pgrp) != 0 {
        return Some(errno::get());
    }
    if flags & (SETSCHEDULER | SETSCHEDPARAM) != 0 {
        let param = SchedParam::with_priority(attr.prio);
        let ret = if flags & SETSCHEDULER != 0 {
            // SAFETY: `param` is a live local.
            unsafe { sched_setscheduler(0, attr.pol, &raw const param) }
        } else {
            // SAFETY: `param` is a live local.
            unsafe { sched_setparam(0, &raw const param) }
        };
        if ret != 0 {
            return Some(errno::get());
        }
    }
    if flags & RESETIDS != 0 {
        if crate::process::setgid(crate::process::getgid()) != 0 {
            return Some(errno::get());
        }
        if crate::process::setuid(crate::process::getuid()) != 0 {
            return Some(errno::get());
        }
    }
    if flags & SETSIGDEF != 0 {
        let action = Sigaction {
            handler: SIG_DFL,
            mask: SigSet::EMPTY,
            flags: 0,
            restorer: 0,
        };
        let mut sig = 1;
        while sig <= NSIG {
            // SAFETY: both are live locals.
            let member = unsafe { sigismember(&raw const attr.def, sig) };
            if member == 1 {
                // SAFETY: `action` is a live local and the old disposition is
                // not wanted. A signal that cannot be caught is refused, and
                // that refusal is not the caller's error: it was already at
                // its default.
                let _ = unsafe { sigaction(sig, &raw const action, null_mut()) };
            }
            sig += 1;
        }
    }
    if flags & SETSIGMASK != 0 {
        // SAFETY: the set is inside the structure the caller vouches for.
        if unsafe { sigprocmask(SIG_SETMASK, &raw const attr.mask, null_mut()) } != 0 {
            return Some(errno::get());
        }
    }
    None
}

/// Applies the file actions in `*acts`, in order, in the child. Returns the
/// error number of the first that failed.
///
/// `writer` is the pipe the failure would be reported through, so an action
/// aimed at that descriptor would leave nothing to report with. Each such
/// action moves the pipe to another descriptor first — the kernel picks a free
/// one — and `*writer_out` follows it.
///
/// # Safety
///
/// `acts` must be null or a list this module made.
unsafe fn apply_actions(acts: *const FileActions, writer: c_int) -> Option<c_int> {
    if acts.is_null() {
        return None;
    }
    let mut writer = writer;
    // SAFETY: the caller vouches for the list.
    let mut at = unsafe { (*acts).actions };
    while !at.is_null() {
        // SAFETY: every link is a node the `add` functions allocated and
        // `destroy` has not freed.
        let action = unsafe { &*at };
        if action.fd == writer && action.kind != Kind::Fchdir {
            // SAFETY: `writer` is this process's pipe descriptor.
            let moved = unsafe { fcntl(writer, F_DUPFD_CLOEXEC, 0) };
            if moved < 0 {
                return Some(errno::get());
            }
            let _ = close(writer);
            writer = moved;
        }
        let ret = match action.kind {
            // SAFETY: the node owns its path, which is NUL-terminated.
            Kind::Open => unsafe { open_onto(action) },
            Kind::Close => close(action.fd),
            Kind::Dup2 => dup_onto(action.from, action.fd),
            // SAFETY: as above.
            Kind::Chdir => unsafe { chdir(action.path) },
            Kind::Fchdir => fchdir(action.fd),
        };
        if ret != 0 {
            return Some(errno::get());
        }
        at = action.next;
    }
    None
}

/// Opens the action's path onto its descriptor number.
///
/// # Safety
///
/// `action` must be an `Open` node, whose path is NUL-terminated.
unsafe fn open_onto(action: &Action) -> c_int {
    // SAFETY: the node owns a NUL-terminated path.
    let fd = unsafe { crate::fcntl::open(action.path, action.oflag, action.mode) };
    if fd < 0 {
        return -1;
    }
    if fd == action.fd {
        return 0;
    }
    let ret = dup_onto(fd, action.fd);
    let error = errno::get();
    let _ = close(fd);
    if ret != 0 {
        errno::set(error);
    }
    ret
}

/// `dup2`, with POSIX's rule for a file action whose two descriptors are
/// equal: the descriptor stays and its close-on-exec flag is cleared, so that
/// it survives the `exec`.
fn dup_onto(from: c_int, to: c_int) -> c_int {
    if from != to {
        return if dup2(from, to) < 0 { -1 } else { 0 };
    }
    // SAFETY: `F_GETFD` and `F_SETFD` read and write no memory.
    let flags = unsafe { fcntl(from, F_GETFD, 0) };
    if flags < 0 {
        return -1;
    }
    let wanted = c_ulong::from((flags & !FD_CLOEXEC) as c_uint);
    // SAFETY: as above.
    if unsafe { fcntl(from, F_SETFD, wanted) } < 0 {
        -1
    } else {
        0
    }
}

/// `F_GETFD`, from `asm-generic/fcntl.h`.
const F_GETFD: c_int = 1;
/// `F_SETFD`, from `asm-generic/fcntl.h`.
const F_SETFD: c_int = 2;
/// `FD_CLOEXEC`, from `asm-generic/fcntl.h`.
const FD_CLOEXEC: c_int = 1;

// A structure C hands over is aligned as its header says, which for both of
// these is the alignment of the `sigset_t` and the pointer inside them.
const _: () = assert!(align_of::<SpawnAttr>() == align_of::<c_ulong>());
const _: () = assert!(align_of::<FileActions>() == align_of::<*mut c_void>());

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh attribute structure has no flags and no scheduling.
    #[test]
    fn attributes_start_empty() {
        let mut attr = core::mem::MaybeUninit::<SpawnAttr>::uninit();
        // SAFETY: the pointer is to a live local.
        assert_eq!(unsafe { posix_spawnattr_init(attr.as_mut_ptr()) }, 0);
        // SAFETY: `init` wrote every byte.
        let attr = unsafe { attr.assume_init_mut() };
        assert_eq!(attr.flags, 0);
        assert_eq!(attr.pgrp, 0);

        let mut flags: c_short = -1;
        // SAFETY: both pointers are to live locals.
        let ret = unsafe { posix_spawnattr_getflags(&raw const *attr, &raw mut flags) };
        assert_eq!(ret, 0);
        assert_eq!(flags, 0);
    }

    /// Every flag `spawn.h` defines is taken, and anything else is refused.
    #[test]
    fn unknown_flags_are_refused() {
        let mut attr = core::mem::MaybeUninit::<SpawnAttr>::uninit();
        // SAFETY: the pointer is to a live local.
        let ret = unsafe { posix_spawnattr_init(attr.as_mut_ptr()) };
        assert_eq!(ret, 0);
        // SAFETY: `init` wrote every byte.
        let attr = unsafe { attr.assume_init_mut() };
        // SAFETY: the pointer is to a live local.
        let ret = unsafe { posix_spawnattr_setflags(&raw mut *attr, KNOWN_FLAGS as c_short) };
        assert_eq!(ret, 0);
        assert_eq!(attr.flags, KNOWN_FLAGS);
        // SAFETY: as above.
        let ret = unsafe { posix_spawnattr_setflags(&raw mut *attr, 256) };
        assert_eq!(ret, errno::EINVAL);
        assert_eq!(attr.flags, KNOWN_FLAGS);
    }

    /// The actions come out in the order they went in, and `destroy` empties
    /// the list.
    #[test]
    fn file_actions_keep_their_order() {
        let mut acts = core::mem::MaybeUninit::<FileActions>::uninit();
        // SAFETY: the pointer is to a live local.
        let ret = unsafe { posix_spawn_file_actions_init(acts.as_mut_ptr()) };
        assert_eq!(ret, 0);
        // SAFETY: `init` wrote every byte.
        let acts = unsafe { acts.assume_init_mut() };
        // SAFETY: the list is this test's own.
        let ret = unsafe { posix_spawn_file_actions_adddup2(&raw mut *acts, 4, 1) };
        assert_eq!(ret, 0);
        // SAFETY: as above.
        let ret = unsafe { posix_spawn_file_actions_addclose(&raw mut *acts, 4) };
        assert_eq!(ret, 0);
        // SAFETY: as above, and the path is a string literal.
        let ret = unsafe {
            posix_spawn_file_actions_addopen(&raw mut *acts, 2, c"/dev/null".as_ptr(), 0, 0)
        };
        assert_eq!(ret, 0);

        let mut kinds = [Kind::Close; 3];
        let mut at = acts.actions;
        let mut n = 0;
        while !at.is_null() && n < kinds.len() {
            // SAFETY: the list holds this test's own nodes.
            let action = unsafe { &*at };
            if let Some(slot) = kinds.get_mut(n) {
                *slot = action.kind;
            }
            at = action.next;
            n += 1;
        }
        assert_eq!(n, 3);
        assert_eq!(kinds, [Kind::Dup2, Kind::Close, Kind::Open]);

        // SAFETY: the list is this test's own and no spawn is running.
        let ret = unsafe { posix_spawn_file_actions_destroy(&raw mut *acts) };
        assert_eq!(ret, 0);
        assert!(acts.actions.is_null());
    }

    /// A negative descriptor is refused before anything is allocated.
    #[test]
    fn negative_descriptors_are_refused() {
        let mut acts = core::mem::MaybeUninit::<FileActions>::uninit();
        // SAFETY: the pointer is to a live local.
        let ret = unsafe { posix_spawn_file_actions_init(acts.as_mut_ptr()) };
        assert_eq!(ret, 0);
        // SAFETY: `init` wrote every byte.
        let acts = unsafe { acts.assume_init_mut() };
        // SAFETY: the list is this test's own.
        let ret = unsafe { posix_spawn_file_actions_addclose(&raw mut *acts, -1) };
        assert_eq!(ret, errno::EBADF);
        // SAFETY: as above.
        let ret = unsafe { posix_spawn_file_actions_adddup2(&raw mut *acts, 0, -1) };
        assert_eq!(ret, errno::EBADF);
        assert!(acts.actions.is_null());
    }
}
