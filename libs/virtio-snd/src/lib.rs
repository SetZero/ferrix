//! A virtio-snd driver, as logic over a transport and DMA memory it is
//! handed.
//!
//! The audio iteration (`docs/AUDIO.md`) runs this driver in a user process
//! under devmgr, one per card, as virtio-input's runs. The process holds an
//! `IoMapping` of the device's register blocks, an `Interrupt`, pinned DMA
//! memory, the core's buffer VMO pinned read-only, and the control channel to
//! the kernel's audio core; none of those can be had in a unit test. So this
//! crate is written against [`Transport`], [`DevicePages`] and [`Scratch`],
//! which the process implements over its handles, and holds everything else:
//!
//! * [`Driver::init`] negotiates features, reads the configuration block,
//!   sets up the control and transmit queues, sets `DRIVER_OK` and asks every
//!   stream's `PCM_INFO`; [`Driver::hello`] is what that comes to in the
//!   core's terms;
//! * [`Driver::on_ready`] takes READY and the published streams' buffers,
//!   and gives each stream its one configuration with `PCM_SET_PARAMS` and
//!   `PCM_PREPARE`;
//! * [`Driver::on_control`] follows the rest of the core's side: SUBMIT posts
//!   one chain on the transmit queue and starts the stream the first time,
//!   HALT stops and releases it, REFUSED and STOP end the driver;
//! * [`Driver::on_interrupt`] takes the device's completions and
//!   [`Driver::pop_message`] hands out the ELAPSED and HALTED they come to;
//! * [`Driver::shutdown`] resets the device and hands the memory back.
//!
//! # `DRIVER_OK` before HELLO
//!
//! virtio-input's driver sets `DRIVER_OK` only on READY, so a device the core
//! refuses never sees it. A sound device describes its streams only through
//! the control queue, and virtio 1.2 §3.1.1 lets a driver use a queue only
//! after `DRIVER_OK`, so this driver sets it at bring-up, before HELLO. A
//! refused device has then been told the driver is ready and is reset at
//! shutdown, which is all a reset is for.
//!
//! # Control requests wait
//!
//! Requests on the control queue are made one at a time, and each is waited
//! for by polling the used ring, [`Options::control_polls`] times with
//! [`Transport::spin`] between polls. They are few -- `PCM_INFO` once,
//! `SET_PARAMS` and `PREPARE` at READY and after each halt, `START` on a
//! stream's first submission, `STOP` and `RELEASE` at a halt -- and QEMU
//! answers them as the doorbell rings. A device that does not answer within
//! the polls is broken.
//!
//! # What goes on the transmit queue
//!
//! One chain per SUBMIT, as virtio 1.2 §5.14.6.8 lays it out: the 4-byte
//! `virtio_snd_pcm_xfer` from this driver's [`Scratch`], the submitted range
//! of the core's buffer as one device-readable descriptor per run of
//! device-contiguous pages, and the 8-byte status the device writes, from
//! [`Scratch`] again. The core's buffer is pinned read-only, so the device
//! reads samples straight from the core's pages and this driver never
//! touches one. The event queue is not set up: QEMU 9.2.4 never writes it
//! (`docs/AUDIO.md` §3.3), and nothing here would read what it wrote.
//!
//! # Completions go back in order
//!
//! The core takes a completion only for the oldest submission in flight
//! (`ferrix_sndctl::pcm`). QEMU completes a stream's buffers in the order it
//! took them, but a device need not, so a completion out of order is held
//! until those before it are done, and ELAPSED goes out oldest first.
//!
//! # A halt
//!
//! HALT is `PCM_STOP` if the stream was started, then `PCM_RELEASE`, which
//! virtio 1.2 §5.14.6.6.5.1 has the device answer only once it has completed
//! every buffer it holds; QEMU does so with status OK. Those completions go
//! to the core as ELAPSED like any other: everything in flight when the core
//! sent HALT is void to it, so it takes them and moves nothing. Once none is
//! left in flight the driver sends HALTED with nothing unplayed and prepares
//! the stream again, so that the next SUBMIT can start it.
//!
//! # Trust
//!
//! The device's word is checked by `ferrix_virtio::snd` -- a block with no
//! streams or too many, a response that is short or not OK, stream
//! information no device could give -- and by [`SplitQueue`] for the rings.
//! A transmit completion that wrote anything but its status, a completion of
//! a chain the driver did not post, a control request unanswered, refused, or
//! answered with the wrong length, and `DEVICE_NEEDS_RESET` all mean the
//! device broke the protocol: it is marked `FAILED` and nothing more is taken
//! from it. A buffer the device refuses with a status is not a broken
//! protocol: it goes to the core as ELAPSED not played, which the core turns
//! into an underrun.
//!
//! The core's word is checked too, since it decides what the device reads: a
//! READY naming a stream that does not play, a buffer too small for its size,
//! a configuration the device did not offer, and a SUBMIT outside its buffer
//! are refused and change nothing.
//!
//! # DMA memory is freed only after a reset
//!
//! As in `ferrix-virtio-input`: the rings and the scratch memory are held in
//! [`ManuallyDrop`], and [`Driver::shutdown`] hands them back as
//! [`Teardown::Released`] only if the reset finished.

#![no_std]
#![cfg_attr(not(test), forbid(unsafe_code))]
#![cfg_attr(test, deny(unsafe_code))]

use core::fmt;
use core::mem::ManuallyDrop;

use ferrix_sndctl::message::{
    Elapsed, Hello, MAX_PUBLISHED, MAX_STREAMS, Message, Published, Ready, Refusal, Submit,
};
use ferrix_sndctl::pcm::MAX_IN_FLIGHT;
use ferrix_virtio::pci::{
    self, CommonConfig, DEVICE_STATUS, QueueAddresses, STATUS_DEVICE_NEEDS_RESET, STATUS_FAILED,
    TransportError,
};
use ferrix_virtio::snd::{
    self, DIRECTION_OUTPUT, DeviceConfig, PCM_INFO_BYTES, PcmCommand, PcmInfo, PcmStatus,
    RESPONSE_BYTES, STATUS_BYTES, SetParams, SndError, XFER_BYTES,
};
use ferrix_virtio::{Buffer, Layout, PAGE_SIZE, QueueError, QueueMemory, SplitQueue};

pub mod format;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

/// ISR status bit: a queue has something for the driver.
pub const ISR_QUEUE: u8 = 1;

/// The control queue's size: one request is in flight at a time, so a few
/// descriptors are plenty.
pub const CONTROL_QUEUE_SIZE: u16 = 8;
/// The transmit queue's size, the size QEMU 9.2.4 gives it
/// (`virtio_add_queue(vdev, 64, …)` in `hw/audio/virtio-snd.c`).
pub const TX_QUEUE_SIZE: u16 = 64;

/// The most pages of the core's buffer a stream can have: 64 KiB.
pub const MAX_BUFFER_PAGES: usize = 16;
/// The most runs of device-contiguous pages one submission may cross.
pub const MAX_SEGMENTS: usize = 4;

/// Where a control request is written in [`Scratch`].
pub const REQUEST_AT: usize = 0;
/// Room for the longest control request.
pub const REQUEST_ROOM: usize = 64;
/// Where a control response is written.
pub const RESPONSE_AT: usize = 64;
/// Room for the longest response: `PCM_INFO` for every stream.
pub const RESPONSE_ROOM: usize = RESPONSE_BYTES + MAX_STREAMS * PCM_INFO_BYTES;
/// Where the transmit slots start: one per submission in flight, each the
/// transfer header at its start and the status [`SLOT_STATUS_AT`] into it.
pub const SLOTS_AT: usize = 512;
/// Bytes of one transmit slot.
pub const SLOT_BYTES: usize = 16;
/// Where the status lies in a slot.
pub const SLOT_STATUS_AT: usize = 8;
/// Bytes of [`Scratch`] the driver needs, all in one page.
pub const SCRATCH_BYTES: usize = SLOTS_AT + MAX_IN_FLIGHT * SLOT_BYTES;

/// Room for messages to the core between two calls of
/// [`Driver::pop_message`]: an ELAPSED per submission in flight, a HALTED per
/// stream, and as many again.
pub const OUTBOX: usize = 2 * MAX_IN_FLIGHT + 2 * MAX_PUBLISHED;

/// The device's registers, as the process that drives it reaches them.
pub trait Transport: CommonConfig + DeviceConfig {
    /// Ring the doorbell for `queue`, whose `queue_notify_off` is
    /// `notify_off`.
    fn notify(&mut self, queue: u16, notify_off: u16);

    /// The MSI-X table entry `queue` should interrupt through, or
    /// [`pci::NO_VECTOR`].
    fn queue_vector(&self, queue: u16) -> u16;

    /// Acknowledge the interrupt and say why it came: the ISR status byte for
    /// a line interrupt, [`ISR_QUEUE`] for MSI-X.
    fn acknowledge_interrupt(&mut self) -> u8;

    /// Called between two polls of a control request the device has not yet
    /// answered: the process may yield here.
    fn spin(&mut self);
}

/// Pinned memory, as the device addresses of its pages, in order.
pub trait DevicePages {
    /// One device address per page, [`PAGE_SIZE`] bytes each, not necessarily
    /// consecutive.
    fn device_pages(&self) -> &[u64];
}

/// The driver's own DMA memory: control requests and responses, and each
/// submission's header and status.
pub trait Scratch: DevicePages {
    /// Read the byte at `offset`.
    fn read_u8(&self, offset: usize) -> u8;
    /// Write the byte at `offset`.
    fn write_u8(&mut self, offset: usize, value: u8);
}

/// Everything a driver is built from.
pub struct Parts<T, R, S> {
    /// The device's registers.
    pub transport: T,
    /// The control queue's rings: pages holding a queue of
    /// [`CONTROL_QUEUE_SIZE`].
    pub control: R,
    /// The transmit queue's rings: pages holding a queue of
    /// [`TX_QUEUE_SIZE`].
    pub tx: R,
    /// The scratch memory: at least [`SCRATCH_BYTES`], in one page.
    pub scratch: S,
}

impl<T, R, S> fmt::Debug for Parts<T, R, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Parts").finish_non_exhaustive()
    }
}

/// How [`Driver::init`] and the control requests go about it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options {
    /// Reads of `device_status` a reset may take.
    pub reset_polls: u32,
    /// Polls of the used ring a control request may take.
    pub control_polls: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            reset_polls: 100_000,
            control_polls: 1_000_000,
        }
    }
}

/// What the driver agreed with the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Info {
    /// The features negotiated.
    pub features: u64,
    /// The configuration block.
    pub config: snd::Config,
    /// The control queue's size.
    pub control_size: u16,
    /// The transmit queue's size.
    pub tx_size: u16,
    /// The MSI-X vector of the control queue, or `NO_VECTOR`.
    pub control_vector: u16,
    /// The MSI-X vector of the transmit queue, or `NO_VECTOR`.
    pub tx_vector: u16,
}

/// Why a device could not be brought up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InitError {
    /// The status protocol failed.
    Transport(TransportError),
    /// The configuration block or a stream's information is unusable.
    Config(SndError),
    /// The rings' pages cannot hold a queue, or the scratch memory is too
    /// small or not contiguous.
    NoRoom,
    /// The device would not give a queue the vector asked for.
    VectorRefused {
        /// The queue.
        queue: u16,
        /// Asked for.
        asked: u16,
        /// Kept.
        kept: u16,
    },
    /// Asking the device its streams broke the protocol.
    Device(DeviceError),
}

/// How the device broke the protocol. The driver has set `FAILED`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceError {
    /// The device set `DEVICE_NEEDS_RESET`.
    NeedsReset,
    /// The rings say something impossible.
    Queue(QueueError),
    /// A response or status that `ferrix_virtio::snd` refuses.
    Protocol(SndError),
    /// A control request answered for a status other than OK: the request's
    /// code and the status.
    Refused {
        /// `REQUEST_*`.
        request: u32,
        /// `STATUS_*`.
        status: u32,
    },
    /// A control request not answered within [`Options::control_polls`].
    ControlTimeout,
    /// A completion for a chain the driver did not post.
    UnknownChain(u16),
}

/// Why a message from the core was not followed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlError {
    /// A message the core does not send now: its type.
    Unexpected(u32),
    /// A READY or SUBMIT the driver cannot follow without the device reading
    /// what it should not.
    Refused(Refusing),
    /// Following it found the device broken.
    Device(DeviceError),
}

/// What a READY or SUBMIT asked that the driver will not do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusing {
    /// A published count over [`MAX_PUBLISHED`], or other than the buffers
    /// given.
    Count,
    /// A stream the device does not have, or one that records.
    Stream(u32),
    /// A buffer smaller than its size, larger than [`MAX_BUFFER_PAGES`], or a
    /// period that does not fit it.
    Buffer,
    /// A format, rate or channel count the device did not offer.
    Configuration,
    /// A submission outside its buffer, empty, or about a stream not
    /// published.
    Range,
    /// More submissions than [`MAX_IN_FLIGHT`] at once, or one while its
    /// stream halts.
    Busy,
    /// A submission across more than [`MAX_SEGMENTS`] runs of pages.
    Scattered,
}

/// Where the driver is in its conversation with the core.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Brought up, HELLO not yet answered.
    Introduced,
    /// READY came: the streams are prepared.
    Running,
    /// The core refused the driver.
    Refused,
    /// The core asked the driver to stop.
    Stopping,
}

/// What a message from the core means for the glue.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
    /// SUBMIT or HALT, followed: send what [`Driver::pop_message`] gives.
    Followed,
    /// REFUSED: shut the driver down.
    Refused(Refusal),
    /// STOP: send STOPPED, then shut the driver down.
    Stop,
}

/// What one [`Driver::on_interrupt`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Drained {
    /// The ISR bits the interrupt was acknowledged with.
    pub isr: u8,
    /// Completions taken.
    pub taken: usize,
    /// Of those, buffers the device refused.
    pub refused: usize,
}

/// Counts over the driver's life.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Stats {
    /// Submissions posted.
    pub submitted: u64,
    /// Completions taken.
    pub completed: u64,
    /// Buffers the device refused.
    pub refused: u64,
    /// Halts done.
    pub halts: u64,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Transport(error) => write!(f, "{error}"),
            Self::Config(error) => write!(f, "{error:?}"),
            Self::NoRoom => f.write_str("a queue or the scratch memory does not fit"),
            Self::VectorRefused { queue, asked, kept } => {
                write!(
                    f,
                    "queue {queue}: the device kept vector {kept:#x} for {asked:#x}"
                )
            }
            Self::Device(error) => write!(f, "{error}"),
        }
    }
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NeedsReset => f.write_str("the device needs a reset"),
            Self::Queue(error) => write!(f, "the rings are corrupt: {error:?}"),
            Self::Protocol(error) => write!(f, "the device answered wrongly: {error:?}"),
            Self::Refused { request, status } => {
                write!(
                    f,
                    "the device refused request {request:#x} with {status:#x}"
                )
            }
            Self::ControlTimeout => f.write_str("the device did not answer a control request"),
            Self::UnknownChain(head) => write!(f, "chain {head} was not posted"),
        }
    }
}

/// A driver's parts, handed back.
pub struct Released<T, R, S> {
    /// The device's registers.
    pub transport: T,
    /// The control queue's rings, inside the queue if one was built.
    pub control: Rings<R>,
    /// The transmit queue's rings.
    pub tx: Rings<R>,
    /// The scratch memory.
    pub scratch: S,
}

impl<T, R, S> fmt::Debug for Released<T, R, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Released")
            .field("control", &self.control)
            .field("tx", &self.tx)
            .finish_non_exhaustive()
    }
}

impl<R> fmt::Debug for Rings<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unused(_) => "Rings::Unused",
            Self::Queue(_) => "Rings::Queue",
        })
    }
}

/// A queue's memory as it comes back.
pub enum Rings<R> {
    /// Bring-up stopped before the queue was built.
    Unused(R),
    /// The queue, with the memory inside it.
    Queue(SplitQueue<R>),
}

/// How a driver ended.
pub enum Teardown<T, R, S> {
    /// The device reset; the memory may be unpinned.
    Released(Released<T, R, S>),
    /// The device did not reset and may still write to the memory, so it is
    /// never dropped.
    Wedged(ManuallyDrop<Released<T, R, S>>),
}

/// A failed [`Driver::init`].
pub struct InitFailure<T, R, S> {
    /// Why.
    pub error: InitError,
    /// The parts, released only if the reset finished.
    pub teardown: Teardown<T, R, S>,
}

impl<T, R, S> fmt::Debug for Teardown<T, R, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Released(_) => "Teardown::Released",
            Self::Wedged(_) => "Teardown::Wedged",
        })
    }
}

impl<T, R, S> fmt::Debug for InitFailure<T, R, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InitFailure")
            .field("error", &self.error)
            .field("teardown", &self.teardown)
            .finish()
    }
}

/// The device address of `len` bytes at `start` of `pages`, if they are
/// device-contiguous.
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
    base.checked_add(u64::try_from(start % page).ok()?)
}

/// Where the rings of `layout` are in `pages`.
fn ring_addresses(layout: &Layout, pages: &[u64]) -> Option<QueueAddresses> {
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

/// The largest power of two at most `max`, which is not zero.
const fn power_of_two_below(max: u16) -> u16 {
    1 << (15 - max.leading_zeros())
}

fn set_failed<T: CommonConfig + ?Sized>(transport: &mut T) {
    let status = transport.read8(DEVICE_STATUS);
    transport.write8(DEVICE_STATUS, status | STATUS_FAILED);
}

fn teardown<T: CommonConfig, R, S>(
    mut transport: T,
    control: Rings<R>,
    tx: Rings<R>,
    scratch: S,
    polls: u32,
) -> Teardown<T, R, S> {
    let reset = pci::reset(&mut transport, polls);
    let released = Released {
        transport,
        control,
        tx,
        scratch,
    };
    match reset {
        Ok(()) => Teardown::Released(released),
        Err(_) => Teardown::Wedged(ManuallyDrop::new(released)),
    }
}

/// A published stream as the driver drives it.
#[derive(Clone, Copy, Debug)]
struct Playback {
    published: Published,
    /// Its buffer's pages, as the device addresses them.
    pages: [u64; MAX_BUFFER_PAGES],
    page_count: usize,
    /// `PCM_START` was sent since the last prepare.
    started: bool,
    /// HALT came and HALTED has not yet gone.
    halting: bool,
}

/// A submission on the transmit queue: slot `i` of [`Scratch`].
#[derive(Clone, Copy, Debug)]
struct Posted {
    /// Which of `streams`.
    playback: usize,
    sequence: u32,
    head: u16,
    /// When it was posted, to send completions oldest first.
    order: u64,
    /// Once completed: whether it played.
    done: Option<bool>,
    latency: u32,
}

/// Messages for the core, oldest first.
#[derive(Clone, Copy, Debug)]
struct Outbox {
    slots: [Option<Message>; OUTBOX],
    first: usize,
    len: usize,
}

impl Outbox {
    const fn new() -> Self {
        Self {
            slots: [None; OUTBOX],
            first: 0,
            len: 0,
        }
    }

    fn push(&mut self, message: Message) -> bool {
        if self.len == OUTBOX {
            return false;
        }
        if let Some(slot) = self.slots.get_mut((self.first + self.len) % OUTBOX) {
            *slot = Some(message);
            self.len += 1;
            true
        } else {
            false
        }
    }

    fn pop(&mut self) -> Option<Message> {
        if self.len == 0 {
            return None;
        }
        let message = self.slots.get_mut(self.first).and_then(Option::take);
        self.first = (self.first + 1) % OUTBOX;
        self.len -= 1;
        message
    }
}

/// A virtio-snd device, brought up and driven.
pub struct Driver<T, R, S> {
    transport: T,
    control: ManuallyDrop<SplitQueue<R>>,
    tx: ManuallyDrop<SplitQueue<R>>,
    scratch: ManuallyDrop<S>,
    info: Info,
    infos: [PcmInfo; MAX_STREAMS],
    control_notify_off: u16,
    tx_notify_off: u16,
    options: Options,
    fault: Option<DeviceError>,
    phase: Phase,
    streams: [Option<Playback>; MAX_PUBLISHED],
    posted: [Option<Posted>; MAX_IN_FLIGHT],
    next_order: u64,
    outbox: Outbox,
    stats: Stats,
}

impl<T, R, S> fmt::Debug for Driver<T, R, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Driver")
            .field("info", &self.info)
            .field("phase", &self.phase)
            .field("fault", &self.fault)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

const NO_INFO: PcmInfo = PcmInfo {
    hda_fn_nid: 0,
    features: 0,
    raw_formats: 0,
    raw_rates: 0,
    direction: 0,
    channels_min: 0,
    channels_max: 0,
};

/// A queue's plan: its layout and where its rings are.
fn plan_queue<T: CommonConfig, R: DevicePages>(
    transport: &mut T,
    index: u16,
    wanted: u16,
    rings: &R,
) -> Result<(Layout, QueueAddresses), InitError> {
    let max = pci::queue_max_size(transport, index).map_err(InitError::Transport)?;
    let size = power_of_two_below(wanted.min(max));
    let layout = Layout::for_size(size).map_err(|_| InitError::NoRoom)?;
    let addresses = ring_addresses(&layout, rings.device_pages()).ok_or(InitError::NoRoom)?;
    Ok((layout, addresses))
}

fn activate<T: Transport>(
    transport: &mut T,
    index: u16,
    layout: &Layout,
    addresses: QueueAddresses,
) -> Result<pci::ActiveQueue, InitError> {
    let asked = transport.queue_vector(index);
    match pci::activate_queue(transport, index, layout.queue_size, addresses, asked) {
        Ok(active) if active.vector != asked => Err(InitError::VectorRefused {
            queue: index,
            asked,
            kept: active.vector,
        }),
        Ok(active) => Ok(active),
        Err(error) => Err(InitError::Transport(error)),
    }
}

impl<T, R, S> Driver<T, R, S>
where
    T: Transport,
    R: QueueMemory + DevicePages,
    S: Scratch,
{
    /// Bring the device up as far as HELLO: features, configuration, the two
    /// queues, `DRIVER_OK`, and every stream's information.
    ///
    /// # Errors
    ///
    /// An [`InitFailure`] with the parts, after `FAILED` and a reset, for a
    /// device that cannot be driven.
    #[expect(
        clippy::result_large_err,
        reason = "a failed bring-up hands every part back by value, as virtio-input's does"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "one bring-up sequence, each step handing its parts back on failure"
    )]
    pub fn init(parts: Parts<T, R, S>, options: Options) -> Result<Self, InitFailure<T, R, S>> {
        let Parts {
            mut transport,
            control,
            tx,
            scratch,
        } = parts;
        let fail = |mut transport: T, control, tx, scratch, error| {
            set_failed(&mut transport);
            Err(InitFailure {
                error,
                teardown: teardown(transport, control, tx, scratch, options.reset_polls),
            })
        };
        let agreed = pci::negotiate(
            &mut transport,
            snd::DRIVER_FEATURES,
            snd::REQUIRED_FEATURES,
            options.reset_polls,
        )
        .map_err(InitError::Transport)
        .and_then(|features| {
            snd::Config::read(&transport)
                .map(|config| (features, config))
                .map_err(InitError::Config)
        })
        .and_then(|agreed| {
            if contiguous(scratch.device_pages(), 0, SCRATCH_BYTES).is_some() {
                Ok(agreed)
            } else {
                Err(InitError::NoRoom)
            }
        });
        let (features, config) = match agreed {
            Ok(agreed) => agreed,
            Err(error) => {
                return fail(
                    transport,
                    Rings::Unused(control),
                    Rings::Unused(tx),
                    scratch,
                    error,
                );
            }
        };
        let plans = plan_queue(
            &mut transport,
            snd::CONTROL_QUEUE,
            CONTROL_QUEUE_SIZE,
            &control,
        )
        .and_then(|control_plan| {
            plan_queue(&mut transport, snd::TX_QUEUE, TX_QUEUE_SIZE, &tx)
                .map(|tx_plan| (control_plan, tx_plan))
        });
        let ((control_layout, control_at), (tx_layout, tx_at)) = match plans {
            Ok(plans) => plans,
            Err(error) => {
                return fail(
                    transport,
                    Rings::Unused(control),
                    Rings::Unused(tx),
                    scratch,
                    error,
                );
            }
        };
        let control = SplitQueue::new(control_layout, control);
        let tx = SplitQueue::new(tx_layout, tx);
        let active = activate(
            &mut transport,
            snd::CONTROL_QUEUE,
            &control_layout,
            control_at,
        )
        .and_then(|control_active| {
            activate(&mut transport, snd::TX_QUEUE, &tx_layout, tx_at)
                .map(|tx_active| (control_active, tx_active))
        });
        let (control_active, tx_active) = match active {
            Ok(active) => active,
            Err(error) => {
                return fail(
                    transport,
                    Rings::Queue(control),
                    Rings::Queue(tx),
                    scratch,
                    error,
                );
            }
        };
        let mut driver = Self {
            transport,
            control: ManuallyDrop::new(control),
            tx: ManuallyDrop::new(tx),
            scratch: ManuallyDrop::new(scratch),
            info: Info {
                features,
                config,
                control_size: control_layout.queue_size,
                tx_size: tx_layout.queue_size,
                control_vector: control_active.vector,
                tx_vector: tx_active.vector,
            },
            infos: [NO_INFO; MAX_STREAMS],
            control_notify_off: control_active.notify_off,
            tx_notify_off: tx_active.notify_off,
            options,
            fault: None,
            phase: Phase::Introduced,
            streams: [None; MAX_PUBLISHED],
            posted: [None; MAX_IN_FLIGHT],
            next_order: 0,
            outbox: Outbox::new(),
            stats: Stats::default(),
        };
        match driver.describe() {
            Ok(()) => Ok(driver),
            Err(error) => {
                set_failed(&mut driver.transport);
                Err(InitFailure {
                    error,
                    teardown: driver.shutdown(),
                })
            }
        }
    }

    /// `DRIVER_OK`, then `PCM_INFO` for every stream.
    fn describe(&mut self) -> Result<(), InitError> {
        pci::driver_ok(&mut self.transport).map_err(InitError::Transport)?;
        let count = self.info.config.streams;
        let mut query = [0_u8; snd::QUERY_BYTES];
        let _ = snd::write_pcm_info_query(0, count, &mut query).map_err(InitError::Config)?;
        let want = snd::pcm_info_response_bytes(count);
        let response = self.request(&query, want).map_err(InitError::Device)?;
        let response = response.get(..want).ok_or(InitError::NoRoom)?;
        for (index, slot) in (0..count).zip(self.infos.iter_mut()) {
            *slot = PcmInfo::read(response, index, count).map_err(InitError::Config)?;
        }
        Ok(())
    }

    /// What was agreed.
    #[must_use]
    pub const fn info(&self) -> &Info {
        &self.info
    }

    /// The streams' information, as the device gave it.
    #[must_use]
    pub fn streams(&self) -> &[PcmInfo] {
        self.infos
            .get(..self.info.config.streams as usize)
            .unwrap_or(&[])
    }

    /// The HELLO to send, for the device at `location`.
    #[must_use]
    pub fn hello(&self, location: u32) -> Hello {
        format::hello(self.streams(), location)
    }

    /// Where the conversation with the core is.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// The error the device broke the protocol with, if it has.
    #[must_use]
    pub const fn fault(&self) -> Option<DeviceError> {
        self.fault
    }

    /// Counts so far.
    #[must_use]
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// Submissions in flight now.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.posted.iter().flatten().count()
    }

    /// The device's registers.
    #[must_use]
    pub const fn transport(&self) -> &T {
        &self.transport
    }

    fn break_down(&mut self, error: DeviceError) -> DeviceError {
        if self.fault.is_none() {
            self.fault = Some(error);
            set_failed(&mut self.transport);
        }
        error
    }

    // -- control requests ----------------------------------------------------

    /// Make one control request and wait for its answer, `response_len`
    /// bytes, which come back.
    fn request(
        &mut self,
        request: &[u8],
        response_len: usize,
    ) -> Result<[u8; RESPONSE_ROOM], DeviceError> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if request.len() > REQUEST_ROOM || response_len > RESPONSE_ROOM || request.is_empty() {
            return Err(self.break_down(DeviceError::Protocol(SndError::Short {
                want: response_len,
                have: RESPONSE_ROOM,
            })));
        }
        for (index, byte) in request.iter().enumerate() {
            self.scratch.write_u8(REQUEST_AT + index, *byte);
        }
        for index in 0..response_len {
            self.scratch.write_u8(RESPONSE_AT + index, 0);
        }
        let pages = self.scratch.device_pages();
        let (Some(asked), Some(answer)) = (
            contiguous(pages, REQUEST_AT, request.len()),
            contiguous(pages, RESPONSE_AT, response_len),
        ) else {
            return Err(self.break_down(DeviceError::Queue(QueueError::DescriptorOutOfRange)));
        };
        // Both lengths are held below a page just above.
        let chain = [
            Buffer::readable(asked, request.len() as u32),
            Buffer::writable(answer, response_len as u32),
        ];
        let head = match self.control.add_chain(&chain) {
            Ok(head) => head,
            Err(error) => return Err(self.break_down(DeviceError::Queue(error))),
        };
        self.transport
            .notify(snd::CONTROL_QUEUE, self.control_notify_off);
        let mut polls = 0;
        let written = loop {
            match self.control.take_used() {
                Ok(Some(used)) if used.head == head => break used.written,
                Ok(Some(used)) => return Err(self.break_down(DeviceError::UnknownChain(used.head))),
                Ok(None) if polls < self.options.control_polls => {
                    polls += 1;
                    self.transport.spin();
                }
                Ok(None) => return Err(self.break_down(DeviceError::ControlTimeout)),
                Err(error) => return Err(self.break_down(DeviceError::Queue(error))),
            }
        };
        if written as usize != response_len {
            return Err(self.break_down(DeviceError::Protocol(SndError::Short {
                want: response_len,
                have: written as usize,
            })));
        }
        let mut response = [0_u8; RESPONSE_ROOM];
        for (index, byte) in response.iter_mut().take(response_len).enumerate() {
            *byte = self.scratch.read_u8(RESPONSE_AT + index);
        }
        Ok(response)
    }

    /// A request answered with a bare status, which must be OK.
    fn command(&mut self, request: &[u8]) -> Result<(), DeviceError> {
        let response = self.request(request, RESPONSE_BYTES)?;
        match snd::check_response(&response) {
            Ok(()) => Ok(()),
            Err(SndError::Status(status)) => {
                let code = request
                    .get(..4)
                    .and_then(|code| code.try_into().ok())
                    .map_or(0, u32::from_le_bytes);
                Err(self.break_down(DeviceError::Refused {
                    request: code,
                    status,
                }))
            }
            Err(error) => Err(self.break_down(DeviceError::Protocol(error))),
        }
    }

    fn stream_command(&mut self, command: PcmCommand, stream: u32) -> Result<(), DeviceError> {
        let mut request = [0_u8; snd::PCM_HDR_BYTES];
        let _ = command
            .write(stream, &mut request)
            .map_err(|error| self.break_down(DeviceError::Protocol(error)))?;
        self.command(&request)
    }

    /// `SET_PARAMS` and `PREPARE` for a published stream.
    fn prepare(&mut self, published: &Published, format: u8, rate: u8) -> Result<(), DeviceError> {
        let params = SetParams {
            stream: published.stream,
            buffer_bytes: published.buffer_bytes,
            period_bytes: published.period_bytes,
            features: 0,
            channels: published.channels,
            format,
            rate,
        };
        let mut request = [0_u8; snd::SET_PARAMS_BYTES];
        let _ = params
            .write(&mut request)
            .map_err(|error| self.break_down(DeviceError::Protocol(error)))?;
        self.command(&request)?;
        self.stream_command(PcmCommand::Prepare, published.stream)
    }

    // -- the core's messages -------------------------------------------------

    /// Judge one published stream of READY against what the device offered.
    fn judge(&self, published: &Published, pages: &[u64]) -> Result<(u8, u8), Refusing> {
        let info = self
            .streams()
            .get(published.stream as usize)
            .ok_or(Refusing::Stream(published.stream))?;
        if info.direction != DIRECTION_OUTPUT {
            return Err(Refusing::Stream(published.stream));
        }
        let page = PAGE_SIZE as usize;
        let buffer = published.buffer_bytes as usize;
        if pages.len() > MAX_BUFFER_PAGES
            || pages.len().saturating_mul(page) < buffer
            || buffer == 0
            || published.period_bytes == 0
            || published.period_bytes > published.buffer_bytes
        {
            return Err(Refusing::Buffer);
        }
        let format = format::to_virtio(u32::from(published.format))
            .filter(|format| info.has_format(*format))
            .ok_or(Refusing::Configuration)?;
        let rate = snd::rate_index(published.rate)
            .filter(|rate| info.has_rate(*rate))
            .ok_or(Refusing::Configuration)?;
        if !info.has_channels(published.channels) {
            return Err(Refusing::Configuration);
        }
        Ok((format, rate))
    }

    /// READY, with the published streams' buffers pinned for the device to
    /// read, in READY's order: each stream is configured and prepared.
    ///
    /// # Errors
    ///
    /// [`ControlError::Unexpected`] out of turn, [`ControlError::Refused`] for
    /// a READY the device cannot follow, before any request is made, and
    /// [`ControlError::Device`] for a device that broke the protocol.
    pub fn on_ready(&mut self, ready: &Ready, buffers: &[&[u64]]) -> Result<(), ControlError> {
        if self.phase != Phase::Introduced {
            return Err(ControlError::Unexpected(ferrix_sndctl::message::READY));
        }
        let count = ready.published as usize;
        if count > MAX_PUBLISHED || count != buffers.len() {
            return Err(ControlError::Refused(Refusing::Count));
        }
        let mut chosen = [(0_u8, 0_u8); MAX_PUBLISHED];
        for ((published, pages), choice) in ready.streams.iter().zip(buffers).zip(&mut chosen) {
            *choice = self
                .judge(published, pages)
                .map_err(ControlError::Refused)?;
        }
        for index in 0..count {
            let (Some(published), Some(pages), Some((format, rate))) = (
                ready.streams.get(index).copied(),
                buffers.get(index),
                chosen.get(index).copied(),
            ) else {
                continue;
            };
            let mut copy = [0_u64; MAX_BUFFER_PAGES];
            for (to, from) in copy.iter_mut().zip(pages.iter()) {
                *to = *from;
            }
            if let Some(slot) = self.streams.get_mut(index) {
                *slot = Some(Playback {
                    published,
                    pages: copy,
                    page_count: pages.len(),
                    started: false,
                    halting: false,
                });
            }
            self.prepare(&published, format, rate)
                .map_err(ControlError::Device)?;
        }
        self.phase = Phase::Running;
        Ok(())
    }

    /// Follow a message from the core other than READY. The glue decodes it,
    /// sends what [`Driver::pop_message`] then gives, and for
    /// [`Control::Refused`] and [`Control::Stop`] shuts the driver down, after
    /// sending STOPPED for the latter.
    ///
    /// # Errors
    ///
    /// [`ControlError::Unexpected`] out of turn, [`ControlError::Refused`] for
    /// a SUBMIT the device must not follow, and [`ControlError::Device`] for a
    /// device that broke the protocol.
    pub fn on_control(&mut self, message: &Message) -> Result<Control, ControlError> {
        let open = matches!(self.phase, Phase::Introduced | Phase::Running);
        match *message {
            Message::Refused(reason) if open => {
                self.phase = Phase::Refused;
                Ok(Control::Refused(reason))
            }
            Message::Stop if open => {
                self.phase = Phase::Stopping;
                Ok(Control::Stop)
            }
            Message::Submit(submit) if self.phase == Phase::Running => {
                self.submit(&submit)?;
                Ok(Control::Followed)
            }
            Message::Halt { stream } if self.phase == Phase::Running => {
                self.halt(stream)?;
                Ok(Control::Followed)
            }
            _ => Err(ControlError::Unexpected(message.kind())),
        }
    }

    fn playback_of(&self, stream: u32) -> Option<usize> {
        self.streams
            .iter()
            .position(|playback| playback.is_some_and(|p| p.published.stream == stream))
    }

    /// The runs of device-contiguous pages `bytes` at `offset` of a buffer
    /// cover, as readable buffers.
    fn segments(
        playback: &Playback,
        offset: usize,
        bytes: usize,
    ) -> Result<([Buffer; MAX_SEGMENTS], usize), Refusing> {
        let page = PAGE_SIZE as usize;
        let pages = playback.pages.get(..playback.page_count).unwrap_or(&[]);
        let mut segments = [Buffer::readable(0, 0); MAX_SEGMENTS];
        let mut count = 0;
        let mut at = offset;
        let end = offset + bytes;
        let mut next_address = None;
        while at < end {
            let base = *pages.get(at / page).ok_or(Refusing::Range)?;
            let address = base + (at % page) as u64;
            let len = (page - at % page).min(end - at);
            if next_address == Some(address) && count > 0 {
                if let Some(last) = segments.get_mut(count - 1) {
                    // Runs are at most a buffer, far inside a `u32`.
                    *last = Buffer::readable(last.address, last.len + len as u32);
                }
            } else {
                let slot = segments.get_mut(count).ok_or(Refusing::Scattered)?;
                *slot = Buffer::readable(address, len as u32);
                count += 1;
            }
            next_address = Some(address + len as u64);
            at += len;
        }
        Ok((segments, count))
    }

    fn submit(&mut self, submit: &Submit) -> Result<(), ControlError> {
        let refuse = |why| Err(ControlError::Refused(why));
        let Some(index) = self.playback_of(submit.stream) else {
            return refuse(Refusing::Range);
        };
        let Some(playback) = self.streams.get(index).copied().flatten() else {
            return refuse(Refusing::Range);
        };
        if playback.halting {
            return refuse(Refusing::Busy);
        }
        let (offset, bytes) = (submit.offset as usize, submit.bytes as usize);
        if bytes == 0 || offset.saturating_add(bytes) > playback.published.buffer_bytes as usize {
            return refuse(Refusing::Range);
        }
        let Some(slot) = self.posted.iter().position(Option::is_none) else {
            return refuse(Refusing::Busy);
        };
        let (segments, count) =
            Self::segments(&playback, offset, bytes).map_err(ControlError::Refused)?;
        let slot_at = SLOTS_AT + slot * SLOT_BYTES;
        for (index, byte) in submit.stream.to_le_bytes().into_iter().enumerate() {
            self.scratch.write_u8(slot_at + index, byte);
        }
        for index in 0..STATUS_BYTES {
            self.scratch.write_u8(slot_at + SLOT_STATUS_AT + index, 0);
        }
        let pages = self.scratch.device_pages();
        let (Some(header), Some(status)) = (
            contiguous(pages, slot_at, XFER_BYTES),
            contiguous(pages, slot_at + SLOT_STATUS_AT, STATUS_BYTES),
        ) else {
            let error = self.break_down(DeviceError::Queue(QueueError::DescriptorOutOfRange));
            return Err(ControlError::Device(error));
        };
        let mut chain = [Buffer::readable(0, 0); MAX_SEGMENTS + 2];
        let mut length = 0;
        for buffer in core::iter::once(Buffer::readable(header, XFER_BYTES as u32))
            .chain(segments.into_iter().take(count))
            .chain(core::iter::once(Buffer::writable(
                status,
                STATUS_BYTES as u32,
            )))
        {
            if let Some(place) = chain.get_mut(length) {
                *place = buffer;
                length += 1;
            }
        }
        let head = match self.tx.add_chain(chain.get(..length).unwrap_or(&[])) {
            Ok(head) => head,
            Err(QueueError::OutOfDescriptors) => return refuse(Refusing::Busy),
            Err(error) => {
                return Err(ControlError::Device(
                    self.break_down(DeviceError::Queue(error)),
                ));
            }
        };
        if let Some(place) = self.posted.get_mut(slot) {
            *place = Some(Posted {
                playback: index,
                sequence: submit.sequence,
                head,
                order: self.next_order,
                done: None,
                latency: 0,
            });
        }
        self.next_order += 1;
        self.stats.submitted += 1;
        if self.tx.device_wants_notification() {
            self.transport.notify(snd::TX_QUEUE, self.tx_notify_off);
        }
        if !playback.started {
            self.stream_command(PcmCommand::Start, submit.stream)
                .map_err(ControlError::Device)?;
            if let Some(Some(playback)) = self.streams.get_mut(index) {
                playback.started = true;
            }
        }
        Ok(())
    }

    fn halt(&mut self, stream: u32) -> Result<(), ControlError> {
        let Some(index) = self.playback_of(stream) else {
            return Err(ControlError::Refused(Refusing::Range));
        };
        let Some(playback) = self.streams.get(index).copied().flatten() else {
            return Err(ControlError::Refused(Refusing::Range));
        };
        if playback.halting {
            return Err(ControlError::Unexpected(ferrix_sndctl::message::HALT));
        }
        if let Some(Some(playback)) = self.streams.get_mut(index) {
            playback.halting = true;
        }
        self.stats.halts += 1;
        if playback.started {
            self.stream_command(PcmCommand::Stop, stream)
                .map_err(ControlError::Device)?;
        }
        // RELEASE is answered only once every buffer has come back.
        self.stream_command(PcmCommand::Release, stream)
            .map_err(ControlError::Device)?;
        if let Some(Some(playback)) = self.streams.get_mut(index) {
            playback.started = false;
        }
        let _ = self.collect().map_err(ControlError::Device)?;
        Ok(())
    }

    // -- the device's completions -------------------------------------------

    /// Take every transmit completion, send ELAPSED oldest first, and finish
    /// any halt that has nothing left in flight. Says how many were taken
    /// and how many of them refused.
    fn collect(&mut self) -> Result<(usize, usize), DeviceError> {
        let mut taken = (0, 0);
        loop {
            let used = match self.tx.take_used() {
                Ok(Some(used)) => used,
                Ok(None) => break,
                Err(error) => return Err(self.break_down(DeviceError::Queue(error))),
            };
            let Some(slot) = self
                .posted
                .iter()
                .position(|posted| posted.is_some_and(|p| p.head == used.head && p.done.is_none()))
            else {
                return Err(self.break_down(DeviceError::UnknownChain(used.head)));
            };
            let at = SLOTS_AT + slot * SLOT_BYTES + SLOT_STATUS_AT;
            let mut status = [0_u8; STATUS_BYTES];
            for (index, byte) in status.iter_mut().enumerate() {
                *byte = self.scratch.read_u8(at + index);
            }
            let (played, latency) = match PcmStatus::read(&status, used.written) {
                Ok(status) => (true, status.latency_bytes),
                Err(SndError::Status(_)) => (false, 0),
                Err(error) => return Err(self.break_down(DeviceError::Protocol(error))),
            };
            if let Some(Some(posted)) = self.posted.get_mut(slot) {
                posted.done = Some(played);
                posted.latency = latency;
            }
            taken.0 += 1;
            if !played {
                taken.1 += 1;
                self.stats.refused += 1;
            }
            self.stats.completed += 1;
        }
        self.send_done();
        self.finish_halts()?;
        Ok(taken)
    }

    /// For each stream, ELAPSED for every completed submission older than
    /// any still in flight.
    fn send_done(&mut self) {
        for index in 0..MAX_PUBLISHED {
            loop {
                let oldest = self
                    .posted
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, posted)| posted.map(|posted| (slot, posted)))
                    .filter(|(_, posted)| posted.playback == index)
                    .min_by_key(|(_, posted)| posted.order);
                let Some((slot, posted)) = oldest else {
                    break;
                };
                let Some(played) = posted.done else {
                    break;
                };
                let Some(stream) = self
                    .streams
                    .get(index)
                    .copied()
                    .flatten()
                    .map(|playback| playback.published.stream)
                else {
                    break;
                };
                let elapsed = Message::Elapsed(Elapsed {
                    stream,
                    sequence: posted.sequence,
                    played,
                    latency_bytes: posted.latency,
                });
                if !self.outbox.push(elapsed) {
                    break;
                }
                if let Some(place) = self.posted.get_mut(slot) {
                    *place = None;
                }
            }
        }
    }

    /// HALTED, and a fresh prepare, for each halting stream with nothing
    /// left in flight.
    fn finish_halts(&mut self) -> Result<(), DeviceError> {
        for index in 0..MAX_PUBLISHED {
            let Some(playback) = self.streams.get(index).copied().flatten() else {
                continue;
            };
            let busy = self
                .posted
                .iter()
                .flatten()
                .any(|posted| posted.playback == index);
            if !playback.halting || busy {
                continue;
            }
            let halted = Message::Halted {
                stream: playback.published.stream,
                unplayed: 0,
            };
            if !self.outbox.push(halted) {
                continue;
            }
            let published = playback.published;
            let format = format::to_virtio(u32::from(published.format)).unwrap_or(snd::FORMAT_S16);
            let rate = snd::rate_index(published.rate).unwrap_or(snd::RATE_48000);
            self.prepare(&published, format, rate)?;
            if let Some(Some(playback)) = self.streams.get_mut(index) {
                playback.halting = false;
            }
        }
        Ok(())
    }

    /// Take what the device completed. Messages are taken afterwards with
    /// [`Driver::pop_message`]. The interrupt is acknowledged first, so a
    /// completion landing during the drain raises another rather than being
    /// lost. Before READY, and after REFUSED or STOP, nothing is taken.
    ///
    /// # Errors
    ///
    /// The [`DeviceError`] a broken device broke the protocol with.
    pub fn on_interrupt(&mut self) -> Result<Drained, DeviceError> {
        let isr = self.transport.acknowledge_interrupt();
        let mut drained = Drained {
            isr,
            ..Drained::default()
        };
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if self.phase != Phase::Running {
            return Ok(drained);
        }
        if self.transport.read8(DEVICE_STATUS) & STATUS_DEVICE_NEEDS_RESET != 0 {
            return Err(self.break_down(DeviceError::NeedsReset));
        }
        let (taken, refused) = self.collect()?;
        drained.taken = taken;
        drained.refused = refused;
        Ok(drained)
    }

    /// The next message for the core, oldest first.
    pub fn pop_message(&mut self) -> Option<Message> {
        if self.fault.is_some() {
            return None;
        }
        self.outbox.pop()
    }

    /// Reset the device and hand everything back, the memory only if the
    /// reset finished.
    pub fn shutdown(self) -> Teardown<T, R, S> {
        let Self {
            transport,
            control,
            tx,
            scratch,
            options,
            ..
        } = self;
        teardown(
            transport,
            Rings::Queue(ManuallyDrop::into_inner(control)),
            Rings::Queue(ManuallyDrop::into_inner(tx)),
            ManuallyDrop::into_inner(scratch),
            options.reset_polls,
        )
    }
}
