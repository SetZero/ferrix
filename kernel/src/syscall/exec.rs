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
use alloc::vec::Vec;

use ferrix_bootinfo::{PAGE_SIZE, USER_VIRT_END};
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{
    AT_CLKTCK, AT_EGID, AT_ENTRY, AT_EUID, AT_GID, AT_PAGESZ, AT_PHDR, AT_PHENT, AT_PHNUM,
    AT_SECURE, AT_UID,
};
use ferrix_ustack::{Spec, Width};
use ferrix_vma::VmaFlags;

use crate::arch;
use crate::syscall::load::{self, LoadError};
use crate::syscall::process::{self, Process, Startup};
use crate::syscall::registry;
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
/// which would be two megabytes here. This is a quarter megabyte: enough for
/// `xargs` and `find -exec` building long command lines, which the first limit
/// of sixteen kilobytes was not, and still one allocation per `execve`.
const STARTUP_BYTES: usize = 256 * 1024;

/// The longest path `execve` accepts, Linux's `PATH_MAX`.
const PATH_MAX: usize = 4096;

/// The longest `#!` line read, Linux's `BINPRM_BUF_SIZE`.
const INTERPRETER_LINE: usize = 256;

/// The status a process ends with when `execve` fails after it has already
/// taken the old program's memory away: 128 plus `SIGSEGV`, which is what
/// Linux kills it with.
const LOST_STATUS: i32 = 128 + 11;

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
    let startup = populate(&space, &process, image, args, env, random)?;
    process.set_startup(startup);
    Ok(registry::register(process))
}

/// Load `image` into `space`, which must be empty, and build its startup stack:
/// what a new process and `execve` share.
///
/// # Errors
///
/// [`ExecError`].
fn populate(
    space: &AddressSpace,
    process: &Process,
    image: &[u8],
    args: &[&[u8]],
    env: &[&[u8]],
    random: [u8; ferrix_ustack::RANDOM_BYTES],
) -> Result<Startup, ExecError> {
    let loaded = load::load(space, image).map_err(ExecError::Load)?;
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
    uaccess::copy_to_user(space, base, &scratch).map_err(|_| ExecError::Startup)?;

    process.record_exec(exec_fn, args);
    Ok(Startup {
        entry: loaded.entry,
        stack: startup.sp,
    })
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

/// How `execve` failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecveError {
    /// Refused before anything changed; the caller returns this error.
    Refused(Errno),
    /// Failed after the old program's memory was already gone, so there is
    /// nothing to return to: the caller ends the process.
    Lost,
}

impl From<Errno> for ExecveError {
    fn from(error: Errno) -> Self {
        ExecveError::Refused(error)
    }
}

/// `execve`: replace the running program with the one at `path`.
///
/// Answers the new program's entry point and stack pointer, for the trap path
/// to enter.
///
/// # The point of no return
///
/// Everything that can be refused is refused first -- the path, the argument
/// and environment strings, the file, a `#!` interpreter, and the ELF headers
/// -- because until then a failure is an error the old program can handle.
/// Then the old program's memory goes, and a failure from there on ends the
/// process, as it does on Linux: loading into the emptied space rather than
/// building a new one keeps the process's identity -- its address space,
/// its task, everything that holds a reference to either -- exactly as it was.
///
/// # Errors
///
/// [`ExecveError`].
pub(crate) fn sys_execve(
    process: &Process,
    path: u64,
    argv: u64,
    envp: u64,
) -> Result<(u64, u64), ExecveError> {
    let space = process.space();
    let mut path_bytes = Vec::new();
    uaccess::copy_cstr_from_user(space, path, PATH_MAX, &mut path_bytes)
        .map_err(|_| Errno::EFAULT)?;
    if path_bytes.is_empty() {
        return Err(Errno::ENOENT.into());
    }

    let mut budget = STARTUP_BYTES;
    let mut args = read_strings(space, argv, &mut budget)?;
    let env = read_strings(space, envp, &mut budget)?;

    // The caller's own root and working directory, so a relative path after
    // `cd` resolves from there. Cloned out: never walk with the lock held.
    let context = process.fs_context().lock().clone();
    let mut image = crate::fs::read_file(&context, None, &path_bytes)?;

    // A script names its interpreter on its first line, which runs with the
    // script's path in place of its own first argument -- what Linux's
    // `binfmt_script` does. One level: an interpreter that is itself a script
    // is refused rather than followed.
    if image.starts_with(b"#!") {
        let (interpreter, argument) = interpreter_line(&image)?;
        let mut replaced = Vec::with_capacity(args.len().saturating_add(2));
        replaced.push(interpreter.clone());
        if let Some(argument) = argument {
            replaced.push(argument);
        }
        replaced.push(path_bytes);
        replaced.extend(args.into_iter().skip(1));
        args = replaced;
        image = crate::fs::read_file(&context, None, &interpreter)?;
        if image.starts_with(b"#!") {
            return Err(Errno::ENOEXEC.into());
        }
    }
    load::check(&image).map_err(|_| Errno::ENOEXEC)?;

    let arg_slices: Vec<&[u8]> = args.iter().map(Vec::as_slice).collect();
    let env_slices: Vec<&[u8]> = env.iter().map(Vec::as_slice).collect();

    // The point of no return.
    empty_user_half(space).map_err(|_| ExecveError::Lost)?;
    process.reset_for_exec();
    let startup = populate(
        space,
        process,
        &image,
        &arg_slices,
        &env_slices,
        random_bytes(),
    )
    .map_err(|_| ExecveError::Lost)?;
    process.set_startup(startup);

    // Descriptors marked close-on-exec go, dropped after the table's lock is
    // let go, since closing one can wake whatever waits on it. And a `vfork`
    // parent, asleep since the fork, may run again.
    let closed = process.files().lock().take_cloexec();
    drop(closed);
    process.mark_execed();

    // The old program's thread pointer and floating-point state are its own
    // and must not reach the new one. The registers are still live on this
    // processor, inside this task's own system call.
    // SAFETY: called by the user task whose registers these are.
    unsafe { arch::reset_user_state() };

    Ok((startup.entry, startup.stack))
}

/// What a failed `execve` past its point of no return ends the process with.
pub(crate) const fn lost_status() -> i32 {
    LOST_STATUS
}

/// Take every mapping out of the user half.
fn empty_user_half(space: &AddressSpace) -> Result<(), SpaceError> {
    space
        .unmap(0, USER_VIRT_END)
        .or_else(|_| space.unmap(PAGE_SIZE, USER_VIRT_END - PAGE_SIZE))
}

/// Read a `NULL`-terminated array of string pointers from the program, as
/// `argv` and `envp` are passed, charging each string to `budget`.
///
/// A null array is an empty one, which Linux accepts for both.
fn read_strings(space: &AddressSpace, at: u64, budget: &mut usize) -> Result<Vec<Vec<u8>>, Errno> {
    let mut strings = Vec::new();
    if at == 0 {
        return Ok(strings);
    }
    let word = size_of::<usize>();
    let stride = word as u64;
    let mut slot = at;
    loop {
        let mut bytes = [0_u8; 8];
        let target = bytes.get_mut(..word).ok_or(Errno::EINVAL)?;
        uaccess::copy_from_user(space, slot, target).map_err(|_| Errno::EFAULT)?;
        let pointer = u64::from_le_bytes(bytes);
        if pointer == 0 {
            return Ok(strings);
        }
        let mut string = Vec::new();
        uaccess::copy_cstr_from_user(space, pointer, ferrix_ustack::MAX_ARG_STRLEN, &mut string)
            .map_err(|_| Errno::EFAULT)?;
        // The string, its terminator and its pointer, which is what it costs
        // on the new program's stack.
        let cost = string.len().saturating_add(1).saturating_add(word);
        *budget = budget.checked_sub(cost).ok_or(Errno::E2BIG)?;
        strings.push(string);
        slot = slot.checked_add(stride).ok_or(Errno::EFAULT)?;
    }
}

/// The interpreter and its one optional argument from a `#!` line.
fn interpreter_line(image: &[u8]) -> Result<(Vec<u8>, Option<Vec<u8>>), Errno> {
    let line = image.get(2..).ok_or(Errno::ENOEXEC)?;
    let end = line
        .iter()
        .take(INTERPRETER_LINE)
        .position(|&byte| byte == b'\n')
        .unwrap_or_else(|| line.len().min(INTERPRETER_LINE));
    let line = line.get(..end).ok_or(Errno::ENOEXEC)?;
    let line = line.trim_ascii();
    let split = line
        .iter()
        .position(u8::is_ascii_whitespace)
        .unwrap_or(line.len());
    let (interpreter, rest) = line.split_at(split);
    if interpreter.is_empty() {
        return Err(Errno::ENOEXEC);
    }
    let rest = rest.trim_ascii();
    let argument = (!rest.is_empty()).then(|| rest.to_vec());
    Ok((interpreter.to_vec(), argument))
}

/// Sixteen bytes for `AT_RANDOM`.
///
/// **Not random.** Two readings of the high-resolution counter, which differ
/// from boot to boot and from program to program, and are good enough that a
/// libc's stack-protector canary is not the same constant everywhere. They are
/// not good enough for anything an attacker is involved in, and nothing here
/// pretends otherwise: the entropy pool is a later stage's.
pub(crate) fn random_bytes() -> [u8; ferrix_ustack::RANDOM_BYTES] {
    let first = arch::counter_now().to_le_bytes();
    let second = arch::counter_now().rotate_left(29).to_le_bytes();
    let mut bytes = [0_u8; ferrix_ustack::RANDOM_BYTES];
    for (slot, value) in bytes.iter_mut().zip(first.iter().chain(second.iter())) {
        *slot = *value;
    }
    bytes
}
