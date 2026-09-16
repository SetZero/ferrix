//! A virtio-input driver, as logic over a transport and DMA memory it is
//! handed.
//!
//! The input iteration (`docs/INPUT.md`) runs this driver in a user process
//! under devmgr, one process per device (§6), as virtio-gpu's runs. The
//! process holds an `IoMapping` of the device's register blocks, an
//! `Interrupt`, pinned DMA memory and the control channel to the kernel's
//! input core; none of those can be had in a unit test. So this crate is
//! written against [`Transport`], [`DevicePages`] and [`EventArea`], which the
//! process implements over its handles, and holds everything else:
//!
//! * [`Driver::init`] negotiates features, reads the device's description
//!   through `ferrix_virtio::input`'s configuration queries and builds the
//!   event queue, and [`Driver::hello`] turns that description into the HELLO
//!   the core judges;
//! * [`Driver::on_control`] follows the core's side: READY sets `DRIVER_OK`
//!   and posts the event queue's buffers, REFUSED and STOP end the driver;
//! * [`Driver::on_interrupt`] takes the events the device wrote, keeps the
//!   event queue full, drops what the core would not publish, and
//!   [`Driver::pop_events`] hands out the EVENTS messages [`batch`] makes of
//!   the rest;
//! * [`Driver::shutdown`] resets the device and hands the memory back.
//!
//! # The order bring-up takes
//!
//! `docs/INPUT.md` §3.2 has the driver send HELLO and set `DRIVER_OK` only on
//! READY. Virtio 1.2 §3.1.1 has queues set up before `DRIVER_OK`, and QEMU
//! discards every event until it is set (`virtio_input_send` returns while
//! the device is not `active`), so [`Driver::init`] stops after enabling the
//! event queue, with `FEATURES_OK` set and no buffer posted. The configuration
//! is read after `FEATURES_OK`, where virtio 1.2 §3.1.1 allows it. READY sets
//! `DRIVER_OK`, then posts a buffer in every descriptor and rings the
//! doorbell. A device the core refuses never sees `DRIVER_OK`.
//!
//! The status queue is left disabled: nothing writes to the device in this
//! iteration (§6, decision 3), and QEMU's device serves an empty one.
//!
//! # The event queue stays full
//!
//! QEMU drops a whole report without telling anyone when the queue lacks a
//! buffer for any of its events (`virtio_input_send` in
//! `hw/input/virtio-input.c`), so every descriptor holds a posted 8-byte
//! buffer, and [`Driver::on_interrupt`] posts again every buffer it took back
//! before it returns, and so before a single event is forwarded. The buffers
//! are slots of one [`EventArea`], [`EVENT_LEN`] bytes each, one per
//! descriptor.
//!
//! # What is forwarded
//!
//! An event the device wrote goes on only if the core publishes it:
//! `SYN_REPORT` of `EV_SYN`, `REP_DELAY` or `REP_PERIOD` of a device declaring
//! `EV_REP`, and otherwise a type and code in `inputctl`'s
//! [`Capabilities`] of this device's HELLO. So `SYN_MT_REPORT`, force
//! feedback, sound, multi-touch axes and `KEY_RESERVED` stop here
//! (`docs/INPUT.md` §6, decision 4), and so does anything the device did not
//! declare. HELLO itself carries the device's declaration as the device gave
//! it (§3.2), less the bits past each kind's `*_MAX`, which no message has
//! room for; the core works out what it leaves out, and says so on the boot
//! line.
//!
//! # DMA memory is freed only after a reset
//!
//! As in `ferrix-virtio-gpu`: the rings and the event area are held in
//! [`ManuallyDrop`], and [`Driver::shutdown`] resets the device and hands them
//! back as [`Teardown::Released`] only if the reset finished. A device that
//! does not reset may still write into the event buffers, so they come back
//! [`Teardown::Wedged`] and are never dropped.
//!
//! # Trust
//!
//! The configuration is checked by `ferrix_virtio::input`: a declared axis
//! with no range, a range whose minimum lies above its maximum, or an answer
//! that breaks the query protocol fails bring-up. The used ring is checked by
//! [`SplitQueue`]. A completion that wrote anything but one event, a
//! completion of a buffer the driver did not post, and `DEVICE_NEEDS_RESET`
//! mean the device broke the protocol: it is marked failed and nothing more
//! is taken from it, as virtio-gpu treats a broken device. An event of a type
//! or code the device did not declare is the device's word and not a broken
//! protocol; it is dropped and counted.
//!
//! # Where this follows virtio-gpu rather than `docs/INPUT.md`
//!
//! The document is silent on these, and `libs/virtio-gpu` decides them: the
//! trait shapes; how a failed bring-up and a shutdown hand memory back; the
//! queue size taken as the smaller of the driver's and the device's; the
//! MSI-X vector the device must keep; and a broken device marked `FAILED`
//! and left alone until the glue shuts it down.

#![no_std]
#![cfg_attr(not(test), forbid(unsafe_code))]
#![cfg_attr(test, deny(unsafe_code))]

use core::fmt;
use core::mem::ManuallyDrop;

use ferrix_inputctl::message::{Events, Hello, Message, RawEvent, Refusal};
use ferrix_inputctl::session::Capabilities;
use ferrix_linux_abi::input::{EV_REP, EV_SYN, REP_MAX, SYN_REPORT};
use ferrix_virtio::input::{self, ConfigSelect, EVENT_LEN, Event, InputError};
use ferrix_virtio::pci::{
    self, CommonConfig, DEVICE_STATUS, QueueAddresses, STATUS_DEVICE_NEEDS_RESET, STATUS_FAILED,
    TransportError,
};
use ferrix_virtio::{Buffer, Layout, PAGE_SIZE, QueueError, QueueMemory, SplitQueue};

pub mod batch;
mod hello;

pub use hello::read_hello;

use batch::{Batch, PUSH_ROOM};

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

/// ISR status bit: a queue has something for the driver.
pub const ISR_QUEUE: u8 = 1;

/// ISR status bit: the device configuration changed. virtio-input gives it
/// no meaning; it is passed back untouched.
pub const ISR_CONFIG: u8 = 2;

/// The event queue's size, the size QEMU 9.2.4 gives it
/// (`virtio_add_queue(vdev, 64, …)` in `hw/input/virtio-input.c`): 64 events
/// posted at once, which is also the longest report QEMU can deliver.
pub const QUEUE_SIZE: u16 = 64;

/// Bytes of the event area a queue of [`QUEUE_SIZE`] needs.
pub const AREA_BYTES: usize = QUEUE_SIZE as usize * EVENT_LEN;

/// The device's registers, as the process that drives it reaches them. The
/// same shape as `ferrix-virtio-gpu`'s, with the configuration writable,
/// since a virtio-input query is a write.
pub trait Transport: CommonConfig + ConfigSelect {
    /// Ring the doorbell for `queue`, whose `queue_notify_off` is
    /// `notify_off`.
    fn notify(&mut self, queue: u16, notify_off: u16);

    /// The MSI-X table entry the event queue should interrupt through, or
    /// [`pci::NO_VECTOR`].
    fn queue_vector(&self) -> u16;

    /// Acknowledge the interrupt and say why it came: the ISR status byte for
    /// a line interrupt, [`ISR_QUEUE`] for MSI-X.
    fn acknowledge_interrupt(&mut self) -> u8;
}

/// Pinned memory, as the device addresses of its pages, in order.
pub trait DevicePages {
    /// One device address per page, [`PAGE_SIZE`] bytes each, not necessarily
    /// consecutive.
    fn device_pages(&self) -> &[u64];
}

/// The memory the event buffers live in: slot `i` is the [`EVENT_LEN`] bytes
/// at `i × EVENT_LEN`.
pub trait EventArea: DevicePages {
    /// Read the byte at `offset`.
    fn read_u8(&self, offset: usize) -> u8;
    /// Write the byte at `offset`.
    fn write_u8(&mut self, offset: usize, value: u8);
}

/// Everything a driver is built from.
pub struct Parts<T, R, A> {
    /// The device's registers.
    pub transport: T,
    /// The event queue's rings: pages holding a queue of [`QUEUE_SIZE`].
    pub rings: R,
    /// The event buffers: at least [`AREA_BYTES`].
    pub area: A,
}

/// How [`Driver::init`] goes about it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options {
    /// Reads of `device_status` a reset may take.
    pub reset_polls: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            reset_polls: 100_000,
        }
    }
}

/// What the driver agreed with the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Info {
    /// The features negotiated.
    pub features: u64,
    /// The MSI-X vector the event queue interrupts through, or `NO_VECTOR`.
    pub vector: u16,
    /// The event queue's size.
    pub queue_size: u16,
    /// Whether the device declared bits past a kind's `*_MAX`, which HELLO
    /// leaves out.
    pub clipped: bool,
}

/// Why a device could not be brought up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InitError {
    /// The status protocol failed.
    Transport(TransportError),
    /// The device's description is unusable.
    Config(InputError),
    /// The rings' pages cannot hold the queue, or the event area has no room
    /// for a buffer per descriptor.
    NoRoom,
    /// The device would not give the queue the vector asked for.
    VectorRefused {
        /// Asked for.
        asked: u16,
        /// Kept.
        kept: u16,
    },
}

/// How the device broke the protocol. The driver has set `FAILED`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceError {
    /// The device set `DEVICE_NEEDS_RESET`.
    NeedsReset,
    /// The rings say something impossible.
    Queue(QueueError),
    /// A completion that is not one event.
    Protocol(InputError),
    /// A completion for a chain the driver did not post.
    UnknownChain(u16),
}

/// Where the driver is in its conversation with the core.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Brought up, HELLO not yet answered.
    Introduced,
    /// READY came: `DRIVER_OK` is set and events flow.
    Running,
    /// The core refused the driver.
    Refused,
    /// The core asked the driver to stop.
    Stopping,
}

/// What a message from the core means for the glue.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
    /// READY: the device is published as `event<node>` and started.
    Started {
        /// The node's index.
        node: u32,
    },
    /// REFUSED: shut the driver down.
    Refused(Refusal),
    /// STOP: send STOPPED, then shut the driver down.
    Stop,
}

/// Why a message from the core was not followed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlError {
    /// A message the core does not send now: its type.
    Unexpected(u32),
    /// Starting the device found it broken.
    Device(DeviceError),
}

/// What one [`Driver::on_interrupt`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Drained {
    /// The ISR bits the interrupt was acknowledged with.
    pub isr: u8,
    /// Completions taken.
    pub taken: usize,
    /// Of those, events dropped as not published.
    pub dropped: usize,
    /// Buffers posted again.
    pub posted: u16,
    /// Whether completions are left in the used ring because the batch was
    /// full: take messages with [`Driver::pop_events`], then call
    /// [`Driver::on_interrupt`] again.
    pub more: bool,
}

/// Counts over the driver's life.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Stats {
    /// Events the device wrote.
    pub events: u64,
    /// Events dropped because the core does not publish them.
    pub dropped: u64,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Transport(error) => write!(f, "{error}"),
            Self::Config(error) => write!(f, "{error}"),
            Self::NoRoom => f.write_str("the queue or the event area does not fit"),
            Self::VectorRefused { asked, kept } => {
                write!(f, "the device kept vector {kept:#x} for {asked:#x}")
            }
        }
    }
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NeedsReset => f.write_str("the device needs a reset"),
            Self::Queue(error) => write!(f, "the rings are corrupt: {error:?}"),
            Self::Protocol(error) => write!(f, "{error}"),
            Self::UnknownChain(head) => write!(f, "chain {head} was not posted"),
        }
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Unexpected(kind) => write!(f, "the core sent message type {kind} out of turn"),
            Self::Device(error) => write!(f, "{error}"),
        }
    }
}

/// A driver's parts, handed back.
pub struct Released<T, R, A> {
    /// The device's registers.
    pub transport: T,
    /// The rings' memory, inside the queue if one was built.
    pub rings: Rings<R>,
    /// The event area.
    pub area: A,
}

/// The rings' memory as it comes back.
pub enum Rings<R> {
    /// Bring-up stopped before the queue was built.
    Unused(R),
    /// The queue, with the memory inside it.
    Queue(SplitQueue<R>),
}

/// How a driver ended.
pub enum Teardown<T, R, A> {
    /// The device reset; the memory may be unpinned.
    Released(Released<T, R, A>),
    /// The device did not reset and may still write to the memory, so it is
    /// never dropped.
    Wedged(ManuallyDrop<Released<T, R, A>>),
}

/// A failed [`Driver::init`].
pub struct InitFailure<T, R, A> {
    /// Why.
    pub error: InitError,
    /// The parts, released only if the reset finished.
    pub teardown: Teardown<T, R, A>,
}

impl<T, R, A> fmt::Debug for Released<T, R, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Released")
            .field("rings", &self.rings)
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

impl<T, R, A> fmt::Debug for Teardown<T, R, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Released(_) => "Teardown::Released",
            Self::Wedged(_) => "Teardown::Wedged",
        })
    }
}

impl<T, R, A> fmt::Debug for InitFailure<T, R, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InitFailure")
            .field("error", &self.error)
            .field("teardown", &self.teardown)
            .finish()
    }
}

impl<T, R, A> fmt::Debug for Parts<T, R, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Parts").finish_non_exhaustive()
    }
}

/// A virtio-input device, brought up and driven.
pub struct Driver<T, R, A> {
    transport: T,
    queue: ManuallyDrop<SplitQueue<R>>,
    area: ManuallyDrop<A>,
    info: Info,
    hello: Hello,
    caps: Capabilities,
    notify_off: u16,
    reset_polls: u32,
    fault: Option<DeviceError>,
    phase: Phase,
    /// For each descriptor that heads a posted chain, the slot its buffer is.
    posted: [Option<u16>; QUEUE_SIZE as usize],
    /// Slots whose buffer is posted, one bit each.
    busy: u64,
    batch: Batch,
    stats: Stats,
}

impl<T, R, A> fmt::Debug for Driver<T, R, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Driver")
            .field("info", &self.info)
            .field("phase", &self.phase)
            .field("fault", &self.fault)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
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

/// Whether every one of `slots` buffers lies device-contiguous in `pages`.
fn slots_fit(pages: &[u64], slots: u16) -> bool {
    (0..usize::from(slots)).all(|slot| contiguous(pages, slot * EVENT_LEN, EVENT_LEN).is_some())
}

fn teardown<T: CommonConfig, R, A>(
    mut transport: T,
    rings: Rings<R>,
    area: A,
    polls: u32,
) -> Teardown<T, R, A> {
    let reset = pci::reset(&mut transport, polls);
    let released = Released {
        transport,
        rings,
        area,
    };
    match reset {
        Ok(()) => Teardown::Released(released),
        Err(_) => Teardown::Wedged(ManuallyDrop::new(released)),
    }
}

fn set_failed<T: CommonConfig + ?Sized>(transport: &mut T) {
    let status = transport.read8(DEVICE_STATUS);
    transport.write8(DEVICE_STATUS, status | STATUS_FAILED);
}

/// Whether the core publishes `event` of a device whose published
/// capabilities are `caps`: the rule `inputctl::session` checks, narrowed to
/// what it does not ignore.
#[must_use]
pub fn is_published(caps: &Capabilities, event: &RawEvent) -> bool {
    match event.kind {
        EV_SYN => event.code == SYN_REPORT,
        EV_REP => caps.bits.has_type(EV_REP) && event.code <= REP_MAX,
        kind => caps.bits.has_code(kind, event.code),
    }
}

impl<T, R, A> Driver<T, R, A>
where
    T: Transport,
    R: QueueMemory + DevicePages,
    A: EventArea,
{
    /// Bring the device up as far as HELLO: reset, features, `FEATURES_OK`,
    /// the description read, the event queue built and enabled. `DRIVER_OK`
    /// waits for READY.
    pub fn init(parts: Parts<T, R, A>, options: Options) -> Result<Self, InitFailure<T, R, A>> {
        let Parts {
            mut transport,
            rings,
            area,
        } = parts;
        let fail = |mut transport: T, rings, area, error| {
            set_failed(&mut transport);
            Err(InitFailure {
                error,
                teardown: teardown(transport, rings, area, options.reset_polls),
            })
        };

        let described = pci::negotiate(
            &mut transport,
            input::DRIVER_FEATURES,
            input::REQUIRED_FEATURES,
            options.reset_polls,
        )
        .map_err(InitError::Transport)
        .and_then(|features| {
            read_hello(&mut transport)
                .map(|(hello, clipped)| (features, hello, clipped))
                .map_err(InitError::Config)
        });
        let (features, hello, clipped) = match described {
            Ok(described) => described,
            Err(error) => return fail(transport, Rings::Unused(rings), area, error),
        };

        let plan = pci::queue_max_size(&mut transport, input::EVENT_QUEUE)
            .map_err(InitError::Transport)
            .and_then(|max| {
                let size = power_of_two_below(QUEUE_SIZE.min(max));
                let layout = Layout::for_size(size).map_err(|_| InitError::NoRoom)?;
                let addresses =
                    ring_addresses(&layout, rings.device_pages()).ok_or(InitError::NoRoom)?;
                if slots_fit(area.device_pages(), size) {
                    Ok((layout, addresses))
                } else {
                    Err(InitError::NoRoom)
                }
            });
        let (layout, addresses) = match plan {
            Ok(plan) => plan,
            Err(error) => return fail(transport, Rings::Unused(rings), area, error),
        };

        let queue = SplitQueue::new(layout, rings);
        let asked = transport.queue_vector();
        let active = match pci::activate_queue(
            &mut transport,
            input::EVENT_QUEUE,
            layout.queue_size,
            addresses,
            asked,
        ) {
            Ok(active) if active.vector != asked => Err(InitError::VectorRefused {
                asked,
                kept: active.vector,
            }),
            Ok(active) => Ok(active),
            Err(error) => Err(InitError::Transport(error)),
        };
        let active = match active {
            Ok(active) => active,
            Err(error) => return fail(transport, Rings::Queue(queue), area, error),
        };

        Ok(Self {
            transport,
            queue: ManuallyDrop::new(queue),
            area: ManuallyDrop::new(area),
            info: Info {
                features,
                vector: active.vector,
                queue_size: layout.queue_size,
                clipped,
            },
            caps: Capabilities::from_hello(&hello),
            hello,
            notify_off: active.notify_off,
            reset_polls: options.reset_polls,
            fault: None,
            phase: Phase::Introduced,
            posted: [None; QUEUE_SIZE as usize],
            busy: 0,
            batch: Batch::new(),
            stats: Stats::default(),
        })
    }

    /// What was agreed.
    #[must_use]
    pub const fn info(&self) -> &Info {
        &self.info
    }

    /// The HELLO to send, for the device at `location`.
    #[must_use]
    pub const fn hello(&self, location: u32) -> Hello {
        let mut hello = self.hello;
        hello.location = location;
        hello
    }

    /// What the core will publish of this device.
    #[must_use]
    pub const fn capabilities(&self) -> &Capabilities {
        &self.caps
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

    /// Reports the batch ended early because they grew too long for the core.
    #[must_use]
    pub const fn split_reports(&self) -> u64 {
        self.batch.split_reports()
    }

    /// Buffers posted in the event queue now.
    #[must_use]
    pub const fn posted(&self) -> u32 {
        self.busy.count_ones()
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

    /// Follow a message from the core. The glue decodes it, sends nothing for
    /// [`Control::Started`], and for [`Control::Refused`] and
    /// [`Control::Stop`] shuts the driver down, after sending STOPPED for the
    /// latter. What the batch holds is discarded on either: the core throws
    /// away a report it has not finished when it stops or refuses a driver.
    pub fn on_control(&mut self, message: &Message) -> Result<Control, ControlError> {
        let open = matches!(self.phase, Phase::Introduced | Phase::Running);
        match *message {
            Message::Ready(ready) if self.phase == Phase::Introduced => {
                self.start().map_err(ControlError::Device)?;
                Ok(Control::Started { node: ready.node })
            }
            Message::Refused(reason) if open => {
                self.phase = Phase::Refused;
                self.batch.clear();
                Ok(Control::Refused(reason))
            }
            Message::Stop if open => {
                self.phase = Phase::Stopping;
                self.batch.clear();
                Ok(Control::Stop)
            }
            _ => Err(ControlError::Unexpected(message.kind())),
        }
    }

    /// `DRIVER_OK`, every buffer posted, the doorbell rung.
    fn start(&mut self) -> Result<(), DeviceError> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if pci::driver_ok(&mut self.transport).is_err() {
            return Err(self.break_down(DeviceError::NeedsReset));
        }
        self.phase = Phase::Running;
        let _ = self.refill()?;
        Ok(())
    }

    /// Post a buffer in every free descriptor and ring the doorbell if any
    /// was posted.
    fn refill(&mut self) -> Result<u16, DeviceError> {
        let mut posted = 0u16;
        let slots = u32::from(self.info.queue_size);
        while self.queue.free_descriptors() > 0 {
            let slot = (!self.busy).trailing_zeros();
            if slot >= slots {
                break;
            }
            let at = slot as usize * EVENT_LEN;
            let Some(address) = contiguous(self.area.device_pages(), at, EVENT_LEN) else {
                break;
            };
            // Zeroed, so a completion the device did not write reads as an
            // empty `SYN_REPORT`, not as the event this buffer held before.
            for offset in at..at + EVENT_LEN {
                self.area.write_u8(offset, 0);
            }
            let head = match self
                .queue
                .add_chain(&[Buffer::writable(address, EVENT_LEN as u32)])
            {
                Ok(head) => head,
                Err(QueueError::OutOfDescriptors) => break,
                Err(error) => return Err(self.break_down(DeviceError::Queue(error))),
            };
            let Some(entry) = self.posted.get_mut(usize::from(head)) else {
                return Err(self.break_down(DeviceError::UnknownChain(head)));
            };
            *entry = Some(slot as u16);
            self.busy |= 1 << slot;
            posted += 1;
        }
        if posted > 0 && self.queue.device_wants_notification() {
            self.transport.notify(input::EVENT_QUEUE, self.notify_off);
        }
        Ok(posted)
    }

    /// Take the next completion's event.
    fn take(&mut self) -> Result<Option<Event>, DeviceError> {
        let used = match self.queue.take_used() {
            Ok(Some(used)) => used,
            Ok(None) => return Ok(None),
            Err(error) => return Err(self.break_down(DeviceError::Queue(error))),
        };
        let Some(slot) = self
            .posted
            .get_mut(usize::from(used.head))
            .and_then(Option::take)
        else {
            return Err(self.break_down(DeviceError::UnknownChain(used.head)));
        };
        self.busy &= !(1 << slot);
        let at = usize::from(slot) * EVENT_LEN;
        let mut bytes = [0u8; EVENT_LEN];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = self.area.read_u8(at + index);
        }
        Event::from_completion(&bytes, used.written)
            .map(Some)
            .map_err(|error| self.break_down(DeviceError::Protocol(error)))
    }

    /// Take the events the device wrote, while the batch has room, and post
    /// their buffers again. Messages are taken afterwards with
    /// [`Driver::pop_events`].
    ///
    /// The interrupt is acknowledged first, so a completion landing during
    /// the drain raises another rather than being lost. Before READY, and
    /// after REFUSED or STOP, nothing is taken.
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
        while self.batch.room() >= PUSH_ROOM {
            let Some(event) = self.take()? else {
                break;
            };
            drained.taken += 1;
            self.stats.events += 1;
            let raw = RawEvent::new(event.kind, event.code, event.value);
            if is_published(&self.caps, &raw) {
                let _ = self.batch.push(raw);
            } else {
                drained.dropped += 1;
                self.stats.dropped += 1;
            }
        }
        drained.more = self.queue.has_used();
        drained.posted = self.refill()?;
        Ok(drained)
    }

    /// The next EVENTS to send, if one is ready.
    pub fn pop_events(&mut self) -> Option<Events> {
        if self.phase != Phase::Running || self.fault.is_some() {
            return None;
        }
        self.batch.pop()
    }

    /// Events taken from the device and not yet handed out in a message.
    #[must_use]
    pub const fn pending(&self) -> usize {
        self.batch.len()
    }

    /// Reset the device and hand everything back, the memory only if the
    /// reset finished.
    pub fn shutdown(self) -> Teardown<T, R, A> {
        let Self {
            transport,
            queue,
            area,
            reset_polls,
            ..
        } = self;
        teardown(
            transport,
            Rings::Queue(ManuallyDrop::into_inner(queue)),
            ManuallyDrop::into_inner(area),
            reset_polls,
        )
    }
}
