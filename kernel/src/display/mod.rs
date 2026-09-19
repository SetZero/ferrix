//! The display core: the kernel's end of a display driver's control channel,
//! and the cards it publishes.
//!
//! `docs/DISPLAY.md` §2 is the design and `ferrix-displayctl` checks every
//! word of the protocol. A process holding a device with `MANAGE` asks for a
//! control channel with `DISPLAY_CONTROL_CREATE` and hands its end to a
//! display driver. One kernel task per card then:
//!
//! 1. waits for HELLO and checks it through [`Session::accept`], the device's
//!    location, and the firmware framebuffer's rule (§2.4), answering REFUSED
//!    with the first failure;
//! 2. makes the card VMO, whose ranges are the dumb buffers, publishes the
//!    card, and answers READY with the VMO (`READ | TRANSFER`) and a port;
//! 3. serves: every message the driver sends goes through the session, which
//!    accepts only the replies it is waiting for; the replies it accepts are
//!    queued as [`Event`]s for whoever made the request to collect;
//! 4. ends when the driver closes its end, says STOPPED, or breaks the
//!    protocol, taking the card away.
//!
//! # The kernel never draws
//!
//! The card VMO's pages are filled by the program that maps them. The core
//! only says which range is which buffer, which is shown, and what changed.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::convert::Infallible;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use ferrix_blkring::identity::Location;
use ferrix_bootinfo::PAGE_SIZE;
use ferrix_displayctl::message::{
    Attach, AttachObject, CARD_VMO_RIGHTS, FORMAT, MAX_BUFFER_PAGES, MAX_BYTES, MAX_SCANOUTS,
    Message, Ready, Rect, Refusal, ScanoutMode, Status,
};
use ferrix_displayctl::session::{Event, RequestError, Session};
use ferrix_native_abi::rights::Rights;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::CHANNEL_MAX_HANDLES;

use crate::device::DeviceNode;
use crate::object::channel::{ChannelMessage, Endpoint, ReadError};
use crate::object::port::Port;
use crate::object::{Object, Transfer};
use crate::sched;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;
use crate::timer;
use crate::user::vmo::Vmo;

pub(crate) mod drm;

/// Linux's major number for DRM devices.
pub(crate) const DRM_MAJOR: u32 = 226;

/// The most replies kept that no request is waiting for.
const MAX_EVENTS: usize = 64;

/// The most memory one card's buffers may take, which is the card VMO's size
/// and so also the most an opener of the card can make it commit.
///
/// A 4K mode is 3840 × 2160 × 4 bytes, about 33 MiB a buffer, so a
/// double-buffered 4K screen is 66 MiB; 256 MiB leaves room for a cursor and
/// a third buffer (`docs/DISPLAY.md` §2.1).
pub(crate) const CARD_BYTES: u64 = 256 * 1024 * 1024;

/// How long the core waits for its driver's HELLO.
const HELLO_PATIENCE_NANOS: u64 = 10_000_000_000;

/// How long the task sleeps before looking at the channel of its own accord.
const RECHECK_NANOS: u64 = 50_000_000;

/// How long a request waits for the driver's reply.
const REPLY_PATIENCE_NANOS: u64 = 5_000_000_000;

/// Whether the firmware framebuffer is in memory the frame allocator owns,
/// as the loader said in `BootInfo.framebuffer.reclaimable`.
static RECLAIMABLE_FRAMEBUFFER: AtomicBool = AtomicBool::new(false);

/// Record what the loader said about the firmware framebuffer, once at boot.
pub(crate) fn note_boot_framebuffer(present: bool, reclaimable: bool) {
    RECLAIMABLE_FRAMEBUFFER.store(present && reclaimable, Ordering::Relaxed);
}

/// Why a control channel could not be made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CreateError {
    /// The device already has one.
    InUse,
    /// No memory for the channel, or no stack for the task.
    NoMemory,
}

/// Why a request to a card did not get its answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CardError {
    /// The request is not one the protocol has a state for now.
    Request(RequestError),
    /// The card has no range, or no buffer id, left for the buffer.
    NoRoom,
    /// The driver's channel is full: it is behind, and the request can be
    /// made again once it has read.
    Busy,
    /// The driver is gone or broke the protocol.
    Gone,
    /// The driver did not answer in time.
    TimedOut,
}

/// A control channel waiting for its task.
#[derive(Debug)]
struct Start {
    id: usize,
    control: Arc<Endpoint>,
    device: Arc<DeviceNode>,
    location: Option<Location>,
}

static STARTING: SpinLock<Vec<Start>> = SpinLock::new(Vec::new());
static CLAIMED: SpinLock<Vec<Arc<DeviceNode>>> = SpinLock::new(Vec::new());
static CARDS: SpinLock<Vec<Arc<Card>>> = SpinLock::new(Vec::new());
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
static NEXT_CARD: AtomicU32 = AtomicU32::new(0);

/// A published card.
pub(crate) struct Card {
    /// `card<index>`.
    pub(crate) index: u32,
    /// The pages the dumb buffers are ranges of.
    pub(crate) vmo: Arc<Vmo>,
    modes: [ScanoutMode; MAX_SCANOUTS],
    scanouts: usize,
    control: Arc<Endpoint>,
    state: SpinLock<State>,
    changed: Arc<WaitQueue>,
    /// Whether an open holds the card: one at a time.
    pub(crate) opened: AtomicBool,
}

/// The range of the card VMO a buffer id was given.
#[derive(Clone, Copy, Debug)]
struct Range {
    buffer: u32,
    offset: u64,
    bytes: u64,
}

/// A reply whose request stopped waiting for it, which the card's task drops.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Unwanted {
    Flipped(u64),
    Detached(u32),
}

struct State {
    session: Session,
    /// Accepted replies, for the requests waiting on them.
    events: Vec<Event>,
    gone: bool,
    /// The next buffer id. Ids are never reused on a card, so no reply can
    /// be taken for a later buffer's, and the driver never sees an id again
    /// that the device may still hold pages under.
    next_buffer: u32,
    /// Where never-used space in the card VMO starts.
    next_offset: u64,
    /// Ranges given back, decommitted, sorted by offset, neighbours merged.
    free: Vec<(u64, u64)>,
    /// The range of every buffer id the session may still track.
    ranges: Vec<Range>,
    /// Buffers nobody holds any more that are not detached yet: attaches
    /// that timed out, and buffers busy when they were let go. Each is
    /// detached as soon as the session allows it.
    orphans: Vec<u32>,
    unwanted: Vec<Unwanted>,
    /// A closed open's scanout-off that found the channel full, sent as soon
    /// as there is room, unless a later SCANOUT supersedes it.
    scanout_off: bool,
}

impl State {
    /// A page-aligned range of `bytes`, the first that fits.
    fn allocate(&mut self, bytes: u64) -> Option<u64> {
        if let Some(at) = self.free.iter().position(|&(_, len)| len >= bytes) {
            let (offset, len) = self.free.remove(at);
            if len > bytes {
                self.free.insert(at, (offset + bytes, len - bytes));
            }
            return Some(offset);
        }
        let end = self
            .next_offset
            .checked_add(bytes)
            .filter(|&end| end <= CARD_BYTES)?;
        let offset = self.next_offset;
        self.next_offset = end;
        Some(offset)
    }

    /// Give a decommitted range back.
    fn give_back(&mut self, offset: u64, bytes: u64) {
        let at = self.free.partition_point(|&(start, _)| start < offset);
        self.free.insert(at, (offset, bytes));
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(self.free.len());
        for (start, len) in self.free.drain(..) {
            match merged.last_mut() {
                Some(last) if last.0 + last.1 == start => last.1 += len,
                _ => merged.push((start, len)),
            }
        }
        if let Some(&(start, len)) = merged.last()
            && start + len == self.next_offset
        {
            self.next_offset = start;
            let _ = merged.pop();
        }
        self.free = merged;
    }

    fn take_range(&mut self, buffer: u32) -> Option<Range> {
        let at = self
            .ranges
            .iter()
            .position(|range| range.buffer == buffer)?;
        Some(self.ranges.remove(at))
    }

    /// Detach `buffer`, which nobody holds, now if the session allows it
    /// and as soon as it does otherwise.
    fn let_go(&mut self, control: &Endpoint, buffer: u32) {
        if !control.peer_has_room() {
            if !self.orphans.contains(&buffer) {
                self.orphans.push(buffer);
            }
            return;
        }
        match self.session.detach(buffer) {
            Ok(message) => {
                self.unwanted.push(Unwanted::Detached(buffer));
                let _ = write(control, self, &message);
            }
            Err(RequestError::Busy) => self.orphans.push(buffer),
            Err(_) if self.session.is_attaching(buffer) => self.orphans.push(buffer),
            // Never attached, or already lost: nothing to detach.
            Err(_) => {}
        }
    }

    /// Settle an accepted reply: account for its range, drop it if nobody
    /// waits for it, and detach whatever orphans it frees. Answers a range
    /// to decommit and give back.
    fn settle(&mut self, control: &Endpoint, event: Event) -> Option<Range> {
        let mut freed = None;
        let mut wanted = true;
        match event {
            Event::Attached { buffer, status } => {
                if status != Status::Ok {
                    // The driver unpins what it pinned before it says so.
                    freed = self.take_range(buffer);
                }
                wanted = !self.orphans.contains(&buffer);
            }
            Event::Flipped { sequence, .. } => {
                wanted = !self.forget(Unwanted::Flipped(sequence));
            }
            Event::Detached { buffer, status } => {
                let range = self.take_range(buffer);
                // A refused detach leaves the pages the device's: its range
                // is never handed out again (`docs/DISPLAY.md` §2.2).
                if status == Status::Ok {
                    freed = range;
                }
                wanted = !self.forget(Unwanted::Detached(buffer));
            }
            Event::Stopped => {}
        }
        if wanted {
            if self.events.len() >= MAX_EVENTS {
                let _ = self.events.remove(0);
            }
            self.events.push(event);
        }
        self.retry(control);
        freed
    }

    /// Send what waited for the session or for room in the channel: a
    /// closed open's scanout-off, then the orphans' detaches.
    fn retry(&mut self, control: &Endpoint) {
        if self.scanout_off
            && control.peer_has_room()
            && let Ok(message) = self.session.scanout(0, 0, Rect::default())
        {
            self.scanout_off = false;
            if write(control, self, &message).is_err() {
                return;
            }
        }
        for buffer in core::mem::take(&mut self.orphans) {
            self.let_go(control, buffer);
        }
    }

    fn forget(&mut self, reply: Unwanted) -> bool {
        let before = self.unwanted.len();
        self.unwanted.retain(|&held| held != reply);
        self.unwanted.len() != before
    }
}

/// Send `message` to the driver. Called with the card's state locked, so
/// messages reach the driver in the order the session made them, and only
/// after [`Endpoint::peer_has_room`] said yes: the kernel is the channel's
/// only writer, so the write fails only when the driver is gone, and the
/// card goes with it.
fn write(control: &Endpoint, state: &mut State, message: &Message) -> Result<(), CardError> {
    let bytes = message.encode().as_bytes().to_vec();
    if control
        .write(bytes, 0, || Ok::<Vec<Transfer>, Infallible>(Vec::new()))
        .is_err()
    {
        state.gone = true;
        return Err(CardError::Gone);
    }
    Ok(())
}

impl core::fmt::Debug for Card {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Card")
            .field("index", &self.index)
            .field("scanouts", &self.scanouts)
            .finish_non_exhaustive()
    }
}

impl Card {
    /// Each scanout's preferred mode, as the driver reported it.
    pub(crate) fn modes(&self) -> &[ScanoutMode] {
        self.modes.get(..self.scanouts).unwrap_or(&[])
    }

    /// Whether the driver is gone.
    pub(crate) fn is_gone(&self) -> bool {
        self.state.lock().gone
    }

    /// What `stat` says of the card: a character device whose size is the
    /// card VMO's, so a mapping's bounds check holds (§2.1).
    pub(crate) fn metadata(&self) -> ferrix_vfs::Metadata {
        use ferrix_vfs::{FileType, Metadata, Timespec};
        Metadata {
            ino: (1u64 << 40) + u64::from(self.index),
            kind: FileType::CharDevice,
            // Owner and group only, as Linux's `video` group has it: whoever
            // opens the card can map every range of it.
            permissions: 0o660,
            nlink: 1,
            uid: 0,
            gid: 0,
            size: CARD_BYTES,
            rdev: ferrix_vfs::initramfs::makedev(DRM_MAJOR, self.index),
            blocks: 0,
            block_size: 4096,
            atime: Timespec::default(),
            mtime: Timespec::default(),
            ctime: Timespec::default(),
        }
    }

    /// Run `make` on the session and send the message it makes, with the
    /// state locked throughout.
    fn send<T>(
        &self,
        make: impl FnOnce(&mut State) -> Result<(Message, T), CardError>,
    ) -> Result<T, CardError> {
        let mut state = self.state.lock();
        if state.gone || self.control.peer_closed() {
            return Err(CardError::Gone);
        }
        // Before the session commits to the request, which it cannot take
        // back: a driver that is behind makes the caller try again, not the
        // card go (os-02's review).
        if !self.control.peer_has_room() {
            return Err(CardError::Busy);
        }
        let (message, made) = make(&mut state)?;
        write(&self.control, &mut state, &message)?;
        drop(state);
        Ok(made)
    }

    /// Wait for the event `wanted` picks out, and take it. On a timeout,
    /// `abandon` runs with the state locked, so the reply is dealt with
    /// whenever it comes.
    fn collect(
        &self,
        wanted: impl Fn(&Event) -> bool,
        abandon: impl FnOnce(&mut State),
    ) -> Result<Event, CardError> {
        let deadline = timer::now_nanos().saturating_add(REPLY_PATIENCE_NANOS);
        let _ = self.changed.wait_until_deadline(
            || {
                let state = self.state.lock();
                state.gone || state.events.iter().any(&wanted)
            },
            deadline,
        );
        let mut state = self.state.lock();
        if let Some(at) = state.events.iter().position(&wanted) {
            return Ok(state.events.remove(at));
        }
        if state.gone {
            return Err(CardError::Gone);
        }
        abandon(&mut state);
        Err(CardError::TimedOut)
    }

    /// Give a new buffer of `width` × `height` rows of `stride` bytes a
    /// range and an id, ATTACH it and wait for ATTACHED: the id, the range's
    /// offset and how the attach went.
    pub(crate) fn attach(
        &self,
        width: u32,
        height: u32,
        stride: u32,
    ) -> Result<(u32, u64, Status), CardError> {
        let pixels = u64::from(stride) * u64::from(height);
        let bytes = pixels
            .checked_next_multiple_of(PAGE_SIZE)
            .filter(|&bytes| bytes > 0 && bytes <= MAX_BUFFER_PAGES * PAGE_SIZE)
            .ok_or(CardError::NoRoom)?;
        let (buffer, offset) = self.send(|state| {
            let buffer = state.next_buffer;
            let next = buffer.checked_add(1).ok_or(CardError::NoRoom)?;
            let offset = state.allocate(bytes).ok_or(CardError::NoRoom)?;
            let attach = Attach {
                buffer,
                format: FORMAT,
                offset,
                length: bytes,
                width,
                height,
                stride,
            };
            match state.session.attach(attach) {
                Ok(message) => {
                    state.next_buffer = next;
                    state.ranges.push(Range {
                        buffer,
                        offset,
                        bytes,
                    });
                    Ok((message, (buffer, offset)))
                }
                Err(error) => {
                    // Never sent, so never pinned, and nothing wrote to it.
                    state.give_back(offset, bytes);
                    Err(CardError::Request(error))
                }
            }
        })?;
        let event = self.collect(
            |event| matches!(event, Event::Attached { buffer: b, .. } if *b == buffer),
            |state| state.orphans.push(buffer),
        )?;
        match event {
            Event::Attached { status, .. } => Ok((buffer, offset, status)),
            _ => Err(CardError::Gone),
        }
    }

    /// Give an object the render core made a buffer id, `ATTACH_OBJ` it and
    /// wait for ATTACHED: the id and how the attach went.
    ///
    /// The other way to make a buffer, and the difference is where the
    /// pixels are. An ATTACH's are a range of the card VMO, which the driver
    /// pins for the device; these are the device's own already, so there is
    /// no range to allocate, nothing to pin, and nothing to send when the
    /// frame changes -- which is the whole of why it exists. What the driver
    /// is handed is the renderer's object id, which is the device's name for
    /// the resource and no business of this core's beyond passing it on.
    ///
    /// # Errors
    ///
    /// [`CardError`], including the driver's own refusal.
    pub(crate) fn attach_object(
        &self,
        object: u32,
        width: u32,
        height: u32,
        stride: u32,
    ) -> Result<(u32, Status), CardError> {
        let buffer = self.send(|state| {
            let buffer = state.next_buffer;
            let next = buffer.checked_add(1).ok_or(CardError::NoRoom)?;
            let attach = AttachObject {
                buffer,
                object,
                format: FORMAT,
                width,
                height,
                stride,
            };
            let message = state
                .session
                .attach_object(attach)
                .map_err(CardError::Request)?;
            state.next_buffer = next;
            Ok((message, buffer))
        })?;
        let event = self.collect(
            |event| matches!(event, Event::Attached { buffer: b, .. } if *b == buffer),
            |state| state.orphans.push(buffer),
        )?;
        match event {
            Event::Attached { status, .. } => Ok((buffer, status)),
            _ => Err(CardError::Gone),
        }
    }

    /// Show `rect` of `buffer` on `scanout`, or turn it off with buffer 0.
    pub(crate) fn scanout(&self, scanout: u32, buffer: u32, rect: Rect) -> Result<(), CardError> {
        self.send(|state| {
            state.scanout_off = false;
            let message = state
                .session
                .scanout(scanout, buffer, rect)
                .map_err(CardError::Request)?;
            Ok((message, ()))
        })
    }

    /// FLUSH `rect` of `buffer` and wait for FLIPPED.
    pub(crate) fn flush(&self, buffer: u32, rect: Rect) -> Result<Status, CardError> {
        let sequence = self.send(|state| {
            let message = state
                .session
                .flush(buffer, rect)
                .map_err(CardError::Request)?;
            let Message::Flush { sequence, .. } = message else {
                return Err(CardError::Gone);
            };
            Ok((message, sequence))
        })?;
        let event = self.collect(
            |event| matches!(event, Event::Flipped { sequence: s, .. } if *s == sequence),
            |state| state.unwanted.push(Unwanted::Flipped(sequence)),
        )?;
        match event {
            Event::Flipped { status, .. } => Ok(status),
            _ => Err(CardError::Gone),
        }
    }

    /// Let go of `buffers` without waiting: detached now, or as soon as the
    /// session allows. Their ranges come back once the device gives them up.
    pub(crate) fn let_go(&self, buffers: &[u32]) {
        let mut state = self.state.lock();
        if state.gone {
            return;
        }
        for &buffer in buffers {
            state.let_go(&self.control, buffer);
        }
    }

    /// Take the screen off and let go of `buffers`: what closing an open
    /// does.
    pub(crate) fn release(&self, buffers: &[u32]) {
        let mut state = self.state.lock();
        if state.gone {
            return;
        }
        if !self.control.peer_has_room() {
            state.scanout_off = true;
        } else if let Ok(message) = state.session.scanout(0, 0, Rect::default())
            && write(&self.control, &mut state, &message).is_err()
        {
            return;
        }
        for &buffer in buffers {
            state.let_go(&self.control, buffer);
        }
    }
}

/// The numbers of every published card, lowest first.
pub(crate) fn card_indices() -> Vec<u32> {
    let mut indices: Vec<u32> = CARDS.lock().iter().map(|card| card.index).collect();
    indices.sort_unstable();
    indices
}

/// The card published as `card<index>`, if its driver is serving it.
pub(crate) fn card(index: u32) -> Option<Arc<Card>> {
    CARDS
        .lock()
        .iter()
        .find(|card| card.index == index)
        .map(Arc::clone)
}

/// Make the control channel for `node` and start its task; answer the
/// driver's end.
pub(crate) fn create(node: &Arc<DeviceNode>) -> Result<Arc<Endpoint>, CreateError> {
    let (kernel_end, driver_end) = Endpoint::pair().ok_or(CreateError::NoMemory)?;
    {
        let mut claimed = CLAIMED.lock();
        if claimed.iter().any(|held| Arc::ptr_eq(held, node)) {
            return Err(CreateError::InUse);
        }
        claimed.push(Arc::clone(node));
    }
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    STARTING.lock().push(Start {
        id,
        control: kernel_end,
        device: Arc::clone(node),
        location: crate::block_ring::location_of(node),
    });
    if sched::spawn("display", run, id, ferrix_sched::NICE_0_WEIGHT).is_err() {
        let _ = take_start(id);
        unclaim(node);
        return Err(CreateError::NoMemory);
    }
    Ok(driver_end)
}

fn take_start(id: usize) -> Option<Start> {
    let mut starting = STARTING.lock();
    let at = starting.iter().position(|start| start.id == id)?;
    Some(starting.remove(at))
}

fn unclaim(node: &Arc<DeviceNode>) {
    CLAIMED.lock().retain(|held| !Arc::ptr_eq(held, node));
}

/// One card's task.
fn run(id: usize) {
    let Some(start) = take_start(id) else {
        return;
    };
    if let Some(card) = take_up(&start) {
        serve(&card);
        CARDS.lock().retain(|held| !Arc::ptr_eq(held, &card));
        crate::console::println!("  display  card{} is gone", card.index);
    }
    unclaim(&start.device);
}

/// The next message on the control channel, waiting up to `deadline`.
/// `None` when the channel closed or nothing came in time.
fn receive(control: &Endpoint, deadline: u64) -> Option<ChannelMessage> {
    loop {
        match control.read(MAX_BYTES, CHANNEL_MAX_HANDLES, false) {
            Ok(message) => return Some(message),
            Err(ReadError::Empty) => {}
            Err(_) => return None,
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

fn refuse(control: &Endpoint, refusal: Refusal) {
    let bytes = Message::Refused(refusal).encode().as_bytes().to_vec();
    let _ = control.write(bytes, 0, || Ok::<Vec<Transfer>, Infallible>(Vec::new()));
}

/// Take a HELLO, refusing it or answering READY, and publish the card.
fn take_up(start: &Start) -> Option<Arc<Card>> {
    let deadline = timer::now_nanos().saturating_add(HELLO_PATIENCE_NANOS);
    let message = receive(&start.control, deadline)?;
    match accept(start, &message) {
        Ok(card) => Some(card),
        Err(refusal) => {
            crate::console::println!("  display  a driver was refused: {refusal}");
            refuse(&start.control, refusal);
            None
        }
    }
}

fn accept(start: &Start, message: &ChannelMessage) -> Result<Arc<Card>, Refusal> {
    let Ok(Message::Hello(hello)) = Message::decode(&message.bytes) else {
        return Err(Refusal::Malformed);
    };
    let rights: Vec<Rights> = message.handles.iter().map(|(_, rights)| *rights).collect();
    let session = Session::accept(&hello, &rights, CARD_BYTES)?;
    let Some((Object::Port(_driver_port), _)) = message.handles.first() else {
        return Err(Refusal::Rights);
    };
    if start.location.map(Location::raw) != Some(hello.location) {
        return Err(Refusal::WrongLocation);
    }
    if RECLAIMABLE_FRAMEBUFFER.load(Ordering::Relaxed) {
        return Err(Refusal::Framebuffer);
    }

    let vmo = Vmo::new_anonymous(CARD_BYTES / PAGE_SIZE);
    let core_port = Port::new();
    let index = NEXT_CARD.fetch_add(1, Ordering::Relaxed);
    let card = Arc::new(Card {
        index,
        vmo: Arc::clone(&vmo),
        modes: hello.modes,
        scanouts: usize::from(hello.scanouts),
        control: Arc::clone(&start.control),
        state: SpinLock::new(State {
            session,
            events: Vec::new(),
            gone: false,
            next_buffer: 1,
            next_offset: 0,
            free: Vec::new(),
            ranges: Vec::new(),
            orphans: Vec::new(),
            unwanted: Vec::new(),
            scanout_off: false,
        }),
        changed: Arc::new(WaitQueue::new()),
        opened: AtomicBool::new(false),
    });

    // Published before READY goes out, as the rings do: devmgr kills a driver
    // that has not published by the time it reports. A READY that cannot be
    // written is not taken back: the channel failing means the driver is
    // gone, and devmgr hears of its death and quiesces the device.
    if let Some(location) = start.location {
        crate::devmgr::published(location);
    }
    let ready = Message::Ready(Ready {
        card: index,
        card_bytes: CARD_BYTES,
    })
    .encode()
    .as_bytes()
    .to_vec();
    let handed = vec![
        (Object::Vmo(vmo), CARD_VMO_RIGHTS),
        (Object::Port(core_port), Rights::WRITE),
    ];
    start
        .control
        .write(ready, 2, || Ok::<Vec<Transfer>, Infallible>(handed))
        .map_err(|_| Refusal::Malformed)?;

    CARDS.lock().push(Arc::clone(&card));
    // What the card is, before what it shows: a line a person reading a
    // boot log can tell a GPU from a framebuffer by, and the one thing
    // `docs/GPU.md`'s Path A can be checked against from outside the guest.
    crate::console::println!(
        "  display  card{index} {}",
        if hello.virgl {
            alloc::format!(
                "is a 3D card: virgl, {} capability set{}, the first #{} of {} bytes",
                hello.capsets,
                if hello.capsets == 1 { "" } else { "s" },
                hello.capset,
                hello.capset_bytes
            )
        } else {
            alloc::string::String::from("is a scanout: no 3D")
        }
    );
    for (scanout, mode) in card.modes().iter().enumerate() {
        crate::console::println!(
            "  display  card{index} scanout {scanout}: {}x{}{}",
            mode.width,
            mode.height,
            if mode.enabled {
                ""
            } else {
                ", nothing attached"
            }
        );
    }
    Ok(card)
}

/// Take a range back from a buffer the driver let go of. Nothing the next
/// buffer's program maps may show what the last one drew, so its pages are
/// decommitted. A page still pinned is skipped by the decommit: the driver
/// replied before it unpinned, which a correct one never does, and the range
/// is kept out of use for good, as a refused detach's is (os-02's review).
fn reclaim(card: &Card, range: Range) {
    let (first, pages) = (range.offset / PAGE_SIZE, range.bytes / PAGE_SIZE);
    let _ = card.vmo.decommit_range(first, pages);
    if card.vmo.holds_any(first, pages) {
        crate::console::println!(
            "  display  card{}: buffer {} is still pinned after its driver let go; \
             {} pages kept out of use",
            card.index,
            range.buffer,
            pages
        );
        return;
    }
    card.state.lock().give_back(range.offset, range.bytes);
}

/// Serve the card until its driver goes.
fn serve(card: &Card) {
    loop {
        if card.state.lock().gone {
            break;
        }
        let deadline = timer::now_nanos().saturating_add(RECHECK_NANOS);
        let message = match card.control.read(MAX_BYTES, CHANNEL_MAX_HANDLES, false) {
            Ok(message) => message,
            Err(ReadError::Empty) => {
                if card.control.signals().intersects(Signals::PEER_CLOSED) {
                    break;
                }
                card.state.lock().retry(&card.control);
                let _ = card.control.waiters().wait_until_deadline(
                    || {
                        card.control
                            .signals()
                            .intersects(Signals::READABLE | Signals::PEER_CLOSED)
                    },
                    deadline,
                );
                continue;
            }
            Err(_) => break,
        };
        crate::object::dispose(message.handles.into_iter().map(|(object, _)| object));
        let Ok(decoded) = Message::decode(&message.bytes) else {
            refuse(&card.control, Refusal::Protocol);
            break;
        };
        let accepted = card.state.lock().session.receive(&decoded);
        match accepted {
            Ok(Event::Stopped) => break,
            Ok(event) => {
                let freed = card.state.lock().settle(&card.control, event);
                if let Some(range) = freed {
                    reclaim(card, range);
                }
                card.changed.wake_all();
            }
            Err(refusal) => {
                crate::console::println!("  display  card{}: {refusal}", card.index);
                refuse(&card.control, refusal);
                break;
            }
        }
    }
    card.state.lock().gone = true;
    card.changed.wake_all();
}
