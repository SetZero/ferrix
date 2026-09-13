//! The smallest native program: a message through a channel and back.
//!
//! It makes a channel, writes a message into one end, waits for the other end
//! to say it is readable, reads the message back and compares it, then closes
//! the writing end and checks the reader is told the peer has gone. Exit 0
//! means every step did what the native ABI says; any other status is the
//! number of the step that did not, so the status is the diagnosis.
//!
//! It proves the runtime rather than the kernel — `_start`, the trap, the
//! argument registers, the pointer layouts, exit — on whichever architecture
//! it was built for. The kernel's side is proven by the boot check's own
//! native programs.

#![no_std]
#![no_main]

use ferrix_rt::native::channel::{self, ReadError};
use ferrix_rt::native::{Deadline, Error, Object, Signals};
use ferrix_rt::{Bootstrap, Kernel};

ferrix_rt::entry!(main);

/// What goes through the channel.
const MESSAGE: &[u8] = b"carried by a native program built on ferrix-rt";

/// Run the steps and turn the first failure into the exit status.
fn main(bootstrap: Bootstrap) -> i32 {
    // Nothing is asked of whoever started the process; holding the channel
    // until exit is all there is to do with it.
    let _held = bootstrap;
    match echo() {
        Ok(()) => 0,
        Err(step) => step,
    }
}

/// The steps, each failing with its own number.
fn echo() -> Result<(), i32> {
    let (writer, reader) = channel::create(Kernel).map_err(|_| 1)?;
    writer.write(MESSAGE).map_err(|_| 2)?;

    let signals = reader
        .wait_one(Signals::READABLE, Deadline::Never)
        .map_err(|_| 3)?;
    if !signals.intersects(Signals::READABLE) {
        return Err(4);
    }

    let mut buffer = [0_u8; 64];
    let received = reader.read(&mut buffer, &mut []).map_err(|_| 5)?;
    if buffer.get(..received.bytes) != Some(MESSAGE) {
        return Err(6);
    }

    writer.close().map_err(|_| 7)?;
    match reader.read(&mut buffer, &mut []) {
        Err(ReadError::Failed(Error::PeerClosed)) => Ok(()),
        _ => Err(8),
    }
}
