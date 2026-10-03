//! `sessiond`'s end of `ferrix.auth.seat` (`docs/AUTH.md` §3.7, P2.5).
//!
//! `hyprix.service` says `Uses=ferrix.auth.seat`, and init gives the
//! bootstrap channel to the unit's own process, which is this one: the
//! compositor is a child and cannot take it. This sends OPEN for the name
//! with one end of a channel of its own; init starts `authd` if it must and
//! forwards the other end, and `authd` says `SEAT_READY` down it. From then on
//! ARM and DISARM go to `authd` and GRANT comes back. A REFUSED, or `authd`
//! closing its end, means no grants until an OPEN gets through again.
//!
//! Everything waits on one port, polled as a descriptor beside the
//! compositor's channels (`port_fd`).

use std::os::fd::{FromRawFd as _, OwnedFd};

use ferrix_auth_proto::{MAX_RECORD, Record};
use ferrix_native::channel::{self, Channel, ReadError};
use ferrix_native::port::{self, Port};
use ferrix_native::{Deadline, Error, Object, Raw, Signals, Syscall, pending};
use ferrix_native_abi::directory::{Kind, MAX_MESSAGE, Message};

use crate::say;

/// The name.
const NAME: &str = "ferrix.auth.seat";

/// The port key of the bootstrap channel, where a REFUSED comes.
const BOOTSTRAP: u64 = 1;

/// The port key of our end of the seat channel.
const END: u64 = 2;

/// Native calls through `syscall`, as `src/user/system/linux/init/init/src/sys.rs` makes them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Native;

impl Syscall for Native {
    fn call(self, raw: Raw<'_>) -> usize {
        let [a0, a1, a2, a3, a4, a5] = raw.args();
        let number = libc::c_long::try_from(raw.number()).unwrap_or(-1);
        #[expect(
            unsafe_code,
            reason = "AUDIT: a native call is a syscall with the arguments src/lib/proto/native made"
        )]
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

/// What came from `authd`'s side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Heard {
    /// The channel is up: `SEAT_READY`.
    Ready,
    /// It is down: REFUSED, or `authd` closed it.
    Gone,
    /// A GRANT.
    Grant {
        /// Whose.
        uid: u32,
        /// Which lock.
        epoch: u64,
    },
}

/// The bootstrap channel, our end of the seat channel while it is open, and
/// the port both are watched on.
pub(crate) struct Authd {
    bootstrap: Channel<Native>,
    end: Option<Channel<Native>>,
    port: Port<Native>,
    fd: OwnedFd,
}

impl Authd {
    /// Take the bootstrap channel and ask for the seat. `None`, said why,
    /// when init gave none: the unit does not `Uses=` it, and the session
    /// has no grants.
    pub(crate) fn open() -> Option<Authd> {
        let bootstrap = match pending::take_bootstrap(Native) {
            Ok(Some(handle)) => Channel::from_owned(handle),
            Ok(None) => {
                say(&format!(
                    "no bootstrap channel, so no {NAME}: this session's locks are refused"
                ));
                return None;
            }
            Err(error) => {
                say(&format!(
                    "process_bootstrap failed ({error:?}): this session's locks are refused"
                ));
                return None;
            }
        };
        let made = || -> Result<Authd, Error> {
            let port = port::create(Native)?;
            let raw = port.descriptor(true)?;
            #[expect(
                unsafe_code,
                reason = "AUDIT: port_fd just made the descriptor, and nothing else owns it"
            )]
            // SAFETY: as the reason says.
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };
            bootstrap.wait_async(&port, Signals::READABLE, BOOTSTRAP)?;
            let mut authd = Authd {
                bootstrap,
                end: None,
                port,
                fd,
            };
            authd.ask()?;
            Ok(authd)
        };
        match made() {
            Ok(authd) => Some(authd),
            Err(error) => {
                say(&format!(
                    "asking for {NAME} failed ({error:?}): this session's locks are refused"
                ));
                None
            }
        }
    }

    /// Send OPEN with a new channel's end, and watch ours.
    pub(crate) fn ask(&mut self) -> Result<(), Error> {
        let (ours, theirs) = channel::create(Native)?;
        let mut bytes = [0_u8; MAX_MESSAGE];
        let len = Message::named(Kind::Open, NAME, "")
            .encode(&mut bytes)
            .ok_or(Error::InvalidArgs)?;
        self.bootstrap
            .write_with(bytes.get(..len).unwrap_or_default(), [theirs.into_owned()])
            .map_err(|(error, _)| error)?;
        ours.wait_async(&self.port, Signals::READABLE | Signals::PEER_CLOSED, END)?;
        self.end = Some(ours);
        Ok(())
    }

    /// Whether the seat channel is open, ready or not.
    pub(crate) const fn is_open(&self) -> bool {
        self.end.is_some()
    }

    /// The descriptor to poll.
    pub(crate) const fn fd(&self) -> &OwnedFd {
        &self.fd
    }

    /// Everything waiting on the port.
    pub(crate) fn drain(&mut self) -> Vec<Heard> {
        let mut heard = Vec::new();
        while let Ok(packet) = self.port.wait(Deadline::At(0)) {
            match packet.key {
                BOOTSTRAP => {
                    self.read_bootstrap(&mut heard);
                    let _ = self
                        .bootstrap
                        .wait_async(&self.port, Signals::READABLE, BOOTSTRAP);
                }
                END => self.after_end(&mut heard),
                _ => {}
            }
        }
        heard
    }

    /// Read our end, then watch it again, or say it has gone.
    fn after_end(&mut self, heard: &mut Vec<Heard>) {
        if !self.read_end(heard) {
            self.end = None;
            heard.push(Heard::Gone);
            return;
        }
        if let Some(end) = &self.end {
            let _ = end.wait_async(&self.port, Signals::READABLE | Signals::PEER_CLOSED, END);
        }
    }

    /// ARM or DISARM to `authd`; false when it could not go.
    pub(crate) fn send(&self, record: &Record<'_>) -> bool {
        let Some(end) = &self.end else {
            return false;
        };
        let mut bytes = [0_u8; MAX_RECORD];
        record
            .encode(&mut bytes)
            .ok()
            .and_then(|len| bytes.get(..len))
            .is_some_and(|packet| end.write(packet).is_ok())
    }

    /// A REFUSED on the bootstrap channel: the name is not this unit's to
    /// use, or nobody offers it.
    fn read_bootstrap(&mut self, heard: &mut Vec<Heard>) {
        loop {
            let mut bytes = [0_u8; MAX_MESSAGE];
            match self.bootstrap.read(&mut bytes, &mut []) {
                Ok(received) => {
                    if let Some(message) =
                        Message::decode(bytes.get(..received.bytes).unwrap_or_default())
                        && message.kind == Kind::Refused
                    {
                        say(&format!(
                            "init refused {}: {}; this session's locks are refused",
                            message.name, message.detail
                        ));
                        self.end = None;
                        heard.push(Heard::Gone);
                    }
                }
                Err(ReadError::Failed(Error::ShouldWait) | _) => return,
            }
        }
    }

    /// `SEAT_READY` and `GRANT`s; false when `authd` closed its end.
    fn read_end(&self, heard: &mut Vec<Heard>) -> bool {
        let Some(end) = &self.end else {
            return false;
        };
        loop {
            let mut packet = [0_u8; MAX_RECORD];
            match end.read(&mut packet, &mut []) {
                Ok(received) => {
                    match Record::decode(packet.get(..received.bytes).unwrap_or_default()) {
                        Ok(Record::SeatReady) => heard.push(Heard::Ready),
                        Ok(Record::Grant { uid, epoch }) => heard.push(Heard::Grant { uid, epoch }),
                        Ok(_) | Err(_) => say("authd sent something not SEAT_READY or GRANT"),
                    }
                }
                Err(ReadError::Failed(Error::ShouldWait)) => return true,
                Err(_) => return false,
            }
        }
    }
}
