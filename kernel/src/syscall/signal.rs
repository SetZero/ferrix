//! Signal dispositions, recorded and reported back, and nothing delivered.
//!
//! # Why a table with no delivery behind it
//!
//! Because that is exactly what a program starting up asks for. musl's
//! startup and busybox's `ash` both install handlers and adjust the blocked
//! mask before they do anything else, and they read the previous values back
//! to restore them later. Nothing in a clean run *raises* a signal, so nothing
//! ever looks for the handler. What a program does see is whether the answers
//! are consistent: the `oldact` from the second `rt_sigaction` has to be the
//! `act` from the first.
//!
//! Delivery -- a frame pushed on the user stack and `rt_sigreturn` to unwind
//! it -- is a large piece of work of its own, and it belongs with the first
//! thing that has to kill a program. When it arrives it reads this table; it
//! does not replace it.
//!
//! # Layouts
//!
//! Both structures are built from native words, so they are narrower on
//! ARMv7-A, and they are read and written as native words here for the reason
//! `writev` gives: `libs/linux-abi`'s `Sigaction` and `Stack` are the 64-bit
//! layouts, and using them for a 32-bit program would read its fields at twice
//! the stride.
//!
//! * `struct sigaction`, the kernel's and not the C library's: handler, flags
//!   and restorer as three words, then the 8-byte mask. All three
//!   architectures define `SA_RESTORER`, so the field is present on each.
//! * `stack_t`: the stack pointer as a word, the flags as an `int`, the size
//!   as a word. On a 64-bit machine the `int` is padded to the word, which is
//!   why the size sits at the second word on both widths.

use ferrix_bootinfo::Arch;
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{
    NSIG, SIG_BLOCK, SIG_SETMASK, SIG_UNBLOCK, SIGKILL, SIGSTOP, SS_DISABLE, SS_ONSTACK,
};

use crate::arch;
use crate::syscall::process::Process;
use crate::syscall::uaccess;

/// The only `sigsetsize` the kernel accepts: one 64-bit word.
///
/// The C library's own `sigset_t` is far larger, and the system call takes a
/// size precisely so the two can differ. Linux refuses anything else with
/// `EINVAL`, and so does this.
pub(crate) const SIGSET_SIZE: u64 = 8;

/// `SS_AUTODISARM`: clear the alternate stack when a handler is entered on
/// it. Not in `libs/linux-abi`; it is a flag *bit* on top of the mode, and the
/// mode check has to take it off before comparing.
const SS_AUTODISARM: i32 = i32::MIN;

/// Bytes in a native word.
const WORD: usize = size_of::<usize>();

/// Bytes in the kernel's `struct sigaction` on this architecture.
const SIGACTION_BYTES: usize = WORD * 3 + 8;

/// Bytes in `stack_t` on this architecture.
const STACK_BYTES: usize = WORD * 3;

/// The two signals nothing may catch, block or ignore.
const UNBLOCKABLE: u64 = bit(SIGKILL) | bit(SIGSTOP);

/// What a program asked to happen on one signal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Disposition {
    /// The handler address, or `SIG_DFL` or `SIG_IGN`.
    pub(crate) handler: u64,
    /// The `SA_*` flags.
    pub(crate) flags: u64,
    /// The trampoline that would issue `rt_sigreturn`.
    pub(crate) restorer: u64,
    /// Signals blocked while the handler runs, never including the two that
    /// cannot be blocked.
    pub(crate) mask: u64,
}

/// An installed alternate signal stack. A size of zero means none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AltStack {
    /// Its lowest address.
    sp: u64,
    /// Its size in bytes.
    size: u64,
    /// Whether `SS_AUTODISARM` was asked for.
    autodisarm: bool,
}

/// Everything about signals a process has told the kernel.
#[derive(Debug)]
pub(crate) struct Signals {
    /// Signal `n` is at index `n - 1`.
    actions: [Disposition; NSIG as usize],
    /// The blocked mask, bit `n - 1` for signal `n`.
    blocked: u64,
    /// The alternate stack, if one is installed.
    alt: AltStack,
}

impl Default for Signals {
    fn default() -> Self {
        Signals {
            actions: [Disposition::default(); NSIG as usize],
            blocked: 0,
            alt: AltStack::default(),
        }
    }
}

/// The mask bit for signal `number`, which must be `1..=64`.
const fn bit(number: u32) -> u64 {
    1 << (number - 1)
}

/// `rt_sigaction`.
///
/// Linux's order, which a program can observe: the new action is read before
/// anything changes, so a bad `act` pointer changes nothing; the old action
/// is written after, so a bad `oldact` pointer reports `EFAULT` with the new
/// action already installed.
pub(crate) fn sys_rt_sigaction(
    process: &Process,
    signal: u32,
    act: u64,
    old: u64,
    sigsetsize: u64,
) -> Result<usize, Errno> {
    if sigsetsize != SIGSET_SIZE {
        return Err(Errno::EINVAL);
    }
    let index = index_of(signal)?;
    let new = if act == 0 {
        None
    } else if signal == SIGKILL || signal == SIGSTOP {
        return Err(Errno::EINVAL);
    } else {
        Some(read_sigaction(process, act)?)
    };

    let previous = process.with_signals(|signals| {
        let slot = signals.actions.get_mut(index).ok_or(Errno::EINVAL)?;
        let previous = *slot;
        if let Some(mut new) = new {
            new.mask &= !UNBLOCKABLE;
            *slot = new;
        }
        Ok(previous)
    })?;

    if old != 0 {
        write_sigaction(process, old, previous)?;
    }
    Ok(0)
}

/// `rt_sigprocmask`.
///
/// `how` is only looked at when there is a set to apply, as on Linux: a query
/// with a nonsense `how` succeeds. When `how` is refused the old mask is not
/// written either.
pub(crate) fn sys_rt_sigprocmask(
    process: &Process,
    how: u32,
    set: u64,
    old: u64,
    sigsetsize: u64,
) -> Result<usize, Errno> {
    if sigsetsize != SIGSET_SIZE {
        return Err(Errno::EINVAL);
    }
    let request = if set == 0 {
        None
    } else {
        let mut bytes = [0_u8; 8];
        uaccess::copy_from_user(process.space(), set, &mut bytes).map_err(|_| Errno::EFAULT)?;
        Some(u64::from_le_bytes(bytes) & !UNBLOCKABLE)
    };

    let previous = process.with_signals(|signals| {
        let previous = signals.blocked;
        if let Some(request) = request {
            signals.blocked = match how {
                SIG_BLOCK => previous | request,
                SIG_UNBLOCK => previous & !request,
                SIG_SETMASK => request,
                _ => return Err(Errno::EINVAL),
            };
        }
        Ok(previous)
    })?;

    if old != 0 {
        uaccess::copy_to_user(process.space(), old, &previous.to_le_bytes())
            .map_err(|_| Errno::EFAULT)?;
    }
    Ok(0)
}

/// `sigaltstack`.
///
/// The old stack is reported only if the new one was accepted, which is
/// Linux's order. Nothing ever runs on the alternate stack yet, so the old
/// flags are never `SS_ONSTACK` and a change is never refused with `EPERM`.
pub(crate) fn sys_sigaltstack(process: &Process, ss: u64, old: u64) -> Result<usize, Errno> {
    let request = if ss == 0 {
        None
    } else {
        Some(read_stack(process, ss)?)
    };

    let previous = process.with_signals(|signals| {
        let previous = signals.alt;
        if let Some((sp, flags, size)) = request {
            let autodisarm = flags & SS_AUTODISARM != 0;
            signals.alt = match flags & !SS_AUTODISARM {
                SS_DISABLE => AltStack {
                    sp: 0,
                    size: 0,
                    autodisarm,
                },
                0 | SS_ONSTACK if size < minimum_stack() => return Err(Errno::ENOMEM),
                0 | SS_ONSTACK => AltStack {
                    sp,
                    size,
                    autodisarm,
                },
                _ => return Err(Errno::EINVAL),
            };
        }
        Ok(previous)
    })?;

    if old != 0 {
        let mut flags = if previous.size == 0 { SS_DISABLE } else { 0 };
        if previous.autodisarm {
            flags |= SS_AUTODISARM;
        }
        write_stack(process, old, previous.sp, flags, previous.size)?;
    }
    Ok(0)
}

/// `MINSIGSTKSZ` for this architecture: the smallest alternate stack the
/// kernel accepts. AArch64's is larger because its signal frame carries the
/// full SIMD state.
const fn minimum_stack() -> u64 {
    match arch::ARCH {
        Arch::AArch64 => 5120,
        Arch::X86_64 | Arch::Armv7a => 2048,
    }
}

/// The table index for `signal`, or `EINVAL` outside `1..=64`.
fn index_of(signal: u32) -> Result<usize, Errno> {
    if signal == 0 || signal > NSIG {
        return Err(Errno::EINVAL);
    }
    usize::try_from(signal - 1).map_err(|_| Errno::EINVAL)
}

/// Read a `struct sigaction` from the program.
fn read_sigaction(process: &Process, at: u64) -> Result<Disposition, Errno> {
    let mut buffer = [0_u8; 32];
    let bytes = buffer.get_mut(..SIGACTION_BYTES).ok_or(Errno::EINVAL)?;
    uaccess::copy_from_user(process.space(), at, bytes).map_err(|_| Errno::EFAULT)?;
    let mut mask = [0_u8; 8];
    mask.copy_from_slice(bytes.get(WORD * 3..).ok_or(Errno::EINVAL)?);
    Ok(Disposition {
        handler: word_at(bytes, 0)?,
        flags: word_at(bytes, WORD)?,
        restorer: word_at(bytes, WORD * 2)?,
        mask: u64::from_le_bytes(mask),
    })
}

/// Write a `struct sigaction` to the program.
fn write_sigaction(process: &Process, at: u64, action: Disposition) -> Result<(), Errno> {
    let mut buffer = [0_u8; 32];
    let bytes = buffer.get_mut(..SIGACTION_BYTES).ok_or(Errno::EINVAL)?;
    put_word(bytes, 0, action.handler)?;
    put_word(bytes, WORD, action.flags)?;
    put_word(bytes, WORD * 2, action.restorer)?;
    bytes
        .get_mut(WORD * 3..)
        .ok_or(Errno::EINVAL)?
        .copy_from_slice(&action.mask.to_le_bytes());
    uaccess::copy_to_user(process.space(), at, bytes).map_err(|_| Errno::EFAULT)
}

/// Read a `stack_t` from the program: pointer, flags, size.
fn read_stack(process: &Process, at: u64) -> Result<(u64, i32, u64), Errno> {
    let mut buffer = [0_u8; 24];
    let bytes = buffer.get_mut(..STACK_BYTES).ok_or(Errno::EINVAL)?;
    uaccess::copy_from_user(process.space(), at, bytes).map_err(|_| Errno::EFAULT)?;
    let mut flags = [0_u8; 4];
    flags.copy_from_slice(bytes.get(WORD..WORD + 4).ok_or(Errno::EINVAL)?);
    Ok((
        word_at(bytes, 0)?,
        i32::from_le_bytes(flags),
        word_at(bytes, WORD * 2)?,
    ))
}

/// Write a `stack_t` to the program. Padding goes out as zero.
fn write_stack(process: &Process, at: u64, sp: u64, flags: i32, size: u64) -> Result<(), Errno> {
    let mut buffer = [0_u8; 24];
    let bytes = buffer.get_mut(..STACK_BYTES).ok_or(Errno::EINVAL)?;
    put_word(bytes, 0, sp)?;
    bytes
        .get_mut(WORD..WORD + 4)
        .ok_or(Errno::EINVAL)?
        .copy_from_slice(&flags.to_le_bytes());
    put_word(bytes, WORD * 2, size)?;
    uaccess::copy_to_user(process.space(), at, bytes).map_err(|_| Errno::EFAULT)
}

/// The native word at `offset`, zero-extended.
fn word_at(bytes: &[u8], offset: usize) -> Result<u64, Errno> {
    let end = offset.checked_add(WORD).ok_or(Errno::EINVAL)?;
    let mut word = [0_u8; 8];
    word.get_mut(..WORD)
        .ok_or(Errno::EINVAL)?
        .copy_from_slice(bytes.get(offset..end).ok_or(Errno::EINVAL)?);
    Ok(u64::from_le_bytes(word))
}

/// Put `value` at `offset` as a native word. On a 32-bit machine every value
/// written here came from a 32-bit read, so the truncation drops nothing.
fn put_word(bytes: &mut [u8], offset: usize, value: u64) -> Result<(), Errno> {
    let end = offset.checked_add(WORD).ok_or(Errno::EINVAL)?;
    bytes
        .get_mut(offset..end)
        .ok_or(Errno::EINVAL)?
        .copy_from_slice(value.to_le_bytes().get(..WORD).ok_or(Errno::EINVAL)?);
    Ok(())
}
