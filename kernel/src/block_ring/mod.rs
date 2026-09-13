//! Stage 10: the kernel's end of a block ring.
//!
//! `docs/BLOCK-RING.md` is the protocol, and `ferrix-blkring` checks every
//! word of it. This is the glue that crate leaves to the kernel (its §8). A
//! process holding a device with `MANAGE` asks for a ring with
//! `BLOCK_RING_CREATE` and hands the channel end it gets to a block driver.
//! One kernel task per ring then:
//!
//! 1. waits for the driver's HELLO and checks it, answering REFUSED with the
//!    first failure in §6.2's order;
//! 2. holds the ring and data VMOs, attaches [`KernelSide`] over the ring, and
//!    publishes the disk as a block device before answering READY with its
//!    completion port;
//! 3. serves the disk's reads: it moves them from `libs/block`'s queue onto
//!    the ring, rings the driver when the driver asked to be rung, takes
//!    completions off the ring, copies what was read out of the data VMO and
//!    wakes the reader;
//! 4. ends when the driver's control channel closes, the driver says STOPPED
//!    or the ring is corrupt, failing every outstanding read with EIO.
//!
//! # The kernel never serves a disk
//!
//! It issues requests and copies payloads. Whatever answers a request is the
//! process at the other end of the ring.
//!
//! # Pinned frames on a driver's death
//!
//! §6.3's rule that pinned pages outlive the driver until its device is reset
//! is kept by the driver's own pin, not here. Closing a pin on a translated
//! domain unmaps its pages before they can go, and on an untranslated domain
//! the pin keeps them. The ring's holds on the VMOs are only what the kernel
//! reads and writes through.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::convert::Infallible;
use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};

use ferrix_blkring::kernel::Ending;
use ferrix_blkring::{
    Completed, Device, DeviceFlags, KernelSide, Location, Message, Refusal, RingMemory, Slot,
    Status, Submission, SubmitError, Wait,
};
use ferrix_block::{Config, Limits, Queue, Request, Token};
use ferrix_bootinfo::PAGE_SIZE;
use ferrix_native_abi::rights::Rights;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::{CHANNEL_MAX_BYTES, CHANNEL_MAX_HANDLES, PACKET_SIGNAL};
use ferrix_vfs::Errno;

use crate::device::{self, DeviceNode};
use crate::object::channel::{Endpoint, ReadError};
use crate::object::port::{Observer, Port};
use crate::object::{self, Object, Transfer};
use crate::sched::{Task, WaitQueue};
use crate::sync::SpinLock;
use crate::user::vmo::{Held, Vmo};
use crate::{mm, sched, timer};

use crate::fs::block::BlockDevice;
use crate::fs::devfs::{BlockRefused, BlockRegistration, register_block};

pub(crate) mod check;

/// The block major every ring's disk is published under. Linux allocates
/// virtio-blk's major dynamically, usually 253 or 254; nothing keys on it.
pub(crate) const VIRTIO_BLK_MAJOR: u32 = 254;

/// The rights the driver's end of the control channel carries: it sends and
/// receives on it, waits on it and is handed it, and nobody copies it.
pub(crate) const CONTROL_RIGHTS: Rights =
    Rights(Rights::TRANSFER.0 | Rights::READ.0 | Rights::WRITE.0 | Rights::WAIT.0);

/// How long a ring waits for its driver's HELLO.
const HELLO_PATIENCE_NANOS: u64 = 10_000_000_000;

/// How long a read waits for its driver before answering EIO.
const READ_PATIENCE_NANOS: u64 = 30_000_000_000;

/// How long the ring's task sleeps before looking again of its own accord.
const RECHECK_NANOS: u64 = 50_000_000;

/// The completion port's key for the control channel's signals. Keys 1 and 2
/// are the ring's own bells.
const CONTROL_KEY: u64 = 3;

/// The completion port's key for a reader's nudge that a read was queued.
const SUBMIT_KEY: u64 = 4;

/// The most reads `libs/block` merges into one ring submission.
const MAX_PARTS: u32 = 16;

/// The most submissions a ring holds outstanding, whatever its data VMO.
const MAX_OUTSTANDING: u64 = 4096;

/// Why a ring could not be made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CreateError {
    /// The device already has a ring, live or ended.
    InUse,
    /// The device is not a PCI function, which HELLO's location names.
    NotPci,
    /// No memory for the channel, or no stack for the ring's task.
    NoMemory,
}

/// A device with a ring.
#[derive(Debug)]
struct Claim {
    /// The ring's number, which its task is started with.
    id: usize,
    /// The device.
    device: Arc<DeviceNode>,
    /// The location its accepted driver serves, while one does.
    served: Option<Location>,
}

/// What a ring's task starts from.
#[derive(Debug)]
struct Start {
    /// The ring's number.
    id: usize,
    /// The kernel's end of the control channel.
    control: Arc<Endpoint>,
    /// The PCI location of the device the ring was made for.
    location: Location,
}

/// Every device with a ring. A device keeps its claim once a driver has been
/// accepted and has died, since nothing then says its device was reset; a
/// driver that answered STOPPED has reset it (BLOCK-RING.md section 6.3), and
/// the device is free for the next ring.
static CLAIMS: SpinLock<Vec<Claim>> = SpinLock::new(Vec::new());

/// Rings made and not yet taken up by their task.
static STARTING: SpinLock<Vec<Start>> = SpinLock::new(Vec::new());

/// Every ring's task, so that a check counting frames can wait for the ones
/// that have ended to have stopped; the dead are pruned as the living join.
static TASKS: SpinLock<Vec<Arc<Task>>> = SpinLock::new(Vec::new());

/// The next ring's number.
static NEXT_RING: AtomicUsize = AtomicUsize::new(1);

/// Make a ring for `node` and start its task; answer the driver's end of its
/// control channel.
///
/// # Errors
///
/// [`CreateError`].
pub(crate) fn create(node: &Arc<DeviceNode>) -> Result<Arc<Endpoint>, CreateError> {
    let location = location_of(node).ok_or(CreateError::NotPci)?;
    let (kernel_end, driver_end) = Endpoint::pair().ok_or(CreateError::NoMemory)?;
    let id = NEXT_RING.fetch_add(1, Ordering::Relaxed);
    {
        let mut claims = CLAIMS.lock();
        if claims.iter().any(|claim| Arc::ptr_eq(&claim.device, node)) {
            return Err(CreateError::InUse);
        }
        claims.push(Claim {
            id,
            device: Arc::clone(node),
            served: None,
        });
    }
    STARTING.lock().push(Start {
        id,
        control: kernel_end,
        location,
    });
    match sched::spawn("block ring", run, id, ferrix_sched::NICE_0_WEIGHT) {
        Ok(task) => {
            let mut tasks = TASKS.lock();
            tasks.retain(|task| !task.is_dead());
            // Room for more rings than any check makes at once, taken on the
            // first ring so the list never grows inside a frame-count window.
            if tasks.capacity() == 0 {
                tasks.reserve(16);
            }
            tasks.push(task);
        }
        Err(_) => {
            let _ = take_start(id);
            unclaim(id);
            return Err(CreateError::NoMemory);
        }
    }
    Ok(driver_end)
}

/// Wait until every ring's task has stopped, or `deadline` passes: for a
/// check counting frames, whose window must not hold a stack the reaper is
/// about to free.
///
/// # Errors
///
/// A ring's task still running at the deadline.
pub(crate) fn wait_until_tasks_stopped(deadline: u64) -> Result<(), &'static str> {
    loop {
        // Sleep before the first look as well as between looks, so a caller
        // measuring frames pays for the timer's entry on every call alike,
        // rather than only on the call whose tasks were slow to stop.
        sched::sleep_for(1_000_000);
        let mut tasks = TASKS.lock();
        if tasks.iter().all(|task| task.is_dead()) {
            tasks.clear();
            return Ok(());
        }
        drop(tasks);
        if timer::now_nanos() >= deadline {
            return Err("a ring's task kept running after its ring ended");
        }
    }
}

/// The PCI location HELLO must name for `node`, if it is a PCI function.
pub(crate) fn location_of(node: &DeviceNode) -> Option<Location> {
    let device::Location::Pci(address) = node.location() else {
        return None;
    };
    Some(Location::new(
        address.segment(),
        address.bus(),
        (address.device() << 3) | address.function(),
    ))
}

/// Take ring `id`'s start off the list.
fn take_start(id: usize) -> Option<Start> {
    let mut starting = STARTING.lock();
    let at = starting.iter().position(|start| start.id == id)?;
    Some(starting.swap_remove(at))
}

/// Give ring `id`'s device back, for a ring whose driver was never accepted.
fn unclaim(id: usize) {
    CLAIMS.lock().retain(|claim| claim.id != id);
}

/// Note that ring `id`'s driver serves `location`, or no longer serves one.
fn set_served(id: usize, location: Option<Location>) {
    if let Some(claim) = CLAIMS.lock().iter_mut().find(|claim| claim.id == id) {
        claim.served = location;
    }
}

/// Whether a ring other than `id` has an accepted driver serving `location`.
fn served_elsewhere(id: usize, location: Location) -> bool {
    CLAIMS
        .lock()
        .iter()
        .any(|claim| claim.id != id && claim.served == Some(location))
}

/// A ring's task.
fn run(id: usize) {
    let Some(start) = take_start(id) else {
        return;
    };
    let Some(hello) = receive_hello(&start.control) else {
        unclaim(id);
        return;
    };
    let accepted = match check_hello(&start, hello) {
        Ok(accepted) => accepted,
        Err(refusal) => {
            refuse(&start.control, refusal);
            unclaim(id);
            return;
        }
    };
    let mut storage = vec![Slot::EMPTY; accepted.regions];
    let Some(mut ring) = attach(&start, accepted, &mut storage) else {
        unclaim(id);
        return;
    };
    set_served(id, Some(start.location));
    let ending = ring.serve();
    set_served(id, None);
    ring.finish(ending);
    if ending == Ending::Stopped {
        unclaim(id);
    }
}

/// The first message on the control channel, or `None` if the driver closed
/// it or said nothing in time.
fn receive_hello(control: &Endpoint) -> Option<object::channel::ChannelMessage> {
    let deadline = timer::now_nanos().saturating_add(HELLO_PATIENCE_NANOS);
    loop {
        match control.read(CHANNEL_MAX_BYTES, CHANNEL_MAX_HANDLES, false) {
            Ok(message) => return Some(message),
            Err(ReadError::Empty) => {}
            // A HELLO carries no channel, and nothing but a HELLO is
            // expected: the ring is over before it began.
            Err(ReadError::PeerClosed | ReadError::NeedsTopology | ReadError::TooSmall { .. }) => {
                return None;
            }
        }
        let ready = control.waiters().wait_until_deadline(
            || {
                control
                    .signals()
                    .intersects(Signals::READABLE | Signals::PEER_CLOSED)
            },
            deadline,
        );
        if !ready {
            return None;
        }
    }
}

/// An accepted HELLO, with the objects it carried.
#[derive(Debug)]
struct Accepted {
    /// The device, as HELLO described it.
    device: Device,
    /// Its node name.
    name: ferrix_blkring::DiskName,
    /// The ring VMO.
    ring: Arc<Vmo>,
    /// The data VMO.
    data: Arc<Vmo>,
    /// The driver's port.
    driver_port: Arc<Port>,
    /// How many data regions of `max_sectors` the data VMO holds, capped.
    regions: usize,
}

/// Every check on HELLO that needs no registry, in §6.2's order.
fn check_hello(
    start: &Start,
    message: object::channel::ChannelMessage,
) -> Result<Accepted, Refusal> {
    let accepted = decode_hello(start, &message);
    object::dispose(message.handles.into_iter().map(|(object, _)| object));
    accepted
}

/// [`check_hello`], before the message's handles are let go.
fn decode_hello(
    start: &Start,
    message: &object::channel::ChannelMessage,
) -> Result<Accepted, Refusal> {
    let hello = match Message::decode(&message.bytes) {
        Ok(Message::Hello(hello)) => hello,
        Ok(_) => return Err(Refusal::Malformed),
        Err(error) => return Err(error.refusal()),
    };
    let rights: Vec<Rights> = message.handles.iter().map(|(_, rights)| *rights).collect();
    let accepted = hello.validate(&rights)?;
    let (
        Some((Object::Vmo(ring), _)),
        Some((Object::Vmo(data), _)),
        Some((Object::Port(driver_port), _)),
    ) = (
        message.handles.first(),
        message.handles.get(1),
        message.handles.get(2),
    )
    else {
        return Err(Refusal::Rights);
    };
    if accepted.identity.location != start.location {
        return Err(Refusal::WrongLocation);
    }
    let device = accepted.device;
    if data.len_bytes() < device.data_vmo_size() {
        return Err(Refusal::Device);
    }
    let region_bytes = u64::from(device.max_sectors()) * u64::from(device.block_size());
    let regions = (device.data_vmo_size() / region_bytes).min(MAX_OUTSTANDING);
    Ok(Accepted {
        device,
        name: accepted.identity.name,
        ring: Arc::clone(ring),
        data: Arc::clone(data),
        driver_port: Arc::clone(driver_port),
        regions: usize::try_from(regions).unwrap_or(1),
    })
}

/// Send REFUSED with `refusal`. A driver that has gone hears nothing.
fn refuse(control: &Endpoint, refusal: Refusal) {
    let bytes = Message::Refused(refusal.raw()).encode().as_bytes().to_vec();
    let _ = control.write(bytes, 0, || Ok::<Vec<Transfer>, Infallible>(Vec::new()));
}

/// Take up the ring an accepted HELLO describes, publish its disk and answer
/// READY; or refuse, and answer `None`.
fn attach<'s>(start: &Start, accepted: Accepted, storage: &'s mut [Slot]) -> Option<Serving<'s>> {
    match take_up(start, accepted, storage) {
        Ok(ring) => Some(ring),
        Err(refusal) => {
            refuse(&start.control, refusal);
            None
        }
    }
}

/// [`attach`]'s body: every step that can refuse, then READY.
fn take_up<'s>(
    start: &Start,
    accepted: Accepted,
    storage: &'s mut [Slot],
) -> Result<Serving<'s>, Refusal> {
    let device = accepted.device;
    let ring_held = accepted
        .ring
        .hold(0, accepted.ring.len_pages())
        .map_err(|_| Refusal::Header)?;
    let data_held = accepted
        .data
        .hold(0, accepted.data.len_pages())
        .map_err(|_| Refusal::Device)?;
    let memory = Pages::over(&ring_held, accepted.ring.len_bytes());
    let side = KernelSide::attach(memory, accepted.ring.len_bytes(), device, storage)
        .map_err(|_| Refusal::Header)?;
    let depth = u32::try_from(side.capacity()).unwrap_or(u32::MAX).max(1);
    let limits = Limits::new(
        device.block_size(),
        device.capacity(),
        device.max_sectors(),
        MAX_PARTS,
        depth,
    )
    .map_err(|_| Refusal::Device)?;
    let kernel_port = Port::new();
    let disk = Arc::new(RingDisk::new(device, limits, Arc::clone(&kernel_port)));
    let registration = register_block(
        accepted.name.as_str().as_bytes(),
        VIRTIO_BLK_MAJOR,
        accepted.name.minor(),
        Arc::clone(&disk) as Arc<dyn BlockDevice>,
    )
    .map_err(|refused| match refused {
        BlockRefused::InvalidName => Refusal::Name,
        BlockRefused::NameInUse | BlockRefused::NumberInUse => Refusal::NameInUse,
    })?;
    if served_elsewhere(start.id, start.location) {
        return Err(Refusal::LocationInUse);
    }
    let ready = Message::Ready.encode().as_bytes().to_vec();
    let handed = (Object::Port(Arc::clone(&kernel_port)), Rights::WRITE);
    if start
        .control
        .write(ready, 1, || Ok::<Vec<Transfer>, Infallible>(vec![handed]))
        .is_err()
    {
        return Err(Refusal::Malformed);
    }
    let region_bytes = u64::from(device.max_sectors()) * u64::from(device.block_size());
    let regions = u32::try_from(side.capacity()).unwrap_or(u32::MAX);
    Ok(Serving {
        side,
        disk,
        control: Arc::clone(&start.control),
        kernel_port,
        driver_port: accepted.driver_port,
        data: Pages::over(&data_held, accepted.data.len_bytes()),
        _ring_held: ring_held,
        _data_held: data_held,
        _registration: registration,
        region_bytes,
        free: (0..regions).collect(),
        flying: BTreeMap::new(),
        watching: false,
    })
}

/// Held frames of a VMO, read and written through the direct map.
#[derive(Debug)]
struct Pages {
    /// Each page's direct-map address, in page order.
    virts: Vec<u64>,
    /// Bytes that may be reached.
    bytes: u64,
}

impl Pages {
    /// The pages `held` holds, `bytes` of them reachable.
    fn over(held: &Held, bytes: u64) -> Pages {
        Pages {
            virts: held
                .frames()
                .iter()
                .map(|&frame| mm::direct_map(frame * PAGE_SIZE))
                .collect(),
            bytes,
        }
    }

    /// Where byte `offset` is, if it is inside.
    fn address(&self, offset: u64) -> Option<*mut u8> {
        if offset >= self.bytes {
            return None;
        }
        let page = usize::try_from(offset / PAGE_SIZE).ok()?;
        let virt = self.virts.get(page)?;
        Some((virt + offset % PAGE_SIZE) as *mut u8)
    }

    /// Copy `out.len()` bytes from `offset` into `out`; `false`, with `out`
    /// partly written, if the range runs past the end.
    fn copy_out(&self, offset: u64, out: &mut [u8]) -> bool {
        for (at, byte) in (offset..).zip(out.iter_mut()) {
            let Some(address) = self.address(at) else {
                return false;
            };
            // SAFETY: `address` is inside a frame the ring's `Held` keeps
            // for as long as this `Pages` is used, reached through the direct
            // map. The driver may write the byte at any moment, so it is
            // read volatilely and whatever it holds is taken as data.
            *byte = unsafe { core::ptr::read_volatile(address) };
        }
        true
    }
}

impl RingMemory for Pages {
    fn read_u8(&self, offset: usize) -> u8 {
        let Some(address) = self.address(offset as u64) else {
            return 0;
        };
        // SAFETY: as in `Pages::copy_out`.
        unsafe { core::ptr::read_volatile(address) }
    }

    fn write_u8(&mut self, offset: usize, value: u8) {
        let Some(address) = self.address(offset as u64) else {
            return;
        };
        // SAFETY: `address` is inside a held frame, as in `Pages::copy_out`;
        // the driver's own mapping of it is the only other writer, and the
        // protocol gives each byte one writer.
        unsafe { core::ptr::write_volatile(address, value) };
    }

    fn barrier(&self) {
        // The want-bell handshake (BLOCK-RING.md section 5.1) needs every
        // store before this ordered against every load after it, on the
        // processor the driver reads from: a full fence, which is Rust's
        // `fence` and needs no assembly of its own.
        core::sync::atomic::fence(Ordering::SeqCst);
    }
}

/// A read a caller is waiting on.
#[derive(Debug)]
struct Pending {
    /// What came of it, once it has.
    result: Option<Result<Vec<u8>, Errno>>,
    /// Whether its caller stopped waiting; the answer is then thrown away.
    abandoned: bool,
}

/// What a ring's disk shares between its readers and its task.
#[derive(Debug)]
struct DiskState {
    /// Reads not yet on the ring, and commands on it.
    queue: Queue,
    /// Every read submitted and not yet collected, by its request id.
    reads: BTreeMap<u64, Pending>,
    /// The next request id.
    next_id: u64,
    /// Whether the ring is over.
    ended: bool,
}

/// A block device served by a ring-3 driver through a ring.
pub(crate) struct RingDisk {
    /// The device, as HELLO described it.
    device: Device,
    /// Reads, shared with the ring's task.
    state: SpinLock<DiskState>,
    /// Woken when a read finishes or the ring ends.
    done: WaitQueue,
    /// The ring's completion port, nudged when a read is queued.
    port: Arc<Port>,
}

impl fmt::Debug for RingDisk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RingDisk")
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

impl RingDisk {
    /// A disk for `device`, whose queue takes `limits`, nudging `port`.
    fn new(device: Device, limits: Limits, port: Arc<Port>) -> RingDisk {
        RingDisk {
            device,
            state: SpinLock::new(DiskState {
                queue: Queue::new(limits, Config::default()),
                reads: BTreeMap::new(),
                next_id: 0,
                ended: false,
            }),
            done: WaitQueue::new(),
            port,
        }
    }

    /// Read `count` sectors from `sector` into `out`, which is exactly that
    /// long, as one request.
    fn read_request(&self, sector: u64, count: u32, out: &mut [u8]) -> Result<(), Errno> {
        let id = {
            let mut state = self.state.lock();
            if state.ended {
                return Err(Errno::EIO);
            }
            let id = state.next_id;
            state.next_id = id.wrapping_add(1);
            state
                .queue
                .submit(ticks(), Request::read(id, sector, count))
                .map_err(|_| Errno::EIO)?;
            let _ = state.reads.insert(
                id,
                Pending {
                    result: None,
                    abandoned: false,
                },
            );
            id
        };
        // A full port already holds a nudge the task has not taken.
        let _ = self.port.queue_user(SUBMIT_KEY, [0; 2]);
        // Not interruptible by signals, as a disk read on Linux is not; the
        // ring's end, or the patience, is what ends it.
        let deadline = timer::now_nanos().saturating_add(READ_PATIENCE_NANOS);
        let _ = self
            .done
            .wait_until_deadline(|| self.finished(id), deadline);
        let mut state = self.state.lock();
        match state.reads.remove(&id) {
            Some(Pending {
                result: Some(Ok(bytes)),
                ..
            }) if bytes.len() == out.len() => {
                out.copy_from_slice(&bytes);
                Ok(())
            }
            Some(Pending {
                result: Some(_), ..
            }) => Err(Errno::EIO),
            Some(Pending { result: None, .. }) => {
                let _ = state.reads.insert(
                    id,
                    Pending {
                        result: None,
                        abandoned: true,
                    },
                );
                Err(Errno::EIO)
            }
            None => Err(Errno::EIO),
        }
    }

    /// Whether read `id` has an answer, or the ring is over.
    fn finished(&self, id: u64) -> bool {
        let state = self.state.lock();
        state.ended
            || state
                .reads
                .get(&id)
                .is_none_or(|pending| pending.result.is_some())
    }
}

impl BlockDevice for RingDisk {
    fn read(&self, sector: u64, buf: &mut [u8]) -> Result<(), Errno> {
        let size = usize::try_from(self.device.block_size()).map_err(|_| Errno::EIO)?;
        if size == 0 || buf.is_empty() || !buf.len().is_multiple_of(size) {
            return Err(Errno::EINVAL);
        }
        let most = usize::try_from(self.device.max_sectors())
            .unwrap_or(usize::MAX)
            .saturating_mul(size);
        let mut at = sector;
        for chunk in buf.chunks_mut(most.max(size)) {
            let count = u32::try_from(chunk.len() / size).map_err(|_| Errno::EIO)?;
            self.read_request(at, count, chunk)?;
            at = at.checked_add(u64::from(count)).ok_or(Errno::EIO)?;
        }
        Ok(())
    }

    fn sectors(&self) -> u64 {
        self.device.capacity()
    }

    fn sector_size(&self) -> u32 {
        self.device.block_size()
    }

    fn read_only(&self) -> bool {
        self.device.flags().contains(DeviceFlags::READ_ONLY)
    }
}

/// The time `libs/block`'s queue reckons in: milliseconds.
fn ticks() -> u64 {
    timer::now_nanos() / 1_000_000
}

/// A ring being served, owned by its task.
struct Serving<'s> {
    /// The kernel's side of the ring.
    side: KernelSide<'s, Pages>,
    /// The disk it serves.
    disk: Arc<RingDisk>,
    /// The kernel's end of the control channel.
    control: Arc<Endpoint>,
    /// The completion port: the driver's bells, the control channel's
    /// signals and readers' nudges.
    kernel_port: Arc<Port>,
    /// The driver's port, rung when the driver asks.
    driver_port: Arc<Port>,
    /// The data VMO's pages.
    data: Pages,
    /// The ring VMO's pages, held while the ring is served.
    _ring_held: Held,
    /// The data VMO's pages, held while the ring is served.
    _data_held: Held,
    /// The disk's node, unpublished when the ring is over.
    _registration: BlockRegistration,
    /// Bytes in one data region.
    region_bytes: u64,
    /// Data regions not in use.
    free: Vec<u32>,
    /// The region each submission on the ring uses, by its token.
    flying: BTreeMap<u64, u32>,
    /// Whether a registration on the control channel is waiting to fire.
    watching: bool,
}

impl fmt::Debug for Serving<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Serving")
            .field("side", &self.side)
            .field("flying", &self.flying.len())
            .finish_non_exhaustive()
    }
}

impl Serving<'_> {
    /// Serve until the ring is over, and say how it ended.
    fn serve(&mut self) -> Ending {
        loop {
            if let Some(ending) = self.take_control() {
                return ending;
            }
            self.dispatch();
            if let Some(bell) = self.side.publish() {
                // A full port already holds a bell the driver has not taken.
                let _ = self.driver_port.queue_user(bell.key(), bell.packet().data);
            }
            if self.complete().is_err() {
                return Ending::DriverDied;
            }
            match self.side.prepare_to_sleep() {
                Ok(Wait::Pending(_)) => continue,
                Ok(Wait::Sleep) => {}
                Err(_) => return Ending::DriverDied,
            }
            if self.dispatchable() {
                self.side.woke();
                continue;
            }
            self.sleep();
            self.side.woke();
        }
    }

    /// Read what the driver said on the control channel: `Some` if the ring
    /// is over.
    fn take_control(&mut self) -> Option<Ending> {
        loop {
            match self
                .control
                .read(CHANNEL_MAX_BYTES, CHANNEL_MAX_HANDLES, false)
            {
                Ok(message) => {
                    let stopped = matches!(Message::decode(&message.bytes), Ok(Message::Stopped));
                    object::dispose(message.handles.into_iter().map(|(object, _)| object));
                    if stopped {
                        return Some(Ending::Stopped);
                    }
                }
                Err(ReadError::Empty) => return None,
                Err(
                    ReadError::PeerClosed | ReadError::NeedsTopology | ReadError::TooSmall { .. },
                ) => {
                    return Some(Ending::DriverDied);
                }
            }
        }
    }

    /// Whether a read is queued and a region is free for it.
    fn dispatchable(&self) -> bool {
        !self.free.is_empty() && self.disk.state.lock().queue.queued() > 0
    }

    /// Move queued reads onto the ring while regions and ring slots last.
    fn dispatch(&mut self) {
        let mut failed = Vec::new();
        {
            let mut state = self.disk.state.lock();
            while let Some(&region) = self.free.last() {
                let Some(command) = state.queue.dispatch(ticks()) else {
                    break;
                };
                let token = command.token;
                let submission = Submission::read(
                    token.raw(),
                    command.sector,
                    command.count,
                    u64::from(region) * self.region_bytes,
                );
                match self.side.submit(submission) {
                    Ok(()) => {
                        let _ = self.free.pop();
                        let _ = self.flying.insert(token.raw(), region);
                    }
                    Err(SubmitError::Full) => {
                        let _ = state.queue.requeue(token);
                        break;
                    }
                    Err(_) => failed.push(token),
                }
            }
            for token in failed {
                answer(&mut state, token, Err(Errno::EIO), &Pages::empty(), 0);
            }
        }
        self.disk.done.wake_all();
    }

    /// Take every completion off the ring and answer its reads.
    fn complete(&mut self) -> Result<(), ferrix_blkring::Corruption> {
        let mut answered = false;
        while let Some(completed) = self.side.poll()? {
            let token = completed.submission.id;
            let Some(region) = self.flying.remove(&token) else {
                continue;
            };
            let result = outcome(&completed);
            let offset = u64::from(region) * self.region_bytes;
            answer(
                &mut self.disk.state.lock(),
                Token::from_raw(token),
                result,
                &self.data,
                offset,
            );
            self.free.push(region);
            answered = true;
        }
        if answered {
            self.disk.done.wake_all();
        }
        Ok(())
    }

    /// Sleep on the completion port until something arrives, or a while.
    fn sleep(&mut self) {
        if !self.watching {
            let observer = Observer::new(
                &self.kernel_port,
                CONTROL_KEY,
                Signals::READABLE | Signals::PEER_CLOSED,
            );
            self.watching = self.control.observe(observer).is_ok();
        }
        let deadline = timer::now_nanos().saturating_add(RECHECK_NANOS);
        let port = &self.kernel_port;
        let _ = port
            .waiters()
            .wait_until_deadline(|| !port.is_empty(), deadline);
        while let Some(packet) = port.take() {
            if packet.key == CONTROL_KEY && packet.kind == PACKET_SIGNAL {
                self.watching = false;
            }
        }
    }

    /// The ring is over: fail every outstanding read and wake every reader.
    fn finish(mut self, ending: Ending) {
        let _outstanding = self.side.end(ending).count();
        {
            let mut state = self.disk.state.lock();
            state.ended = true;
            for pending in state.reads.values_mut() {
                if pending.result.is_none() {
                    pending.result = Some(Err(Errno::EIO));
                }
            }
        }
        self.disk.done.wake_all();
    }
}

impl Pages {
    /// Pages holding nothing.
    const fn empty() -> Pages {
        Pages {
            virts: Vec::new(),
            bytes: 0,
        }
    }
}

/// What a completion means for its reads.
fn outcome(completed: &Completed) -> Result<(), Errno> {
    if completed.status == Status::Ok {
        Ok(())
    } else {
        Err(Errno::EIO)
    }
}

/// Finish the command `token` names with `result`, copying each read's bytes
/// out of `data` from `offset` when it succeeded.
fn answer(
    state: &mut DiskState,
    token: Token,
    result: Result<(), Errno>,
    data: &Pages,
    offset: u64,
) {
    let Ok(completion) = state.queue.complete(token, ()) else {
        return;
    };
    let size = u64::from(completion.count.max(1)).max(1);
    let block = completion_block(state, size);
    for part in completion.parts() {
        let Some(pending) = state.reads.get_mut(&part.id.0) else {
            continue;
        };
        let answer = result.and_then(|()| {
            let start = offset + (part.sector - completion.sector) * block;
            let len = usize::try_from(u64::from(part.count) * block).map_err(|_| Errno::EIO)?;
            let mut bytes = vec![0; len];
            if data.copy_out(start, &mut bytes) {
                Ok(bytes)
            } else {
                Err(Errno::EIO)
            }
        });
        if pending.abandoned {
            let _ = state.reads.remove(&part.id.0);
        } else {
            pending.result = Some(answer);
        }
    }
}

/// Bytes in one of the queue's sectors.
fn completion_block(state: &DiskState, _count: u64) -> u64 {
    u64::from(state.queue.limits().logical_block_size())
}
