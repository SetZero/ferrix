//! Turning an ELF image into a running program.
//!
//! The three pieces that already exist — the loader, `libs/ustack`, and the
//! architecture's way into ring 3 — meet here, and they meet in exactly one
//! place on purpose. The first process and `execve` need the same two numbers
//! by different routes, and the way those two routes drift apart is by each
//! assembling the numbers itself.
//!
//! # Where the stack comes from, and why not from the loader
//!
//! The loader maps what the ELF says to map and nothing else. A stack is not
//! in the ELF: its size is a policy, its address is a policy, and what goes on
//! it — the argument vector, the environment, the auxiliary vector — comes
//! from the caller and from the loader's *results*, not from the image. So the
//! loader returns `AT_PHDR` and friends and this function decides the rest.

use alloc::sync::Arc;
use alloc::vec;

use ferrix_bootinfo::{PAGE_SIZE, USER_VIRT_END};
use ferrix_linux_abi::types::{
    AT_CLKTCK, AT_EGID, AT_ENTRY, AT_EUID, AT_GID, AT_PAGESZ, AT_PHDR, AT_PHENT, AT_PHNUM,
    AT_SECURE, AT_UID,
};
use ferrix_ustack::{Spec, Width};
use ferrix_vma::VmaFlags;

use crate::arch;
use crate::syscall::load::{self, LoadError};
use crate::syscall::process::Process;
use crate::syscall::uaccess;
use crate::user::space::{AddressSpace, SpaceError};

/// How much address space a program's stack gets.
///
/// Eight megabytes, which is what `RLIMIT_STACK` defaults to on Linux and
/// what a program's own guard-page arithmetic assumes. It costs nothing until
/// touched: the pages arrive on fault.
const STACK_SIZE: u64 = 8 * 1024 * 1024;

/// The most the argument vector, environment and auxiliary vector may occupy.
///
/// Built in kernel memory and copied in, so this bounds a kernel allocation
/// rather than a user one. Linux's own limit is a quarter of the stack rlimit,
/// which would be two megabytes here; this is smaller because nothing yet
/// needs more and a page of kernel heap per `execve` is cheap.
const STARTUP_BYTES: usize = 16 * 1024;

/// Why a program could not be started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecError {
    /// The image could not be loaded.
    Load(LoadError),
    /// The address space refused the stack.
    Space(SpaceError),
    /// The startup image did not fit, or could not be written.
    Startup,
    /// This architecture cannot enter user mode yet.
    NoUserMode(&'static str),
}

/// Where the stack goes: as high in the user half as a page allows.
///
/// Below `USER_VIRT_END` rather than at it, because the top page is left
/// unmapped deliberately — a program that walks off the end of its stack
/// should fault rather than wrap to zero.
fn stack_top() -> u64 {
    (USER_VIRT_END - PAGE_SIZE) & !(ferrix_ustack::STACK_ALIGN - 1)
}

/// This build's pointer width, as `libs/ustack` wants it told.
///
/// From the width rather than from a `cfg`, because that is what the question
/// actually is, and because generic kernel code naming an architecture is what
/// the layering check forbids.
fn width() -> Width {
    if size_of::<usize>() == 8 {
        Width::Bits64
    } else {
        Width::Bits32
    }
}

/// Load `image` into a fresh address space and run it.
///
/// Returns the status the program exited with.
///
/// # Errors
///
/// [`ExecError`]. Nothing is left installed on this processor either way.
pub(crate) fn run(
    image: &[u8],
    args: &[&[u8]],
    env: &[&[u8]],
    random: [u8; ferrix_ustack::RANDOM_BYTES],
) -> Result<i32, ExecError> {
    let space = AddressSpace::new().map_err(ExecError::Space)?;
    let process = Process::new(Arc::clone(&space));

    let loaded = load::load(&space, image).map_err(ExecError::Load)?;

    // The stack region. Reserved whole; paid for a page at a time.
    let top = stack_top();
    let low = top - STACK_SIZE;
    let _ = space
        .map_anonymous(low, STACK_SIZE, VmaFlags::READ_WRITE)
        .map_err(ExecError::Space)?;

    // Build the startup image in kernel memory, then copy it in. It cannot be
    // built in place: `libs/ustack` needs a `&mut [u8]` and the only way to
    // reach user memory is through the copy layer, one page at a time.
    let mut scratch = vec![0_u8; STARTUP_BYTES];
    let base = top - STARTUP_BYTES as u64;
    let auxv = [
        (AT_PAGESZ, PAGE_SIZE),
        (AT_PHDR, loaded.phdr),
        (AT_PHENT, loaded.phent),
        (AT_PHNUM, loaded.phnum),
        (AT_ENTRY, loaded.entry),
        // No credentials yet, and no set-user-id path that could change them,
        // so `AT_SECURE` is honestly zero rather than defensively one.
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_SECURE, 0),
        (AT_CLKTCK, 100),
    ];
    let exec_fn = args.first().copied().unwrap_or(b"");
    let spec = Spec {
        args,
        env,
        auxv: &auxv,
        random,
        exec_fn,
        platform: None,
        width: width(),
    };
    let startup = ferrix_ustack::build(&spec, top, &mut scratch).map_err(|_| ExecError::Startup)?;
    uaccess::copy_to_user(&space, base, &scratch).map_err(|_| ExecError::Startup)?;

    // From here the program is the one running, which is what a system call
    // and a page fault from ring 3 both look up.
    let previous = crate::syscall::process::set_current(Some(Arc::new(process)));
    let outcome = enter(&space, loaded.entry, startup.sp);
    let _ = crate::syscall::process::set_current(previous);
    outcome
}

/// Install the space, run the program, and take the space down again.
///
/// Interrupts are masked across the window for the reason
/// [`AddressSpace::install`] gives: until the scheduler knows about address
/// spaces, being preempted here would leave another task running with this
/// program's translations installed.
fn enter(space: &Arc<AddressSpace>, entry: u64, stack: u64) -> Result<i32, ExecError> {
    use ferrix_sync::IrqControl;

    let state = <arch::Irq as IrqControl>::disable();

    // SAFETY: `space` is held by the caller for the whole of this function,
    // and interrupts are masked so nothing else runs on this processor.
    unsafe { space.install() };

    // SAFETY: the space is installed, and `entry` and `stack` came from the
    // loader and `libs/ustack` respectively, both of which produced addresses
    // inside it.
    let outcome = unsafe { arch::run_user(entry, stack) };

    // SAFETY: the program is gone; nothing on this processor needs a user
    // address any more.
    unsafe { crate::user::space::uninstall() };
    <arch::Irq as IrqControl>::restore(state);

    outcome.map_err(ExecError::NoUserMode)
}
