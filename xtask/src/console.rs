//! This terminal, attached to the guest's serial port.
//!
//! # Why the guest does not get `-serial stdio` on Windows
//!
//! QEMU's Windows stdio backend does not hold what the guest cannot take. A
//! thread reads one byte from the console and hands it to the main loop, and
//! `win_stdio_thread_wait_func` in `chardev/char-win-stdio.c` passes it on
//! only `if (qemu_chr_be_can_write(chr))` — with no queue behind that test, so
//! a byte the guest cannot take at that instant is dropped and never
//! mentioned again. The console-handle path a few lines below it drops
//! keystrokes the same way.
//!
//! What the guest can take is the room left in the 16550's receive FIFO
//! *below the interrupt trigger level*: `serial_can_receive` in
//! `hw/char/serial.c` answers `itl - used`, and `arch::x86_64::console` sets
//! the trigger level to fourteen bytes, the highest the part offers. So the
//! first fourteen bytes of anything pasted into the console reach the kernel
//! and the rest is discarded before the port has even interrupted:
//! `cat /etc/os-release` arrives at a shell as `cat /etc/os-re`. No kernel
//! can recover them, and no setting makes it better — a lower trigger level
//! only narrows the window, as the EDK2 console demonstrates by leaving the
//! FIFO off altogether and receiving exactly one byte of a paste.
//!
//! POSIX hosts have no such hole. `chardev/char-fd.c` asks the guest the same
//! question but uses the answer as the *size of its read*, so what does not
//! fit stays in the pipe until the guest has room. There `-serial stdio` is
//! right, and it is what a run on one still gets.
//!
//! # What a Windows run gets instead
//!
//! A socket. QEMU's socket backend is one of the flow-controlled ones, and
//! this program sits on the other end carrying bytes between it and the
//! console — which it first puts into raw mode, so that what reaches the
//! guest is what was typed: no line editing, no echo, and Ctrl-C delivered to
//! the guest's terminal rather than acted on here. `Ctrl-A x`, which QEMU's
//! multiplexer would provide if this were its terminal, is implemented here
//! instead, because in raw mode there is otherwise no way out of a guest that
//! has stopped listening.

#[cfg(windows)]
mod relay;

#[cfg(windows)]
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::process::Command;

use crate::Result;

/// How the guest's serial port reaches this terminal.
#[derive(Debug)]
pub(crate) enum Console {
    /// QEMU has the terminal itself, through `-serial stdio`.
    Owned,
    /// QEMU connects back to this socket and the bytes are carried by hand.
    /// Windows only, and [`the module documentation`](self) says why.
    #[cfg(windows)]
    Relayed(TcpListener),
}

/// The console a run on this host should use.
///
/// # Errors
///
/// When the socket a Windows run relays through cannot be opened.
#[cfg(windows)]
pub(crate) fn open() -> Result<Console> {
    // Port zero: the operating system picks one that is free, and
    // `arguments` reads back which. Bound before QEMU starts, because QEMU is
    // the one that connects.
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0);
    let listener = TcpListener::bind(address).map_err(|error| {
        crate::Error::new(format!(
            "could not open a local socket for the guest's console: {error}"
        ))
    })?;
    Ok(Console::Relayed(listener))
}

/// The console a run on this host should use: its own terminal, since a
/// POSIX QEMU does not drop what it cannot pass on.
///
/// # Errors
///
/// Never, on a host whose QEMU can be given the terminal.
#[cfg(not(windows))]
pub(crate) fn open() -> Result<Console> {
    Ok(Console::Owned)
}

impl Console {
    /// The QEMU arguments that attach the guest's serial port to it.
    ///
    /// # Errors
    ///
    /// When the port a relayed console was bound to cannot be read back.
    pub(crate) fn arguments(&self) -> Result<Vec<String>> {
        match self {
            Console::Owned => Ok(vec!["-serial".to_owned(), "stdio".to_owned()]),
            #[cfg(windows)]
            Console::Relayed(listener) => {
                let port = listener
                    .local_addr()
                    .map_err(|error| {
                        crate::Error::new(format!(
                            "could not read back the console socket's port: {error}"
                        ))
                    })?
                    .port();
                Ok(vec![
                    "-chardev".to_owned(),
                    // `nodelay=on`: a keystroke is one byte, and Nagle would
                    // hold it back waiting for company.
                    format!("socket,id=console,host=127.0.0.1,port={port},nodelay=on"),
                    "-serial".to_owned(),
                    "chardev:console".to_owned(),
                ])
            }
        }
    }

    /// Run `command` with this console attached, and wait for it to finish.
    ///
    /// # Errors
    ///
    /// When QEMU cannot be started, never opens the console, or exits
    /// unsuccessfully — except after a `Ctrl-A x`, which is a way out rather
    /// than a failure.
    pub(crate) fn attach(self, command: Command) -> Result<()> {
        match self {
            Console::Owned => crate::cargo::run(command, "qemu"),
            #[cfg(windows)]
            Console::Relayed(listener) => relay::carry(&listener, command),
        }
    }
}
