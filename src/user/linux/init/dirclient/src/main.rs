//! `dirclient NAME`: a Linux program using the init's directory
//! (`docs/INIT.md` §6), as `test-init`'s stage five runs it.
//!
//! It takes the bootstrap channel init gave it (`process_bootstrap`), makes
//! a channel, and sends OPEN for `NAME` with one end. Then it waits up to
//! ten seconds on both: an answer down its own end means init routed the
//! OPEN to the provider, and it prints `dir-answer: <what came>` and exits
//! 0; a REFUSED on the bootstrap channel means init refused it, and it
//! prints `dir-refused: <why>` and exits 1. Anything else is 2, with a line.
//! A Linux program makes native calls the way init does: `syscall` with the
//! call's number.

use std::io::Write as _;
use std::process::ExitCode;

use ferrix_native::channel::{self, Channel, ReadError};
use ferrix_native::pending;
use ferrix_native::port;
use ferrix_native::{Deadline, Error, Object, Raw, Signals, Syscall};
use ferrix_native_abi::directory::{Kind, MAX_MESSAGE, Message};

/// Native calls through `syscall`, as `src/user/linux/init/init/src/sys.rs` makes them.
#[derive(Debug, Clone, Copy)]
struct Native;

impl Syscall for Native {
    fn call(self, raw: Raw<'_>) -> usize {
        let [a0, a1, a2, a3, a4, a5] = raw.args();
        let number = libc::c_long::try_from(raw.number()).unwrap_or(-1);
        // SAFETY: `src/lib/proto/native` built `raw` from memory it borrows for as
        // long as `raw` lives, which is past this call.
        let ret = unsafe { libc::syscall(number, a0, a1, a2, a3, a4, a5) };
        if ret == -1 {
            let errno = std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO);
            (-isize::try_from(errno).unwrap_or(isize::MAX)).cast_unsigned()
        } else {
            usize::try_from(ret).unwrap_or(usize::MAX)
        }
    }
}

/// Say a line and end with `status`.
fn end(status: u8, line: &str) -> ExitCode {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    ExitCode::from(status)
}

/// Now plus ten seconds, on `CLOCK_MONOTONIC`, in nanoseconds.
fn ten_seconds_on() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is writable.
    let _ = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    let nanos = u64::try_from(now.tv_sec).unwrap_or(0) * 1_000_000_000
        + u64::try_from(now.tv_nsec).unwrap_or(0);
    nanos + 10_000_000_000
}

fn main() -> ExitCode {
    let Some(name) = std::env::args().nth(1) else {
        return end(2, "usage: dirclient NAME");
    };
    let bootstrap = match pending::take_bootstrap(Native) {
        Ok(Some(handle)) => Channel::from_owned(handle),
        Ok(None) => return end(2, "dir-none: init gave this service no bootstrap channel"),
        Err(error) => return end(2, &format!("dir-error: process_bootstrap: {error:?}")),
    };
    let Ok((mine, theirs)) = channel::create(Native) else {
        return end(2, "dir-error: channel_create");
    };
    let mut bytes = [0_u8; MAX_MESSAGE];
    let Some(len) = Message::named(Kind::Open, &name, "").encode(&mut bytes) else {
        return end(2, "dir-error: the name is too long");
    };
    if let Err((error, _)) =
        bootstrap.write_with(bytes.get(..len).unwrap_or_default(), [theirs.into_owned()])
    {
        return end(2, &format!("dir-error: sending OPEN: {error:?}"));
    }
    let Ok(port) = port::create(Native) else {
        return end(2, "dir-error: port_create");
    };
    let armed = mine
        .wait_async(&port, Signals::READABLE | Signals::PEER_CLOSED, 1)
        .and_then(|()| bootstrap.wait_async(&port, Signals::READABLE, 2));
    if let Err(error) = armed {
        return end(2, &format!("dir-error: object_wait_async: {error:?}"));
    }
    let packet = match port.wait(Deadline::At(ten_seconds_on())) {
        Ok(packet) => packet,
        Err(error) => {
            return end(
                2,
                &format!("dir-silent: nothing within ten seconds ({error:?})"),
            );
        }
    };
    let mut got = [0_u8; MAX_MESSAGE];
    if packet.key == 1 {
        match mine.read(&mut got, &mut []) {
            Ok(received) => {
                let text = String::from_utf8_lossy(got.get(..received.bytes).unwrap_or_default());
                return end(0, &format!("dir-answer: {text}"));
            }
            // Closed with nothing in it: refused, and the reason is on the
            // bootstrap channel, sent before the end was closed.
            Err(ReadError::Failed(Error::PeerClosed)) => {}
            Err(error) => return end(2, &format!("dir-error: the answer: {error:?}")),
        }
    }
    match bootstrap.read(&mut got, &mut []) {
        Ok(received) => match Message::decode(got.get(..received.bytes).unwrap_or_default()) {
            Some(message) if message.kind == Kind::Refused => end(
                1,
                &format!("dir-refused: {} ({})", message.name, message.detail),
            ),
            _ => end(2, "dir-error: init said something that is not REFUSED"),
        },
        Err(error) => end(2, &format!("dir-error: the refusal: {error:?}")),
    }
}
