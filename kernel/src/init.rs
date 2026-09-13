//! The first program: a shell, if one was built in, or a list of commands.
//!
//! Started after the boot marker rather than before it, which is what lets one
//! kernel serve both uses. `cargo xtask test-boot` stops QEMU the moment it
//! sees the marker, so the boot test is unchanged whether or not a shell is
//! embedded; `cargo xtask run` leaves the serial port attached to the terminal,
//! so the same image hands a person a prompt.
//!
//! # Why busybox and not something written for the purpose
//!
//! Because the point is somebody else's binary. A shell written against this
//! kernel would work by construction and prove nothing about the ABI; a static
//! `busybox` was linked against Linux by people who have never heard of
//! Ferrix, and every system call it makes is one this kernel either answers
//! the way Linux does or gets wrong in a way the shell will show.
//!
//! # A list of commands, for stage 8's exit
//!
//! Stage 8's exit criterion is several programs over one filesystem, and this
//! busybox's shell starts every program it does not have built in with
//! `clone`, `execve` and `wait4` (measured in
//! `docs/STAGE8-WHAT-THE-EXIT-NEEDS.md`). So a build can carry a list instead
//! of a script, and init starts each program in turn itself. The program is
//! `/bin/busybox`, read from the initramfs through the VFS rather than built
//! into the kernel, because loading a program from a file is part of what the
//! stage is for.
//!
//! Each command's start and end go on lines of their own, in a format
//! `xtask/src/vfs.rs` parses; the two change together. While a command runs,
//! every call answered `ENOSYS` is reported too, up to a bound, which is what
//! turns a failing run into the name of the call that is missing.

use alloc::vec::Vec;
use core::fmt;

use crate::arch;
use crate::console::println;
use crate::fs;
use crate::syscall::{self, exec};

/// The embedded program, or nothing. See `kernel/build.rs`.
pub(crate) static IMAGE: &[u8] = include_bytes!(env!("FERRIX_INIT_IMAGE"));

/// A script for the shell to run with `-c`, or nothing for an interactive
/// one. See `kernel/build.rs`.
static SCRIPT: &[u8] = include_bytes!(env!("FERRIX_INIT_SCRIPT_FILE"));

/// Commands to run in turn instead of the shell, or nothing. See
/// `kernel/build.rs` for the encoding.
static COMMANDS: &[u8] = include_bytes!(env!("FERRIX_INIT_COMMANDS_FILE"));

/// The program every command is run with.
const PROGRAM: &str = "/bin/busybox";

/// The environment every command starts with.
const ENVIRONMENT: &[&[u8]] = &[b"PATH=/bin", b"HOME=/", b"TERM=dumb"];

/// How many unanswered calls each command may report. Enough to name the
/// first few missing calls; few enough that a program retrying one forever
/// does not bury the rest of the log.
const UNANSWERED_LINES: u32 = 400;

/// The longest argument printed as itself. A script is longer, and a line of
/// the log is not the place to read it.
const SHOWN_BYTES: usize = 60;

/// Start the shell or run the commands, and report how each ended.
///
/// Returns when the last program exits, which on an interactive session is
/// when somebody types `exit`.
pub(crate) fn run() {
    if !COMMANDS.is_empty() {
        run_commands(COMMANDS);
        return;
    }
    if IMAGE.is_empty() {
        return;
    }
    let interactive: [&[u8]; 2] = [b"sh", b"-i"];
    let scripted: [&[u8]; 3] = [b"sh", b"-c", SCRIPT];
    let (args, how): (&[&[u8]], _) = if SCRIPT.is_empty() {
        (&interactive, "`sh -i`")
    } else {
        (&scripted, "`sh -c` with a built-in script")
    };
    println!(
        "  init     {} KiB program built in, starting {how}",
        IMAGE.len() / 1024
    );

    let status = exec::run(
        IMAGE,
        args,
        &[b"PATH=/bin", b"HOME=/", b"TERM=dumb", b"PS1=ferrix# "],
        random_bytes(),
    );
    match status {
        Ok(status) => println!("  init     the shell exited with {status}"),
        Err(problem) => println!("  init     the shell could not be started: {problem:?}"),
    }
}

/// Read [`PROGRAM`] from the root and run each command in `list` with it.
fn run_commands(list: &[u8]) {
    let commands = parse(list);
    let ctx = fs::namespace().context();
    let (program, exe) = match fs::read_program(&ctx, None, PROGRAM.as_bytes()) {
        Ok(read) => read,
        Err(errno) => {
            println!("  init     {PROGRAM} could not be read: errno {}", errno.0);
            return;
        }
    };
    println!(
        "  init     {PROGRAM} is {} KiB, running {} commands",
        program.len() / 1024,
        commands.len()
    );
    for (index, argv) in commands.iter().enumerate() {
        println!("  init     command {index}: {}", Argv(argv));
        syscall::report_unanswered(UNANSWERED_LINES);
        match start(&program, &exe, argv) {
            Ok(status) => println!("  init     command {index} exited with {status}"),
            Err(problem) => {
                println!("  init     command {index} could not be started: {problem:?}");
            }
        }
    }
    syscall::report_unanswered(0);
    println!("  init     every command has run");
}

/// Start one program, and wait for it to end.
///
/// The one place that knows how a program is started, so that the loop above
/// does not change when that does.
///
/// `exe` is where the program was read from, resolved, which is what
/// `/proc/self/exe` must say: the name as written in [`PROGRAM`] may pass
/// through a symbolic link, and glibc's static startup asserts the link it
/// reads back is absolute.
fn start(program: &[u8], exe: &[u8], argv: &[&[u8]]) -> Result<i32, exec::ExecError> {
    let executable = exec::Executable {
        image: program,
        exe,
        exec_fn: PROGRAM.as_bytes(),
    };
    exec::run_executable(executable, argv, ENVIRONMENT, random_bytes())
}

/// The commands in a list `kernel/build.rs` embedded: each argument ends in a
/// NUL, and each command in an empty argument.
///
/// A command left unterminated at the end is dropped rather than run with
/// arguments missing; the build script refuses such a list, so this is the
/// second line of defence rather than the first.
fn parse(list: &[u8]) -> Vec<Vec<&[u8]>> {
    let mut commands = Vec::new();
    let mut current = Vec::new();
    for word in list.split(|byte| *byte == 0) {
        if !word.is_empty() {
            current.push(word);
        } else if !current.is_empty() {
            commands.push(core::mem::take(&mut current));
        }
    }
    commands
}

/// An argument vector as one line of the log.
#[derive(Debug)]
struct Argv<'a>(&'a [&'a [u8]]);

impl fmt::Display for Argv<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (at, arg) in self.0.iter().enumerate() {
            if at > 0 {
                f.write_str(" ")?;
            }
            match core::str::from_utf8(arg) {
                Ok(text) if text.len() <= SHOWN_BYTES && !text.contains('\n') => {
                    f.write_str(text)?;
                }
                _ => write!(f, "<{} bytes>", arg.len())?,
            }
        }
        Ok(())
    }
}

/// Sixteen bytes for `AT_RANDOM`.
///
/// **Not random.** Two readings of the high-resolution counter, which differ
/// from boot to boot and are good enough that a libc's stack-protector canary
/// is not the same constant on every machine. They are not good enough for
/// anything an attacker is involved in, and nothing here pretends otherwise:
/// the entropy pool is a later stage's.
fn random_bytes() -> [u8; ferrix_ustack::RANDOM_BYTES] {
    let first = arch::counter_now().to_le_bytes();
    let second = arch::counter_now().rotate_left(29).to_le_bytes();
    let mut bytes = [0_u8; ferrix_ustack::RANDOM_BYTES];
    for (slot, value) in bytes.iter_mut().zip(first.iter().chain(second.iter())) {
        *slot = *value;
    }
    bytes
}
