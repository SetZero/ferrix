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
//!
//! # A program named on the command line
//!
//! `ferrix.init=<path>` starts pid 1 from that file instead, in the `/` the
//! kernel switched to, as Linux's `init=` does (`docs/INIT.md` §8.1). It is
//! how a real init starts, and how an image that is not a gate boots without
//! a program built into its kernel. The file may be a `#!` script, which runs
//! under its interpreter as `execve` would run it. A file that is missing or
//! will not start is said on one line and the built-in program runs as if
//! nothing had been named, so a mistyped path costs a boot log line and not a
//! machine that does nothing.
//!
//! With nothing named and nothing built in, `/sbin/init` is started if the
//! image has one, which is §8.1's default. Built-in first, because every gate
//! builds its program in and none of them may change: no image carries a
//! `/sbin/init` today, and one that starts to must not take a gate's boot
//! from it.
//!
//! # A bootstrap channel
//!
//! Every program started here is started with a bootstrap channel, as
//! `devmgr` is (`docs/INIT.md` §6, K2). The kernel writes one message on its
//! end before the program runs -- `libs/native-abi`'s `bootstrap` module says
//! what it holds -- and keeps that end for as long as the program runs, so a
//! later message can carry what the first does not. The program takes its end
//! with `process_bootstrap`; one that never does, which is every program a
//! gate runs today, leaves it to be closed when it ends.
//!
//! # Which program, and how
//!
//! This file decides *which* program runs as pid 1 and says how it ended.
//! *How* a program is opened and started -- the root filesystem it is read
//! from, `#!` lines, the dynamic linker, the Linux personality's process --
//! is above the certified item, so init names none of it: the load ring
//! registers a [`Launcher`] at bring-up, from `main.rs`, and the boot checks
//! that it did before the boot marker.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::convert::Infallible;
use core::fmt;

use ferrix_bootinfo::{BootView, option_in};
use ferrix_linux_abi::errno::Errno;
use ferrix_native_abi::bootstrap::init_hello;
use ferrix_native_abi::rights::Rights;
use ferrix_sync::Once;

use crate::console::println;
use crate::fallible;
use crate::object::channel::Endpoint;
use crate::object::{Object, Transfer};
use crate::sync::SpinLock;
use crate::syscall;
use crate::syscall::program::ProgramFile;

/// A program opened to be started: its image, and the path it was read from
/// with its links resolved, which `/proc/self/exe` names.
#[derive(Debug)]
pub(crate) struct Opened {
    /// The file, with its headers read.
    pub(crate) program: ProgramFile,
    /// Where it was read from, resolved.
    pub(crate) exe: Vec<u8>,
}

/// An image to start: the one built into the kernel, or one opened from a
/// file.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Image<'a> {
    /// [`IMAGE`], in kernel memory.
    BuiltIn(&'static [u8]),
    /// A file [`Launcher::open`] opened.
    File(&'a ProgramFile),
}

/// Everything a program is started with, as `execve` would be told it.
#[derive(Debug)]
pub(crate) struct Start<'a> {
    /// What runs.
    pub(crate) image: Image<'a>,
    /// What `/proc/self/exe` names.
    pub(crate) exe: &'a [u8],
    /// What `AT_EXECFN` names: the path before it was resolved.
    pub(crate) exec_fn: &'a [u8],
    /// Its arguments.
    pub(crate) argv: &'a [&'a [u8]],
    /// Its environment.
    pub(crate) env: &'a [&'a [u8]],
    /// Its end of its bootstrap channel, for the launcher to hold in the new
    /// process for `process_bootstrap`; `None` when there was no memory for
    /// one.
    pub(crate) bootstrap: Option<Transfer>,
}

/// Why a program that opened would not start.
#[derive(Debug)]
pub(crate) enum Failure {
    /// The dynamic linker it names could not be read.
    Linker(Errno),
    /// It would not load or run, as the personality put it.
    Exec(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Linker(errno) => write!(f, "its linker: errno {}", errno.0),
            Failure::Exec(why) => f.write_str(why),
        }
    }
}

/// Read a `#!` line from the start of a file: its interpreter, and the one
/// argument it may carry.
pub(crate) type InterpreterLine = fn(&[u8]) -> Result<(Vec<u8>, Option<Vec<u8>>), Errno>;

/// How a program is opened and started as pid 1: what init needs from above
/// the certified item, registered by the load ring with
/// [`register_launcher`].
#[derive(Debug)]
pub(crate) struct Launcher {
    /// Open the program at an absolute path in `/`.
    pub(crate) open: fn(&[u8]) -> Result<Opened, Errno>,
    /// Read a `#!` line from the start of a file.
    pub(crate) interpreter_line: InterpreterLine,
    /// Start a program as pid 1 with a fresh `AT_RANDOM` and its bootstrap,
    /// and wait for it to end: its status.
    pub(crate) start: fn(Start<'_>) -> Result<i32, Failure>,
}

/// The registered [`Launcher`].
static LAUNCHER: Once<&'static Launcher> = Once::new();

/// Start programs with `launcher`. The first registration stands.
pub(crate) fn register_launcher(launcher: &'static Launcher) {
    let _ = LAUNCHER.call_once(|| launcher);
}

/// Whether a [`Launcher`] is registered: the boot's check that [`run`] will
/// have something to start init with.
pub(crate) fn has_launcher() -> bool {
    LAUNCHER.get().is_some()
}

/// The kernel's end of the bootstrap channel of the program started last,
/// kept while it runs so that its end does not read as closed, and so that a
/// later message has somewhere to be written.
static CHANNEL: SpinLock<Option<Arc<Endpoint>>> = SpinLock::new(None);

/// A bootstrap channel with the kernel's first message written on it: the
/// kernel's end, and the program's (K2). What [`run`] gives each program it
/// starts, and what the boot check reads. `None` when there was no memory
/// for the channel or the message.
pub(crate) fn bootstrap_channel() -> Option<(Arc<Endpoint>, Arc<Endpoint>)> {
    let (kernel_end, program_end) = Endpoint::pair().ok()?;
    let hello = fallible::try_to_vec(&init_hello()).ok()?;
    kernel_end
        .write(hello, 0, || Ok::<Vec<Transfer>, Infallible>(Vec::new()))
        .ok()?;
    Some((kernel_end, program_end))
}

/// A bootstrap for the next program: its end of a new channel, the kernel's
/// end kept in [`CHANNEL`] in place of the last program's. `None`, said on a
/// line, when there is no memory for one; the program starts without.
fn next_bootstrap() -> Option<Transfer> {
    let Some((kernel_end, program_end)) = bootstrap_channel() else {
        println!("  init     no memory for a bootstrap channel; starting the program without one");
        return None;
    };
    let last = CHANNEL.lock().replace(kernel_end);
    // Through `dispose`, with the lock let go: what the last program sent the
    // kernel and nobody read may carry handles.
    crate::object::dispose(last.map(Object::Channel));
    Some((Object::Channel(program_end), Rights::CHANNEL))
}

/// The command-line option naming the file pid 1 is started from.
const OPTION: &str = "ferrix.init";

/// The file started when nothing is named and nothing is built in.
const DEFAULT_INIT: &[u8] = b"/sbin/init";

/// What `ferrix.init=` named, read once, early.
static NAMED: Once<Vec<u8>> = Once::new();

/// Read `ferrix.init` from the loader's command line or, on a machine
/// described by a device tree, from `/chosen/bootargs`, as `power::init`
/// reads its option.
///
/// Early rather than when init starts, for `power::init`'s reason: a typo is
/// better reported at the start of a log than at the end of one. A path that
/// is not absolute is refused here, since there is no working directory yet
/// to resolve it from.
pub(crate) fn read_option(view: &BootView<'_>) {
    let tree = crate::fdt::open(view).ok();
    let value = view.option(OPTION).or_else(|| {
        tree.as_ref()
            .and_then(|tree| option_in(tree.bootargs()?, OPTION))
    });
    match value {
        None => {}
        Some(path) if path.starts_with('/') => {
            // FATAL-ALLOC: boot only: the command line is read once, as the kernel comes up.
            let _ = NAMED.call_once(|| Vec::from(path.as_bytes()));
            println!("  init     {OPTION}={path}: pid 1 is started from that file");
        }
        Some(other) => println!(
            "  init     {OPTION}={other} is not an absolute path; the built-in program is started"
        ),
    }
}

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

/// Start the program `ferrix.init=` named, or the shell or the commands built
/// in, or `/sbin/init`, and report how each ended.
///
/// Returns when the last program exits, which on an interactive session is
/// when somebody types `exit`.
pub(crate) fn run() {
    let Some(&launcher) = LAUNCHER.get() else {
        println!("  init     nothing is registered to start a program with");
        return;
    };
    if let Some(path) = NAMED.get() {
        match run_file(launcher, path) {
            Ok(status) => {
                println!("  init     {} exited with {status}", Argv(&[path]));
                return;
            }
            Err(why) => println!(
                "  init     {OPTION}={} could not be started: {why}; falling back to the \
                 built-in program",
                Argv(&[path])
            ),
        }
    }
    if !COMMANDS.is_empty() {
        run_commands(launcher, COMMANDS);
        return;
    }
    if IMAGE.is_empty() {
        run_default(launcher);
        return;
    }
    run_built_in(launcher);
}

/// Why a file could not be started as pid 1.
#[derive(Debug)]
enum Refusal {
    /// Opening it, or its `#!` interpreter, was refused.
    Open(Errno),
    /// It opened, and would not load or run.
    Start(Failure),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Open(errno) => write!(f, "errno {}", errno.0),
            Refusal::Start(failure) => write!(f, "{failure}"),
        }
    }
}

/// A refusal for want of memory.
fn no_memory(_: fallible::AllocError) -> Refusal {
    Refusal::Open(Errno::ENOMEM)
}

/// Start the file at `path` as pid 1, and wait for it to end.
///
/// A `#!` script runs under its interpreter with its own path as the last
/// argument, one level deep, as `execve` runs one; any other file runs with
/// its path as its only argument, as Linux starts `init=`.
fn run_file(launcher: &Launcher, path: &[u8]) -> Result<i32, Refusal> {
    let mut opened = (launcher.open)(path).map_err(Refusal::Open)?;
    let mut argv: Vec<Vec<u8>> = Vec::new();
    if opened.program.head().starts_with(b"#!") {
        let (interpreter, argument) =
            (launcher.interpreter_line)(opened.program.head()).map_err(Refusal::Open)?;
        opened = (launcher.open)(&interpreter).map_err(Refusal::Open)?;
        if opened.program.head().starts_with(b"#!") {
            return Err(Refusal::Open(Errno::ENOEXEC));
        }
        fallible::try_push(&mut argv, interpreter).map_err(no_memory)?;
        fallible::try_extend(&mut argv, argument).map_err(no_memory)?;
    }
    let own = fallible::try_to_vec(path).map_err(no_memory)?;
    fallible::try_push(&mut argv, own).map_err(no_memory)?;
    let args: Vec<&[u8]> =
        fallible::try_collect(argv.iter().map(Vec::as_slice)).map_err(no_memory)?;
    println!("  init     starting {}", Argv(&args));
    start(launcher, &opened, path, &args).map_err(Refusal::Start)
}

/// §8.1's default: `/sbin/init`, when nothing was named or built in. An image
/// without one is every image today, and says nothing.
fn run_default(launcher: &Launcher) {
    match run_file(launcher, DEFAULT_INIT) {
        Ok(status) => println!("  init     {} exited with {status}", Argv(&[DEFAULT_INIT])),
        Err(Refusal::Open(Errno::ENOENT)) => {}
        Err(why) => println!(
            "  init     {} could not be started: {why}",
            Argv(&[DEFAULT_INIT])
        ),
    }
}

/// Start the program built into the kernel: `sh -i`, or `sh -c` with the
/// built-in script.
fn run_built_in(launcher: &Launcher) {
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
    // initramfs, where `cargo xtask test-shell --interpreter` put them, and
    // the launcher reads them from there.
    let status = (launcher.start)(Start {
        image: Image::BuiltIn(IMAGE),
        exe: BUILT_IN_EXE,
        exec_fn: name,
        argv: args,
        // `ENV` is what an interactive POSIX shell reads before its first
        // prompt; xtask's initramfs puts the network setup there.
        env: &[
            b"PATH=/bin",
            b"HOME=/",
            b"TERM=dumb",
            b"PS1=ferrix# ",
            b"ENV=/etc/profile",
        ],
        bootstrap: next_bootstrap(),
    });
    match status {
        Ok(status) => println!("  init     the shell exited with {status}"),
        Err(problem) => println!("  init     the shell could not be started: {problem}"),
    }
}

/// Run each command in `list`, each with the program its `argv[0]` names in
/// [`PROGRAM_DIR`].
fn run_commands(launcher: &Launcher, list: &[u8]) {
    let Ok(commands) = parse(list) else {
        println!("  init     no memory to read the commands");
        return;
    };
    println!("  init     running {} commands", commands.len());
    // The programs the commands have needed so far, each with the path it was
    // asked for by and the name it resolved to, so that twenty commands over
    // three programs open three files: uutils/coreutils is one binary
    // answering to a hundred names. Since 2026-09-24 an open program is its
    // headers and its file, which the loader maps, so keeping one costs
    // nothing; before, each was the whole file read into memory.
    let mut loaded: Vec<(Vec<u8>, Opened)> = Vec::new();
    for (index, argv) in commands.iter().enumerate() {
        println!("  init     command {index}: {}", Argv(argv));
        syscall::report_unanswered(UNANSWERED_LINES);
        let Some(name) = argv.first() else {
            println!("  init     command {index} names no program");
            continue;
        };
        let Ok(mut path) = fallible::try_to_vec(PROGRAM_DIR) else {
            println!("  init     command {index}: no memory for its path");
            continue;
        };
        if fallible::try_extend_from_slice(&mut path, name).is_err() {
            println!("  init     command {index}: no memory for its path");
            continue;
        }

        let known = loaded.iter().position(|(seen, ..)| *seen == path);
        let at = match known {
            Some(at) => at,
            None => match (launcher.open)(&path) {
                Ok(opened) => {
                    if fallible::try_push(&mut loaded, (path, opened)).is_err() {
                        println!("  init     command {index}: no memory to keep its program");
                        continue;
                    }
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
        let Some((path, opened)) = loaded.get(at) else {
            continue;
        };
        match start(launcher, opened, path, argv) {
            Ok(status) => println!("  init     command {index} exited with {status}"),
            Err(problem) => {
                println!("  init     command {index} could not be started: {problem}");
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
///
/// init comes out of the initramfs like any other program, so it may be
/// dynamically linked like any other program; the launcher reads the linker
/// it names from the same place. There is no process to fail back to here,
/// which is why this is the one caller that reports the failure itself.
fn start(
    launcher: &Launcher,
    opened: &Opened,
    path: &[u8],
    argv: &[&[u8]],
) -> Result<i32, Failure> {
    (launcher.start)(Start {
        image: Image::File(&opened.program),
        exe: &opened.exe,
        exec_fn: path,
        argv,
        env: ENVIRONMENT,
        bootstrap: next_bootstrap(),
    })
}

/// The commands in a list `kernel/build.rs` embedded: each argument ends in a
/// NUL, and each command in an empty argument.
///
/// A command left unterminated at the end is dropped rather than run with
/// arguments missing; the build script refuses such a list, so this is the
/// second line of defence rather than the first.
///
/// # Errors
///
/// [`fallible::AllocError`] when there is no memory for the list.
fn parse(list: &[u8]) -> Result<Vec<Vec<&[u8]>>, fallible::AllocError> {
    let mut commands = Vec::new();
    let mut current = Vec::new();
    for word in list.split(|byte| *byte == 0) {
        if !word.is_empty() {
            fallible::try_push(&mut current, word)?;
        } else if !current.is_empty() {
            fallible::try_push(&mut commands, core::mem::take(&mut current))?;
        }
    }
    Ok(commands)
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
