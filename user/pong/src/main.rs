//! A native service for the init's directory (`docs/INIT.md` §6, and §15's
//! stage five): the other end of what `Offers=` and `Uses=` promise.
//!
//! Started by init as `Type=native`, in its unit's cgroup, with its bootstrap
//! channel. It says READY on it, then answers every CONNECT init forwards:
//! down the client's end it writes `pong <name> to <client>`, and closes the
//! end. When init closes the bootstrap channel -- the service is being
//! stopped -- it exits 0. Any other exit status is the number of the step
//! that went wrong, so the status is the diagnosis.

#![no_std]
#![no_main]

use ferrix_native_abi::directory::{Kind, MAX_MESSAGE, Message};
use ferrix_rt::native::channel::{Channel, ReadError};
use ferrix_rt::native::{Deadline, Error, Object, Signals};
use ferrix_rt::native::{Handle, OwnedHandle};
use ferrix_rt::{Bootstrap, Kernel};

ferrix_rt::entry!(main);

/// How a run ended, as its exit status.
#[derive(Debug, Clone, Copy)]
#[repr(i32)]
enum Step {
    NoBootstrap = 1,
    SayReady = 2,
    Wait = 3,
    Read = 4,
    NotAMessage = 5,
    Answer = 6,
}

fn main(bootstrap: Bootstrap) -> i32 {
    match serve(bootstrap) {
        Ok(()) => 0,
        Err(step) => step as i32,
    }
}

/// READY, then CONNECTs until init lets go.
fn serve(bootstrap: Bootstrap) -> Result<(), Step> {
    let channel = bootstrap.ok_or(Step::NoBootstrap)?;
    let mut out = [0_u8; MAX_MESSAGE];
    let len = Message::ready().encode(&mut out).ok_or(Step::SayReady)?;
    channel
        .write(out.get(..len).unwrap_or_default())
        .map_err(|_| Step::SayReady)?;
    loop {
        let _ = channel
            .wait_one(Signals::READABLE | Signals::PEER_CLOSED, Deadline::Never)
            .map_err(|_| Step::Wait)?;
        let mut bytes = [0_u8; MAX_MESSAGE];
        let mut handles = [Handle::INVALID; 1];
        let received = match channel.read(&mut bytes, &mut handles) {
            Ok(received) => received,
            Err(ReadError::Failed(Error::ShouldWait)) => continue,
            Err(ReadError::Failed(Error::PeerClosed)) => return Ok(()),
            Err(_) => return Err(Step::Read),
        };
        let [end] = handles;
        let end = (received.handles == 1).then(|| OwnedHandle::from_raw(Kernel, end));
        let message = Message::decode(bytes.get(..received.bytes).unwrap_or_default())
            .ok_or(Step::NotAMessage)?;
        if message.kind != Kind::Connect {
            continue;
        }
        let Some(end) = end else {
            return Err(Step::NotAMessage);
        };
        answer(&Channel::from_owned(end), message)?;
    }
}

/// `pong <name> to <client>`, down the client's end.
fn answer(end: &Channel<Kernel>, message: Message<'_>) -> Result<(), Step> {
    let mut text = [0_u8; MAX_MESSAGE];
    let mut at = 0;
    for part in ["pong ", message.name, " to ", message.detail] {
        let bytes = part.as_bytes();
        let slot = text.get_mut(at..at + bytes.len()).ok_or(Step::Answer)?;
        slot.copy_from_slice(bytes);
        at += bytes.len();
    }
    end.write(text.get(..at).unwrap_or_default())
        .map_err(|_| Step::Answer)
}
