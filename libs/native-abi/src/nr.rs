//! The native system call numbers.
//!
//! Grouped in blocks of eight by object, with gaps, so a call added to a
//! family later lands next to its siblings rather than at the end of the
//! table. A number, once assigned, is never reused for a different call.
//!
//! Each call's arguments are listed on its [`NativeCall`] variant in register
//! order. `*u64` means a pointer to a 64-bit value in the caller's memory, for
//! the reason the crate documentation gives.

/// The first native number.
pub const FIRST: usize = 0x1000;
/// The last number the native range may ever use.
pub const LAST: usize = 0x1FFF;

/// [`NativeCall::HandleClose`].
pub const HANDLE_CLOSE: usize = 0x1000;
/// [`NativeCall::HandleDuplicate`].
pub const HANDLE_DUPLICATE: usize = 0x1001;
/// [`NativeCall::HandleReplace`].
pub const HANDLE_REPLACE: usize = 0x1002;

/// [`NativeCall::ObjectWaitOne`].
pub const OBJECT_WAIT_ONE: usize = 0x1008;
/// [`NativeCall::ObjectWaitAsync`].
pub const OBJECT_WAIT_ASYNC: usize = 0x1009;

/// [`NativeCall::ChannelCreate`].
pub const CHANNEL_CREATE: usize = 0x1010;
/// [`NativeCall::ChannelWrite`].
pub const CHANNEL_WRITE: usize = 0x1011;
/// [`NativeCall::ChannelRead`].
pub const CHANNEL_READ: usize = 0x1012;

/// [`NativeCall::PortCreate`].
pub const PORT_CREATE: usize = 0x1018;
/// [`NativeCall::PortQueue`].
pub const PORT_QUEUE: usize = 0x1019;
/// [`NativeCall::PortWait`].
pub const PORT_WAIT: usize = 0x101A;

/// [`NativeCall::VmoCreate`].
pub const VMO_CREATE: usize = 0x1020;
/// [`NativeCall::VmoRead`].
pub const VMO_READ: usize = 0x1021;
/// [`NativeCall::VmoWrite`].
pub const VMO_WRITE: usize = 0x1022;
/// [`NativeCall::VmoGetSize`].
pub const VMO_GET_SIZE: usize = 0x1023;
/// [`NativeCall::VmoMap`].
pub const VMO_MAP: usize = 0x1024;

/// [`NativeCall::JobCreate`].
pub const JOB_CREATE: usize = 0x1028;
/// [`NativeCall::JobKill`].
pub const JOB_KILL: usize = 0x1029;

/// [`NativeCall::InterruptCreate`].
pub const INTERRUPT_CREATE: usize = 0x1038;
/// [`NativeCall::InterruptBind`].
pub const INTERRUPT_BIND: usize = 0x1039;
/// [`NativeCall::InterruptAck`].
pub const INTERRUPT_ACK: usize = 0x103A;

/// [`NativeCall::IoMappingCreate`].
pub const IO_MAPPING_CREATE: usize = 0x1040;
/// [`NativeCall::IoMappingMap`].
pub const IO_MAPPING_MAP: usize = 0x1041;

/// A native system call.
///
/// `0x1030..=0x1037` is left for process creation, which is decided together
/// with the scheduler work that makes a process a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeCall {
    /// `(handle)`. Close a handle. The object lives on if anything else holds it.
    HandleClose,
    /// `(handle, rights)` → handle. A second handle with the same or fewer
    /// rights. Needs `DUPLICATE`.
    HandleDuplicate,
    /// `(handle, rights)` → handle. Swap a handle for one with the same or
    /// fewer rights; the original is closed. Needs no right, because it can
    /// only give something up.
    HandleReplace,
    /// `(handle, signals, deadline: *u64, observed: *u32)`. Block until any of
    /// `signals` is asserted or the deadline passes, then write the signals
    /// asserted at that moment. The deadline is absolute, in `CLOCK_MONOTONIC`
    /// nanoseconds; a null pointer waits forever, and a null `observed` is
    /// not written. Needs `WAIT`.
    ObjectWaitOne,
    /// `(handle, port, signals, key: *u64)`. Queue one packet on `port`, with
    /// `key`, the next time any of `signals` is asserted — at once, if one
    /// already is. One-shot. Needs `WAIT` on the object and `WRITE` on the port.
    ObjectWaitAsync,
    /// `(out: *[u32; 2])`. Make a channel, writing its two endpoints' handles.
    ChannelCreate,
    /// `(channel, bytes, byte_count, handles: *u32, handle_count)`. Send one
    /// message. The handles leave this process only if the write succeeds.
    /// Needs `WRITE`, and `TRANSFER` on every handle sent.
    ChannelWrite,
    /// `(channel, bytes, byte_capacity, handles: *u32, handle_capacity,
    /// actual: *ReadActual)`. Receive one message. If it does not fit, it stays
    /// queued and `actual` reports what it needs. Needs `READ`.
    ChannelRead,
    /// `()` → handle. Make a port.
    PortCreate,
    /// `(port, packet: *PortPacket)`. Queue a user packet. Needs `WRITE`.
    PortQueue,
    /// `(port, deadline: *u64, packet: *PortPacket)`. Take the next packet,
    /// waiting for one up to the deadline. Needs `READ`.
    PortWait,
    /// `(bytes)` → handle. Make an anonymous VMO, rounded up to whole pages.
    VmoCreate,
    /// `(vmo, buffer, count, offset: *u64)`. Copy out of a VMO. Needs `READ`.
    VmoRead,
    /// `(vmo, buffer, count, offset: *u64)`. Copy into a VMO. Needs `WRITE`.
    VmoWrite,
    /// `(vmo, size: *u64)`. The VMO's size in bytes.
    VmoGetSize,
    /// `(vmo, address, length, protection, offset: *u64)` → address. Map
    /// `length` bytes of a VMO from `offset`, both whole pages, at `address`,
    /// or wherever there is room if it is zero. Shared: a write through the
    /// mapping is a write to the VMO. `protection` is `MAP_READ`, or
    /// `MAP_READ | MAP_WRITE`; nothing maps a VMO executable. Needs `MAP`,
    /// and `READ`/`WRITE` for the protection asked for.
    VmoMap,
    /// `(parent)` → handle. Make a job inside `parent`. Needs `MANAGE`.
    JobCreate,
    /// `(job)`. End every process in the job and every job inside it. Needs
    /// `MANAGE`.
    JobKill,
    /// `(resource, vector)` → handle. Claim a hardware interrupt.
    InterruptCreate,
    /// `(interrupt, port, key: *u64)`. Deliver the interrupt to a port as
    /// packets carrying `key`. Needs `MANAGE`.
    InterruptBind,
    /// `(interrupt)`. Re-arm an interrupt after servicing it. Needs `MANAGE`.
    InterruptAck,
    /// `(resource, spec: *IoMappingSpec)` → handle. Claim an MMIO aperture.
    IoMappingCreate,
    /// `(mapping, address)` → address. Map an aperture. Needs `MAP`.
    IoMappingMap,
}

/// Every native call, in number order.
pub const ALL: [NativeCall; 23] = [
    NativeCall::HandleClose,
    NativeCall::HandleDuplicate,
    NativeCall::HandleReplace,
    NativeCall::ObjectWaitOne,
    NativeCall::ObjectWaitAsync,
    NativeCall::ChannelCreate,
    NativeCall::ChannelWrite,
    NativeCall::ChannelRead,
    NativeCall::PortCreate,
    NativeCall::PortQueue,
    NativeCall::PortWait,
    NativeCall::VmoCreate,
    NativeCall::VmoRead,
    NativeCall::VmoWrite,
    NativeCall::VmoGetSize,
    NativeCall::VmoMap,
    NativeCall::JobCreate,
    NativeCall::JobKill,
    NativeCall::InterruptCreate,
    NativeCall::InterruptBind,
    NativeCall::InterruptAck,
    NativeCall::IoMappingCreate,
    NativeCall::IoMappingMap,
];

/// Whether `number` is in the native range at all.
///
/// The dispatcher asks this before any Linux table, which is what keeps the
/// two ABIs from ever having to agree on a number.
#[must_use]
pub const fn is_native(number: usize) -> bool {
    number >= FIRST && number <= LAST
}

/// The call a number names, or `None` for a gap.
#[must_use]
pub const fn decode(number: usize) -> Option<NativeCall> {
    let call = match number {
        HANDLE_CLOSE => NativeCall::HandleClose,
        HANDLE_DUPLICATE => NativeCall::HandleDuplicate,
        HANDLE_REPLACE => NativeCall::HandleReplace,
        OBJECT_WAIT_ONE => NativeCall::ObjectWaitOne,
        OBJECT_WAIT_ASYNC => NativeCall::ObjectWaitAsync,
        CHANNEL_CREATE => NativeCall::ChannelCreate,
        CHANNEL_WRITE => NativeCall::ChannelWrite,
        CHANNEL_READ => NativeCall::ChannelRead,
        PORT_CREATE => NativeCall::PortCreate,
        PORT_QUEUE => NativeCall::PortQueue,
        PORT_WAIT => NativeCall::PortWait,
        VMO_CREATE => NativeCall::VmoCreate,
        VMO_READ => NativeCall::VmoRead,
        VMO_WRITE => NativeCall::VmoWrite,
        VMO_GET_SIZE => NativeCall::VmoGetSize,
        VMO_MAP => NativeCall::VmoMap,
        JOB_CREATE => NativeCall::JobCreate,
        JOB_KILL => NativeCall::JobKill,
        INTERRUPT_CREATE => NativeCall::InterruptCreate,
        INTERRUPT_BIND => NativeCall::InterruptBind,
        INTERRUPT_ACK => NativeCall::InterruptAck,
        IO_MAPPING_CREATE => NativeCall::IoMappingCreate,
        IO_MAPPING_MAP => NativeCall::IoMappingMap,
        _ => return None,
    };
    Some(call)
}

/// The number a call is made with.
#[must_use]
pub const fn number(call: NativeCall) -> usize {
    match call {
        NativeCall::HandleClose => HANDLE_CLOSE,
        NativeCall::HandleDuplicate => HANDLE_DUPLICATE,
        NativeCall::HandleReplace => HANDLE_REPLACE,
        NativeCall::ObjectWaitOne => OBJECT_WAIT_ONE,
        NativeCall::ObjectWaitAsync => OBJECT_WAIT_ASYNC,
        NativeCall::ChannelCreate => CHANNEL_CREATE,
        NativeCall::ChannelWrite => CHANNEL_WRITE,
        NativeCall::ChannelRead => CHANNEL_READ,
        NativeCall::PortCreate => PORT_CREATE,
        NativeCall::PortQueue => PORT_QUEUE,
        NativeCall::PortWait => PORT_WAIT,
        NativeCall::VmoCreate => VMO_CREATE,
        NativeCall::VmoRead => VMO_READ,
        NativeCall::VmoWrite => VMO_WRITE,
        NativeCall::VmoGetSize => VMO_GET_SIZE,
        NativeCall::VmoMap => VMO_MAP,
        NativeCall::JobCreate => JOB_CREATE,
        NativeCall::JobKill => JOB_KILL,
        NativeCall::InterruptCreate => INTERRUPT_CREATE,
        NativeCall::InterruptBind => INTERRUPT_BIND,
        NativeCall::InterruptAck => INTERRUPT_ACK,
        NativeCall::IoMappingCreate => IO_MAPPING_CREATE,
        NativeCall::IoMappingMap => IO_MAPPING_MAP,
    }
}
