//! Carrying bytes between this console and the guest's serial socket.
//!
//! Windows only: [`the parent module`](super) says why a Windows run cannot
//! simply hand QEMU the terminal.
//!
//! The console is talked to through `ReadFile` and `WriteFile` on the standard
//! handles rather than through [`std::io`], for the same reason QEMU's own
//! backend does: the standard library's Windows console path is a character
//! path. It reads UTF-16 and takes a typed Ctrl-Z as the end of the input —
//! which on the other side of this would be a `SIGTSTP` that never arrives —
//! and it refuses to print a byte sequence that is not UTF-8, which a guest
//! reading a binary is entitled to produce. A terminal carrying a serial port
//! is a byte path in both directions.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::{Error, Result};

/// The running guest, shared with the thread that reads the keyboard so that
/// `Ctrl-A x` can stop it.
///
/// The lock is held for the length of one call and never across a wait for
/// input: the keyboard thread takes it to kill, and this thread takes it only
/// once the guest has closed the console, by which time the wait it does
/// under the lock returns at once.
type Guest = Arc<Mutex<Child>>;

/// What starts an escape: `Ctrl-A`, as QEMU's own multiplexer uses.
const ESCAPE: u8 = 0x01;

/// Start `command`, carry the console to and from the socket QEMU connects
/// back to, and wait for it to finish.
///
/// # Errors
///
/// When QEMU cannot be started, never opens the console, or exits
/// unsuccessfully — except after a `Ctrl-A x`, which is how a run is meant to
/// end and so is not a failure however QEMU then exits.
pub(super) fn carry(listener: &TcpListener, mut command: Command) -> Result<()> {
    let child = command
        .spawn()
        .map_err(|error| Error::new(format!("could not run qemu: {error}")))?;
    let guest: Guest = Arc::new(Mutex::new(child));
    let stream = accept(listener, &guest)?;
    let keyboard = stream
        .try_clone()
        .map_err(|error| Error::new(format!("could not share the console socket: {error}")))?;

    let quitting = Arc::new(AtomicBool::new(false));
    let typing = (Arc::clone(&guest), Arc::clone(&quitting));
    // Raw only while the guest has the console, and restored by the drop at
    // the end of this function however it is reached.
    let console = RawConsole::enter();
    // Detached, and never joined: it spends its life blocked in a read of a
    // terminal that only a keystroke ends, and the guest usually stops first.
    let _typist = thread::spawn(move || type_to_guest(&keyboard, &typing.0, &typing.1));
    print_from_guest(&stream);

    let status = lock(&guest)
        .wait()
        .map_err(|error| Error::new(format!("could not wait for qemu: {error}")))?;
    drop(console);
    if quitting.load(Ordering::Relaxed) || status.success() {
        return Ok(());
    }
    Err(Error::new(format!(
        "qemu failed{}",
        status
            .code()
            .map_or(String::new(), |code| format!(" (exit {code})"))
    )))
}

/// The guest's connection to the console socket.
///
/// # Errors
///
/// When QEMU stops before it connects — a bad command line is the usual
/// reason, and its own message will already be on this terminal — or when it
/// has not connected long after it should have.
fn accept(listener: &TcpListener, guest: &Guest) -> Result<TcpStream> {
    /// Far longer than QEMU takes to open its chardevs, and short enough that
    /// one which never does is an error rather than a hang.
    const PATIENCE: Duration = Duration::from_secs(30);
    /// How long to wait between looks. Startup, so a tenth of the time it
    /// takes a person to notice.
    const PAUSE: Duration = Duration::from_millis(10);

    listener
        .set_nonblocking(true)
        .map_err(|error| Error::new(format!("could not poll the console socket: {error}")))?;
    let deadline = Instant::now() + PATIENCE;
    loop {
        match listener.accept() {
            Ok((stream, _from)) => {
                stream.set_nonblocking(false).map_err(|error| {
                    Error::new(format!("could not settle the console socket: {error}"))
                })?;
                return Ok(stream);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => {
                return Err(Error::new(format!(
                    "could not accept the guest's console connection: {error}"
                )));
            }
        }
        if let Ok(Some(status)) = lock(guest).try_wait() {
            return Err(Error::new(format!(
                "qemu stopped before it opened the console ({status})"
            )));
        }
        if Instant::now() >= deadline {
            return Err(Error::new("qemu never opened the console socket"));
        }
        thread::sleep(PAUSE);
    }
}

/// Print what the guest sends, until it stops sending.
fn print_from_guest(stream: &TcpStream) {
    let mut from_guest = stream;
    let mut buffer = [0_u8; 4096];
    loop {
        let Ok(count) = from_guest.read(&mut buffer) else {
            return;
        };
        let Some(bytes) = buffer.get(..count) else {
            return;
        };
        if bytes.is_empty() || !write_console(bytes) {
            return;
        }
    }
}

/// Send what is typed to the guest, until the terminal or the guest ends.
///
/// `Ctrl-A x` stops the guest and `Ctrl-A a` sends a literal `Ctrl-A`, as they
/// do under QEMU's multiplexer. Any other escape is swallowed: a terminal that
/// passed an unrecognised one through would make `Ctrl-A` unusable as a
/// prefix, and the guest's own shell binds it to the start of the line.
fn type_to_guest(stream: &TcpStream, guest: &Guest, quitting: &AtomicBool) {
    let mut to_guest = stream;
    let mut buffer = [0_u8; 1024];
    let mut escaped = false;
    loop {
        let Some(count) = read_console(&mut buffer) else {
            return;
        };
        let Some(typed) = buffer.get(..count) else {
            return;
        };
        if typed.is_empty() {
            return;
        }
        let mut send = Vec::with_capacity(count);
        for &byte in typed {
            match (escaped, byte) {
                (false, ESCAPE) => escaped = true,
                (false, _) => send.push(byte),
                (true, b'x') => {
                    quitting.store(true, Ordering::Relaxed);
                    let _stopped = lock(guest).kill();
                    return;
                }
                (true, _) => {
                    if byte == b'a' {
                        send.push(ESCAPE);
                    }
                    escaped = false;
                }
            }
        }
        if !send.is_empty() && to_guest.write_all(&send).is_err() {
            return;
        }
    }
}

/// The guest, whoever poisoned the lock.
///
/// A thread that panicked while holding the handle leaves the guest running,
/// and refusing to touch it afterwards would leave it running for good.
fn lock(guest: &Guest) -> MutexGuard<'_, Child> {
    guest.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A Windows `HANDLE`.
type Handle = *mut core::ffi::c_void;

/// `STD_INPUT_HANDLE`.
const STD_INPUT: u32 = -10_i32 as u32;
/// `STD_OUTPUT_HANDLE`.
const STD_OUTPUT: u32 = -11_i32 as u32;

/// The console input modes a terminal has to be *without* to carry a serial
/// port: line assembly, echo, and the processing that turns Ctrl-C into this
/// program's problem rather than the guest's.
const COOKED_INPUT: u32 = 0x0001 | 0x0002 | 0x0004;
/// `ENABLE_VIRTUAL_TERMINAL_INPUT`: arrow and function keys arrive as the
/// escape sequences a terminal sends, which is what the guest expects to read.
const VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
/// `ENABLE_PROCESSED_OUTPUT` and `ENABLE_VIRTUAL_TERMINAL_PROCESSING`, so the
/// escape sequences the guest prints are drawn rather than shown.
const VIRTUAL_TERMINAL_OUTPUT: u32 = 0x0001 | 0x0004;

// AUDIT: four entry points of `kernel32`, declared rather than depended on.
// `xtask` has no crates.io dependencies by policy, and a binding crate is a
// larger thing to own than these four lines; every call below is in a block
// of its own with the argument it passes justified there.
#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "GetStdHandle"]
    fn get_std_handle(which: u32) -> Handle;
    #[link_name = "GetConsoleMode"]
    fn get_console_mode(handle: Handle, mode: *mut u32) -> i32;
    #[link_name = "SetConsoleMode"]
    fn set_console_mode(handle: Handle, mode: u32) -> i32;
    #[link_name = "ReadFile"]
    fn read_file(handle: Handle, buffer: *mut u8, length: u32, read: *mut u32, over: Handle)
    -> i32;
    #[link_name = "WriteFile"]
    fn write_file(
        handle: Handle,
        buffer: *const u8,
        length: u32,
        written: *mut u32,
        over: Handle,
    ) -> i32;
}

/// This terminal's console modes as they were, restored when the guest is
/// finished with them.
///
/// `None` for a standard handle that is not a console — a run whose output is
/// redirected to a file has one — which has no modes to change or to put back.
#[derive(Debug)]
struct RawConsole {
    /// The input mode as it was found.
    input: Option<u32>,
    /// The output mode as it was found.
    output: Option<u32>,
}

impl RawConsole {
    /// Put the console into the state a serial terminal needs.
    fn enter() -> RawConsole {
        let input = mode(STD_INPUT);
        if let Some(cooked) = input {
            set_mode(STD_INPUT, (cooked & !COOKED_INPUT) | VIRTUAL_TERMINAL_INPUT);
        }
        let output = mode(STD_OUTPUT);
        if let Some(plain) = output {
            set_mode(STD_OUTPUT, plain | VIRTUAL_TERMINAL_OUTPUT);
        }
        RawConsole { input, output }
    }
}

impl Drop for RawConsole {
    fn drop(&mut self) {
        if let Some(cooked) = self.input {
            set_mode(STD_INPUT, cooked);
        }
        if let Some(plain) = self.output {
            set_mode(STD_OUTPUT, plain);
        }
    }
}

/// One of this process's standard handles, if it has one.
fn standard_handle(which: u32) -> Option<Handle> {
    // SAFETY: `which` is one of the two `STD_` constants above, and the call
    // does nothing but look that handle up in this process.
    let handle = unsafe { get_std_handle(which) };
    if handle.is_null() || handle == ptr::without_provenance_mut(usize::MAX) {
        return None;
    }
    Some(handle)
}

/// The console mode of a standard handle, or `None` if it is not a console.
fn mode(which: u32) -> Option<u32> {
    let handle = standard_handle(which)?;
    let mut mode = 0_u32;
    // SAFETY: `handle` is a standard handle of this process and `mode` is a
    // writable `u32`, which is what the call fills in.
    let read = unsafe { get_console_mode(handle, &raw mut mode) };
    (read != 0).then_some(mode)
}

/// Set the console mode of a standard handle, if it is a console.
fn set_mode(which: u32, mode: u32) {
    let Some(handle) = standard_handle(which) else {
        return;
    };
    // SAFETY: `handle` is a standard handle of this process, and `mode` is a
    // mode word read from that same handle with bits of its own kind changed.
    let _set = unsafe { set_console_mode(handle, mode) };
}

/// Read what has been typed, waiting until something has been.
///
/// `None` when the terminal has ended: the handle is gone, or the read failed.
fn read_console(buffer: &mut [u8]) -> Option<usize> {
    let handle = standard_handle(STD_INPUT)?;
    let length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    let mut read = 0_u32;
    // SAFETY: `handle` is this process's input, `buffer` is writable for
    // `length` bytes — its own length, or as much of it as a `u32` can name —
    // and `read` is a writable `u32`. A null overlapped pointer asks for the
    // blocking read this wants.
    let ok = unsafe {
        read_file(
            handle,
            buffer.as_mut_ptr(),
            length,
            &raw mut read,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    usize::try_from(read).ok()
}

/// Print `bytes` to the console. `false` when it can no longer be printed to.
fn write_console(bytes: &[u8]) -> bool {
    let Some(handle) = standard_handle(STD_OUTPUT) else {
        return false;
    };
    let mut done = 0_usize;
    while let Some(rest) = bytes.get(done..).filter(|rest| !rest.is_empty()) {
        let length = u32::try_from(rest.len()).unwrap_or(u32::MAX);
        let mut written = 0_u32;
        // SAFETY: `handle` is this process's output, `rest` is readable for
        // `length` bytes, and `written` is a writable `u32`. A null overlapped
        // pointer asks for a blocking write.
        let ok = unsafe {
            write_file(
                handle,
                rest.as_ptr(),
                length,
                &raw mut written,
                ptr::null_mut(),
            )
        };
        let Ok(count) = usize::try_from(written) else {
            return false;
        };
        if ok == 0 || count == 0 {
            return false;
        }
        done = done.saturating_add(count);
    }
    true
}
