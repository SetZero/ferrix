//! `pthread.h`'s thread attributes: `pthread_attr_t` and its functions, the
//! process-wide defaults, and `pthread_getattr_np`.
//!
//! # The default stack size
//!
//! glibc's rule, because programs built for glibc expect it: the soft
//! `RLIMIT_STACK` when the program starts a thread, or 2 MiB when that is
//! unlimited, rounded up to a page. (glibc 2.43 on x86-64 was measured to
//! give 8 MiB under the usual 8 MiB limit, 2 MiB with `ulimit -s unlimited`,
//! and to follow any other limit.) The result is clamped to between 16 KiB
//! and 1 GiB, so that a tiny limit still leaves a usable stack and a huge one
//! cannot make every thread reserve most of the address space. musl's default
//! is a much smaller 128 KiB.
//!
//! The default guard is one page, as in glibc.

use core::ffi::{c_int, c_void};
use core::mem::{offset_of, size_of};
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::resource::{self, RLIM_INFINITY, Rlimit};
use crate::syscall::{self, nr};
use crate::thread::{self, Thread};
use crate::{auxv, errno, pthread};

/// `PTHREAD_STACK_MIN`, from `limits.h`.
pub const STACK_MIN: usize = 2048;
/// `PTHREAD_CREATE_DETACHED`.
const CREATE_DETACHED: c_int = 1;
/// `PTHREAD_EXPLICIT_SCHED`.
pub const EXPLICIT_SCHED: c_int = 1;
/// `PTHREAD_SCOPE_SYSTEM`.
const SCOPE_SYSTEM: c_int = 0;
/// `PTHREAD_SCOPE_PROCESS`.
const SCOPE_PROCESS: c_int = 1;
/// `SCHED_OTHER`, `SCHED_FIFO` and `SCHED_RR`, from `linux/sched.h`.
const SCHED_POLICIES: [c_int; 3] = [0, 1, 2];

/// `RLIMIT_STACK`, from `asm-generic/resource.h`.
const RLIMIT_STACK: c_int = 3;
/// `AT_EXECFN`, from `linux/auxvec.h`: the address of the program's file
/// name, near the top of the initial stack.
const AT_EXECFN: usize = 31;
/// The default stack when the stack limit is unlimited: glibc's
/// `ARCH_STACK_DEFAULT_SIZE` on x86-64.
const UNLIMITED_STACK: usize = 2 << 20;
/// The smallest default stack.
const DEFAULT_STACK_MIN: usize = 16 << 10;
/// The largest default stack.
const DEFAULT_STACK_MAX: usize = 1 << 30;

/// `pthread_attr_t`, in musl's layout: three words, then `int`s.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Attr {
    /// `_a_stacksize`.
    pub stacksize: usize,
    /// `_a_guardsize`.
    pub guardsize: usize,
    /// `_a_stackaddr`: the top of a stack the program supplied, or zero.
    pub stackaddr: usize,
    /// `_a_detach`: nonzero for a detached thread.
    pub detach: c_int,
    /// `_a_sched`: `PTHREAD_EXPLICIT_SCHED` to use the policy and priority
    /// below instead of inheriting the creator's.
    pub inherit: c_int,
    /// `_a_policy`.
    pub policy: c_int,
    /// `_a_prio`.
    pub priority: c_int,
    /// The rest of the 56 bytes.
    reserved: [c_int; 4],
}

const _: () = assert!(size_of::<Attr>() == 56);
const _: () = assert!(offset_of!(Attr, detach) == 24);
const _: () = assert!(offset_of!(Attr, priority) == 36);

impl Attr {
    /// Every field zero.
    pub const ZERO: Self = Self {
        stacksize: 0,
        guardsize: 0,
        stackaddr: 0,
        detach: 0,
        inherit: 0,
        policy: 0,
        priority: 0,
        reserved: [0; 4],
    };
}

/// `struct sched_param`, from `sched.h`: the priority, then reserved space.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SchedParam {
    /// `sched_priority`.
    pub sched_priority: c_int,
    /// The rest of the 48 bytes.
    reserved: [c_int; 11],
    /// musl's reserved fields hold `long`s, so the structure is aligned to 8.
    align: [usize; 0],
}

const _: () = assert!(size_of::<SchedParam>() == 48);

/// A default stack size set with `pthread_setattr_default_np`, or zero.
static DEFAULT_STACK: AtomicUsize = AtomicUsize::new(0);
/// A default guard size set with `pthread_setattr_default_np`, or zero.
static DEFAULT_GUARD: AtomicUsize = AtomicUsize::new(0);

/// The default stack size for a stack limit of `limit` bytes, where
/// [`RLIM_INFINITY`] is none, on pages of `page` bytes.
fn stack_size_for(limit: u64, page: usize) -> usize {
    let size = if limit == RLIM_INFINITY {
        UNLIMITED_STACK
    } else {
        usize::try_from(limit).unwrap_or(DEFAULT_STACK_MAX)
    };
    let size = size.clamp(DEFAULT_STACK_MIN, DEFAULT_STACK_MAX);
    size.checked_next_multiple_of(page).unwrap_or(size)
}

/// The stack size a thread gets unless told otherwise.
pub fn default_stack_size() -> usize {
    let set = DEFAULT_STACK.load(Ordering::Relaxed);
    if set != 0 {
        return set;
    }
    let mut limit = Rlimit::default();
    // SAFETY: `limit` is a live local, and nothing is set.
    let ok = unsafe { resource::prlimit(0, RLIMIT_STACK, core::ptr::null(), &raw mut limit) } == 0;
    let soft = if ok { limit.rlim_cur } else { RLIM_INFINITY };
    stack_size_for(soft, crate::sysconf::page_size())
}

/// The guard size a thread gets unless told otherwise.
pub fn default_guard_size() -> usize {
    match DEFAULT_GUARD.load(Ordering::Relaxed) {
        0 => crate::sysconf::page_size(),
        set => set,
    }
}

/// The attributes of a thread created with none.
pub fn defaults() -> Attr {
    Attr {
        stacksize: default_stack_size(),
        guardsize: default_guard_size(),
        ..Attr::ZERO
    }
}

/// Whether `size` is a stack size the attribute functions accept.
const fn stack_size_ok(size: usize) -> bool {
    size >= STACK_MIN && size <= usize::MAX / 4
}

/// Initialises `*attr` with the defaults.
///
/// # Safety
///
/// `attr` must be valid for a write of a `pthread_attr_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_init(attr: *mut Attr) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    unsafe { attr.write(defaults()) };
    0
}

/// Destroys `*attr`, which holds nothing to free.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_attr_destroy(_attr: *mut Attr) -> c_int {
    0
}

/// Stores the detach state in `*state`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object and `state` valid for a
/// write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_getdetachstate(
    attr: *const Attr,
    state: *mut c_int,
) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    let value = unsafe { (*attr).detach };
    // SAFETY: the caller vouches for `state`.
    unsafe { state.write(value) };
    0
}

/// Sets the detach state: `PTHREAD_CREATE_JOINABLE` or
/// `PTHREAD_CREATE_DETACHED`. Anything else fails with `EINVAL`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_setdetachstate(attr: *mut Attr, state: c_int) -> c_int {
    if !(0..=CREATE_DETACHED).contains(&state) {
        return errno::EINVAL;
    }
    // SAFETY: the caller vouches for `attr`.
    unsafe { (*attr).detach = state };
    0
}

/// Stores the stack size in `*size`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object and `size` valid for a
/// write of a `size_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_getstacksize(attr: *const Attr, size: *mut usize) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    let value = unsafe { (*attr).stacksize };
    // SAFETY: the caller vouches for `size`.
    unsafe { size.write(value) };
    0
}

/// Sets the stack size, and forgets any stack `pthread_attr_setstack` set.
/// Fails with `EINVAL` below `PTHREAD_STACK_MIN` or absurdly large.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_setstacksize(attr: *mut Attr, size: usize) -> c_int {
    if !stack_size_ok(size) {
        return errno::EINVAL;
    }
    // SAFETY: the caller vouches for `attr`.
    let attr = unsafe { &mut *attr };
    attr.stackaddr = 0;
    attr.stacksize = size;
    0
}

/// Stores the guard size in `*size`.
///
/// # Safety
///
/// As [`pthread_attr_getstacksize`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_getguardsize(attr: *const Attr, size: *mut usize) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    let value = unsafe { (*attr).guardsize };
    // SAFETY: the caller vouches for `size`.
    unsafe { size.write(value) };
    0
}

/// Sets the guard size, rounded up to a page when a thread is created. Zero
/// means no guard. Fails with `EINVAL` if absurdly large.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_setguardsize(attr: *mut Attr, size: usize) -> c_int {
    if size > usize::MAX / 8 {
        return errno::EINVAL;
    }
    // SAFETY: the caller vouches for `attr`.
    unsafe { (*attr).guardsize = size };
    0
}

/// Stores the lowest address and the size of the stack
/// `pthread_attr_setstack` set. Fails with `EINVAL` if none was set.
///
/// # Safety
///
/// `attr` must be an initialised attribute object, `addr` valid for a write of
/// a pointer and `size` of a `size_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_getstack(
    attr: *const Attr,
    addr: *mut *mut c_void,
    size: *mut usize,
) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    let attr = unsafe { &*attr };
    if attr.stackaddr == 0 {
        return errno::EINVAL;
    }
    // SAFETY: the caller vouches for `size`.
    unsafe { size.write(attr.stacksize) };
    let low = attr.stackaddr.wrapping_sub(attr.stacksize);
    // SAFETY: the caller vouches for `addr`.
    unsafe { addr.write(core::ptr::with_exposed_provenance_mut(low)) };
    0
}

/// Makes threads created with `attr` run on the `size` bytes at `addr`, which
/// the program owns and frees. The thread gets no guard. Fails with `EINVAL`
/// for a size `pthread_attr_setstacksize` would refuse.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_setstack(
    attr: *mut Attr,
    addr: *mut c_void,
    size: usize,
) -> c_int {
    if !stack_size_ok(size) {
        return errno::EINVAL;
    }
    // SAFETY: the caller vouches for `attr`.
    let attr = unsafe { &mut *attr };
    attr.stackaddr = addr.expose_provenance().wrapping_add(size);
    attr.stacksize = size;
    0
}

/// Stores the contention scope, always `PTHREAD_SCOPE_SYSTEM`, in `*scope`.
///
/// # Safety
///
/// `scope` must be valid for a write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_getscope(_attr: *const Attr, scope: *mut c_int) -> c_int {
    // SAFETY: the caller vouches for `scope`.
    unsafe { scope.write(SCOPE_SYSTEM) };
    0
}

/// Sets the contention scope. Linux threads compete system-wide, so
/// `PTHREAD_SCOPE_PROCESS` fails with `ENOTSUP`, and anything else but
/// `PTHREAD_SCOPE_SYSTEM` with `EINVAL`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_attr_setscope(_attr: *mut Attr, scope: c_int) -> c_int {
    match scope {
        SCOPE_SYSTEM => 0,
        SCOPE_PROCESS => errno::ENOTSUP,
        _ => errno::EINVAL,
    }
}

/// Stores whether threads inherit their creator's scheduling in `*inherit`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object and `inherit` valid for a
/// write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_getinheritsched(
    attr: *const Attr,
    inherit: *mut c_int,
) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    let value = unsafe { (*attr).inherit };
    // SAFETY: the caller vouches for `inherit`.
    unsafe { inherit.write(value) };
    0
}

/// Sets whether threads inherit their creator's scheduling:
/// `PTHREAD_INHERIT_SCHED` or `PTHREAD_EXPLICIT_SCHED`. Anything else fails
/// with `EINVAL`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_setinheritsched(attr: *mut Attr, inherit: c_int) -> c_int {
    if !(0..=EXPLICIT_SCHED).contains(&inherit) {
        return errno::EINVAL;
    }
    // SAFETY: the caller vouches for `attr`.
    unsafe { (*attr).inherit = inherit };
    0
}

/// Stores the scheduling policy in `*policy`.
///
/// # Safety
///
/// As [`pthread_attr_getinheritsched`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_getschedpolicy(
    attr: *const Attr,
    policy: *mut c_int,
) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    let value = unsafe { (*attr).policy };
    // SAFETY: the caller vouches for `policy`.
    unsafe { policy.write(value) };
    0
}

/// Sets the scheduling policy: `SCHED_OTHER`, `SCHED_FIFO` or `SCHED_RR`, as
/// glibc accepts. Anything else fails with `EINVAL`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_setschedpolicy(attr: *mut Attr, policy: c_int) -> c_int {
    if !SCHED_POLICIES.contains(&policy) {
        return errno::EINVAL;
    }
    // SAFETY: the caller vouches for `attr`.
    unsafe { (*attr).policy = policy };
    0
}

/// Stores the scheduling priority in `param`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object and `param` valid for a
/// write of a `struct sched_param`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_getschedparam(
    attr: *const Attr,
    param: *mut SchedParam,
) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    let value = unsafe { (*attr).priority };
    // SAFETY: the caller vouches for `param`.
    unsafe { (*param).sched_priority = value };
    0
}

/// Sets the scheduling priority from `param`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object and `param` valid for a
/// read of a `struct sched_param`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_attr_setschedparam(
    attr: *mut Attr,
    param: *const SchedParam,
) -> c_int {
    // SAFETY: the caller vouches for `param`.
    let value = unsafe { (*param).sched_priority };
    // SAFETY: the caller vouches for `attr`.
    unsafe { (*attr).priority = value };
    0
}

/// The top of the initial stack and the size mapped below it, found as musl
/// does: from the page above the program's file name, grow a one-page probe
/// downwards with `mremap` until the page below is no longer mapped.
fn main_stack() -> (usize, usize) {
    let page = crate::sysconf::page_size();
    let name = auxv::get(AT_EXECFN).unwrap_or(0);
    let top = name.checked_next_multiple_of(page).unwrap_or(name);
    let mut size = page;
    while let Some(probe) = top.checked_sub(size + page) {
        // SAFETY: growing a page in place with no flags never moves or
        // changes a mapping that exists: it fails with `ENOMEM` if the page
        // above is in use, which is how the stack is measured, and `EFAULT`
        // once the probe is below the stack. It could only extend a lone
        // mapping, which the stack's own pages exclude.
        let ret = unsafe { syscall::syscall6(nr::MREMAP, probe, page, 2 * page, 0, 0, 0) };
        if errno::decode(ret) != Err(errno::ENOMEM) {
            break;
        }
        size += page;
    }
    (top, size)
}

/// Stores thread `t`'s attributes in `*attr`: whether it is detached, its
/// guard, and its stack, which for the main thread is measured.
///
/// # Safety
///
/// `t` must be a running thread and `attr` valid for a write of a
/// `pthread_attr_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_getattr_np(t: *mut Thread, attr: *mut Attr) -> c_int {
    // SAFETY: the caller vouches for `t`.
    let state = unsafe { thread::state(t) };
    let mut out = Attr {
        detach: c_int::from(state.detach_state.load(Ordering::SeqCst) >= pthread::DT_DETACHED),
        guardsize: state.guard_size.load(Ordering::SeqCst),
        ..Attr::ZERO
    };
    let stack = state.stack.load(Ordering::SeqCst);
    if stack != 0 {
        out.stackaddr = stack;
        out.stacksize = state.stack_size.load(Ordering::SeqCst);
    } else {
        (out.stackaddr, out.stacksize) = main_stack();
    }
    // SAFETY: the caller vouches for `attr`.
    unsafe { attr.write(out) };
    0
}

/// Stores the attributes threads get by default in `*attr`.
///
/// # Safety
///
/// `attr` must be valid for a write of a `pthread_attr_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_getattr_default_np(attr: *mut Attr) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    unsafe { attr.write(defaults()) };
    0
}

/// Sets the stack and guard size threads get by default from `*attr`. Other
/// attributes must be their defaults, or it fails with `EINVAL`, as in musl.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_setattr_default_np(attr: *const Attr) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    let attr = unsafe { &*attr };
    if attr.stackaddr != 0
        || attr.detach != 0
        || attr.inherit != 0
        || attr.policy != 0
        || attr.priority != 0
        || !stack_size_ok(attr.stacksize)
        || attr.guardsize > usize::MAX / 8
    {
        return errno::EINVAL;
    }
    DEFAULT_STACK.store(attr.stacksize, Ordering::Relaxed);
    DEFAULT_GUARD.store(attr.guardsize.max(1), Ordering::Relaxed);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_stack_follows_the_limit_as_glibc_does() {
        assert_eq!(stack_size_for(8 << 20, 4096), 8 << 20);
        assert_eq!(stack_size_for(RLIM_INFINITY, 4096), 2 << 20);
        assert_eq!(stack_size_for(100_000, 4096), 102_400);
        assert_eq!(stack_size_for(1, 4096), 16 << 10);
        assert_eq!(stack_size_for(1 << 40, 4096), 1 << 30);
    }

    #[test]
    fn attribute_setters_refuse_what_they_cannot_hold() {
        let mut attr = Attr::ZERO;
        // SAFETY: `attr` is a live local, here and below.
        let ret = unsafe { pthread_attr_setstacksize(&raw mut attr, STACK_MIN - 1) };
        assert_eq!(ret, errno::EINVAL);
        // SAFETY: as above.
        let ret = unsafe { pthread_attr_setstacksize(&raw mut attr, 1 << 20) };
        assert_eq!(ret, 0);
        // SAFETY: as above.
        let ret = unsafe { pthread_attr_setdetachstate(&raw mut attr, 2) };
        assert_eq!(ret, errno::EINVAL);
        // SAFETY: as above.
        let ret = unsafe { pthread_attr_setinheritsched(&raw mut attr, -1) };
        assert_eq!(ret, errno::EINVAL);
        // SAFETY: as above.
        let ret = unsafe { pthread_attr_setschedpolicy(&raw mut attr, 7) };
        assert_eq!(ret, errno::EINVAL);
        // SAFETY: as above.
        let ret = unsafe { pthread_attr_setguardsize(&raw mut attr, usize::MAX) };
        assert_eq!(ret, errno::EINVAL);
        assert_eq!(
            pthread_attr_setscope(&raw mut attr, SCOPE_PROCESS),
            errno::ENOTSUP
        );
        assert_eq!(pthread_attr_setscope(&raw mut attr, 5), errno::EINVAL);
        let mut addr = core::ptr::null_mut();
        let mut size = 0;
        // SAFETY: all three are live locals.
        let ret = unsafe { pthread_attr_getstack(&raw const attr, &raw mut addr, &raw mut size) };
        assert_eq!(ret, errno::EINVAL);
    }
}
