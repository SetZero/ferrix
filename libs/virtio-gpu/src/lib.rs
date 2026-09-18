//! A virtio-gpu 2D driver, as logic over a transport and DMA memory it is
//! handed.
//!
//! Iteration 1 of the display (`docs/DISPLAY.md`) runs this driver in a user
//! process under devmgr, as virtio-blk's runs. The process holds an
//! `IoMapping` of the device's register blocks, an `Interrupt`, pinned DMA
//! memory, the card VMO it pins buffers from, and the control channel to the
//! kernel's display core; none of those can be had in a unit test. So this
//! crate is written against [`Transport`], [`DevicePages`] and [`CommandArea`],
//! which the process implements over its handles, and holds everything else:
//!
//! * [`Driver`] brings the device up and runs one control command at a time
//!   through the control queue, checking every response;
//! * [`pipeline::Pipeline`] turns the display core's requests — ATTACH,
//!   SCANOUT, FLUSH, DETACH — into the device commands each takes, and the
//!   commands' outcomes into the replies the core waits for.
//!
//! # One command at a time
//!
//! A frame is two commands and the core waits for each FLIPPED, so there is
//! nothing to gain from running commands concurrently and a lot of
//! bookkeeping to lose. The command area holds the one request in flight at
//! its start and the response at its end; [`Driver::submit`] refuses a second
//! command until [`Driver::on_interrupt`] has taken the first's response.
//!
//! # DMA memory is freed only after a reset
//!
//! As in `ferrix-virtio-blk`: the rings and the command area are held in
//! [`ManuallyDrop`], [`Driver::shutdown`] resets the device and hands them
//! back as [`Teardown::Released`] only if the reset finished.
//!
//! # Trust
//!
//! The used ring is checked by [`SplitQueue`], a response by
//! [`gpu::Response::parse_for`], the configuration by [`gpu::Config::read`].
//! A device that answers a command with an error response has refused that
//! command and is still trusted; a device that breaks the protocol is marked
//! failed and sent nothing more.

#![no_std]
#![cfg_attr(not(test), forbid(unsafe_code))]
#![cfg_attr(test, deny(unsafe_code))]

use core::fmt;
use core::mem::ManuallyDrop;

use ferrix_virtio::gpu::{
    self, Command, Config, DeviceConfig, DeviceError as Refusal, GpuError, PAGE_SIZE, Response,
};
use ferrix_virtio::pci::{
    self, CommonConfig, DEVICE_STATUS, QueueAddresses, STATUS_DEVICE_NEEDS_RESET, STATUS_FAILED,
    TransportError,
};
use ferrix_virtio::{Buffer, Layout, QueueError, QueueMemory, SplitQueue};

pub mod pipeline;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

/// ISR status bit: a queue has something for the driver.
pub const ISR_QUEUE: u8 = 1;

/// ISR status bit: the device configuration changed, which for a GPU means a
/// display was attached or detached.
pub const ISR_CONFIG: u8 = 2;

/// Bytes at the end of the command area kept for the response.
///
/// One page. `GET_DISPLAY_INFO`'s 408 bytes were the longest response until
/// 3D, and 512 was that rounded up; a capability set is the longest now.
/// virglrenderer's `virgl_caps_v2` is about 1.4 KiB and a device names its
/// own size, so the driver either has room for what the device offers or
/// cannot fetch it -- and a page is comfortably more than any renderer's
/// today while staying one page of pinned memory per card.
///
/// [`CAPSET_ROOM`] is what that leaves for a capability set itself.
pub const RESPONSE_BYTES: usize = 4096;

/// Bytes of a capability set the response buffer has room for: everything
/// but the header a `GET_CAPSET` response starts with.
///
/// A driver asks for `min(what the device says, this)` and says so when the
/// device's is larger, rather than asking for a set that will not fit.
pub const CAPSET_ROOM: u32 = (RESPONSE_BYTES - gpu::HEADER_LEN) as u32;

/// The control queue's size. A command is at most a few chains' worth of
/// request pages and one response buffer, and only one is in flight.
pub const QUEUE_SIZE: u16 = 64;

/// The device's registers, as the process that drives it reaches them. The
/// same shape as `ferrix-virtio-blk`'s.
pub trait Transport: CommonConfig + DeviceConfig {
    /// Ring the doorbell for `queue`, whose `queue_notify_off` is
    /// `notify_off`.
    fn notify(&mut self, queue: u16, notify_off: u16);

    /// The MSI-X table entry the control queue should interrupt through, or
    /// [`pci::NO_VECTOR`].
    fn queue_vector(&self) -> u16;

    /// Acknowledge the interrupt and say why it came: the ISR status byte for
    /// a line interrupt, [`ISR_QUEUE`] for MSI-X.
    fn acknowledge_interrupt(&mut self) -> u8;

    /// Write the little-endian `u32` at `offset` of the device-specific
    /// configuration: virtio-gpu's one writable field is `events_clear`.
    fn config_write32(&mut self, offset: u32, value: u32);
}

/// Pinned memory, as the device addresses of its pages, in order.
pub trait DevicePages {
    /// One device address per page, [`PAGE_SIZE`] bytes each, not necessarily
    /// consecutive.
    fn device_pages(&self) -> &[u64];
}

/// The memory a command's request and response live in.
pub trait CommandArea: DevicePages {
    /// Read the byte at `offset`.
    fn read_u8(&self, offset: usize) -> u8;
    /// Write the byte at `offset`.
    fn write_u8(&mut self, offset: usize, value: u8);
}

/// Everything a driver is built from.
pub struct Parts<T, R, A> {
    /// The device's registers.
    pub transport: T,
    /// The control queue's rings: pages holding a queue of [`QUEUE_SIZE`].
    pub rings: R,
    /// Requests and responses: at least one page.
    pub area: A,
}

/// How [`Driver::init`] goes about it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options {
    /// Reads of `device_status` a reset may take.
    pub reset_polls: u32,
    /// Whether to accept the features 3D needs, which is what makes a
    /// `virtio-gpu-gl` device bring its renderer up: `gpu::DRIVER_FEATURES_3D`
    /// rather than `gpu::DRIVER_FEATURES`.
    ///
    /// Off by default, because accepting a feature is not free and a
    /// driver that will never send a 3D command has no business asking a
    /// host to start a renderer. A driver that wants to *know* what the
    /// card is turns it on: a 2D device cannot offer the feature, so asking
    /// costs that device nothing, and on a 3D one the answer is the point.
    pub want_3d: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            reset_polls: 100_000,
            want_3d: false,
        }
    }
}

/// What the driver agreed with the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Info {
    /// The features negotiated.
    pub features: u64,
    /// The configuration as read at bring-up.
    pub config: Config,
    /// The MSI-X vector the control queue interrupts through, or `NO_VECTOR`.
    pub vector: u16,
}

/// Why a device could not be brought up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InitError {
    /// The status protocol failed.
    Transport(TransportError),
    /// The configuration is unusable.
    Config(GpuError),
    /// The rings' pages cannot hold the queue, or the command area has no
    /// room for a request beside the response.
    NoRoom,
    /// The device would not give the queue the vector asked for.
    VectorRefused {
        /// Asked for.
        asked: u16,
        /// Kept.
        kept: u16,
    },
}

/// Why a command was not submitted. The command is not in flight.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubmitError {
    /// The device has failed; shut the driver down.
    Broken,
    /// A command is already in flight.
    Busy,
    /// The request does not fit the command area or the queue.
    TooLarge,
    /// The command itself is malformed.
    Command(GpuError),
    /// The device broke the queue while the command was published.
    Device(DeviceError),
}

/// How the device broke the protocol. The driver has set `FAILED`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceError {
    /// The device set `DEVICE_NEEDS_RESET`.
    NeedsReset,
    /// The rings say something impossible.
    Queue(QueueError),
    /// A response is impossible.
    Protocol(GpuError),
    /// A completion for a chain the driver did not publish.
    UnknownChain(u16),
}

/// A command's outcome.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Done {
    /// The command's type code.
    pub command: u32,
    /// The response, or the device's refusal of the command.
    pub result: Result<Response, Refusal>,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Transport(error) => write!(f, "{error}"),
            Self::Config(error) => write!(f, "{error}"),
            Self::NoRoom => f.write_str("the queue or the command area does not fit"),
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
            Self::UnknownChain(head) => write!(f, "chain {head} is not in flight"),
        }
    }
}

/// A driver's parts, handed back.
pub struct Released<T, R, A> {
    /// The device's registers.
    pub transport: T,
    /// The rings' memory, inside the queue if one was built.
    pub rings: Rings<R>,
    /// The command area.
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

/// The command in flight.
#[derive(Clone, Copy, Debug)]
struct InFlight {
    head: u16,
    command: u32,
}

/// A virtio-gpu device, brought up and driven.
pub struct Driver<T, R, A> {
    transport: T,
    queue: ManuallyDrop<SplitQueue<R>>,
    area: ManuallyDrop<A>,
    info: Info,
    notify_off: u16,
    reset_polls: u32,
    fault: Option<DeviceError>,
    in_flight: Option<InFlight>,
}

impl<T, R, A> fmt::Debug for Driver<T, R, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Driver")
            .field("info", &self.info)
            .field("fault", &self.fault)
            .field("in_flight", &self.in_flight)
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

impl<T, R, A> Driver<T, R, A>
where
    T: Transport,
    R: QueueMemory + DevicePages,
    A: CommandArea,
{
    /// Bring the device up: reset, features, `FEATURES_OK`, the configuration
    /// read, the control queue built and enabled, `DRIVER_OK`. The cursor
    /// queue is left disabled; iteration 1 draws no hardware cursor.
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

        let wanted = if options.want_3d {
            gpu::DRIVER_FEATURES_3D
        } else {
            gpu::DRIVER_FEATURES
        };
        let features = match pci::negotiate(
            &mut transport,
            wanted,
            gpu::REQUIRED_FEATURES,
            options.reset_polls,
        ) {
            Ok(features) => features,
            Err(error) => {
                return fail(
                    transport,
                    Rings::Unused(rings),
                    area,
                    InitError::Transport(error),
                );
            }
        };
        let config = match Config::read(&transport) {
            Ok(config) => config,
            Err(error) => {
                return fail(
                    transport,
                    Rings::Unused(rings),
                    area,
                    InitError::Config(error),
                );
            }
        };
        let room = area.device_pages().len() * PAGE_SIZE as usize > RESPONSE_BYTES;
        let plan = pci::queue_max_size(&mut transport, gpu::CONTROL_QUEUE)
            .map_err(InitError::Transport)
            .and_then(|max| {
                let size = QUEUE_SIZE.min(max);
                let layout = Layout::for_size(size).map_err(|_| InitError::NoRoom)?;
                let addresses =
                    ring_addresses(&layout, rings.device_pages()).ok_or(InitError::NoRoom)?;
                if room {
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
        let started = match pci::activate_queue(
            &mut transport,
            gpu::CONTROL_QUEUE,
            layout.queue_size,
            addresses,
            asked,
        ) {
            Ok(active) if active.vector != asked => Err(InitError::VectorRefused {
                asked,
                kept: active.vector,
            }),
            Ok(active) => pci::driver_ok(&mut transport)
                .map(|()| active)
                .map_err(InitError::Transport),
            Err(error) => Err(InitError::Transport(error)),
        };
        let active = match started {
            Ok(active) => active,
            Err(error) => return fail(transport, Rings::Queue(queue), area, error),
        };

        Ok(Self {
            transport,
            queue: ManuallyDrop::new(queue),
            area: ManuallyDrop::new(area),
            info: Info {
                features,
                config,
                vector: active.vector,
            },
            notify_off: active.notify_off,
            reset_polls: options.reset_polls,
            fault: None,
            in_flight: None,
        })
    }

    /// What was agreed.
    #[must_use]
    pub const fn info(&self) -> &Info {
        &self.info
    }

    /// The error the device broke the protocol with, if it has.
    #[must_use]
    pub const fn fault(&self) -> Option<DeviceError> {
        self.fault
    }

    /// Whether a command is in flight.
    #[must_use]
    pub const fn is_busy(&self) -> bool {
        self.in_flight.is_some()
    }

    /// The device's registers.
    #[must_use]
    pub const fn transport(&self) -> &T {
        &self.transport
    }

    fn break_down(&mut self, error: DeviceError) {
        if self.fault.is_none() {
            self.fault = Some(error);
            set_failed(&mut self.transport);
        }
    }

    /// Bytes of the command area a request may use.
    fn request_room(&self) -> usize {
        (self.area.device_pages().len() * PAGE_SIZE as usize).saturating_sub(RESPONSE_BYTES)
    }

    /// Write `command` into the command area, publish it with a response
    /// buffer, and notify the device.
    pub fn submit(&mut self, command: &Command<'_>) -> Result<(), SubmitError> {
        if self.fault.is_some() {
            return Err(SubmitError::Broken);
        }
        if self.in_flight.is_some() {
            return Err(SubmitError::Busy);
        }
        let len = command.len();
        let room = self.request_room();
        if len > room {
            return Err(SubmitError::TooLarge);
        }

        // One readable descriptor per run of device-consecutive pages the
        // request covers, then the response buffer.
        let mut buffers = [Buffer::readable(0, 0); QUEUE_SIZE as usize];
        let page = PAGE_SIZE as usize;
        let pages = self.area.device_pages();
        let mut count = 0usize;
        let mut offset = 0usize;
        while offset < len {
            let chunk = (page - offset % page).min(len - offset);
            let address = contiguous(pages, offset, chunk).ok_or(SubmitError::TooLarge)?;
            let joined = count
                .checked_sub(1)
                .and_then(|last| buffers.get_mut(last))
                .filter(|last| last.address.checked_add(u64::from(last.len)) == Some(address));
            if let Some(last) = joined {
                last.len += u32::try_from(chunk).map_err(|_| SubmitError::TooLarge)?;
            } else {
                let slot = buffers
                    .get_mut(count)
                    .filter(|_| count + 1 < usize::from(QUEUE_SIZE))
                    .ok_or(SubmitError::TooLarge)?;
                *slot = Buffer::readable(
                    address,
                    u32::try_from(chunk).map_err(|_| SubmitError::TooLarge)?,
                );
                count += 1;
            }
            offset += chunk;
        }
        let response_at = room;
        let response =
            contiguous(pages, response_at, RESPONSE_BYTES).ok_or(SubmitError::TooLarge)?;
        let slot = buffers.get_mut(count).ok_or(SubmitError::TooLarge)?;
        *slot = Buffer::writable(response, RESPONSE_BYTES as u32);
        count += 1;
        let chain = buffers.get(..count).ok_or(SubmitError::TooLarge)?;

        let area = &mut *self.area;
        let _ = command
            .write_with(|at, bytes| {
                for (index, &byte) in bytes.iter().enumerate() {
                    area.write_u8(at + index, byte);
                }
            })
            .map_err(SubmitError::Command)?;
        // The response buffer starts unwritten, so a device that completes
        // the chain without writing a response is caught.
        for index in 0..4 {
            self.area.write_u8(response_at + index, 0);
        }

        let head = match self.queue.add_chain(chain) {
            Ok(head) => head,
            Err(QueueError::ChainTooLong | QueueError::OutOfDescriptors) => {
                return Err(SubmitError::TooLarge);
            }
            Err(error) => {
                self.break_down(DeviceError::Queue(error));
                return Err(SubmitError::Device(DeviceError::Queue(error)));
            }
        };
        self.in_flight = Some(InFlight {
            head,
            command: command.code(),
        });
        if self.queue.device_wants_notification() {
            self.transport.notify(gpu::CONTROL_QUEUE, self.notify_off);
        }
        Ok(())
    }

    /// Take the response of the command in flight, if the device has written
    /// it. Returns the ISR bits alongside, so a display change is not lost.
    pub fn on_interrupt(&mut self) -> Result<(Option<Done>, u8), DeviceError> {
        let isr = self.transport.acknowledge_interrupt();
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if self.transport.read8(DEVICE_STATUS) & STATUS_DEVICE_NEEDS_RESET != 0 {
            self.break_down(DeviceError::NeedsReset);
            return Err(DeviceError::NeedsReset);
        }
        let used = match self.queue.take_used() {
            Ok(Some(used)) => used,
            Ok(None) => return Ok((None, isr)),
            Err(error) => {
                self.break_down(DeviceError::Queue(error));
                return Err(DeviceError::Queue(error));
            }
        };
        let Some(flight) = self
            .in_flight
            .take()
            .filter(|flight| flight.head == used.head)
        else {
            let error = DeviceError::UnknownChain(used.head);
            self.break_down(error);
            return Err(error);
        };
        let at = self.request_room();
        let mut response = [0u8; RESPONSE_BYTES];
        for (index, byte) in response.iter_mut().enumerate() {
            *byte = self.area.read_u8(at + index);
        }
        let result = match Response::parse_for(flight.command, &response, used.written) {
            Ok(response) => Ok(response),
            Err(GpuError::Device(refusal)) => Err(refusal),
            Err(error) => {
                self.break_down(DeviceError::Protocol(error));
                return Err(DeviceError::Protocol(error));
            }
        };
        Ok((
            Some(Done {
                command: flight.command,
                result,
            }),
            isr,
        ))
    }

    /// Read the configuration's pending events and acknowledge them.
    pub fn take_events(&mut self) -> u32 {
        let events = self.transport.config_read32(gpu::CONFIG_EVENTS_READ);
        if events != 0 {
            self.transport
                .config_write32(gpu::CONFIG_EVENTS_CLEAR, events);
        }
        events
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
