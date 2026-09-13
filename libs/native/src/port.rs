//! Ports: the event queue one thread waits on for many sources.

use ferrix_native_abi::nr;
use ferrix_native_abi::types::{PACKET_USER, PortPacket};

use crate::call::{Call, Syscall};
use crate::error::{Error, decode_handle, decode_unit};
use crate::handle::{Deadline, Object, OwnedHandle, object_handle, register};

/// A packet's size in a caller's buffer.
const PACKET_BYTES: usize = size_of::<PortPacket>();

object_handle!(
    /// A port.
    Port
);

/// `port_create`.
///
/// # Errors
///
/// [`Error::NoMemory`], [`Error::NoHandles`].
pub fn create<S: Syscall>(sys: S) -> Result<Port<S>, Error> {
    let handle = decode_handle(Call::new(nr::PORT_CREATE).make(sys))?;
    Ok(Port::from_owned(OwnedHandle::from_raw(sys, handle)))
}

impl<S: Syscall> Port<S> {
    /// `port_queue`: a user packet carrying `key` and `data`.
    ///
    /// # Errors
    ///
    /// [`Error::ShouldWait`] when the queue is full; [`Error::AccessDenied`]
    /// without `WRITE`.
    pub fn queue(&self, key: u64, data: [u64; 2]) -> Result<(), Error> {
        let packet = packet_bytes(&PortPacket {
            key,
            kind: PACKET_USER,
            signals: 0,
            data,
        });
        let value = Call::new(nr::PORT_QUEUE)
            .value(register(self.handle()))
            .input(&packet)
            .make(self.syscall());
        decode_unit(value)
    }

    /// `port_wait`: the next packet, waiting up to `deadline` for one.
    ///
    /// # Errors
    ///
    /// [`Error::TimedOut`]; [`Error::AccessDenied`] without `READ`;
    /// [`Error::Interrupted`] if the process is killed while it waits.
    pub fn wait(&self, deadline: Deadline) -> Result<PortPacket, Error> {
        let deadline = deadline.bytes();
        let mut packet = [0_u8; PACKET_BYTES];
        let value = Call::new(nr::PORT_WAIT)
            .value(register(self.handle()))
            .optional_input(deadline.as_ref().map(<[u8; 8]>::as_slice))
            .output(&mut packet)
            .make(self.syscall());
        decode_unit(value).map(|()| packet_from(packet))
    }
}

/// A packet as the ABI lays it out: key, kind, signals, two data words.
pub(crate) fn packet_bytes(packet: &PortPacket) -> [u8; PACKET_BYTES] {
    let [first, second] = packet.data;
    let fields = packet
        .key
        .to_ne_bytes()
        .into_iter()
        .chain(packet.kind.to_ne_bytes())
        .chain(packet.signals.to_ne_bytes())
        .chain(first.to_ne_bytes())
        .chain(second.to_ne_bytes());
    let mut bytes = [0_u8; PACKET_BYTES];
    for (slot, byte) in bytes.iter_mut().zip(fields) {
        *slot = byte;
    }
    bytes
}

/// The packet `bytes` holds.
pub(crate) fn packet_from(bytes: [u8; PACKET_BYTES]) -> PortPacket {
    // Every offset is a constant inside the 32-byte array, so neither reader
    // can miss; the zero is only what a lint-clean read spells "cannot happen".
    let u64_at = |at: usize| {
        bytes
            .get(at..at + 8)
            .and_then(|word| <[u8; 8]>::try_from(word).ok())
            .map_or(0, u64::from_ne_bytes)
    };
    let u32_at = |at: usize| {
        bytes
            .get(at..at + 4)
            .and_then(|word| <[u8; 4]>::try_from(word).ok())
            .map_or(0, u32::from_ne_bytes)
    };
    PortPacket {
        key: u64_at(0),
        kind: u32_at(8),
        signals: u32_at(12),
        data: [u64_at(16), u64_at(24)],
    }
}
