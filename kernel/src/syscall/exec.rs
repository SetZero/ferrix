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

use crate::syscall::load::{self, LoadError};
use crate::syscall::process::{self, Process, Startup};
use crate::syscall::uaccess;
use crate::user::space::{AddressSpace, SpaceError};

/// How much address space a program's stack gets.
///
/// Eight megabytes, which is what `RLIMIT_STACK` defaults to on Linux and
/// what a program's own guard-page arithmetic assumes. It costs nothing until
/// touched: the pages arrive on fault.
const STACK_SIZE: u64 = 8 * 1024 * 1024;

/// Address space kept inaccessible beneath the stack.
///
/// A mebibyte, which is Linux's `stack_guard_gap`. Reserved rather than
/// merely left free, because `mmap` searches for free space from the top of
/// the user half downwards and would otherwise place the next mapping flush
/// against the bottom of the stack -- where an overflow writes into it rather
/// than faulting.
const STACK_GUARD: u64 = 1024 * 1024;

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
    /// The program was loaded, but its task could not be started.
    Start(&'static str),
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

/// Load `image` into a new process, ready to run and not yet running.
///
/// # Errors
///
/// [`ExecError`].
pub(crate) fn load(
    image: &[u8],
    args: &[&[u8]],
    env: &[&[u8]],
    random: [u8; ferrix_ustack::RANDOM_BYTES],
) -> Result<Arc<Process>, ExecError> {
    let space = AddressSpace::new().map_err(ExecError::Space)?;
    let process = Process::new(Arc::clone(&space));

    let loaded = load::load(&space, image).map_err(ExecError::Load)?;
    process.set_heap_base(loaded.end);

    // The stack region. Reserved whole; paid for a page at a time.
    let top = stack_top();
    let low = top - STACK_SIZE;
    let _ = space
        .map_anonymous(low, STACK_SIZE, VmaFlags::READ_WRITE)
        .map_err(ExecError::Space)?;

    // Guard regions either side of the stack, with no access at all. Not
    // decoration: the first busybox run's first `mmap` landed at
    // 0x7FFFFFFFF000, the page this leaves unmapped above the stack, because
    // the free-space search runs top-down and that page was the highest hole.
    let _ = space
        .map_anonymous(top, USER_VIRT_END - top, VmaFlags::NONE)
        .map_err(ExecError::Space)?;
    if let Some(guard_low) = low.checked_sub(STACK_GUARD) {
        let _ = space
            .map_anonymous(guard_low, STACK_GUARD, VmaFlags::NONE)
            .map_err(ExecError::Space)?;
    }

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

    process.set_startup(Startup {
        entry: loaded.entry,
        stack: startup.sp,
    });
    Ok(Arc::new(process))
}

/// Load `image`, run it as a task of its own, and wait for it to end.
///
/// Returns the status the program exited with. The caller blocks for as long
/// as the program runs, which is the point for the first program and for the
/// checks; everything else wants [`load`] and [`process::start`] separately.
///
/// # Errors
///
/// [`ExecError`].
pub(crate) fn run(
    image: &[u8],
    args: &[&[u8]],
    env: &[&[u8]],
    random: [u8; ferrix_ustack::RANDOM_BYTES],
) -> Result<i32, ExecError> {
    let process = load(image, args, env, random)?;
    let _task = process::start(&process).map_err(ExecError::Start)?;
    process
        .wait_for_exit(u64::MAX)
        .ok_or(ExecError::Start("the program never reported how it ended"))
}
