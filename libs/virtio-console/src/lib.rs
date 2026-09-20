//! A virtio-console driver, as logic over a transport and DMA memory it is
//! handed.
//!
//! This is `ferrix-virtio-net` for the multiport console device, over the same
//! traits and with the same two invariants, so what follows says only what is
//! different. `ferrix_virtio::console` is the device's numbers -- its queue
//! arithmetic, its configuration block, its control messages -- and this crate
//! is the order those are said in, what to do with each, and what to do when
//! the device lies.
//!
//! # A port is opened before it carries anything
//!
//! Every other driver here is running the moment `DRIVER_OK` is set. This one
//! is not. `docs/CLIPBOARD.md` §3.3 has a conversation to walk first, on the
//! control queues, and until it finishes the data queues carry nothing:
//!
//! 1. the driver sends `DEVICE_READY`, once its queues are up;
//! 2. the device sends `PORT_ADD` for each port it has;
//! 3. the driver answers `PORT_READY`, with `value` 0 for a port whose queues
//!    it did not set up -- see *Which queues are set up* below;
//! 4. the device sends `PORT_NAME`, which is how the wanted port is
//!    recognised;
//! 5. the device sends `PORT_OPEN` when the host end is ready;
//! 6. the driver answers `PORT_OPEN`, and only then do bytes flow.
//!
//! [`Driver::on_interrupt`] drives all of it, so a caller waits on its
//! interrupt and watches [`Driver::port`] rather than sequencing any of this
//! itself. The steps are not assumed to arrive in order: `PORT_NAME` may come
//! before or after `PORT_OPEN`, and [`Port::Open`] is reached when both the
//! name has matched and the host end is open, whichever was last.
//!
//! # The host end closing is not an error
//!
//! `PORT_OPEN` with `value` 0 means the viewer's window closed or the chardev
//! disconnected. The port goes back to [`Port::Waiting`], every chunk in
//! flight is abandoned, and the port may open again later -- the same device,
//! the same queues, a new host. A caller holding state that belonged to the
//! old host (a clipboard grab, say) must drop it when [`Event::Closed`]
//! arrives, which is the whole reason that event exists.
//!
//! # Which queues are set up
//!
//! Virtio 1.2 §5.3.2 numbers port *N*'s queues `2N + 2` and `2N + 3` for every
//! *N* above zero, and port 0's 0 and 1. The control pair is 2 and 3. So the
//! device QEMU builds for `docs/CLIPBOARD.md` §3.1 -- one `virtserialport` --
//! has its port at queues 4 and 5, and queues 0 and 1 belong to a port 0 that
//! is never used and must exist anyway.
//!
//! That is six queues, which is [`QUEUE_COUNT`], and this driver sets up
//! exactly those: ports 0 and 1, and the control pair. It does **not** set up
//! a queue per port the device declares, which for QEMU's default
//! `max_ports` of 31 would be sixty-four queues and sixty-four rings of pinned
//! memory for a device with one port on it.
//!
//! The cost is written down rather than glossed: the wanted port must be port
//! 0 or port 1, and a device that puts it anywhere else fails bring-up with
//! [`ConsoleError::PortNotPrepared`] rather than being driven wrongly. The
//! port is still found **by name** and never by number -- a number is the
//! device's to choose -- so this is a bound on which numbers can be served,
//! not an assumption about which one it will be. A second `virtserialport`
//! ahead of the clipboard's would want [`QUEUE_COUNT`] raised, and nothing
//! else.
//!
//! # DMA memory is freed only after a reset
//!
//! As in `ferrix-virtio-net`: the rings, the control area and both data
//! regions are held in [`ManuallyDrop`], dropping a [`Driver`] leaks rather
//! than frees, and only [`Driver::shutdown`] gives them back -- as
//! [`Teardown::Released`] if the reset finished and [`Teardown::Wedged`] if it
//! did not.
//!
//! # Trust
//!
//! Everything the device says is checked before it is acted on: the used rings
//! by [`SplitQueue`], the configuration and every control message by
//! `ferrix_virtio::console`, and a completion's length against the buffer that
//! was posted. A control message longer than the slot it was written into, a
//! completion for a chain the driver did not publish, and `DEVICE_NEEDS_RESET`
//! each mean the device broke the protocol: it is marked `FAILED`, nothing
//! more is taken from it, and it waits for [`Driver::shutdown`].

#![no_std]
// Forbidden in the shipped library. The tests implement
// `ferrix_virtio::QueueMemory`, an unsafe trait, for their fake memory, and a
// forbid cannot be relaxed for one item; there it is denied, and the one
// implementation is exempted and argued at its site.
#![cfg_attr(not(test), forbid(unsafe_code))]
#![cfg_attr(test, deny(unsafe_code))]

use core::fmt;
use core::mem::ManuallyDrop;

use ferrix_virtio::console::{
    self, CONTROL_BYTES, Config, ConsoleError as WireError, Control, SPICE_PORT_NAME,
};
use ferrix_virtio::pci::{
    self, CommonConfig, DEVICE_STATUS, QueueAddresses, STATUS_DEVICE_NEEDS_RESET, STATUS_FAILED,
    TransportError,
};
use ferrix_virtio::{
    Buffer, DeviceConfig, Layout, MAX_QUEUE_SIZE, PAGE_SIZE, QueueError, QueueMemory, SplitQueue,
};

// At the root, so every test module can name `std`.
#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

/// Queues this driver sets up: ports 0 and 1, and the control pair. See
/// *Which queues are set up*.
pub const QUEUE_COUNT: usize = 6;

/// The highest port number whose queues are among [`QUEUE_COUNT`].
pub const MAX_PREPARED_PORT: u32 = 1;

/// ISR status bit: a queue has something for the driver (virtio 1.2 §4.1.4.5).
pub const ISR_QUEUE: u8 = 1;

/// ISR status bit: the device's configuration changed. A console's
/// `max_nr_ports` does not move under a running driver, so this is reported
/// and not acted on.
pub const ISR_CONFIG: u8 = 2;

/// Bytes of control area one control message takes.
///
/// A control message is [`CONTROL_BYTES`] and then, for `PORT_NAME`, a name.
/// Sixty-four is enough for any name QEMU sends -- `com.redhat.spice.0` is
/// eighteen -- and divides a page, so a slot never straddles two pages whose
/// device addresses do not follow on, and every control buffer is exactly one
/// descriptor.
pub const CONTROL_SLOT: usize = 64;

/// Control buffers posted for the device to write into.
///
/// The device sends at most a handful in a burst -- one `PORT_ADD`, one
/// `PORT_NAME` and one `PORT_OPEN` per port -- and every one taken is posted
/// again before [`Driver::on_interrupt`] returns.
pub const CONTROL_RECEIVE_SLOTS: u16 = 8;

/// Control messages the driver may have in flight at once.
pub const CONTROL_TRANSMIT_SLOTS: u16 = 4;

/// Control slots altogether: the receive bank and then the transmit bank.
pub const CONTROL_SLOTS: u16 = CONTROL_RECEIVE_SLOTS + CONTROL_TRANSMIT_SLOTS;

/// The smallest queue worth running: the control conversation needs a buffer
/// posted for each of `PORT_ADD`, `PORT_NAME` and `PORT_OPEN` with room to
/// answer, and a data queue with fewer than four entries cannot keep a buffer
/// posted while another is held.
const MIN_QUEUE_SIZE: u16 = 4;

/// The most runs of device-contiguous pages one chunk may be scattered over.
///
/// A receive buffer is [`Info::receive_stride`] bytes, a power of two that
/// divides a page, so it is always one run. A chunk the caller submits is the
/// caller's own bytes and may straddle pages the pin placed apart; four runs
/// is a chunk of at least three whole pages, which is far more than the
/// clipboard's traffic and bounds the chain a hostile length could ask for.
pub const MAX_SEGMENTS: usize = 4;

/// The device's registers, as the process that drives it reaches them.
///
/// The [`CommonConfig`] accessors are the common configuration block, exactly
/// as the kernel implements them over its mapping; the [`DeviceConfig`]
/// accessors are the device-specific block. Both take offsets within their
/// block.
pub trait Transport: CommonConfig + DeviceConfig {
    /// Ring the doorbell for `queue`, whose `queue_notify_off` the device
    /// reported as `notify_off`: write `queue` as a `u16` at
    /// `notify_off × notify_off_multiplier` in the notification block
    /// ([`pci::notify_offset`]).
    fn notify(&mut self, queue: u16, notify_off: u16);

    /// The MSI-X table entry `queue` should interrupt through, or
    /// [`pci::NO_VECTOR`] for a line interrupt. A device with one vector for
    /// every queue answers the same number each time, which is allowed: the
    /// handler drains all of them either way.
    fn queue_vector(&self, queue: u16) -> u16;

    /// Acknowledge the interrupt that woke the driver and say why it came:
    /// the ISR status byte, whose read clears it, for a line interrupt; or
    /// [`ISR_QUEUE`] for MSI-X, whose vector already says.
    fn acknowledge_interrupt(&mut self) -> u8;
}

/// Pinned memory, as the device addresses of its pages.
///
/// The addresses are the ones the pin's address query wrote after the range
/// was pinned into the device's domain. The driver must not assume any
/// relation between them and physical addresses: under a translating IOMMU the
/// device reaches only what its domain maps.
pub trait DevicePages {
    /// The device address of page `i` of the pinned range, for every page in
    /// order: [`PAGE_SIZE`] bytes each, and not necessarily consecutive.
    fn device_pages(&self) -> &[u64];
}

/// A region of pinned memory the driver reads and writes byte by byte.
///
/// Offsets are bytes from the start of the region; the driver passes only
/// offsets below `device_pages().len() × PAGE_SIZE`.
pub trait Bytes: DevicePages {
    /// Read the byte at `offset`.
    fn read_u8(&self, offset: usize) -> u8;
    /// Write the byte at `offset`.
    fn write_u8(&mut self, offset: usize, value: u8);
}

/// One run of bytes to hand the device, already device-contiguous.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Segments {
    /// The runs themselves.
    buffers: [Buffer; MAX_SEGMENTS],
    /// How many of them are meant.
    count: usize,
}

impl Segments {
    /// The runs, as a slice.
    fn as_slice(&self) -> &[Buffer] {
        self.buffers.get(..self.count).unwrap_or(&[])
    }
}

/// A chunk of bytes to send, as it sits in the transmit data region.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chunk {
    /// The caller's name for it, handed back when the device is done with it.
    pub id: u64,
    /// Where the chunk starts, in bytes from the start of the transmit data
    /// region.
    pub offset: u64,
    /// How many bytes.
    pub len: u32,
}

/// Where the conversation of `docs/CLIPBOARD.md` §3.3 has got to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Port {
    /// `DEVICE_READY` is sent and no port has been named yet.
    Waiting,
    /// The wanted port has been named, and its host end is not open. Bytes
    /// submitted now are refused.
    Named {
        /// Which port it turned out to be.
        port: u32,
    },
    /// The port is open and carries bytes both ways.
    Open {
        /// Which port.
        port: u32,
    },
    /// The device removed the port. It does not come back without a reset.
    Removed {
        /// Which port.
        port: u32,
    },
}

impl Port {
    /// The port's number, once one has been named.
    #[must_use]
    pub const fn number(&self) -> Option<u32> {
        match *self {
            Port::Waiting => None,
            Port::Named { port } | Port::Open { port } | Port::Removed { port } => Some(port),
        }
    }

    /// Whether bytes flow now.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        matches!(*self, Port::Open { .. })
    }
}

/// Something the device did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// Bytes arrived on the port. They are `len` from `offset` of the receive
    /// data region and stay there until [`Driver::release`] gives the buffer
    /// back.
    Received {
        /// The buffer they came in, for [`Driver::release`].
        buffer: u16,
        /// Where they start in the receive data region.
        offset: u64,
        /// How many bytes.
        len: u32,
    },
    /// A chunk was sent: its bytes in the transmit data region are the
    /// caller's again.
    Sent {
        /// The id [`Driver::submit`] was given.
        id: u64,
    },
    /// The wanted port was named and matched. Bytes do not flow until
    /// [`Event::Opened`].
    Named {
        /// Which port it turned out to be.
        port: u32,
    },
    /// The host end opened. The port carries bytes from here.
    Opened {
        /// Which port.
        port: u32,
    },
    /// The host end went away. Whatever state belonged to that host must be
    /// forgotten; the port may open again.
    Closed {
        /// Which port.
        port: u32,
        /// Chunks accepted and now abandoned, which will never be
        /// [`Event::Sent`].
        abandoned: u16,
    },
    /// The device removed the port.
    Removed {
        /// Which port.
        port: u32,
    },
}

/// What [`Driver::on_interrupt`] found.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Drained {
    /// Events written to the start of the caller's slice.
    pub events: usize,
    /// The device says its configuration changed.
    pub config_changed: bool,
    /// The device has completed more than the slice had room for, or the
    /// control conversation has more to say. Call again.
    pub more: bool,
    /// Receive buffers posted again after the drain.
    pub refilled: u16,
}

/// How [`Driver::init`] should go about it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options {
    /// Reads of `device_status` a reset may take.
    pub reset_polls: u32,
    /// The largest queue wanted; the device, the memory and the slots may make
    /// it smaller.
    pub max_queue_size: u16,
    /// Bytes from one receive buffer to the next: a power of two that divides
    /// a page, so a buffer is always one descriptor.
    pub receive_stride: u32,
    /// The name of the port to open. A port with any other name is answered
    /// `PORT_READY` and then left alone.
    pub port_name: &'static [u8],
}

impl Default for Options {
    fn default() -> Self {
        Options {
            reset_polls: 100_000,
            max_queue_size: 64,
            receive_stride: 512,
            port_name: SPICE_PORT_NAME,
        }
    }
}

/// What the driver agreed with the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Info {
    /// The features negotiated.
    pub features: u64,
    /// The device's configuration.
    pub config: Config,
    /// Entries in each queue; all six are the same size.
    pub queue_size: u16,
    /// Receive buffers carved out of the receive data region.
    ///
    /// Half the queue: a receive chain is one descriptor, so posting more than
    /// this could never fill and posting fewer would leave descriptors idle
    /// while the caller holds what it has been handed.
    pub receive_buffers: u16,
    /// Bytes from one receive buffer to the next.
    pub receive_stride: u32,
}

/// Bookkeeping for one queue entry.
///
/// The caller provides [`QUEUE_COUNT`] banks of them, each as long as a queue,
/// so the driver allocates nothing. Within a bank `chain` belongs to the chain
/// whose *head descriptor* is this index, and `busy`/`held` to the *buffer* of
/// this index; the two are unrelated, and a buffer's chain very often ends up
/// recorded in some other buffer's slot.
#[derive(Clone, Copy, Debug)]
pub struct Slot {
    /// The chain whose head descriptor is this slot's index within its bank.
    chain: Option<ChainRecord>,
    /// Whether this slot's buffer is in use, by the device or by the caller.
    busy: bool,
    /// Whether the caller holds this slot's receive buffer, having been handed
    /// what was in it and not yet given it back.
    held: bool,
}

impl Slot {
    /// A slot with nothing in it.
    pub const EMPTY: Slot = Slot {
        chain: None,
        busy: false,
        held: false,
    };
}

impl Default for Slot {
    fn default() -> Self {
        Slot::EMPTY
    }
}

/// One published chain.
#[derive(Clone, Copy, Debug)]
struct ChainRecord {
    /// The buffer slot it used.
    index: u16,
    /// The caller's id, for a chunk being sent; `None` for a buffer posted for
    /// the device to write, which nobody is waiting on.
    id: Option<u64>,
    /// Bytes the chain covers, for checking what the device says it wrote.
    len: u32,
}

/// Why a device could not be brought up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InitError {
    /// The status protocol failed: a reset that did not finish, a device
    /// without `VERSION_1` or without `MULTIPORT`, refused features, a missing
    /// queue, or a device that needs a reset.
    Transport(TransportError),
    /// The configuration block is truncated or declares an unusable
    /// `max_nr_ports`.
    Config(WireError),
    /// The device has fewer queues than the ports it declares, which is the
    /// one thing `queue_count` can catch before a queue is touched.
    QueuesMissing {
        /// Queues the device says it has.
        have: u16,
        /// Queues [`QUEUE_COUNT`] needs.
        want: u16,
    },
    /// [`Options::receive_stride`] is not a power of two dividing a page.
    BadStride(u32),
    /// No queue of at least four entries fits the device, the rings' pages,
    /// the control area, the receive buffers and the slots together.
    NoRoom,
    /// The device would not give a queue the MSI-X vector asked for.
    VectorRefused {
        /// The queue.
        queue: u16,
        /// The vector asked for.
        asked: u16,
        /// What the device kept.
        kept: u16,
    },
    /// The device broke the protocol while the queues were being filled or
    /// `DEVICE_READY` sent.
    Device(ConsoleError),
}

/// Why a chunk was not accepted.
///
/// A refused chunk is never tracked: whichever of these came back, the id is
/// not in flight and no [`Event::Sent`] will name it. The error is the answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubmitError {
    /// The device has already failed; shut the driver down.
    Broken,
    /// The port is not open, so there is nothing to send into.
    NotOpen,
    /// A chunk of no bytes.
    Empty,
    /// The chunk is past the end of the transmit data region.
    OutsideData,
    /// A page's device address overflows.
    BadAddress,
    /// The chunk is scattered over more than [`MAX_SEGMENTS`] runs of pages.
    TooManySegments,
    /// No free slot or not enough free descriptors now; take some completions
    /// and try again.
    QueueFull,
    /// The device broke the protocol while the chunk was being published, so
    /// the driver has failed and will send nothing more. The chunk did not go
    /// out.
    Device(ConsoleError),
}

/// Why a receive buffer could not be given back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReleaseError {
    /// There is no such buffer.
    OutOfRange(u16),
    /// That buffer is not one the caller was handed: it is in the ring, or it
    /// was released already.
    NotHeld(u16),
}

/// How the device broke the protocol. After any of these the driver has set
/// `FAILED` and takes nothing more from it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConsoleError {
    /// The driver has already failed on an earlier error.
    Broken,
    /// The device set `DEVICE_NEEDS_RESET`.
    NeedsReset,
    /// The rings say something impossible.
    Queue(QueueError),
    /// A control message the device wrote is malformed, or names a port whose
    /// queues do not exist.
    Wire(WireError),
    /// The device says it wrote more into a buffer than the buffer holds.
    Overrun {
        /// The queue it came back on.
        queue: u16,
        /// What the device claimed.
        written: u32,
        /// What was posted.
        capacity: u32,
    },
    /// The wanted port was named, and its queues are not among the
    /// [`QUEUE_COUNT`] this driver set up.
    PortNotPrepared(u32),
    /// A completion names a chain the driver has no record of.
    UnknownChain {
        /// The queue it came back on.
        queue: u16,
        /// The head descriptor named.
        head: u16,
    },
    /// The driver's own records disagree with the queue -- which only memory
    /// the device can write can have caused.
    Bookkeeping,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            InitError::Transport(error) => write!(f, "{error}"),
            InitError::Config(error) => write!(f, "{error:?}"),
            InitError::QueuesMissing { have, want } => {
                write!(f, "the device has {have} queues and needs {want}")
            }
            InitError::BadStride(stride) => {
                write!(f, "a receive stride of {stride} does not divide a page")
            }
            InitError::NoRoom => f.write_str("no queue fits the memory given"),
            InitError::VectorRefused { queue, asked, kept } => {
                write!(f, "queue {queue} kept vector {kept:#x} for {asked:#x}")
            }
            InitError::Device(error) => write!(f, "{error}"),
        }
    }
}

impl fmt::Display for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match *self {
            SubmitError::Broken => "the device has failed",
            SubmitError::NotOpen => "the port is not open",
            SubmitError::Empty => "the chunk has no bytes",
            SubmitError::OutsideData => "the chunk is past the end of its region",
            SubmitError::BadAddress => "a page's address overflows",
            SubmitError::TooManySegments => "the chunk is scattered over too many pages",
            SubmitError::QueueFull => "the transmit queue is full",
            SubmitError::Device(error) => return write!(f, "{error}"),
        })
    }
}

impl fmt::Display for ReleaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ReleaseError::OutOfRange(buffer) => write!(f, "there is no buffer {buffer}"),
            ReleaseError::NotHeld(buffer) => write!(f, "buffer {buffer} is not held"),
        }
    }
}

impl fmt::Display for ConsoleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ConsoleError::Broken => f.write_str("the device has failed"),
            ConsoleError::NeedsReset => f.write_str("the device needs a reset"),
            ConsoleError::Queue(error) => write!(f, "the rings are corrupt: {error:?}"),
            ConsoleError::Wire(error) => write!(f, "a control message is bad: {error:?}"),
            ConsoleError::Overrun {
                queue,
                written,
                capacity,
            } => write!(
                f,
                "queue {queue} says it wrote {written} bytes into {capacity}"
            ),
            ConsoleError::PortNotPrepared(port) => {
                write!(f, "port {port} has no queues this driver set up")
            }
            ConsoleError::UnknownChain { queue, head } => {
                write!(f, "chain {head} is not in flight on queue {queue}")
            }
            ConsoleError::Bookkeeping => {
                f.write_str("the driver's records disagree with the queue")
            }
        }
    }
}

/// Everything a driver is built from.
pub struct Parts<T, R, A, D, S> {
    /// The device's registers.
    pub transport: T,
    /// One ring region per queue of [`QUEUE_COUNT`], indexed by queue number.
    pub rings: [R; QUEUE_COUNT],
    /// The control messages' buffers: [`CONTROL_SLOT`] bytes per slot of
    /// [`CONTROL_SLOTS`].
    pub control: A,
    /// Where bytes arriving on the port are put.
    pub receive_data: D,
    /// Where bytes to send are written by the caller.
    pub transmit_data: D,
    /// Bookkeeping, [`QUEUE_COUNT`] banks of a queue's worth each.
    pub slots: S,
}

/// A queue's memory as it comes back: never given to a queue, or inside the
/// queue it was built into.
pub enum Rings<R> {
    /// Initialisation stopped before the queue was built.
    Unused(R),
    /// The queue, with the memory inside it.
    Queue(SplitQueue<R>),
}

/// A driver's parts, handed back.
pub struct Released<T, R, A, D, S> {
    /// The device's registers.
    pub transport: T,
    /// Each queue's memory, indexed by queue number.
    pub rings: [Rings<R>; QUEUE_COUNT],
    /// The control area.
    pub control: A,
    /// The receive data region.
    pub receive_data: D,
    /// The transmit data region.
    pub transmit_data: D,
    /// The bookkeeping.
    pub slots: S,
}

impl<T, R, A, D, S: AsRef<[Slot]>> Released<T, R, A, D, S> {
    /// The ids of chunks accepted and never answered, which the caller must
    /// now treat as lost.
    pub fn abandoned(&self) -> impl Iterator<Item = u64> + '_ {
        self.slots
            .as_ref()
            .iter()
            .filter_map(|slot| slot.chain.and_then(|record| record.id))
    }
}

/// How a driver ended.
pub enum Teardown<T, R, A, D, S> {
    /// The device reset: it holds no address into this memory, which may be
    /// unpinned.
    Released(Released<T, R, A, D, S>),
    /// The device did not reset and may still write to this memory, so it is
    /// never dropped. Take it out of the `ManuallyDrop` only to leak it
    /// somewhere else.
    Wedged(ManuallyDrop<Released<T, R, A, D, S>>),
}

/// A failed [`Driver::init`]: why, and the parts.
pub struct InitFailure<T, R, A, D, S> {
    /// What went wrong.
    pub error: InitError,
    /// The parts, released only if the reset after the failure finished.
    pub teardown: Teardown<T, R, A, D, S>,
}

impl<R> fmt::Debug for Rings<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Rings::Unused(_) => "Rings::Unused",
            Rings::Queue(_) => "Rings::Queue",
        })
    }
}

impl<T, R, A, D, S> fmt::Debug for Parts<T, R, A, D, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Parts").finish_non_exhaustive()
    }
}

impl<T, R, A, D, S> fmt::Debug for Released<T, R, A, D, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Released")
            .field("rings", &self.rings)
            .finish_non_exhaustive()
    }
}

impl<T, R, A, D, S> fmt::Debug for Teardown<T, R, A, D, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Teardown::Released(released) => f.debug_tuple("Released").field(released).finish(),
            Teardown::Wedged(released) => f.debug_tuple("Wedged").field(&**released).finish(),
        }
    }
}

impl<T, R, A, D, S> fmt::Debug for InitFailure<T, R, A, D, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InitFailure")
            .field("error", &self.error)
            .field("teardown", &self.teardown)
            .finish()
    }
}

/// Everything the driver's memory is, before a queue has been built from any
/// of it.
struct Memory<R, A, D, S> {
    /// One ring region per queue.
    rings: [R; QUEUE_COUNT],
    /// The control area.
    control: A,
    /// The receive data region.
    receive_data: D,
    /// The transmit data region.
    transmit_data: D,
    /// The bookkeeping.
    slots: S,
}

/// Reset the device, and hand the parts back as that went.
fn teardown<T: CommonConfig, R, A, D, S>(
    mut transport: T,
    rings: [Rings<R>; QUEUE_COUNT],
    rest: (A, D, D, S),
    polls: u32,
) -> Teardown<T, R, A, D, S> {
    let reset = pci::reset(&mut transport, polls);
    let (control, receive_data, transmit_data, slots) = rest;
    let released = Released {
        transport,
        rings,
        control,
        receive_data,
        transmit_data,
        slots,
    };
    match reset {
        Ok(()) => Teardown::Released(released),
        Err(_) => Teardown::Wedged(ManuallyDrop::new(released)),
    }
}

/// Set `FAILED`, keeping the rest of the status.
fn set_failed<T: CommonConfig + ?Sized>(transport: &mut T) {
    let status = transport.read8(DEVICE_STATUS);
    transport.write8(DEVICE_STATUS, status | STATUS_FAILED);
}

/// The device address of `len` bytes at `start` in a region of `pages`, if
/// they are device-contiguous.
fn contiguous(pages: &[u64], start: usize, len: usize) -> Option<u64> {
    let page = usize::try_from(PAGE_SIZE).ok()?;
    let first = start / page;
    let last = start.checked_add(len)?.checked_sub(1)? / page;
    let base = *pages.get(first)?;
    let mut expected = base;
    for index in first + 1..=last {
        expected = expected.checked_add(PAGE_SIZE)?;
        if *pages.get(index)? != expected {
            return None;
        }
    }
    let address = base.checked_add(u64::try_from(start % page).ok()?)?;
    let _ = address.checked_add(u64::try_from(len).ok()?)?;
    Some(address)
}

/// Where the device finds each of `layout`'s three areas in `pages`, if each
/// is device-contiguous.
fn ring_addresses(layout: &Layout, pages: &[u64]) -> Option<QueueAddresses> {
    let region = u64::try_from(pages.len()).ok()?.checked_mul(PAGE_SIZE)?;
    if u64::try_from(layout.total_size).ok()? > region {
        return None;
    }
    Some(QueueAddresses {
        descriptors: contiguous(
            pages,
            layout.descriptor_table,
            layout.available_ring - layout.descriptor_table,
        )?,
        driver: contiguous(
            pages,
            layout.available_ring,
            layout.used_ring - layout.available_ring,
        )?,
        device: contiguous(
            pages,
            layout.used_ring,
            layout.total_size - layout.used_ring,
        )?,
    })
}

/// Bytes a region of `pages` pages holds.
fn region_bytes(pages: &[u64]) -> u64 {
    u64::try_from(pages.len())
        .unwrap_or(u64::MAX)
        .saturating_mul(PAGE_SIZE)
}

/// Split a run of `len` bytes at `offset` of `pages` into device-contiguous
/// runs, at most [`MAX_SEGMENTS`] of them.
fn segments(pages: &[u64], offset: u64, len: u32, writable: bool) -> Option<Segments> {
    if len == 0 {
        return None;
    }
    let end = offset.checked_add(u64::from(len))?;
    if end > region_bytes(pages) {
        return None;
    }
    let mut buffers = [Buffer::readable(0, 0); MAX_SEGMENTS];
    let mut count = 0;
    let mut at = offset;
    while at < end {
        let page = at / PAGE_SIZE;
        let base = *pages.get(usize::try_from(page).ok()?)?;
        let address = base.checked_add(at % PAGE_SIZE)?;
        // The run reaches the end of this page, and then on over every page
        // whose device address follows the one before it, until the chunk
        // ends or the addresses stop following on.
        let mut run_end = page.checked_add(1)?.checked_mul(PAGE_SIZE)?.min(end);
        while run_end < end {
            let next = run_end / PAGE_SIZE;
            let previous = *pages.get(usize::try_from(next.checked_sub(1)?).ok()?)?;
            let here = *pages.get(usize::try_from(next).ok()?)?;
            if here != previous.checked_add(PAGE_SIZE)? {
                break;
            }
            run_end = next.checked_add(1)?.checked_mul(PAGE_SIZE)?.min(end);
        }
        let span = u32::try_from(run_end.checked_sub(at)?).ok()?;
        let slot = buffers.get_mut(count)?;
        *slot = if writable {
            Buffer::writable(address, span)
        } else {
            Buffer::readable(address, span)
        };
        count += 1;
        at = run_end;
    }
    Some(Segments { buffers, count })
}

/// A queue size that fits everything, and where each queue's rings are.
struct Sizing {
    /// The layout every queue shares.
    layout: Layout,
    /// Each queue's ring addresses, by queue number.
    addresses: [QueueAddresses; QUEUE_COUNT],
}

/// What negotiation settled, before any queue is built.
struct Plan {
    /// The features agreed.
    features: u64,
    /// The configuration.
    config: Config,
    /// The queues' size and addresses.
    sizing: Sizing,
    /// Receive buffers the receive data region holds.
    receive_buffers: u16,
}

/// A virtio-console device, brought up and driven.
pub struct Driver<T, R, A, D, S> {
    /// The device's registers.
    transport: T,
    /// Every queue, by queue number, never dropped but by
    /// [`Driver::shutdown`].
    queues: [ManuallyDrop<SplitQueue<R>>; QUEUE_COUNT],
    /// The control messages' buffers, likewise.
    control: ManuallyDrop<A>,
    /// Where arriving bytes go, likewise.
    receive_data: ManuallyDrop<D>,
    /// Where bytes to send come from, likewise.
    transmit_data: ManuallyDrop<D>,
    /// Bookkeeping.
    slots: S,
    /// What was agreed.
    info: Info,
    /// Each queue's `queue_notify_off`, by queue number.
    notify_offs: [u16; QUEUE_COUNT],
    /// Reads a reset may take.
    reset_polls: u32,
    /// The name of the port to open.
    port_name: &'static [u8],
    /// Where the control conversation has got to.
    port: Port,
    /// Whether the wanted port's name has matched, which with `host_open`
    /// makes [`Port::Open`].
    named: bool,
    /// Whether the device has said its host end is open.
    host_open: bool,
    /// The error the device broke the protocol with, once it has.
    fault: Option<ConsoleError>,
    /// Chunks accepted and not yet answered.
    sending: u16,
    /// Receive buffers the caller holds.
    held: u16,
}

impl<T, R, A, D, S> fmt::Debug for Driver<T, R, A, D, S> {
    /// Leaves out the memory, whose reads have side effects for the device.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Driver")
            .field("info", &self.info)
            .field("port", &self.port)
            .field("fault", &self.fault)
            .field("sending", &self.sending)
            .field("held", &self.held)
            .finish_non_exhaustive()
    }
}

impl<T, R, A, D, S> Driver<T, R, A, D, S>
where
    T: Transport,
    R: QueueMemory + DevicePages,
    A: Bytes,
    D: DevicePages,
    S: AsRef<[Slot]> + AsMut<[Slot]>,
{
    /// Bring the device up: reset, `ACKNOWLEDGE`, `DRIVER`, features,
    /// `FEATURES_OK` confirmed, the configuration read, all [`QUEUE_COUNT`]
    /// queues built and enabled, `DRIVER_OK`, the control receive buffers
    /// posted, and `DEVICE_READY` sent.
    ///
    /// The port is not open when this returns and no byte has crossed it. The
    /// caller waits on the interrupt and drives [`Driver::on_interrupt`] until
    /// [`Driver::port`] is [`Port::Open`].
    ///
    /// # Errors
    ///
    /// On any failure the device is marked `FAILED` and reset, and the parts
    /// come back in an [`InitFailure`].
    pub fn init(
        parts: Parts<T, R, A, D, S>,
        options: Options,
    ) -> Result<Self, InitFailure<T, R, A, D, S>> {
        let Parts {
            mut transport,
            rings,
            control,
            receive_data,
            transmit_data,
            mut slots,
        } = parts;
        slots.as_mut().fill(Slot::EMPTY);

        let memory = Memory {
            rings,
            control,
            receive_data,
            transmit_data,
            slots,
        };
        let plan = match Self::plan(&mut transport, &memory, &options) {
            Ok(plan) => plan,
            Err(error) => return Err(Self::give_up_early(transport, memory, error, &options)),
        };
        let driver = Self::start(transport, memory, &plan, &options)?;
        Self::announce(driver)
    }

    /// Negotiate and size the queues against the memory in hand.
    fn plan(
        transport: &mut T,
        memory: &Memory<R, A, D, S>,
        options: &Options,
    ) -> Result<Plan, InitError> {
        let stride = options.receive_stride;
        if stride == 0 || !stride.is_power_of_two() || u64::from(stride) > PAGE_SIZE {
            return Err(InitError::BadStride(stride));
        }
        let features = pci::negotiate(
            transport,
            console::DRIVER_FEATURES,
            console::REQUIRED_FEATURES,
            options.reset_polls,
        )
        .map_err(InitError::Transport)?;
        let config = Config::read(&*transport).map_err(InitError::Config)?;

        // `queue_count` is what the device must have for the ports it says it
        // has; this driver only touches the first `QUEUE_COUNT` of them, but a
        // device declaring ports it has no queues for is broken before a queue
        // is selected.
        let want = u16::try_from(QUEUE_COUNT).unwrap_or(u16::MAX);
        let have = console::queue_count(config.ports);
        if have < want {
            return Err(InitError::QueuesMissing { have, want });
        }

        let mut device_max = MAX_QUEUE_SIZE;
        for queue in 0..want {
            let max = pci::queue_max_size(transport, queue).map_err(InitError::Transport)?;
            device_max = device_max.min(max);
        }

        let control_slots = u64::from(CONTROL_SLOTS) * CONTROL_SLOT as u64;
        if region_bytes(memory.control.device_pages()) < control_slots {
            return Err(InitError::NoRoom);
        }
        let frames = region_bytes(memory.receive_data.device_pages());
        let buffers = u16::try_from(frames / u64::from(stride)).unwrap_or(u16::MAX);

        let cap = device_max
            .min(options.max_queue_size)
            .min(MAX_QUEUE_SIZE)
            .min(u16::try_from(memory.slots.as_ref().len() / QUEUE_COUNT).unwrap_or(u16::MAX));
        if cap < MIN_QUEUE_SIZE || buffers < MIN_QUEUE_SIZE / 2 {
            return Err(InitError::NoRoom);
        }

        // The largest power of two no larger than `cap`, which is at least
        // four. A smaller queue is tried when a ring region straddles pages
        // whose device addresses do not follow on, since a smaller table may
        // fit inside one page.
        let mut size: u16 = 1 << (15 - cap.leading_zeros());
        while size >= MIN_QUEUE_SIZE {
            if let Ok(layout) = Layout::for_size(size)
                && let Some(addresses) = Self::all_ring_addresses(&layout, &memory.rings)
            {
                return Ok(Plan {
                    features,
                    config,
                    sizing: Sizing { layout, addresses },
                    receive_buffers: buffers.min(size / 2),
                });
            }
            size /= 2;
        }
        Err(InitError::NoRoom)
    }

    /// Where every queue's rings are, if each region can hold them.
    fn all_ring_addresses(
        layout: &Layout,
        rings: &[R; QUEUE_COUNT],
    ) -> Option<[QueueAddresses; QUEUE_COUNT]> {
        let blank = QueueAddresses {
            descriptors: 0,
            driver: 0,
            device: 0,
        };
        let mut addresses = [blank; QUEUE_COUNT];
        for (slot, region) in addresses.iter_mut().zip(rings.iter()) {
            *slot = ring_addresses(layout, region.device_pages())?;
        }
        Some(addresses)
    }

    /// Fail before any queue was built.
    fn give_up_early(
        mut transport: T,
        memory: Memory<R, A, D, S>,
        error: InitError,
        options: &Options,
    ) -> InitFailure<T, R, A, D, S> {
        set_failed(&mut transport);
        let teardown = teardown(
            transport,
            memory.rings.map(Rings::Unused),
            (
                memory.control,
                memory.receive_data,
                memory.transmit_data,
                memory.slots,
            ),
            options.reset_polls,
        );
        InitFailure { error, teardown }
    }

    /// Build every queue, activate them all and say the driver is ready.
    fn start(
        mut transport: T,
        memory: Memory<R, A, D, S>,
        plan: &Plan,
        options: &Options,
    ) -> Result<Self, InitFailure<T, R, A, D, S>> {
        let Memory {
            rings,
            control,
            receive_data,
            transmit_data,
            slots,
        } = memory;
        // Every queue exists in memory before the device is told where any of
        // them is: `SplitQueue::new` zeroes the rings, and a device already
        // looking at one would see it wiped underneath it.
        let built = rings.map(|region| SplitQueue::new(plan.sizing.layout, region));

        let size = plan.sizing.layout.queue_size;
        let mut notify_offs = [0_u16; QUEUE_COUNT];
        let mut failure = None;
        for (queue, address) in plan.sizing.addresses.iter().enumerate() {
            let number = u16::try_from(queue).unwrap_or(u16::MAX);
            match Self::activate(&mut transport, number, size, *address) {
                Ok(active) => {
                    if let Some(slot) = notify_offs.get_mut(queue) {
                        *slot = active.notify_off;
                    }
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        let failure = failure.or_else(|| {
            pci::driver_ok(&mut transport)
                .err()
                .map(InitError::Transport)
        });
        if let Some(error) = failure {
            set_failed(&mut transport);
            // `built` is moved into the teardown whole: the queues were
            // created, so their memory is inside them whether or not the
            // device was ever told about them.
            let teardown = teardown(
                transport,
                built.map(Rings::Queue),
                (control, receive_data, transmit_data, slots),
                options.reset_polls,
            );
            return Err(InitFailure { error, teardown });
        }

        Ok(Driver {
            transport,
            queues: built.map(ManuallyDrop::new),
            control: ManuallyDrop::new(control),
            receive_data: ManuallyDrop::new(receive_data),
            transmit_data: ManuallyDrop::new(transmit_data),
            slots,
            info: Info {
                features: plan.features,
                config: plan.config,
                queue_size: size,
                receive_buffers: plan.receive_buffers,
                receive_stride: options.receive_stride,
            },
            notify_offs,
            reset_polls: options.reset_polls,
            port_name: options.port_name,
            port: Port::Waiting,
            named: false,
            host_open: false,
            fault: None,
            sending: 0,
            held: 0,
        })
    }

    /// Describe one queue to the device and check it kept the vector asked
    /// for.
    fn activate(
        transport: &mut T,
        queue: u16,
        size: u16,
        addresses: QueueAddresses,
    ) -> Result<pci::ActiveQueue, InitError> {
        let asked = transport.queue_vector(queue);
        let active = pci::activate_queue(transport, queue, size, addresses, asked)
            .map_err(InitError::Transport)?;
        if active.vector != asked {
            return Err(InitError::VectorRefused {
                queue,
                asked,
                kept: active.vector,
            });
        }
        Ok(active)
    }

    /// Post the control receive buffers and send `DEVICE_READY`, or give the
    /// whole driver back.
    fn announce(mut driver: Self) -> Result<Self, InitFailure<T, R, A, D, S>> {
        let started = driver
            .refill_control()
            .and_then(|_| driver.send_control(&Control::DeviceReady { ready: true }));
        match started {
            Ok(()) => Ok(driver),
            Err(error) => Err(InitFailure {
                error: InitError::Device(error),
                teardown: driver.shutdown(),
            }),
        }
    }

    /// What was agreed with the device.
    #[must_use]
    pub const fn info(&self) -> &Info {
        &self.info
    }

    /// Where the control conversation has got to.
    #[must_use]
    pub const fn port(&self) -> Port {
        self.port
    }

    /// The error the device broke the protocol with, if it has.
    #[must_use]
    pub const fn fault(&self) -> Option<ConsoleError> {
        self.fault
    }

    /// Chunks accepted and not yet answered.
    #[must_use]
    pub const fn chunks_in_flight(&self) -> u16 {
        self.sending
    }

    /// Receive buffers the caller has been handed and not given back.
    #[must_use]
    pub const fn buffers_held(&self) -> u16 {
        self.held
    }

    /// The device's registers.
    #[must_use]
    pub const fn transport(&self) -> &T {
        &self.transport
    }

    /// Send a chunk the caller has already written into the transmit data
    /// region.
    ///
    /// # Errors
    ///
    /// [`SubmitError`], none of which leaves the id tracked. In particular
    /// [`SubmitError::NotOpen`] until the conversation of §3.3 has finished,
    /// and again after [`Event::Closed`].
    pub fn submit(&mut self, chunk: &Chunk) -> Result<(), SubmitError> {
        if self.fault.is_some() {
            return Err(SubmitError::Broken);
        }
        let Port::Open { port } = self.port else {
            return Err(SubmitError::NotOpen);
        };
        if chunk.len == 0 {
            return Err(SubmitError::Empty);
        }
        let queue = console::transmit_queue(port);
        let plan = segments(
            self.transmit_data.device_pages(),
            chunk.offset,
            chunk.len,
            false,
        )
        .ok_or(SubmitError::OutsideData)?;
        if plan.count == 0 {
            return Err(SubmitError::Empty);
        }
        if usize::from(self.free_descriptors(queue)) < plan.count {
            return Err(SubmitError::QueueFull);
        }
        let index = self.take_buffer(queue).ok_or(SubmitError::QueueFull)?;

        match self.publish(queue, index, &plan, Some(chunk.id), chunk.len) {
            Ok(()) => {
                self.sending = self.sending.saturating_add(1);
                self.kick(queue);
                Ok(())
            }
            Err(ConsoleError::Queue(QueueError::OutOfDescriptors)) => {
                self.release_buffer(queue, index);
                Err(SubmitError::QueueFull)
            }
            Err(error) => {
                self.release_buffer(queue, index);
                self.break_down(error);
                Err(SubmitError::Device(error))
            }
        }
    }

    /// Give a receive buffer back, once what arrived in it has been read.
    ///
    /// Until this is called the bytes stay where [`Event::Received`] said they
    /// were, and the buffer is not in the ring. A caller that never releases
    /// stops receiving, which is the failure mode worth having: the
    /// alternative is a device writing over bytes being read.
    ///
    /// # Errors
    ///
    /// [`ReleaseError`] for a buffer that does not exist or was not held.
    pub fn release(&mut self, buffer: u16) -> Result<(), ReleaseError> {
        let Some(port) = self.port.number() else {
            return Err(ReleaseError::NotHeld(buffer));
        };
        if buffer >= self.info.receive_buffers {
            return Err(ReleaseError::OutOfRange(buffer));
        }
        let queue = console::receive_queue(port);
        if !self.slot(queue, buffer).is_some_and(|slot| slot.held) {
            return Err(ReleaseError::NotHeld(buffer));
        }
        if let Some(slot) = self.slot_mut(queue, buffer) {
            slot.held = false;
        }
        self.release_buffer(queue, buffer);
        self.held = self.held.saturating_sub(1);
        Ok(())
    }

    /// Take what the device has done, after an interrupt.
    ///
    /// The interrupt is acknowledged first, so a completion that lands while
    /// this runs raises another rather than being lost behind the
    /// acknowledgement. The control queues are drained before the data ones,
    /// because it is a control message that opens the port the data are on.
    ///
    /// # Errors
    ///
    /// A [`ConsoleError`] once the device breaks the protocol. Events taken
    /// before the fault in the same call are returned first, and the fault on
    /// the next call, so none is lost.
    pub fn on_interrupt(&mut self, out: &mut [Event]) -> Result<Drained, ConsoleError> {
        let isr = self.transport.acknowledge_interrupt();
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if self.transport.read8(DEVICE_STATUS) & STATUS_DEVICE_NEEDS_RESET != 0 {
            self.break_down(ConsoleError::NeedsReset);
            return Err(ConsoleError::NeedsReset);
        }

        let mut events = 0;
        let mut fault = None;

        // The control conversation first: a `PORT_OPEN` taken here is what
        // makes the data queues below worth draining in the same call.
        match self.drain_control(out, events) {
            Ok(taken) => events += taken,
            Err((taken, error)) => {
                events += taken;
                fault = Some(error);
            }
        }
        // The driver's own answers to what was just read, and the control
        // buffers posted again.
        if fault.is_none()
            && let Err(error) = self.refill_control()
        {
            fault = Some(error);
        }

        if fault.is_none()
            && let Some(port) = self.port.number()
        {
            let transmit = console::transmit_queue(port);
            match self.drain_data(transmit, out, events) {
                Ok(taken) => events += taken,
                Err((taken, error)) => {
                    events += taken;
                    fault = Some(error);
                }
            }
            if fault.is_none() {
                let receive = console::receive_queue(port);
                match self.drain_data(receive, out, events) {
                    Ok(taken) => events += taken,
                    Err((taken, error)) => {
                        events += taken;
                        fault = Some(error);
                    }
                }
            }
        }

        if let Some(error) = fault {
            self.break_down(error);
            if events == 0 {
                return Err(error);
            }
        }
        // A refill that fails has already recorded the fault, which the next
        // call returns; returning it here would drop the events just taken.
        let refilled = self.refill().unwrap_or(0);
        Ok(Drained {
            events,
            config_changed: isr & ISR_CONFIG != 0,
            more: self.fault.is_none() && self.anything_used(),
            refilled,
        })
    }

    /// Post every free receive buffer on the open port, and notify the device
    /// if any went in.
    ///
    /// Called at the end of every [`Driver::on_interrupt`] and when the port
    /// opens, so a caller that releases its buffers promptly never has to
    /// think about this.
    ///
    /// # Errors
    ///
    /// [`ConsoleError`], after which the driver is failed. A queue with no
    /// room left is not an error: this posts what fits and says how many that
    /// was.
    pub fn refill(&mut self) -> Result<u16, ConsoleError> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        let Port::Open { port } = self.port else {
            return Ok(0);
        };
        let queue = console::receive_queue(port);
        let stride = self.info.receive_stride;
        let mut posted = 0;
        for index in 0..self.info.receive_buffers {
            if self.slot(queue, index).is_none_or(|slot| slot.busy) {
                continue;
            }
            let offset = u64::from(index) * u64::from(stride);
            let Some(plan) = segments(self.receive_data.device_pages(), offset, stride, true)
            else {
                self.break_down(ConsoleError::Bookkeeping);
                return Err(ConsoleError::Bookkeeping);
            };
            if usize::from(self.free_descriptors(queue)) < plan.count {
                break;
            }
            if let Some(slot) = self.slot_mut(queue, index) {
                slot.busy = true;
            }
            match self.publish(queue, index, &plan, None, stride) {
                Ok(()) => posted += 1,
                Err(ConsoleError::Queue(QueueError::OutOfDescriptors)) => {
                    self.release_buffer(queue, index);
                    break;
                }
                Err(error) => {
                    self.release_buffer(queue, index);
                    self.break_down(error);
                    return Err(error);
                }
            }
        }
        if posted > 0 {
            self.kick(queue);
        }
        Ok(posted)
    }

    /// Reset the device and hand everything back -- the memory only if the
    /// reset finished. Chunks still in flight are [`Released::abandoned`].
    pub fn shutdown(self) -> Teardown<T, R, A, D, S> {
        let Driver {
            transport,
            queues,
            control,
            receive_data,
            transmit_data,
            slots,
            reset_polls,
            ..
        } = self;
        teardown(
            transport,
            queues.map(|queue| Rings::Queue(ManuallyDrop::into_inner(queue))),
            (
                ManuallyDrop::into_inner(control),
                ManuallyDrop::into_inner(receive_data),
                ManuallyDrop::into_inner(transmit_data),
                slots,
            ),
            reset_polls,
        )
    }

    // -----------------------------------------------------------------------
    // The control conversation.
    // -----------------------------------------------------------------------

    /// Take every control message the device wrote and follow it.
    fn drain_control(
        &mut self,
        out: &mut [Event],
        at: usize,
    ) -> Result<usize, (usize, ConsoleError)> {
        let queue = console::CONTROL_RECEIVE_QUEUE;
        let mut written = 0;
        // Each completion frees a chain the driver published, so there are
        // never more to take than the queue holds.
        for _ in 0..=self.info.queue_size {
            let used = match self
                .queue_mut(queue)
                .and_then(|q| q.take_used().transpose())
            {
                Some(Ok(used)) => used,
                None => break,
                Some(Err(error)) => return Err((written, ConsoleError::Queue(error))),
            };
            let record = match self
                .slot(queue, used.head)
                .and_then(|slot| slot.chain)
                .ok_or(ConsoleError::UnknownChain {
                    queue,
                    head: used.head,
                }) {
                Ok(record) => record,
                Err(error) => return Err((written, error)),
            };
            if used.written > record.len {
                return Err((
                    written,
                    ConsoleError::Overrun {
                        queue,
                        written: used.written,
                        capacity: record.len,
                    },
                ));
            }
            let mut bytes = [0_u8; CONTROL_SLOT];
            let base = usize::from(record.index) * CONTROL_SLOT;
            let taken = usize::try_from(used.written)
                .unwrap_or(CONTROL_SLOT)
                .min(CONTROL_SLOT);
            for (index, byte) in bytes.iter_mut().enumerate().take(taken) {
                *byte = self.control.read_u8(base + index);
            }
            self.clear_chain(queue, used.head);
            self.release_buffer(queue, record.index);

            let event = match self.follow(bytes.get(..taken).unwrap_or(&[])) {
                Ok(event) => event,
                Err(error) => return Err((written, error)),
            };
            if let Some(event) = event {
                match out.get_mut(at + written) {
                    Some(slot) => {
                        *slot = event;
                        written += 1;
                    }
                    // The caller's slice is full. The state has already moved,
                    // so the event cannot be replayed -- but `Drained::more`
                    // will be set, and the state itself is readable through
                    // `Driver::port`.
                    None => break,
                }
            }
        }
        Ok(written)
    }

    /// Follow one control message the device sent.
    fn follow(&mut self, bytes: &[u8]) -> Result<Option<Event>, ConsoleError> {
        let ports = self.info.config.ports;
        let message = Control::read(bytes, ports).map_err(ConsoleError::Wire)?;
        match message {
            Control::Add { port } => {
                // Every port the device adds is answered, so that its own
                // state machine moves on; a port whose queues this driver did
                // not set up is answered `ready` 0 and left alone.
                let ready = port <= MAX_PREPARED_PORT;
                self.send_control(&Control::Ready { port, ready })?;
                Ok(None)
            }
            Control::Name { port, name } => {
                if name != self.port_name || self.named {
                    return Ok(None);
                }
                if port > MAX_PREPARED_PORT {
                    return Err(ConsoleError::PortNotPrepared(port));
                }
                self.named = true;
                Ok(Some(self.settle(port)))
            }
            Control::Open { port, open } => {
                // Only the wanted port, and only while it is one this driver
                // is still carrying: a port the device removed does not open
                // again without a reset.
                if self.port.number() != Some(port) || matches!(self.port, Port::Removed { .. }) {
                    return Ok(None);
                }
                if open {
                    if self.host_open {
                        return Ok(None);
                    }
                    self.host_open = true;
                    // The device's `PORT_OPEN` is answered with the driver's,
                    // and only then does either side send bytes.
                    self.send_control(&Control::Open { port, open: true })?;
                    Ok(Some(self.settle(port)))
                } else {
                    if !self.host_open {
                        return Ok(None);
                    }
                    self.host_open = false;
                    let abandoned = self.forget_port(port);
                    self.port = Port::Named { port };
                    Ok(Some(Event::Closed { port, abandoned }))
                }
            }
            Control::Remove { port } => {
                if self.port.number() != Some(port) {
                    return Ok(None);
                }
                self.host_open = false;
                let _ = self.forget_port(port);
                self.port = Port::Removed { port };
                Ok(Some(Event::Removed { port }))
            }
            // A console's size, an event for a port that is not the wanted
            // one, and an event a later device invents are all ignored: none
            // of them is an error, and answering them is not this driver's.
            Control::Console { .. }
            | Control::Resize { .. }
            | Control::Unknown { .. }
            | Control::Ready { .. }
            | Control::DeviceReady { .. } => Ok(None),
        }
    }

    /// Move to [`Port::Open`] if both halves have happened, and say which
    /// event that was.
    fn settle(&mut self, port: u32) -> Event {
        if self.host_open {
            self.port = Port::Open { port };
            // The ring is filled the moment the port opens, so the first byte
            // the host sends has somewhere to land. A failure here is recorded
            // as the fault and returned by the next call, as every refill is.
            let _ = self.refill();
            Event::Opened { port }
        } else {
            self.port = Port::Named { port };
            Event::Named { port }
        }
    }

    /// Drop every chain in flight on a port that has closed, and say how many
    /// of them were chunks the caller is still waiting on.
    fn forget_port(&mut self, port: u32) -> u16 {
        let mut abandoned = 0;
        for queue in [console::receive_queue(port), console::transmit_queue(port)] {
            let Some(bank) = self.bank(queue) else {
                continue;
            };
            let size = usize::from(self.info.queue_size);
            let Some(slots) = self.slots.as_mut().get_mut(bank..bank + size) else {
                continue;
            };
            for slot in slots.iter_mut() {
                if slot.chain.and_then(|record| record.id).is_some() {
                    abandoned += 1;
                }
                *slot = Slot::EMPTY;
            }
        }
        self.sending = self.sending.saturating_sub(abandoned);
        self.held = 0;
        abandoned
    }

    /// Write one control message into a transmit slot and publish it.
    fn send_control(&mut self, message: &Control<'_>) -> Result<(), ConsoleError> {
        let queue = console::CONTROL_TRANSMIT_QUEUE;
        // The transmit slots sit after the receive ones in the same area.
        let index = self.take_buffer(queue).ok_or(ConsoleError::Bookkeeping)?;
        let mut bytes = [0_u8; CONTROL_BYTES];
        let written = message.write(&mut bytes).map_err(ConsoleError::Wire)?;
        let base = usize::from(CONTROL_RECEIVE_SLOTS + index) * CONTROL_SLOT;
        for (at, byte) in bytes.iter().enumerate().take(written) {
            self.control.write_u8(base + at, *byte);
        }
        let len = u32::try_from(written).unwrap_or(CONTROL_BYTES as u32);
        let Some(address) = contiguous(self.control.device_pages(), base, written) else {
            self.release_buffer(queue, index);
            return Err(ConsoleError::Bookkeeping);
        };
        let plan = Segments {
            buffers: [Buffer::readable(address, len); MAX_SEGMENTS],
            count: 1,
        };
        match self.publish(queue, index, &plan, None, len) {
            Ok(()) => {
                self.kick(queue);
                Ok(())
            }
            Err(error) => {
                self.release_buffer(queue, index);
                Err(error)
            }
        }
    }

    /// Post every free control receive buffer, and take back the transmit
    /// buffers the device has finished reading.
    fn refill_control(&mut self) -> Result<u16, ConsoleError> {
        // The driver's own messages first: a slot the device has read is a
        // slot the next answer can go in, and the control conversation is
        // short enough that running out would deadlock it.
        let transmit = console::CONTROL_TRANSMIT_QUEUE;
        for _ in 0..=self.info.queue_size {
            let used = match self
                .queue_mut(transmit)
                .and_then(|queue| queue.take_used().transpose())
            {
                Some(Ok(used)) => used,
                None => break,
                Some(Err(error)) => return Err(ConsoleError::Queue(error)),
            };
            let Some(record) = self.slot(transmit, used.head).and_then(|slot| slot.chain) else {
                return Err(ConsoleError::UnknownChain {
                    queue: transmit,
                    head: used.head,
                });
            };
            self.clear_chain(transmit, used.head);
            self.release_buffer(transmit, record.index);
        }

        let queue = console::CONTROL_RECEIVE_QUEUE;
        let mut posted = 0;
        for index in 0..CONTROL_RECEIVE_SLOTS {
            if self.slot(queue, index).is_none_or(|slot| slot.busy) {
                continue;
            }
            let base = usize::from(index) * CONTROL_SLOT;
            let Some(address) = contiguous(self.control.device_pages(), base, CONTROL_SLOT) else {
                return Err(ConsoleError::Bookkeeping);
            };
            if self.free_descriptors(queue) == 0 {
                break;
            }
            // Zeroed, so a completion the device did not write reads as a
            // message of no bytes rather than as whatever was there before.
            for at in base..base + CONTROL_SLOT {
                self.control.write_u8(at, 0);
            }
            if let Some(slot) = self.slot_mut(queue, index) {
                slot.busy = true;
            }
            let len = CONTROL_SLOT as u32;
            let plan = Segments {
                buffers: [Buffer::writable(address, len); MAX_SEGMENTS],
                count: 1,
            };
            match self.publish(queue, index, &plan, None, len) {
                Ok(()) => posted += 1,
                Err(ConsoleError::Queue(QueueError::OutOfDescriptors)) => {
                    self.release_buffer(queue, index);
                    break;
                }
                Err(error) => {
                    self.release_buffer(queue, index);
                    return Err(error);
                }
            }
        }
        if posted > 0 {
            self.kick(queue);
        }
        Ok(posted)
    }

    // -----------------------------------------------------------------------
    // The data queues.
    // -----------------------------------------------------------------------

    /// Take completions from a data queue into `out` from `at`.
    fn drain_data(
        &mut self,
        queue: u16,
        out: &mut [Event],
        at: usize,
    ) -> Result<usize, (usize, ConsoleError)> {
        let receive = self.port.number().map(console::receive_queue) == Some(queue);
        let mut written = 0;
        for _ in 0..=self.info.queue_size {
            if out.get(at + written).is_none() {
                break;
            }
            let used = match self
                .queue_mut(queue)
                .and_then(|q| q.take_used().transpose())
            {
                Some(Ok(used)) => used,
                None => break,
                Some(Err(error)) => return Err((written, ConsoleError::Queue(error))),
            };
            let record = match self
                .slot(queue, used.head)
                .and_then(|slot| slot.chain)
                .ok_or(ConsoleError::UnknownChain {
                    queue,
                    head: used.head,
                }) {
                Ok(record) => record,
                Err(error) => return Err((written, error)),
            };
            // A device that says it wrote more than it was given is the one
            // lie that would make the caller read past its own buffer.
            if receive && used.written > record.len {
                return Err((
                    written,
                    ConsoleError::Overrun {
                        queue,
                        written: used.written,
                        capacity: record.len,
                    },
                ));
            }
            self.clear_chain(queue, used.head);

            let event = if receive {
                if let Some(slot) = self.slot_mut(queue, record.index) {
                    slot.held = true;
                }
                self.held = self.held.saturating_add(1);
                Event::Received {
                    buffer: record.index,
                    offset: u64::from(record.index) * u64::from(self.info.receive_stride),
                    len: used.written,
                }
            } else {
                self.release_buffer(queue, record.index);
                self.sending = self.sending.saturating_sub(1);
                Event::Sent {
                    id: record.id.unwrap_or_default(),
                }
            };
            if let Some(slot) = out.get_mut(at + written) {
                *slot = event;
                written += 1;
            }
        }
        Ok(written)
    }

    // -----------------------------------------------------------------------
    // Queues, slots and buffers.
    // -----------------------------------------------------------------------

    /// Whether any queue has a completion waiting.
    fn anything_used(&self) -> bool {
        self.queues.iter().any(|queue| queue.has_used())
    }

    /// Queue `queue`, if this driver set it up.
    fn queue_mut(&mut self, queue: u16) -> Option<&mut SplitQueue<R>> {
        self.queues
            .get_mut(usize::from(queue))
            .map(|queue| &mut **queue)
    }

    /// Free descriptors in `queue`, or none if it was not set up.
    fn free_descriptors(&self, queue: u16) -> u16 {
        self.queues
            .get(usize::from(queue))
            .map_or(0, |queue| queue.free_descriptors())
    }

    /// Ring `queue`'s doorbell, unless the device asked not to be notified.
    fn kick(&mut self, queue: u16) {
        let wants = self
            .queues
            .get(usize::from(queue))
            .is_some_and(|queue| queue.device_wants_notification());
        let notify_off = self
            .notify_offs
            .get(usize::from(queue))
            .copied()
            .unwrap_or_default();
        if wants {
            self.transport.notify(queue, notify_off);
        }
    }

    /// Publish a chain on `queue` and record it against `index`.
    fn publish(
        &mut self,
        queue: u16,
        index: u16,
        plan: &Segments,
        id: Option<u64>,
        len: u32,
    ) -> Result<(), ConsoleError> {
        let Some(ring) = self.queue_mut(queue) else {
            return Err(ConsoleError::Bookkeeping);
        };
        let head = ring
            .add_chain(plan.as_slice())
            .map_err(ConsoleError::Queue)?;
        let slot = self
            .slot_mut(queue, head)
            .filter(|slot| slot.chain.is_none())
            .ok_or(ConsoleError::Bookkeeping)?;
        slot.chain = Some(ChainRecord { index, id, len });
        Ok(())
    }

    /// Where `queue`'s bookkeeping bank starts, if it has one.
    fn bank(&self, queue: u16) -> Option<usize> {
        let queue = usize::from(queue);
        (queue < QUEUE_COUNT).then(|| queue * usize::from(self.info.queue_size))
    }

    /// Slot `index` of `queue`'s bank.
    fn slot(&self, queue: u16, index: u16) -> Option<&Slot> {
        let bank = self.bank(queue)?;
        self.slots.as_ref().get(bank + usize::from(index))
    }

    /// Slot `index` of `queue`'s bank, to be changed.
    fn slot_mut(&mut self, queue: u16, index: u16) -> Option<&mut Slot> {
        let at = self.bank(queue)? + usize::from(index);
        self.slots.as_mut().get_mut(at)
    }

    /// Forget the chain at head `head` of `queue`.
    fn clear_chain(&mut self, queue: u16, head: u16) {
        if let Some(slot) = self.slot_mut(queue, head) {
            slot.chain = None;
        }
    }

    /// How many buffers `queue` has, which is not the same as its entries: the
    /// control queues' buffers are slots of the control area, and the receive
    /// queue's are the receive data region's.
    fn buffer_count(&self, queue: u16) -> u16 {
        if queue == console::CONTROL_RECEIVE_QUEUE {
            CONTROL_RECEIVE_SLOTS
        } else if queue == console::CONTROL_TRANSMIT_QUEUE {
            CONTROL_TRANSMIT_SLOTS
        } else if self.port.number().map(console::receive_queue) == Some(queue) {
            self.info.receive_buffers
        } else {
            self.info.queue_size
        }
    }

    /// Take a free buffer of `queue`, or `None` when every one is in use.
    fn take_buffer(&mut self, queue: u16) -> Option<u16> {
        let most = usize::from(self.buffer_count(queue));
        let bank = self.bank(queue)?;
        let slots = self.slots.as_mut().get_mut(bank..)?;
        let found = slots
            .iter_mut()
            .take(most)
            .enumerate()
            .find(|(_, slot)| !slot.busy)?;
        found.1.busy = true;
        u16::try_from(found.0).ok()
    }

    /// Give a buffer of `queue` back.
    fn release_buffer(&mut self, queue: u16, index: u16) {
        if let Some(slot) = self.slot_mut(queue, index) {
            slot.busy = false;
        }
    }

    /// Stop trusting the device: record why, and tell it `FAILED`.
    fn break_down(&mut self, error: ConsoleError) {
        if self.fault.is_none() {
            self.fault = Some(error);
        }
        set_failed(&mut self.transport);
    }
}
