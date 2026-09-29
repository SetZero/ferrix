//! A virtio-blk driver, as logic over a transport and DMA memory it is handed.
//!
//! Stage 10 of `docs/ROADMAP.md` runs the first driver — this one — as a user
//! process. That process holds an `IoMapping` of the device's register blocks,
//! an `Interrupt`, and pinned DMA memory whose device addresses came from an
//! IOMMU domain, and none of those can be had in a unit test. So the driver is
//! written against three traits the process implements over its handles —
//! [`Transport`], [`DevicePages`] and [`RequestArea`], plus
//! [`ferrix_virtio::QueueMemory`] for the rings — and this crate is everything
//! that is left: the order of the status protocol, which features to take,
//! how a request becomes chains and a completion becomes an answer, and what
//! to do when the device lies. It knows nothing of handles, ports or VMOs.
//!
//! # The transport is the common configuration, plus three things
//!
//! [`Transport`] extends [`pci::CommonConfig`], the register block the
//! kernel's own virtio-rng check drives, so the process implements the same
//! six accessors over its mapping of the same block and every step of the
//! status protocol here is one of `ferrix_virtio::pci`'s tested functions.
//! What a driver needs beyond the common configuration is the device-specific
//! configuration ([`blk::DeviceConfig`]), a doorbell, and a way to acknowledge
//! an interrupt — reading the ISR byte for a line interrupt, or acknowledging
//! the `Interrupt` object for MSI-X.
//!
//! # DMA memory is freed only after a reset
//!
//! A device holds addresses into the rings, the request headers and the data
//! pages for as long as it is not reset. Unpinning any of them first lets the
//! device write into memory that now belongs to something else — which is why
//! the kernel's entropy check resets before it frees, and keeps the pages for
//! good when the reset does not finish. This crate makes that rule a type. The
//! driver holds its memory in [`ManuallyDrop`], so dropping a [`Driver`]
//! leaks rather than frees; [`Driver::shutdown`] resets the device and hands
//! the memory back as [`Teardown::Released`] only if the reset finished, and
//! as [`Teardown::Wedged`], still in `ManuallyDrop`, if it did not. A failed
//! [`Driver::init`] ends the same way.
//!
//! # A request completes exactly once
//!
//! A caller's request — an id, a read, write or flush, a sector, a count and
//! a data offset, which is what the shared ring protocol carries — may become
//! several chains, because a device bounds a chain's segments and each page
//! whose device address does not follow the last one's is a segment of its
//! own. All of a request's chains are published or none is, and the request
//! completes once, when the last of them does, with the bytes up to the first
//! chain that failed.
//!
//! Every id [`Driver::submit`] accepted is answered exactly once: by a
//! [`Completion`] from [`Driver::on_interrupt`], or — once the device has
//! broken the protocol and the driver has stopped trusting it — by
//! [`Released::abandoned`] after the reset. An id `submit` refused for a
//! reason of the caller's is not tracked at all.
//!
//! # Trust
//!
//! Everything the device says is checked before it is acted on: the used
//! ring by [`SplitQueue`], a completion's `written` and status byte by
//! [`blk::parse_completion`], the configuration by [`blk::Config::read`] and
//! [`blk::Limits::new`], read again if `config_generation` moves. A device
//! that fails a check is marked failed, is sent nothing more, and waits for
//! [`Driver::shutdown`].

#![no_std]
// Forbidden in the shipped library. The tests implement
// `ferrix_virtio::QueueMemory`, an unsafe trait, for their fake memory, and a
// forbid cannot be relaxed for one item; there it is denied, and the one
// implementation is exempted and argued at its site.
#![cfg_attr(not(test), forbid(unsafe_code))]
#![cfg_attr(test, deny(unsafe_code))]

use core::fmt;
use core::mem::ManuallyDrop;

use ferrix_virtio::blk::{
    self, BlkError, Chain, Config, Data, DeviceConfig, HEADER_LEN, Header, Limits, PAGE_SIZE,
    RequestType, SECTOR_SIZE, Segments,
};
use ferrix_virtio::pci::{
    self, CONFIG_GENERATION, CommonConfig, DEVICE_STATUS, QueueAddresses,
    STATUS_DEVICE_NEEDS_RESET, STATUS_FAILED, TransportError,
};
use ferrix_virtio::{Layout, MAX_QUEUE_SIZE, QueueError, QueueMemory, SplitQueue};

pub use ferrix_virtio::blk::Status;

// At the root, so every test module can name `std`.
#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

/// The request queue: virtio-blk's queue 0.
pub const REQUEST_QUEUE: u16 = 0;

/// ISR status bit: a queue has something for the driver (virtio 1.2
/// §4.1.4.5).
pub const ISR_QUEUE: u8 = 1;

/// ISR status bit: the device configuration changed.
pub const ISR_CONFIG: u8 = 2;

/// What the status byte holds until the device writes it: no status virtio
/// defines, so a device that completes a chain without writing one is caught.
const STATUS_UNWRITTEN: u8 = 0xFF;

/// The smallest queue worth running: one chain of header, one data segment
/// and status, with room for another request beside it.
const MIN_QUEUE_SIZE: u16 = 4;

/// Bytes of the request area one in-flight chain uses: its header and its
/// status byte.
const AREA_PER_CHAIN: u64 = HEADER_LEN as u64 + 1;

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

    /// The MSI-X table entry the request queue should interrupt through, or
    /// [`pci::NO_VECTOR`] for a line interrupt.
    fn queue_vector(&self) -> u16;

    /// Acknowledge the interrupt that woke the driver and say why it came:
    /// the ISR status byte, whose read clears it, for a line interrupt; or
    /// [`ISR_QUEUE`] for MSI-X, whose vector already says.
    fn acknowledge_interrupt(&mut self) -> u8;
}

/// Pinned memory, as the device addresses of its pages.
///
/// The addresses are the ones the pin's address query (native call 0x1026)
/// wrote after `VMO_PIN` (0x1025) pinned the range into the device's domain.
/// The driver must not assume any relation between them and physical
/// addresses: under a translating IOMMU, VT-d on x86-64 or the `SMMUv3` on
/// AArch64, the device reaches only what its domain maps, and where the kernel
/// maps a page is the kernel's choice.
pub trait DevicePages {
    /// The device address of page `i` of the pinned range, for every page in
    /// order, as the pin's address query returned them: [`PAGE_SIZE`] bytes
    /// each, and not necessarily consecutive. Offsets into the region, such as
    /// a request's `data_offset`, count from the start of the pinned range, not
    /// from the start of the VMO it was pinned from.
    fn device_pages(&self) -> &[u64];
}

/// The memory a driver keeps request headers and status bytes in.
///
/// Offsets are bytes from the start of the region; the driver passes only
/// offsets below `device_pages().len() × PAGE_SIZE`.
pub trait RequestArea: DevicePages {
    /// Read the byte at `offset`.
    fn read_u8(&self, offset: usize) -> u8;
    /// Write the byte at `offset`.
    fn write_u8(&mut self, offset: usize, value: u8);
}

/// What a request asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    /// Read `count` sectors into the data.
    Read,
    /// Write `count` sectors from the data.
    Write,
    /// Make every completed write durable.
    Flush,
}

/// One request, as the ring protocol carries it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Request {
    /// The caller's name for it, handed back on completion.
    pub id: u64,
    /// What to do.
    pub op: Op,
    /// The first 512-byte sector. Ignored by a flush.
    pub sector: u64,
    /// How many 512-byte sectors. Ignored by a flush.
    pub count: u32,
    /// Where the data starts in the data region. Ignored by a flush.
    pub data_offset: u64,
}

/// A finished request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Completion {
    /// The request's id.
    pub id: u64,
    /// How it went: that of the first of its chains that did not succeed.
    pub status: Status,
    /// Bytes read or written from the start of the data before the first
    /// chain that did not succeed — all of them on success.
    pub bytes: u64,
}

/// What [`Driver::submit`] did with a request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Accepted {
    /// Published as this many chains; the completion will come.
    Queued {
        /// Chains the request became.
        chains: u16,
    },
    /// Answered without the device: a flush to a device without
    /// `VIRTIO_BLK_F_FLUSH`, which has no volatile cache to flush.
    Completed(Completion),
}

/// What [`Driver::on_interrupt`] found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Drained {
    /// Completions written to the start of the caller's slice.
    pub completions: usize,
    /// The device says its configuration changed; see
    /// [`Driver::refresh_config`].
    pub config_changed: bool,
    /// The device has completed more than the slice had room for.
    pub more: bool,
}

/// How [`Driver::init`] should go about it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options {
    /// Reads of `device_status` a reset may take.
    pub reset_polls: u32,
    /// The largest queue wanted; the device, the memory and the slots may
    /// make it smaller.
    pub max_queue_size: u16,
    /// Reads of the configuration before giving up on one that keeps
    /// changing under the driver.
    pub config_attempts: u32,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            reset_polls: 100_000,
            max_queue_size: 256,
            config_attempts: 8,
        }
    }
}

/// What the driver agreed with the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Info {
    /// The features negotiated.
    pub features: u64,
    /// The device's configuration, as last read.
    pub config: Config,
    /// What a request may be.
    pub limits: Limits,
    /// `VIRTIO_BLK_F_RO`: writes are refused.
    pub read_only: bool,
    /// `VIRTIO_BLK_F_FLUSH`: flushes go to the device.
    pub flush: bool,
    /// Entries in the request queue.
    pub queue_size: u16,
    /// The MSI-X vector the queue interrupts through, or `NO_VECTOR`.
    pub vector: u16,
}

/// Bookkeeping for one queue entry. The caller provides at least as many as
/// the queue has entries — see [`Options::max_queue_size`] — so the driver
/// allocates nothing.
#[derive(Clone, Copy, Debug)]
pub struct Slot {
    /// The chain whose head descriptor is this slot's index.
    chain: Option<ChainRecord>,
    /// Whether the header and status at this slot's index are in use.
    header_busy: bool,
    /// A request, at whichever free slot it was given.
    request: Option<RequestRecord>,
}

impl Slot {
    /// A slot with nothing in it.
    pub const EMPTY: Slot = Slot {
        chain: None,
        header_busy: false,
        request: None,
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
    /// The slot holding its request.
    request: u16,
    /// The header slot it uses.
    header: u16,
    /// Where its data starts within the request's.
    offset: u64,
    /// What was published.
    chain: Chain,
}

/// One accepted request.
#[derive(Clone, Copy, Debug)]
struct RequestRecord {
    /// The caller's id.
    id: u64,
    /// Chains not yet completed.
    pending: u16,
    /// Bytes of data in the whole request.
    total: u64,
    /// The earliest chain that did not succeed: its offset and status.
    failure: Option<(u64, Status)>,
}

/// Why a device could not be brought up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InitError {
    /// The status protocol failed: a reset that did not finish, a device
    /// without `VERSION_1`, refused features, no request queue, or a device
    /// that needs a reset.
    Transport(TransportError),
    /// The configuration is truncated or names an unusable block size.
    Config(BlkError),
    /// `config_generation` changed on every read.
    ConfigUnstable,
    /// No queue of at least four entries fits the device, the rings' pages,
    /// the request area and the slots together.
    NoRoom,
    /// The device would not give the queue the MSI-X vector asked for.
    VectorRefused {
        /// The vector asked for.
        asked: u16,
        /// What the device kept.
        kept: u16,
    },
}

/// Why a request was not accepted. Except for [`SubmitError::Device`], the
/// request is not tracked and will not complete.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubmitError {
    /// The device has already failed; shut the driver down.
    Broken,
    /// A write to a read-only disk.
    ReadOnly,
    /// A read or write of no sectors.
    Empty,
    /// Not whole logical blocks, or not starting on one.
    NotAligned,
    /// Sectors past the end of the disk.
    OutOfRange,
    /// Data past the end of the data region.
    OutsideData,
    /// A data page's device address overflows.
    BadAddress,
    /// A block of the data needs more segments than the device allows.
    Unsplittable,
    /// More descriptors than the queue has, however empty it gets.
    TooLarge,
    /// Not enough free descriptors now; complete something and retry.
    QueueFull,
    /// The device broke the protocol while the request was being published.
    /// The request is tracked and will be reported by
    /// [`Released::abandoned`].
    Device(DeviceError),
}

/// How the device broke the protocol. After any of these the driver has set
/// `FAILED` and sends nothing more.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceError {
    /// The driver has already failed on an earlier error.
    Broken,
    /// The device set `DEVICE_NEEDS_RESET`.
    NeedsReset,
    /// The rings say something impossible.
    Queue(QueueError),
    /// A completion's `written` or status byte is impossible.
    Protocol(BlkError),
    /// A completion names a chain the driver has no record of.
    UnknownChain(u16),
    /// The configuration changed on every read.
    ConfigUnstable,
    /// The driver's own records disagree with the queue — which only memory
    /// the device can write can have caused.
    Bookkeeping,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            InitError::Transport(error) => write!(f, "{error}"),
            InitError::Config(error) => write!(f, "{error}"),
            InitError::ConfigUnstable => f.write_str("the configuration kept changing"),
            InitError::NoRoom => f.write_str("no queue fits the memory given"),
            InitError::VectorRefused { asked, kept } => {
                write!(f, "the device kept vector {kept:#x} for {asked:#x}")
            }
        }
    }
}

impl fmt::Display for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match *self {
            SubmitError::Broken => "the device has failed",
            SubmitError::ReadOnly => "the disk is read-only",
            SubmitError::Empty => "the request has no sectors",
            SubmitError::NotAligned => "the request is not whole blocks",
            SubmitError::OutOfRange => "the request is past the end of the disk",
            SubmitError::OutsideData => "the data is past the end of its region",
            SubmitError::BadAddress => "a data page's address overflows",
            SubmitError::Unsplittable => "a block needs more segments than allowed",
            SubmitError::TooLarge => "the request needs more descriptors than the queue has",
            SubmitError::QueueFull => "the queue is full",
            SubmitError::Device(error) => return write!(f, "{error}"),
        })
    }
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            DeviceError::Broken => f.write_str("the device has failed"),
            DeviceError::NeedsReset => f.write_str("the device needs a reset"),
            DeviceError::Queue(error) => write!(f, "the rings are corrupt: {error:?}"),
            DeviceError::Protocol(error) => write!(f, "{error}"),
            DeviceError::UnknownChain(head) => write!(f, "chain {head} is not in flight"),
            DeviceError::ConfigUnstable => f.write_str("the configuration kept changing"),
            DeviceError::Bookkeeping => f.write_str("the driver's records disagree with the queue"),
        }
    }
}

/// Everything a driver is built from.
pub struct Parts<T, R, A, D, S> {
    /// The device's registers.
    pub transport: T,
    /// The rings' memory: at least one page.
    pub rings: R,
    /// Headers and status bytes: 17 bytes an entry.
    pub area: A,
    /// The data region requests' offsets are into.
    pub data: D,
    /// Bookkeeping, one slot an entry.
    pub slots: S,
}

/// The rings' memory as it comes back: never given to a queue, or inside the
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
    /// The rings' memory.
    pub rings: Rings<R>,
    /// The request area.
    pub area: A,
    /// The data region.
    pub data: D,
    /// The bookkeeping.
    pub slots: S,
}

impl<T, R, A, D, S: AsRef<[Slot]>> Released<T, R, A, D, S> {
    /// The ids of requests accepted and never completed, which the caller
    /// must now answer as failed.
    pub fn abandoned(&self) -> impl Iterator<Item = u64> + '_ {
        self.slots
            .as_ref()
            .iter()
            .filter_map(|slot| slot.request.map(|request| request.id))
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

/// Reset the device, and hand the parts back as that went.
fn teardown<T: CommonConfig, R, A, D, S>(
    mut transport: T,
    rings: Rings<R>,
    area: A,
    data: D,
    slots: S,
    polls: u32,
) -> Teardown<T, R, A, D, S> {
    let reset = pci::reset(&mut transport, polls);
    let released = Released {
        transport,
        rings,
        area,
        data,
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

/// Read the configuration, again while `config_generation` moves under it.
fn read_config<T: Transport>(
    transport: &T,
    features: u64,
    attempts: u32,
) -> Result<Config, InitError> {
    for _ in 0..attempts.max(1) {
        let before = transport.read8(CONFIG_GENERATION);
        let config = Config::read(transport, features);
        if transport.read8(CONFIG_GENERATION) == before {
            return config.map_err(InitError::Config);
        }
    }
    Err(InitError::ConfigUnstable)
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

/// The largest queue that fits everything, and where its rings are.
///
/// A smaller queue is tried when the rings' areas straddle pages whose device
/// addresses do not follow on, since a smaller table may fit inside one page.
fn choose_queue(
    device_max: u16,
    wanted: u16,
    slots: usize,
    ring_pages: &[u64],
    area_pages: &[u64],
) -> Result<(Layout, QueueAddresses), InitError> {
    let cap = device_max
        .min(wanted)
        .min(MAX_QUEUE_SIZE)
        .min(u16::try_from(slots).unwrap_or(u16::MAX));
    if cap < MIN_QUEUE_SIZE {
        return Err(InitError::NoRoom);
    }
    let area = u64::try_from(area_pages.len())
        .unwrap_or(u64::MAX)
        .saturating_mul(PAGE_SIZE);
    // The largest power of two no larger than `cap`, which is at least four.
    let mut size: u16 = 1 << (15 - cap.leading_zeros());
    while size >= MIN_QUEUE_SIZE {
        if u64::from(size) * AREA_PER_CHAIN <= area
            && let Ok(layout) = Layout::for_size(size)
            && let Some(addresses) = ring_addresses(&layout, ring_pages)
        {
            return Ok((layout, addresses));
        }
        size /= 2;
    }
    Err(InitError::NoRoom)
}

/// What negotiation settled, before the queue is built.
struct Plan {
    /// The features agreed.
    features: u64,
    /// The configuration.
    config: Config,
    /// Its limits.
    limits: Limits,
    /// The queue's layout.
    layout: Layout,
    /// Where its rings are.
    addresses: QueueAddresses,
}

/// Reset, agree on features, read the configuration and size the queue.
fn negotiate<T: Transport>(
    transport: &mut T,
    ring_pages: &[u64],
    area_pages: &[u64],
    slots: usize,
    options: &Options,
) -> Result<Plan, InitError> {
    let features = pci::negotiate(
        transport,
        blk::DRIVER_FEATURES,
        blk::REQUIRED_FEATURES,
        options.reset_polls,
    )
    .map_err(InitError::Transport)?;
    let config = read_config(transport, features, options.config_attempts)?;
    let limits = Limits::new(&config).map_err(InitError::Config)?;
    let max = pci::queue_max_size(transport, REQUEST_QUEUE).map_err(InitError::Transport)?;
    let (layout, addresses) =
        choose_queue(max, options.max_queue_size, slots, ring_pages, area_pages)?;
    Ok(Plan {
        features,
        config,
        limits,
        layout,
        addresses,
    })
}

/// A virtio-blk device, brought up and driven.
pub struct Driver<T, R, A, D, S> {
    /// The device's registers.
    transport: T,
    /// The request queue, never dropped but by [`Driver::shutdown`].
    queue: ManuallyDrop<SplitQueue<R>>,
    /// Headers and status bytes, likewise.
    area: ManuallyDrop<A>,
    /// The data region, likewise.
    data: ManuallyDrop<D>,
    /// Bookkeeping.
    slots: S,
    /// What was agreed.
    info: Info,
    /// The request queue's `queue_notify_off`.
    notify_off: u16,
    /// Reads a reset may take.
    reset_polls: u32,
    /// Reads of the configuration before giving up.
    config_attempts: u32,
    /// The error the device broke the protocol with, once it has.
    fault: Option<DeviceError>,
    /// Requests accepted and not yet completed.
    requests: u16,
}

impl<T, R, A, D, S> fmt::Debug for Driver<T, R, A, D, S> {
    /// Leaves out the memory, whose reads have side effects for the device.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Driver")
            .field("info", &self.info)
            .field("fault", &self.fault)
            .field("requests", &self.requests)
            .finish_non_exhaustive()
    }
}

impl<T, R, A, D, S> Driver<T, R, A, D, S>
where
    T: Transport,
    R: QueueMemory + DevicePages,
    A: RequestArea,
    D: DevicePages,
    S: AsRef<[Slot]> + AsMut<[Slot]>,
{
    /// Bring the device up: reset, `ACKNOWLEDGE`, `DRIVER`, features,
    /// `FEATURES_OK` confirmed, the configuration read, the request queue
    /// built and enabled, `DRIVER_OK`.
    ///
    /// Features are [`blk::DRIVER_FEATURES`] where offered, and a device
    /// without `VERSION_1` is refused. The queue is the largest power of two
    /// the device, `options`, the slots, the request area and the rings' pages
    /// all allow, down to four entries.
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
            area,
            data,
            mut slots,
        } = parts;
        slots.as_mut().fill(Slot::EMPTY);

        let slot_count = slots.as_ref().len();
        let plan = match negotiate(
            &mut transport,
            rings.device_pages(),
            area.device_pages(),
            slot_count,
            &options,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                set_failed(&mut transport);
                let teardown = teardown(
                    transport,
                    Rings::Unused(rings),
                    area,
                    data,
                    slots,
                    options.reset_polls,
                );
                return Err(InitFailure { error, teardown });
            }
        };

        // Written before the device is told where the rings are.
        let queue = SplitQueue::new(plan.layout, rings);
        let asked = transport.queue_vector();
        let started = match pci::activate_queue(
            &mut transport,
            REQUEST_QUEUE,
            plan.layout.queue_size,
            plan.addresses,
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
            Err(error) => {
                set_failed(&mut transport);
                let teardown = teardown(
                    transport,
                    Rings::Queue(queue),
                    area,
                    data,
                    slots,
                    options.reset_polls,
                );
                return Err(InitFailure { error, teardown });
            }
        };

        Ok(Driver {
            transport,
            queue: ManuallyDrop::new(queue),
            area: ManuallyDrop::new(area),
            data: ManuallyDrop::new(data),
            slots,
            info: Info {
                features: plan.features,
                config: plan.config,
                limits: plan.limits,
                read_only: plan.features & blk::FEATURE_RO != 0,
                flush: plan.features & blk::FEATURE_FLUSH != 0,
                queue_size: plan.layout.queue_size,
                vector: active.vector,
            },
            notify_off: active.notify_off,
            reset_polls: options.reset_polls,
            config_attempts: options.config_attempts,
            fault: None,
            requests: 0,
        })
    }

    /// What was agreed with the device.
    #[must_use]
    pub const fn info(&self) -> &Info {
        &self.info
    }

    /// The error the device broke the protocol with, if it has.
    #[must_use]
    pub const fn fault(&self) -> Option<DeviceError> {
        self.fault
    }

    /// Requests accepted and not yet completed.
    #[must_use]
    pub const fn requests_in_flight(&self) -> u16 {
        self.requests
    }

    /// Descriptors free in the request queue.
    #[must_use]
    pub fn free_descriptors(&self) -> u16 {
        self.queue.free_descriptors()
    }

    /// Whether nothing at all is in flight: no request, no chain, no header,
    /// and every descriptor free.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.requests == 0
            && self.queue.free_descriptors() == self.info.queue_size
            && self
                .slots
                .as_ref()
                .iter()
                .all(|slot| slot.chain.is_none() && !slot.header_busy && slot.request.is_none())
    }

    /// The device's registers.
    #[must_use]
    pub const fn transport(&self) -> &T {
        &self.transport
    }

    /// Submit a request.
    ///
    /// A read or write is checked — whole logical blocks, on the disk, inside
    /// the data region, not a write to a read-only disk — then laid out as
    /// however many chains the device's segment limits and the data pages'
    /// device addresses need. Either all of those chains are published, and
    /// the device notified, or none is.
    ///
    /// # Errors
    ///
    /// [`SubmitError`]; only [`SubmitError::Device`] leaves the request
    /// tracked.
    pub fn submit(&mut self, request: &Request) -> Result<Accepted, SubmitError> {
        if self.fault.is_some() {
            return Err(SubmitError::Broken);
        }
        let (kind, bytes) = match request.op {
            Op::Flush if !self.info.flush => {
                return Ok(Accepted::Completed(Completion {
                    id: request.id,
                    status: Status::Ok,
                    bytes: 0,
                }));
            }
            Op::Flush => (RequestType::Flush, 0),
            Op::Write if self.info.read_only => return Err(SubmitError::ReadOnly),
            Op::Write => (RequestType::Out, self.check_io(request)?),
            Op::Read => (RequestType::In, self.check_io(request)?),
        };
        let sector = if bytes == 0 { 0 } else { request.sector };

        let chains = self.count_chains(kind, request.data_offset, bytes)?;
        let slot = self.free_request_slot().ok_or(SubmitError::QueueFull)?;
        if let Some(record) = self.slots.as_mut().get_mut(usize::from(slot)) {
            record.request = Some(RequestRecord {
                id: request.id,
                pending: 0,
                total: bytes,
                failure: None,
            });
        }
        self.requests += 1;

        if let Err(error) = self.publish_all(slot, kind, sector, request.data_offset, bytes) {
            self.break_down(error);
            return Err(SubmitError::Device(error));
        }
        if self.queue.device_wants_notification() {
            self.transport.notify(REQUEST_QUEUE, self.notify_off);
        }
        Ok(Accepted::Queued { chains })
    }

    /// Take what the device has completed, after an interrupt.
    ///
    /// The interrupt is acknowledged first, so a completion that lands while
    /// this runs raises another rather than being lost behind the
    /// acknowledgement. Completions are written to the start of `out`; if it
    /// fills, [`Drained::more`] says there is more to take.
    ///
    /// # Errors
    ///
    /// A [`DeviceError`] once the device breaks the protocol. Completions
    /// taken before the fault in the same call are returned first, and the
    /// fault on the next call, so none is lost.
    pub fn on_interrupt(&mut self, out: &mut [Completion]) -> Result<Drained, DeviceError> {
        let isr = self.transport.acknowledge_interrupt();
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if self.transport.read8(DEVICE_STATUS) & STATUS_DEVICE_NEEDS_RESET != 0 {
            self.break_down(DeviceError::NeedsReset);
            return Err(DeviceError::NeedsReset);
        }

        let mut emitted = 0;
        // Each completion frees a chain the driver published, so there are
        // never more to take than the queue holds.
        for _ in 0..=self.info.queue_size {
            let Some(slot) = out.get_mut(emitted) else {
                break;
            };
            let result = match self.queue.take_used() {
                Ok(Some(used)) => self.finish(used),
                Ok(None) => break,
                Err(error) => Err(DeviceError::Queue(error)),
            };
            match result {
                Ok(Some(completion)) => {
                    *slot = completion;
                    emitted += 1;
                }
                Ok(None) => {}
                Err(error) => {
                    self.break_down(error);
                    if emitted == 0 {
                        return Err(error);
                    }
                    break;
                }
            }
        }
        Ok(Drained {
            completions: emitted,
            config_changed: isr & ISR_CONFIG != 0,
            more: self.fault.is_none() && self.queue.has_used(),
        })
    }

    /// Read the configuration again, after [`Drained::config_changed`], and
    /// take the disk's new capacity.
    ///
    /// # Errors
    ///
    /// [`DeviceError::ConfigUnstable`] or [`DeviceError::Protocol`]; neither
    /// fails the driver, which keeps the capacity it had.
    pub fn refresh_config(&mut self) -> Result<u64, DeviceError> {
        let config = read_config(&self.transport, self.info.features, self.config_attempts)
            .map_err(|error| match error {
                InitError::Config(error) => DeviceError::Protocol(error),
                _ => DeviceError::ConfigUnstable,
            })?;
        self.info.config.capacity = config.capacity;
        self.info.limits.capacity = config.capacity;
        Ok(config.capacity)
    }

    /// Reset the device and hand everything back — the memory only if the
    /// reset finished. Requests still in flight are
    /// [`Released::abandoned`].
    pub fn shutdown(self) -> Teardown<T, R, A, D, S> {
        let Driver {
            transport,
            queue,
            area,
            data,
            slots,
            reset_polls,
            ..
        } = self;
        teardown(
            transport,
            Rings::Queue(ManuallyDrop::into_inner(queue)),
            ManuallyDrop::into_inner(area),
            ManuallyDrop::into_inner(data),
            slots,
            reset_polls,
        )
    }

    /// Check a read or write against the disk and the data region, and
    /// return its length in bytes.
    fn check_io(&self, request: &Request) -> Result<u64, SubmitError> {
        if request.count == 0 {
            return Err(SubmitError::Empty);
        }
        let bytes = u64::from(request.count) * u64::from(SECTOR_SIZE);
        let unit = u64::from(self.info.limits.block_size);
        let sectors_per_unit = unit / u64::from(SECTOR_SIZE);
        if !bytes.is_multiple_of(unit) || !request.sector.is_multiple_of(sectors_per_unit) {
            return Err(SubmitError::NotAligned);
        }
        let end = request
            .sector
            .checked_add(u64::from(request.count))
            .ok_or(SubmitError::OutOfRange)?;
        if end > self.info.limits.capacity {
            return Err(SubmitError::OutOfRange);
        }
        let region = u64::try_from(self.data.device_pages().len())
            .unwrap_or(u64::MAX)
            .saturating_mul(PAGE_SIZE);
        let data_end = request
            .data_offset
            .checked_add(bytes)
            .ok_or(SubmitError::OutsideData)?;
        if data_end > region {
            return Err(SubmitError::OutsideData);
        }
        Ok(bytes)
    }

    /// The data descriptors for the chain carrying `len` bytes from `offset`
    /// of the data region.
    fn segments(&self, kind: RequestType, offset: u64, len: u64) -> Result<Segments, SubmitError> {
        let limits = &self.info.limits;
        let room = usize::from(self.info.queue_size).saturating_sub(2);
        let most = usize::try_from(limits.max_segments)
            .unwrap_or(usize::MAX)
            .min(room);
        let data = Data {
            pages: self.data.device_pages(),
            offset,
            len,
        };
        blk::plan(
            &data,
            kind.device_writes_data(),
            limits.block_size,
            most,
            limits.max_segment_size,
        )
        .map_err(|error| match error {
            BlkError::OutsideRegion => SubmitError::OutsideData,
            BlkError::NotWholeUnits => SubmitError::NotAligned,
            BlkError::Unsplittable => SubmitError::Unsplittable,
            _ => SubmitError::BadAddress,
        })
    }

    /// How many chains `bytes` from `offset` becomes, if the queue can take
    /// them all now.
    fn count_chains(&self, kind: RequestType, offset: u64, bytes: u64) -> Result<u16, SubmitError> {
        let size = self.info.queue_size;
        let free = self.queue.free_descriptors();
        if bytes == 0 {
            return if free >= 2 {
                Ok(1)
            } else {
                Err(SubmitError::QueueFull)
            };
        }
        let mut at = 0;
        let mut chains: u16 = 0;
        let mut descriptors: usize = 0;
        // Every chain takes at least three descriptors, so this ends within a
        // third of the queue.
        while at < bytes {
            let segments = self.segments(kind, offset + at, bytes - at)?;
            chains += 1;
            descriptors += segments.count() + 2;
            if descriptors > usize::from(size) {
                return Err(SubmitError::TooLarge);
            }
            at += segments.bytes();
        }
        if descriptors > usize::from(free) {
            return Err(SubmitError::QueueFull);
        }
        Ok(chains)
    }

    /// A slot with no request in it.
    fn free_request_slot(&self) -> Option<u16> {
        let size = usize::from(self.info.queue_size);
        let index = self
            .slots
            .as_ref()
            .iter()
            .take(size)
            .position(|slot| slot.request.is_none())?;
        u16::try_from(index).ok()
    }

    /// Publish every chain of the request in slot `request`.
    fn publish_all(
        &mut self,
        request: u16,
        kind: RequestType,
        sector: u64,
        offset: u64,
        bytes: u64,
    ) -> Result<(), DeviceError> {
        let mut at = 0;
        for _ in 0..self.info.queue_size {
            let segments = if bytes == 0 {
                Segments::none()
            } else {
                self.segments(kind, offset + at, bytes - at)
                    .map_err(|_| DeviceError::Bookkeeping)?
            };
            let header = Header {
                kind,
                sector: sector + at / u64::from(SECTOR_SIZE),
            };
            self.publish_chain(request, header, at, &segments)?;
            at += segments.bytes();
            if at >= bytes {
                return Ok(());
            }
            if segments.bytes() == 0 {
                break;
            }
        }
        Err(DeviceError::Bookkeeping)
    }

    /// Write one chain's header, publish the chain, and record it.
    fn publish_chain(
        &mut self,
        request: u16,
        header: Header,
        offset: u64,
        segments: &Segments,
    ) -> Result<(), DeviceError> {
        let slot = self.take_header().ok_or(DeviceError::Bookkeeping)?;
        let Some((header_at, status_at)) = self.header_addresses(slot) else {
            self.release_header(slot);
            return Err(DeviceError::Bookkeeping);
        };
        let header_offset = usize::from(slot) * usize::from(HEADER_LEN as u16);
        for (index, byte) in header.encode().into_iter().enumerate() {
            self.area.write_u8(header_offset + index, byte);
        }
        let status_offset = self.status_offset(slot);
        self.area.write_u8(status_offset, STATUS_UNWRITTEN);

        let chain = match blk::publish(&mut self.queue, header_at, segments, status_at) {
            Ok(chain) => chain,
            Err(error) => {
                self.release_header(slot);
                return Err(match error {
                    BlkError::Queue(error) => DeviceError::Queue(error),
                    error => DeviceError::Protocol(error),
                });
            }
        };
        let slots = self.slots.as_mut();
        let record = slots
            .get_mut(usize::from(chain.head))
            .filter(|record| record.chain.is_none())
            .ok_or(DeviceError::Bookkeeping)?;
        record.chain = Some(ChainRecord {
            request,
            header: slot,
            offset,
            chain,
        });
        let owner = slots
            .get_mut(usize::from(request))
            .and_then(|record| record.request.as_mut())
            .ok_or(DeviceError::Bookkeeping)?;
        owner.pending += 1;
        Ok(())
    }

    /// Account for one completed chain, and return its request's completion
    /// if that was the request's last.
    fn finish(
        &mut self,
        used: ferrix_virtio::Completion,
    ) -> Result<Option<Completion>, DeviceError> {
        let record = self
            .slots
            .as_mut()
            .get_mut(usize::from(used.head))
            .and_then(|slot| slot.chain.take())
            .ok_or(DeviceError::UnknownChain(used.head))?;
        let status_byte = self.area.read_u8(self.status_offset(record.header));
        self.release_header(record.header);
        let status = blk::parse_completion(&record.chain, used.written, status_byte)
            .map_err(DeviceError::Protocol)?;

        let slot = self
            .slots
            .as_mut()
            .get_mut(usize::from(record.request))
            .ok_or(DeviceError::Bookkeeping)?;
        let request = slot.request.as_mut().ok_or(DeviceError::Bookkeeping)?;
        request.pending = request
            .pending
            .checked_sub(1)
            .ok_or(DeviceError::Bookkeeping)?;
        if status != Status::Ok && request.failure.is_none_or(|(at, _)| record.offset < at) {
            request.failure = Some((record.offset, status));
        }
        if request.pending > 0 {
            return Ok(None);
        }
        let completion = Completion {
            id: request.id,
            status: request.failure.map_or(Status::Ok, |(_, status)| status),
            bytes: request.failure.map_or(request.total, |(at, _)| at),
        };
        slot.request = None;
        self.requests = self.requests.saturating_sub(1);
        Ok(Some(completion))
    }

    /// Take a free header slot.
    fn take_header(&mut self) -> Option<u16> {
        let size = usize::from(self.info.queue_size);
        let slot = self
            .slots
            .as_mut()
            .iter_mut()
            .take(size)
            .enumerate()
            .find(|(_, slot)| !slot.header_busy)?;
        slot.1.header_busy = true;
        u16::try_from(slot.0).ok()
    }

    /// Give a header slot back.
    fn release_header(&mut self, header: u16) {
        if let Some(slot) = self.slots.as_mut().get_mut(usize::from(header)) {
            slot.header_busy = false;
        }
    }

    /// Where header slot `slot`'s status byte is in the request area.
    fn status_offset(&self, slot: u16) -> usize {
        usize::from(self.info.queue_size) * usize::from(HEADER_LEN as u16) + usize::from(slot)
    }

    /// The device addresses of header slot `slot`'s header and status.
    ///
    /// A header starts at a multiple of sixteen, and so never straddles a
    /// page; neither can one byte.
    fn header_addresses(&self, slot: u16) -> Option<(u64, u64)> {
        let pages = self.area.device_pages();
        let address = |offset: usize| {
            let offset = u64::try_from(offset).ok()?;
            let page = usize::try_from(offset / PAGE_SIZE).ok()?;
            pages.get(page)?.checked_add(offset % PAGE_SIZE)
        };
        let header = address(usize::from(slot) * usize::from(HEADER_LEN as u16))?;
        let status = address(self.status_offset(slot))?;
        Some((header, status))
    }

    /// Stop trusting the device: record why, and tell it `FAILED`.
    fn break_down(&mut self, error: DeviceError) {
        if self.fault.is_none() {
            self.fault = Some(error);
        }
        set_failed(&mut self.transport);
    }
}
