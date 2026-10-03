//! The seat channel (`docs/AUTH.md` §3.7): `ferrix.auth.seat`, which
//! `auth.service` offers and only the unit that `Uses=` it may open, so the
//! other end is init's word and nobody's claim.
//!
//! `authd` takes the bootstrap channel init gave it (`process_bootstrap`),
//! sends OFFER for the name with one end of a channel of its own, and reads
//! CONNECTs from the other: each carries a client's end, which replaces the
//! one before it, since a session has one compositor. On that end
//! `sessiond` sends ARM and DISARM, and `authd` sends GRANT. Nothing else is
//! read from it, and those three records are never taken from the socket.
//!
//! Everything waits on one port, polled as a descriptor beside the socket's
//! (`port_fd`). An `authd` that init gave no bootstrap channel -- a test's,
//! or one run by hand -- has no seat, and grants nothing.

use std::os::fd::{FromRawFd as _, OwnedFd};

use ferrix_auth_proto::{MAX_RECORD, Record};
use ferrix_native::channel::{self, Channel, ReadError};
use ferrix_native::port::{self, Port};
use ferrix_native::{Deadline, Error, Handle, Object, OwnedHandle, Raw, Signals, Syscall, pending};
use ferrix_native_abi::directory::{Kind, MAX_MESSAGE, Message};

use crate::audit::say;
use crate::engine::Engine;

/// The name `auth.service` offers.
pub(crate) const NAME: &str = "ferrix.auth.seat";

/// The port key of the channel CONNECTs arrive on.
const OFFERED: u64 = 1;

/// The port key of the client's end.
const CLIENT: u64 = 2;

/// Native calls through `syscall`, as `src/user/system/linux/init/init/src/sys.rs` makes them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Native;

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

/// The offered name, the client's end once one has connected, and the port
/// both are watched on.
pub(crate) struct Seat {
    offered: Channel<Native>,
    client: Option<Channel<Native>>,
    port: Port<Native>,
    fd: OwnedFd,
}

impl Seat {
    /// Offer [`NAME`] on the bootstrap channel. `None`, said why, when init
    /// gave none or the offer could not be made: `authd` then answers the
    /// socket as before and grants nothing.
    pub(crate) fn offer() -> Option<Seat> {
        let bootstrap = match pending::take_bootstrap(Native) {
            Ok(Some(handle)) => Channel::from_owned(handle),
            Ok(None) => {
                say("authd: no bootstrap channel; no seat channel, no grants");
                return None;
            }
            Err(error) => {
                say(&format!(
                    "authd: process_bootstrap failed ({error:?}); no grants"
                ));
                return None;
            }
        };
        let made = || -> Result<Seat, Error> {
            let (offered, theirs) = channel::create(Native)?;
            let mut bytes = [0_u8; MAX_MESSAGE];
            let len = Message::named(Kind::Offer, NAME, "")
                .encode(&mut bytes)
                .ok_or(Error::InvalidArgs)?;
            bootstrap
                .write_with(bytes.get(..len).unwrap_or_default(), [theirs.into_owned()])
                .map_err(|(error, _)| error)?;
            let port = port::create(Native)?;
            let raw = port.descriptor(true)?;
            // SAFETY: `port_fd` just made `raw`, and nothing else owns it.
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };
            offered.wait_async(&port, Signals::READABLE | Signals::PEER_CLOSED, OFFERED)?;
            Ok(Seat {
                offered,
                client: None,
                port,
                fd,
            })
        };
        match made() {
            Ok(seat) => {
                say(&format!("authd: offering {NAME}"));
                Some(seat)
            }
            Err(error) => {
                say(&format!(
                    "authd: offering {NAME} failed ({error:?}); no grants"
                ));
                None
            }
        }
    }

    /// The descriptor to poll: readable while the port has a packet.
    pub(crate) fn fd(&self) -> &OwnedFd {
        &self.fd
    }

    /// Take every packet off the port: CONNECTs, and ARM and DISARM from the
    /// client.
    pub(crate) fn drain(&mut self, engine: &mut Engine) {
        while let Ok(packet) = self.port.wait(Deadline::At(0)) {
            match packet.key {
                OFFERED => {
                    self.read_offered(engine);
                    let _ = self.offered.wait_async(
                        &self.port,
                        Signals::READABLE | Signals::PEER_CLOSED,
                        OFFERED,
                    );
                }
                CLIENT => self.after_client(engine),
                _ => {}
            }
        }
    }

    /// Read the client, then watch it again, or let it go.
    fn after_client(&mut self, engine: &mut Engine) {
        if !self.read_client(engine) {
            self.client = None;
            engine.seat_gone();
            say("authd: the seat's client went; nothing is armed");
            return;
        }
        if let Some(client) = &self.client {
            let _ = client.wait_async(&self.port, Signals::READABLE | Signals::PEER_CLOSED, CLIENT);
        }
    }

    /// Send the engine's grants to the client. A grant with no client to go
    /// to is dropped: it was for a session that is gone.
    pub(crate) fn send(&mut self, engine: &mut Engine) {
        for grant in engine.take_grants() {
            let Some(client) = &self.client else {
                continue;
            };
            let record = Record::Grant {
                uid: grant.uid,
                epoch: grant.epoch,
            };
            let mut bytes = [0_u8; MAX_RECORD];
            let sent = record
                .encode(&mut bytes)
                .ok()
                .and_then(|len| bytes.get(..len))
                .map(|packet| client.write(packet));
            if !matches!(sent, Some(Ok(()))) {
                say("authd: a grant could not be sent down the seat channel");
            }
        }
    }

    /// CONNECTs on the offered channel: each one's end is the client now.
    fn read_offered(&mut self, engine: &mut Engine) {
        loop {
            let mut bytes = [0_u8; MAX_MESSAGE];
            let mut handles = [Handle::INVALID; 1];
            let received = match self.offered.read(&mut bytes, &mut handles) {
                Ok(received) => received,
                Err(ReadError::Failed(Error::ShouldWait)) => return,
                Err(_) => return,
            };
            let [end] = handles;
            let end = (received.handles == 1).then(|| OwnedHandle::from_raw(Native, end));
            let Some(message) = Message::decode(bytes.get(..received.bytes).unwrap_or_default())
            else {
                continue;
            };
            let (Kind::Connect, Some(end)) = (message.kind, end) else {
                continue;
            };
            let client = Channel::from_owned(end);
            if client
                .wait_async(&self.port, Signals::READABLE | Signals::PEER_CLOSED, CLIENT)
                .is_err()
            {
                continue;
            }
            // A new session's compositor: what the last one armed is void.
            engine.seat_gone();
            let mut ready = [0_u8; MAX_RECORD];
            if let Ok(len) = Record::SeatReady.encode(&mut ready)
                && client.write(ready.get(..len).unwrap_or_default()).is_err()
            {
                say("authd: the seat's client could not be greeted");
            }
            self.client = Some(client);
            say(&format!("authd: {} opened {NAME}", message.detail));
        }
    }

    /// ARM and DISARM from the client; false when it has gone.
    fn read_client(&mut self, engine: &mut Engine) -> bool {
        let Some(client) = &self.client else {
            return false;
        };
        loop {
            let mut packet = [0_u8; MAX_RECORD];
            match client.read(&mut packet, &mut []) {
                Ok(received) => {
                    match Record::decode(packet.get(..received.bytes).unwrap_or_default()) {
                        Ok(Record::Arm { uid, epoch }) => engine.arm(uid, epoch),
                        Ok(Record::Disarm { epoch }) => engine.disarm(epoch),
                        Ok(_) | Err(_) => {
                            say("authd: the seat's client sent something not ARM or DISARM");
                        }
                    }
                }
                Err(ReadError::Failed(Error::ShouldWait)) => return true,
                Err(_) => return false,
            }
        }
    }
}
