//! How the Linux personality starts pid 1 from the root filesystem, lent to
//! `init` as an [`init::Launcher`](crate::init::Launcher).
//!
//! `init` is part of the certified item and decides which program runs;
//! opening that program in `/`, following its `#!` line, reading the dynamic
//! linker it names and starting it as a Linux process are all above the item
//! (`docs/certification/ITEM.md`), so they are here, and `main.rs` registers
//! them with [`install`] at bring-up.

use alloc::format;

use ferrix_linux_abi::errno::Errno;

use crate::fs;
use crate::init::{Failure, Image, Launcher, Opened, Start};
use crate::syscall::exec;
use crate::syscall::load::Source;

/// What `init` is lent.
static LAUNCHER: Launcher = Launcher {
    open,
    interpreter_line: exec::interpreter_line,
    start,
};

/// Lend `init` this personality's way of starting a program.
///
/// Called once from `main.rs`, before the boot marker; `init::run` is the
/// only caller and runs after it.
pub(crate) fn install() {
    crate::init::register_launcher(&LAUNCHER);
}

/// Open the program at `path` in the context a new process starts in: the
/// btrfs root once it is in place, the initramfs before that.
fn open(path: &[u8]) -> Result<Opened, Errno> {
    let context = fs::root_disk::process_context();
    let (program, exe, _set_ids) = fs::open_program(&context, None, path)?;
    Ok(Opened { program, exe })
}

/// Start `start` as pid 1, with the dynamic linker it names read from the
/// same root, and wait for it to end.
///
/// Set-id bits are not honoured for pid 1: it starts as root already, as
/// the built-in program always has.
fn start(start: &Start<'_>) -> Result<i32, Failure> {
    let image = match start.image {
        Image::BuiltIn(bytes) => Source::Bytes(bytes),
        Image::File(program) => Source::File(program),
    };
    run(start, image).map_err(|problem| match problem {
        exec::ExecError::Linker(errno) => Failure::Linker(errno),
        other => Failure::Exec(format!("{other:?}")),
    })
}

/// [`start`], in the personality's own terms.
fn run(start: &Start<'_>, image: Source<'_>) -> Result<i32, exec::ExecError> {
    let context = fs::root_disk::process_context();
    let linker = exec::linker_for(&context, image).map_err(exec::ExecError::Linker)?;
    let executable = exec::Executable {
        image,
        exe: start.exe,
        exec_fn: start.exec_fn,
        set_ids: fs::SetIds::NONE,
        interpreter: linker.as_ref().map(Source::File),
    };
    exec::run_init(executable, start.argv, start.env, exec::random_bytes())
}
