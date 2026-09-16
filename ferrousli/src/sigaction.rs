//! Signal dispositions: `sigaction`, and `signal`, `bsd_signal`,
//! `siginterrupt`, `sigignore` and `sigset` built on it.
//!
//! # Two `struct sigaction`s
//!
//! C's `struct sigaction` in `signal.h` is the handler, a 128-byte `sigset_t`,
//! an `int` of flags and the restorer. The kernel's, which `rt_sigaction`
//! takes, is the handler, an `unsigned long` of flags, the restorer, and an
//! 8-byte mask last. glibc's C structure has the same layout as musl's on
//! x86-64. Every call converts in both directions.
//!
//! # The restorer
//!
//! When a handler returns, it returns to the address the kernel pushed as its
//! return address. On x86-64 the kernel has no code of its own to put there:
//! the C library must pass one with `SA_RESTORER`, and it must call
//! `rt_sigreturn`. So every action this library installs carries
//! [`__restore_rt`], whatever the program put in `sa_restorer`, as musl and
//! glibc do.
//!
//! # What is left out
//!
//! musl takes a lock around changing `SIGABRT`'s disposition, so that `abort`
//! on one thread cannot race a handler being installed on another, and unblocks
//! its reserved signals before the first handler is installed. With no threads
//! yet, neither can matter.

use core::ffi::{c_int, c_ulong};
use core::mem::{offset_of, size_of};
use core::ptr::{null, null_mut};

use crate::errno;
use crate::sigset::{self, KERNEL_SIGSET_SIZE, SIG_BLOCK, SIG_UNBLOCK, SigSet};
use crate::syscall::{self, nr};

/// A signal handler's address, or one of the values below that stand for a
/// disposition. `void (*)(int)` in C, which is a pointer.
pub type Handler = usize;

/// `SIG_DFL`: the default action.
pub const SIG_DFL: Handler = 0;
/// `SIG_IGN`: ignore the signal.
pub const SIG_IGN: Handler = 1;
/// `SIG_HOLD`: `sigset`'s request to block the signal.
pub const SIG_HOLD: Handler = 2;
/// `SIG_ERR`: what `signal` returns on failure, `(void (*)(int))-1`.
pub const SIG_ERR: Handler = usize::MAX;

/// `SA_SIGINFO`: call the handler with a `siginfo_t` and a context.
pub const SA_SIGINFO: c_int = 0x0000_0004;
/// `SA_ONSTACK`: run the handler on the alternate stack.
pub const SA_ONSTACK: c_int = 0x0800_0000;
/// `SA_RESTART`: restart a system call the signal interrupted.
pub const SA_RESTART: c_int = 0x1000_0000;
/// `SA_NODEFER`: do not block the signal while its handler runs.
pub const SA_NODEFER: c_int = 0x4000_0000;
/// `SA_RESETHAND`: restore the default action once the handler is called.
pub const SA_RESETHAND: c_int = 0x8000_0000_u32.cast_signed();
/// `SA_RESTORER`: `sa_restorer` holds the code the handler returns to. From
/// `asm/signal.h`.
pub const SA_RESTORER: c_int = 0x0400_0000;

/// C's `struct sigaction`, as `signal.h` lays it out.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Sigaction {
    /// `sa_handler` or `sa_sigaction`, a union of two function pointers.
    pub handler: Handler,
    /// `sa_mask`: signals blocked while the handler runs.
    pub mask: SigSet,
    /// `sa_flags`.
    pub flags: c_int,
    /// `sa_restorer`. Ignored when installing; filled in when reporting.
    pub restorer: usize,
}

const _: () = assert!(size_of::<Sigaction>() == 152);
const _: () = assert!(offset_of!(Sigaction, mask) == 8);
const _: () = assert!(offset_of!(Sigaction, flags) == 136);
const _: () = assert!(offset_of!(Sigaction, restorer) == 144);

impl Sigaction {
    /// The default action, with an empty mask and no flags.
    pub const DEFAULT: Self = Self {
        handler: SIG_DFL,
        mask: SigSet::EMPTY,
        flags: 0,
        restorer: 0,
    };
}

/// The kernel's `struct sigaction` on x86-64, from `asm/signal.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelSigaction {
    /// `sa_handler`.
    pub handler: Handler,
    /// `sa_flags`.
    pub flags: c_ulong,
    /// `sa_restorer`.
    pub restorer: usize,
    /// `sa_mask`, the kernel's 8-byte set.
    pub mask: c_ulong,
}

const _: () = assert!(size_of::<KernelSigaction>() == 32);
const _: () = assert!(offset_of!(KernelSigaction, flags) == 8);
const _: () = assert!(offset_of!(KernelSigaction, restorer) == 16);
const _: () = assert!(offset_of!(KernelSigaction, mask) == 24);

impl KernelSigaction {
    /// The default action with nothing else set.
    pub const DEFAULT: Self = Self {
        handler: SIG_DFL,
        flags: 0,
        restorer: 0,
        mask: 0,
    };

    /// The kernel's form of `action`, returning through `restorer`.
    ///
    /// The flags are zero-extended. Sign-extending `SA_RESETHAND`, as a plain
    /// cast would, sets 32 bits the kernel does not define. The kernel clears
    /// bits it does not define, so either works; zero-extending passes the
    /// value C wrote and nothing more.
    pub fn from_c(action: &Sigaction, restorer: usize) -> Self {
        Self {
            handler: action.handler,
            flags: c_ulong::from(action.flags.cast_unsigned()) | SA_RESTORER as c_ulong,
            restorer,
            mask: action.mask.word(),
        }
    }

    /// C's form of this action. The flags keep `SA_RESTORER`, as with musl and
    /// glibc.
    pub fn to_c(self) -> Sigaction {
        Sigaction {
            handler: self.handler,
            mask: SigSet::from_word(self.mask),
            // The kernel defines no flag above bit 31.
            flags: (self.flags as u32).cast_signed(),
            restorer: self.restorer,
        }
    }
}

/// Changes or reads signal `sig`'s disposition in the kernel, with no checks.
///
/// # Safety
///
/// Installing a handler must be sound: its address must be code that can take
/// the signal, and its restorer code that calls `rt_sigreturn`.
pub unsafe fn kernel_sigaction(
    sig: c_int,
    new: Option<&KernelSigaction>,
    old: Option<&mut KernelSigaction>,
) -> Result<(), c_int> {
    let new = new.map_or(null(), core::ptr::from_ref);
    let old = old.map_or(null_mut(), core::ptr::from_mut);
    // SAFETY: the kernel reads `new` and writes `old`, which are null or
    // borrowed for this call. The caller vouches for the handler. Casting the
    // signal sign-extends, and the kernel reads the low 32 bits back.
    let ret = unsafe {
        syscall::syscall4(
            nr::RT_SIGACTION,
            sig as usize,
            new.addr(),
            old.addr(),
            KERNEL_SIGSET_SIZE,
        )
    };
    errno::decode(ret).map(|_| ())
}

/// Whether a program may change `sig`'s disposition here. The kernel refuses
/// `SIGKILL` and `SIGSTOP` itself.
fn changeable(sig: c_int) -> bool {
    sigset::usable_bit(sig).is_some()
}

/// Examines or changes the action for `sig`.
///
/// A signal that does not exist, or that the library reserves (32 to 34),
/// fails with `EINVAL`, as in musl.
///
/// # Safety
///
/// `act` must be null or valid for reads of a `struct sigaction`, and `old`
/// null or valid for writes of one. A handler `act` installs must be a
/// function that is sound to call when the signal arrives.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigaction(
    sig: c_int,
    act: *const Sigaction,
    old: *mut Sigaction,
) -> c_int {
    if !changeable(sig) {
        errno::set(errno::EINVAL);
        return -1;
    }
    // SAFETY: the caller passes null or a readable action.
    let new = unsafe { act.as_ref() }.map(|act| KernelSigaction::from_c(act, restorer()));
    let mut previous = KernelSigaction::DEFAULT;
    let wanted = (!old.is_null()).then_some(&mut previous);
    // SAFETY: the caller vouches for the handler, and `restorer` is this
    // library's.
    if let Err(error) = unsafe { kernel_sigaction(sig, new.as_ref(), wanted) } {
        errno::set(error);
        return -1;
    }
    if !old.is_null() {
        // SAFETY: the caller passes a writable action. `act` was read before
        // the call, so writing this cannot change what the kernel saw.
        unsafe { old.write(previous.to_c()) };
    }
    0
}

/// Installs `handler` for `sig` with BSD semantics, and returns the previous
/// handler, or `SIG_ERR` with `errno` set.
///
/// BSD semantics, as glibc and musl give `signal`: the handler stays installed
/// after it runs, the signal is blocked while it runs, and system calls it
/// interrupts restart.
///
/// # Safety
///
/// `handler` must be `SIG_DFL`, `SIG_IGN`, or a function that is sound to call
/// when the signal arrives.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn signal(sig: c_int, handler: Handler) -> Handler {
    let action = Sigaction {
        handler,
        flags: SA_RESTART,
        ..Sigaction::DEFAULT
    };
    let mut old = Sigaction::DEFAULT;
    // SAFETY: both are live locals, and the caller vouches for the handler.
    if unsafe { sigaction(sig, &raw const action, &raw mut old) } < 0 {
        return SIG_ERR;
    }
    old.handler
}

/// [`signal`], by the name 4.2BSD gave it.
///
/// # Safety
///
/// As [`signal`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn bsd_signal(sig: c_int, handler: Handler) -> Handler {
    // SAFETY: the caller's promise is the same.
    unsafe { signal(sig, handler) }
}

/// Makes a system call that `sig` interrupts fail with `EINTR` if `flag` is
/// nonzero, or restart if it is zero.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn siginterrupt(sig: c_int, flag: c_int) -> c_int {
    let mut action = Sigaction::DEFAULT;
    // SAFETY: `action` is a live local, and nothing is installed.
    if unsafe { sigaction(sig, null(), &raw mut action) } < 0 {
        return -1;
    }
    if flag != 0 {
        action.flags &= !SA_RESTART;
    } else {
        action.flags |= SA_RESTART;
    }
    // SAFETY: the handler is the one already installed.
    unsafe { sigaction(sig, &raw const action, null_mut()) }
}

/// Ignores `sig`. System V.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sigignore(sig: c_int) -> c_int {
    let action = Sigaction {
        handler: SIG_IGN,
        ..Sigaction::DEFAULT
    };
    // SAFETY: `action` is a live local, and ignoring a signal runs no code.
    unsafe { sigaction(sig, &raw const action, null_mut()) }
}

/// System V's `sigset`: installs `handler` for `sig` and unblocks it, or with
/// `SIG_HOLD` blocks it and leaves the handler alone.
///
/// Returns `SIG_HOLD` if the signal was blocked before, and otherwise the
/// previous handler, or `SIG_ERR` with `errno` set.
///
/// # Safety
///
/// As [`signal`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigset(sig: c_int, handler: Handler) -> Handler {
    let mut mask = SigSet::EMPTY;
    // SAFETY: `mask` is a live local.
    if unsafe { sigset::sigaddset(&raw mut mask, sig) } < 0 {
        return SIG_ERR;
    }
    let mut old = Sigaction::DEFAULT;
    let mut old_mask = SigSet::EMPTY;
    let how = if handler == SIG_HOLD {
        // SAFETY: `old` is a live local, and nothing is installed.
        if unsafe { sigaction(sig, null(), &raw mut old) } < 0 {
            return SIG_ERR;
        }
        SIG_BLOCK
    } else {
        let action = Sigaction {
            handler,
            ..Sigaction::DEFAULT
        };
        // SAFETY: both are live locals, and the caller vouches for the
        // handler.
        if unsafe { sigaction(sig, &raw const action, &raw mut old) } < 0 {
            return SIG_ERR;
        }
        SIG_UNBLOCK
    };
    // SAFETY: both are live locals.
    if unsafe { sigset::sigprocmask(how, &raw const mask, &raw mut old_mask) } < 0 {
        return SIG_ERR;
    }
    if old_mask.word() & mask.word() != 0 {
        SIG_HOLD
    } else {
        old.handler
    }
}

/// The restorer every installed action returns through.
pub fn restorer() -> usize {
    (__restore_rt as unsafe extern "C" fn() as *const ()).addr()
}

// The x86-64 restorer.
//
// Debuggers and unwinders have no unwind information for a signal frame. They
// recognise one by the code its return address points at: gdb and libgcc's
// fallback unwinder compare the bytes there with `mov $15, %rax; syscall`,
// encoded as
// `48 c7 c0 0f 00 00 00 0f 05`. So the restorer must be exactly that
// sequence, and no shorter encoding of the same instruction. A unit test reads
// the bytes back.
//
// The `nop` before it is part of the same contract. An unwinder looks up the
// unwind entry for a return address minus one, since a call's return address
// may be the first byte of the next function. With no `nop`, that byte would
// belong to whatever the linker placed before the restorer, and the unwinder
// would use that function's entry instead of recognising the signal frame.
//
// glibc and musl both name it `__restore_rt`. It is hidden: nothing outside the
// library calls it by name.
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".pushsection .text.__restore_rt,\"ax\",@progbits",
    "nop",
    ".globl __restore_rt",
    ".hidden __restore_rt",
    ".type __restore_rt,@function",
    "__restore_rt:",
    "movq ${rt_sigreturn}, %rax",
    "syscall",
    ".size __restore_rt,.-__restore_rt",
    ".popsection",
    rt_sigreturn = const nr::RT_SIGRETURN,
    options(att_syntax),
);

// The bytes unwinders match hold the number 15 itself.
#[cfg(target_arch = "x86_64")]
const _: () = assert!(nr::RT_SIGRETURN == 15);

#[cfg(target_arch = "x86_64")]
unsafe extern "C" {
    /// Returns from a signal handler by calling `rt_sigreturn`. Only its
    /// address is used; calling it from anywhere but a signal frame is
    /// undefined.
    fn __restore_rt();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_restorer_is_the_sequence_unwinders_recognise() {
        let code = core::ptr::with_exposed_provenance::<[u8; 9]>(restorer());
        // SAFETY: the restorer is nine bytes of code in this binary's text,
        // which is readable.
        let bytes = unsafe { code.read() };
        assert_eq!(bytes, [0x48, 0xc7, 0xc0, 0x0f, 0, 0, 0, 0x0f, 0x05]);
        let before = core::ptr::with_exposed_provenance::<u8>(restorer() - 1);
        // SAFETY: the byte before it is the `nop` in the same section.
        assert_eq!(unsafe { before.read() }, 0x90);
    }

    #[test]
    fn actions_convert_both_ways() {
        let action = Sigaction {
            handler: 0x1234,
            mask: SigSet::from_word(1 << 9 | 1 << 63),
            flags: SA_SIGINFO | SA_RESETHAND | SA_ONSTACK,
            restorer: 0x999,
        };
        let kernel = KernelSigaction::from_c(&action, 0x5678);
        assert_eq!(
            kernel,
            KernelSigaction {
                handler: 0x1234,
                flags: 0x8c00_0004,
                restorer: 0x5678,
                mask: 1 << 9 | 1 << 63,
            }
        );
        let back = kernel.to_c();
        assert_eq!(back.handler, 0x1234);
        assert_eq!(back.mask, action.mask);
        assert_eq!(back.flags, action.flags | SA_RESTORER);
        assert_eq!(back.restorer, 0x5678);
    }

    #[test]
    fn reserved_and_nonexistent_signals_are_refused_before_the_kernel() {
        for sig in [0, 32, 33, 34, 65, -1] {
            errno::set(0);
            let mut old = Sigaction::DEFAULT;
            // SAFETY: `old` is a live local, and nothing is installed.
            assert_eq!(unsafe { sigaction(sig, null(), &raw mut old) }, -1);
            // SAFETY: the pointer is this thread's `errno`.
            assert_eq!(unsafe { errno::__errno_location().read() }, errno::EINVAL);
        }
    }
}
