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
//! of a script, and init starts each program in turn itself. Each command's
//! program is the name its `argv[0]` has in `/bin`, read from the initramfs
//! through the VFS rather than built into the kernel, because loading a
//! program from a file is part of what the stage is for. Which binary that
//! name belongs to -- busybox, uutils/coreutils, zinc -- is the image's
//! business and not this file's.
//!
//! Each command's start and end go on lines of their own, in a format
//! `xtask/src/vfs.rs` parses; the two change together. While a command runs,
//! every call answered `ENOSYS` is reported too, up to a bound, which is what
//! turns a failing run into the name of the call that is missing.

use alloc::vec::Vec;
use core::fmt;

use crate::console::println;
use crate::fs;
use crate::syscall::{self, exec};

/// The embedded program, or nothing. See `kernel/build.rs`.
pub(crate) static IMAGE: &[u8] = include_bytes!(env!("FERRIX_INIT_IMAGE"));

/// What `/proc/self/exe` names for [`IMAGE`], which has no file of its own.
///
/// Absolute, because glibc's static start-up reads that link back and asserts
/// it is (`_dl_get_origin`): named after its first argument, `sh`, Ubuntu's
/// static busybox aborted with 134 before `main`. It is named where busybox
/// lives; `AT_EXECFN` keeps the name it was started by.
const BUILT_IN_EXE: &[u8] = b"/bin/busybox";

/// A script for the shell to run with `-c`, or nothing for an interactive
/// one. See `kernel/build.rs`.
static SCRIPT: &[u8] = include_bytes!(env!("FERRIX_INIT_SCRIPT_FILE"));

/// Commands to run in turn instead of the shell, or nothing. See
/// `kernel/build.rs` for the encoding.
static COMMANDS: &[u8] = include_bytes!(env!("FERRIX_INIT_COMMANDS_FILE"));

/// Where a command's program is looked for: `PATH`, which is one directory.
///
/// A command's `argv[0]` names its program, and the program is whatever that
/// name is in `/bin` -- a link to busybox, to uutils/coreutils, or to zinc.
/// Init resolves it the way the shell would, rather than knowing which binary
/// owns which name, so that moving a name from one to the other is a change
/// to the image and not to the kernel.
const PROGRAM_DIR: &[u8] = b"/bin/";

/// The environment every command starts with.
const ENVIRONMENT: &[&[u8]] = &[b"PATH=/bin", b"HOME=/", b"TERM=dumb"];

/// How many unanswered calls each command may report. Enough to name the
/// first few missing calls; few enough that a program retrying one forever
/// does not bury the rest of the log.
const UNANSWERED_LINES: u32 = 16;

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

    let name = args.first().copied().unwrap_or(b"");
    // A built-in program may be dynamically linked too, as a distribution's
    // shell is: its linker and libraries are not built in but read from the
    // initramfs, where `cargo xtask test-shell --interpreter` put them.
    let context = fs::root_disk::process_context();
    let linker = match exec::linker_for(&context, IMAGE) {
        Ok(linker) => linker,
        Err(errno) => {
            println!(
                "  init     the shell could not be started: its linker: errno {}",
                errno.0
            );
            return;
        }
    };
    let program = exec::Executable {
        image: IMAGE,
        exe: BUILT_IN_EXE,
        exec_fn: name,
        set_ids: fs::SetIds::NONE,
        interpreter: linker.as_deref(),
    };
    let status = exec::run_init(
        program,
        args,
        // `ENV` is what an interactive POSIX shell reads before its first
        // prompt; xtask's initramfs puts the network setup there.
        &[
            b"PATH=/bin",
            b"HOME=/",
            b"TERM=dumb",
            b"PS1=ferrix# ",
            b"ENV=/etc/profile",
        ],
        random_bytes(),
    );
    match status {
        Ok(status) => println!("  init     the shell exited with {status}"),
        Err(problem) => println!("  init     the shell could not be started: {problem:?}"),
    }
}

/// Run each command in `list`, each with the program its `argv[0]` names in
/// [`PROGRAM_DIR`].
fn run_commands(list: &[u8]) {
    let commands = parse(list);
    let ctx = fs::root_disk::process_context();
    println!("  init     running {} commands", commands.len());
    // The programs the commands have needed so far, so that twenty commands
    // over three programs read three files. Worth keeping rather than reading
    // each time: uutils/coreutils is one binary of some 14 MiB answering to a
    // hundred names, and reading it once per command would be most of the
    // boot.
    // The image type is the one `read_program` returns; only the path and
    // the resolved name are named here.
    let mut loaded: Vec<(Vec<u8>, _, Vec<u8>)> = Vec::new();
    for (index, argv) in commands.iter().enumerate() {
        println!("  init     command {index}: {}", Argv(argv));
        syscall::report_unanswered(UNANSWERED_LINES);
        let Some(name) = argv.first() else {
            println!("  init     command {index} names no program");
            continue;
        };
        let mut path = Vec::from(PROGRAM_DIR);
        path.extend_from_slice(name);

        let known = loaded.iter().position(|(seen, ..)| seen == &path);
        let at = match known {
            Some(at) => at,
            None => match fs::read_program(&ctx, None, &path) {
                Ok((image, exe, _set_ids)) => {
                    loaded.push((path.clone(), image, exe));
                    loaded.len().saturating_sub(1)
                }
                Err(errno) => {
                    println!(
                        "  init     command {index} could not be read: errno {}",
                        errno.0
                    );
                    continue;
                }
            },
        };
        let Some((_, image, exe)) = loaded.get(at) else {
            continue;
        };
        match start(image, exe, &path, argv) {
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
/// `/proc/self/exe` must say: the name in `/bin` is a symbolic link, and
/// glibc's static startup asserts the link it reads back is absolute.
///
/// `path` is the name before it was resolved, which is what `AT_EXECFN` says:
/// a multicall binary reads it, or `argv[0]`, to know which of its programs
/// it has been asked for.
fn start(program: &[u8], exe: &[u8], path: &[u8], argv: &[&[u8]]) -> Result<i32, exec::ExecError> {
    // init comes out of the initramfs like any other program, so it may be
    // dynamically linked like any other program, and the linker it names is
    // read from the same initramfs. There is no process to fail back to here,
    // which is why this is the one caller that reports the failure itself.
    let context = fs::root_disk::process_context();
    let linker = exec::linker_for(&context, program).map_err(exec::ExecError::Linker)?;
    let executable = exec::Executable {
        image: program,
        exe,
        exec_fn: path,
        set_ids: fs::SetIds::NONE,
        interpreter: linker.as_deref(),
    };
    exec::run_init(executable, argv, ENVIRONMENT, random_bytes())
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

/// Sixteen bytes for `AT_RANDOM`: [`crate::syscall::exec::random_bytes`].
fn random_bytes() -> [u8; ferrix_ustack::RANDOM_BYTES] {
    exec::random_bytes()
}
