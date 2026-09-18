//! The render core: the kernel's half of `libs/renderctl`, and what will
//! sit under `/dev/dri/renderD<N>`.
//!
//! `docs/GPU.md` §3.3 is the specification and the reason this is a core of
//! its own rather than part of the display's. The two are different
//! conversations about the same card -- one about what is on the screen and
//! one about what the GPU computes -- and only this one has a second
//! implementation coming (§4's NVIDIA driver). So what is here knows a
//! renderer's *conversation* and nothing about virtio: which contexts and
//! objects exist, which fences are outstanding, and what the driver is
//! called. A command buffer's bytes go through a VMO neither side reads on
//! the way past.
//!
//! The shape is `crate::display`'s, because the problems are the same: one
//! control channel per device made by a native call, a task per device, a
//! HELLO within its patience or the driver is refused, and `libs/renderctl`'s
//! [`Session`] judging every reply so that a driver which answers a question
//! nobody asked is caught before the kernel acts on it.
//!
//! # The renderer outlives its proof
//!
//! A [`Renderer`] is published once its driver has answered, and its task
//! then *serves* it: every reply is taken from the channel, judged by the
//! session and left where the request waiting for it will find it. That is
//! what lets a request be made from a caller's own task -- the node's
//! ioctls, when they are written -- rather than only from this one. The
//! same division as the display's: [`Renderer::request`] sends with the
//! state locked, [`Renderer::collect`] sleeps on the wait queue, and no spin
//! lock is ever held across the sleep.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::convert::Infallible;
use core::sync::atomic::{AtomicUsize, Ordering};

use ferrix_blkring::identity::Location;
use ferrix_bootinfo::PAGE_SIZE;
use ferrix_native_abi::rights::Rights;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::CHANNEL_MAX_HANDLES;
use ferrix_renderctl::message::{
    MAX_BYTES, Message, Ready, Refusal, Status, VERSION, WORK_VMO_RIGHTS, Work, flags,
};
use ferrix_renderctl::session::{Event, RequestError, Session};

use crate::device::DeviceNode;
use crate::object::channel::{ChannelMessage, Endpoint, ReadError};
use crate::object::port::Port;
use crate::object::{Object, Transfer};
use crate::sched;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;
use crate::timer;
use crate::user::vmo::Vmo;

/// How much work VMO one renderer gets: command buffers and object
/// descriptions on their way to the device.
///
/// A frame's worth of virgl is tens of kilobytes; a megabyte is room for
/// many in flight without being memory a guest notices.
pub(crate) const WORK_BYTES: u64 = 1024 * 1024;

/// How long the core waits for its driver's HELLO.
const HELLO_PATIENCE_NANOS: u64 = 10_000_000_000;

/// How long a request waits for the driver's reply.
const REPLY_PATIENCE_NANOS: u64 = 5_000_000_000;

/// How long the task sleeps before looking at the channel of its own accord,
/// so that a renderer whose driver went quiet still notices it is gone.
const RECHECK_NANOS: u64 = 1_000_000_000;

/// Why a control channel could not be made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CreateError {
    /// The device already has one.
    InUse,
    /// No memory for the channel, or no stack for the task.
    NoMemory,
}

/// Why a request on a published renderer could not be answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[expect(
    dead_code,
    reason = "the node that answers with these is the next step"
)]
pub(crate) enum RenderError {
    /// The request is not one the protocol has a state for now.
    Request(RequestError),
    /// The driver's channel is full: it is behind, and the request can be
    /// made again once it has read.
    Busy,
    /// The driver is gone or broke the protocol.
    Gone,
    /// The driver did not answer in time.
    TimedOut,
}

/// A device whose render channel has been made, waiting for its task.
struct Start {
    id: usize,
    control: Arc<Endpoint>,
    device: Arc<DeviceNode>,
    location: Option<Location>,
}

/// What the core knows about one renderer's conversation.
struct State {
    session: Session,
    /// Accepted replies, for the requests waiting on them.
    events: Vec<Event>,
    gone: bool,
}

/// A published renderer: `/dev/dri/renderD<index>`.
pub(crate) struct Renderer {
    /// `renderD<index>`, which is 128 upwards as Linux numbers render nodes.
    pub(crate) index: u32,
    /// The work VMO, whose ranges carry object descriptions and command
    /// buffers. The core hands out ranges of it and never reads what is
    /// written there (`docs/GPU.md` §3.3).
    #[expect(dead_code, reason = "the node that hands out ranges is the next step")]
    pub(crate) work: Arc<Vmo>,
    control: Arc<Endpoint>,
    state: SpinLock<State>,
    changed: Arc<WaitQueue>,
}

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
static NEXT_RENDERER: AtomicUsize = AtomicUsize::new(128);
static STARTING: SpinLock<Vec<Start>> = SpinLock::new(Vec::new());
static CLAIMED: SpinLock<Vec<Arc<DeviceNode>>> = SpinLock::new(Vec::new());
static RENDERERS: SpinLock<Vec<Arc<Renderer>>> = SpinLock::new(Vec::new());

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
    if sched::spawn("render", run, id, ferrix_sched::NICE_0_WEIGHT).is_err() {
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

/// The numbers of every published renderer, lowest first.
#[expect(dead_code, reason = "the node that lists these is the next step")]
pub(crate) fn renderer_indices() -> Vec<u32> {
    let mut indices: Vec<u32> = RENDERERS
        .lock()
        .iter()
        .map(|renderer| renderer.index)
        .collect();
    indices.sort_unstable();
    indices
}

/// The renderer published as `renderD<index>`, if its driver is serving it.
#[expect(dead_code, reason = "the node that opens this is the next step")]
pub(crate) fn renderer(index: u32) -> Option<Arc<Renderer>> {
    RENDERERS
        .lock()
        .iter()
        .find(|renderer| renderer.index == index)
        .map(Arc::clone)
}

/// One renderer's task.
///
/// The proof runs before the renderer is published, so nothing can open the
/// node and take the session while the core is still asking its own
/// questions; then the renderer is served until its driver goes.
fn run(id: usize) {
    let Some(start) = take_start(id) else {
        return;
    };
    if let Some(renderer) = take_up(&start) {
        prove(&renderer);
        RENDERERS.lock().push(Arc::clone(&renderer));
        serve(&renderer);
        RENDERERS
            .lock()
            .retain(|held| !Arc::ptr_eq(held, &renderer));
        crate::console::println!("  render   renderD{} is gone", renderer.index);
    }
    unclaim(&start.device);
}

/// The next message on the control channel, waiting up to `deadline`.
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

fn send(control: &Endpoint, message: &Message) -> bool {
    let bytes = message.encode().as_bytes().to_vec();
    control
        .write(bytes, 0, || Ok::<Vec<Transfer>, Infallible>(Vec::new()))
        .is_ok()
}

fn refuse(control: &Endpoint, refusal: Refusal) {
    let _ = send(control, &Message::Refused(refusal));
}

/// Take a HELLO, refusing it or answering READY.
fn take_up(start: &Start) -> Option<Arc<Renderer>> {
    let deadline = timer::now_nanos().saturating_add(HELLO_PATIENCE_NANOS);
    let message = receive(&start.control, deadline)?;
    match accept(start, &message) {
        Ok(renderer) => Some(renderer),
        Err(refusal) => {
            crate::console::println!("  render   a driver was refused: {refusal}");
            refuse(&start.control, refusal);
            None
        }
    }
}

fn accept(start: &Start, message: &ChannelMessage) -> Result<Arc<Renderer>, Refusal> {
    let Some(Message::Hello(hello)) = Message::decode(&message.bytes) else {
        return Err(Refusal::Malformed);
    };
    let rights: Vec<Rights> = message.handles.iter().map(|(_, rights)| *rights).collect();
    let session = Session::accept(&hello, &rights, WORK_BYTES)?;
    let Some((Object::Port(_driver_port), _)) = message.handles.first() else {
        return Err(Refusal::Rights);
    };
    if start.location.map(Location::raw) != Some(hello.location) {
        return Err(Refusal::WrongLocation);
    }
    let index = NEXT_RENDERER.fetch_add(1, Ordering::Relaxed);
    let index = u32::try_from(index).unwrap_or(128);
    // READY carries the two handles `Ready::HANDLE_RIGHTS` fixes: the work
    // VMO, which is where an object's description and a command buffer live
    // on their way to the device, and the core's port, which is how a driver
    // will say a fence has passed. The VMO is the core's: it hands out ranges
    // of it and never reads what is written there (`docs/GPU.md` §3.3), and
    // it keeps its own reference because the node writes those ranges.
    let work = Vmo::new_anonymous(WORK_BYTES / PAGE_SIZE);
    let core_port = Port::new();
    let renderer = Arc::new(Renderer {
        index,
        work: Arc::clone(&work),
        control: Arc::clone(&start.control),
        state: SpinLock::new(State {
            session,
            events: Vec::new(),
            gone: false,
        }),
        changed: Arc::new(WaitQueue::new()),
    });
    let ready = Message::Ready(Ready {
        renderer: index,
        work_bytes: WORK_BYTES,
    })
    .encode()
    .as_bytes()
    .to_vec();
    let handed = vec![
        (Object::Vmo(work), WORK_VMO_RIGHTS),
        (Object::Port(core_port), Rights::WRITE),
    ];
    start
        .control
        .write(ready, 2, || Ok::<Vec<Transfer>, Infallible>(handed))
        .map_err(|_| Refusal::Malformed)?;
    let state = renderer.state.lock();
    crate::console::println!(
        "  render   renderD{index} is `{}`, version {VERSION}, capset {}, objects to {} MiB",
        state.session.name(),
        state.session.capset(),
        state.session.object_limit() / (1024 * 1024),
    );
    drop(state);
    Ok(renderer)
}

impl Renderer {
    /// Whether the driver is gone.
    #[expect(dead_code, reason = "the node that asks is the next step")]
    pub(crate) fn is_gone(&self) -> bool {
        self.state.lock().gone
    }

    /// What `stat` says of the render node: a character device of major 226
    /// whose minor is its number, as Linux numbers `renderD128` 226:128.
    ///
    /// Its size is zero: unlike a card, whose VMO is what `MODE_MAP_DUMB`'s
    /// offsets are into, nothing is mapped through this inode yet. An object
    /// has no backing to map until the protocol carries one.
    #[expect(dead_code, reason = "the node that reports this is the next step")]
    pub(crate) fn metadata(&self) -> ferrix_vfs::Metadata {
        use ferrix_vfs::{FileType, Metadata, Timespec};
        Metadata {
            ino: (1u64 << 41) + u64::from(self.index),
            kind: FileType::CharDevice,
            // Owner and group only, as Linux's `render` group has it.
            permissions: 0o660,
            nlink: 1,
            uid: 0,
            gid: 0,
            size: 0,
            rdev: ferrix_vfs::initramfs::makedev(crate::display::DRM_MAJOR, self.index),
            blocks: 0,
            block_size: 4096,
            atime: Timespec::default(),
            mtime: Timespec::default(),
            ctime: Timespec::default(),
        }
    }

    /// Run `make` on the session and send the message it makes, with the
    /// state locked throughout.
    #[expect(dead_code, reason = "the node that makes requests is the next step")]
    fn request<T>(
        &self,
        make: impl FnOnce(&mut State) -> Result<(Message, T), RenderError>,
    ) -> Result<T, RenderError> {
        let mut state = self.state.lock();
        if state.gone || self.control.peer_closed() {
            return Err(RenderError::Gone);
        }
        // Before the session commits to the request, which it cannot take
        // back: a driver that is behind makes the caller try again, rather
        // than leaving the session waiting for a reply to a message that was
        // never written.
        if !self.control.peer_has_room() {
            return Err(RenderError::Busy);
        }
        let (message, made) = make(&mut state)?;
        if !send(&self.control, &message) {
            return Err(RenderError::Gone);
        }
        drop(state);
        Ok(made)
    }

    /// Wait for the event `wanted` picks out, and take it. On a timeout,
    /// `abandon` runs with the state locked, so the reply is dealt with
    /// whenever it comes.
    #[expect(dead_code, reason = "the node that waits for replies is the next step")]
    fn collect(
        &self,
        wanted: impl Fn(&Event) -> bool,
        abandon: impl FnOnce(&mut State),
    ) -> Result<Event, RenderError> {
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
            return Err(RenderError::Gone);
        }
        abandon(&mut state);
        Err(RenderError::TimedOut)
    }
}

/// Take every reply the driver sends, judge it and leave it where the
/// request waiting for it will find it, until the driver goes.
fn serve(renderer: &Renderer) {
    loop {
        if renderer.state.lock().gone {
            break;
        }
        let deadline = timer::now_nanos().saturating_add(RECHECK_NANOS);
        let message = match renderer.control.read(MAX_BYTES, CHANNEL_MAX_HANDLES, false) {
            Ok(message) => message,
            Err(ReadError::Empty) => {
                if renderer.control.signals().intersects(Signals::PEER_CLOSED) {
                    break;
                }
                let _ = renderer.control.waiters().wait_until_deadline(
                    || {
                        renderer
                            .control
                            .signals()
                            .intersects(Signals::READABLE | Signals::PEER_CLOSED)
                    },
                    deadline,
                );
                continue;
            }
            Err(_) => break,
        };
        // A reply carries no handles; anything sent with one is disposed of
        // rather than leaked, as the display's task does.
        crate::object::dispose(message.handles.into_iter().map(|(object, _)| object));
        let Some(decoded) = Message::decode(&message.bytes) else {
            refuse(&renderer.control, Refusal::Protocol);
            break;
        };
        let accepted = renderer.state.lock().session.receive(&decoded);
        match accepted {
            Ok(Event::Stopped) => break,
            Ok(event) => {
                renderer.state.lock().events.push(event);
                renderer.changed.wake_all();
            }
            Err(refusal) => {
                crate::console::println!("  render   renderD{}: {refusal}", renderer.index);
                refuse(&renderer.control, refusal);
                break;
            }
        }
    }
    renderer.state.lock().gone = true;
    renderer.changed.wake_all();
}

/// Make one context and take it away again, which is the whole of what the
/// core does with a renderer until the node above it exists.
///
/// It is not ceremony: it is the only thing that says the conversation
/// works end to end against a real device, and it is what the boot log
/// reports. `docs/GPU.md` §3.2 -- the commands under it were tested against
/// QEMU's header, and this is them against QEMU.
///
/// It runs before the renderer is published, so it has the session to
/// itself and can read the channel directly rather than through [`serve`].
fn prove(renderer: &Renderer) {
    let context = 1;
    let capset = renderer.state.lock().session.capset();
    let Ok(ask) = renderer.state.lock().session.make_context(context, capset) else {
        return;
    };
    match exchange(renderer, &ask) {
        Some(Event::ContextMade {
            status: Status::Ok, ..
        }) => {
            crate::console::println!(
                "  render   renderD{} made context {context} on the device",
                renderer.index
            );
            prove_object(renderer, context);
        }
        Some(Event::ContextMade { status, .. }) => {
            crate::console::println!("  render   the device refused a context: {status:?}");
        }
        _ => {}
    }
}

/// How big the object the proof asks for is: one page, which is enough to be
/// a real resource on the device and small enough to cost nothing.
const PROOF_OBJECT_BYTES: u64 = PAGE_SIZE;

/// Make an object in `context` and take it away again.
///
/// This is `prove`'s second half and the rest of `docs/GPU.md` step 1:
/// `RESOURCE_CREATE_3D` against the device rather than against QEMU's
/// header. The description is **empty** -- `Work { at: 0, len: 0 }` -- and
/// that is the point of the seam: the core says how many bytes it wants and
/// nothing about what the resource is, and the driver, which is the only
/// side that knows virgl, chooses the target, format and bind words. A core
/// that wrote them would be a core an NVIDIA driver could not reuse.
fn prove_object(renderer: &Renderer, context: u32) {
    let object = 1;
    let Ok(ask) = renderer.state.lock().session.make_object(
        object,
        context,
        PROOF_OBJECT_BYTES,
        flags::TO_DEVICE,
        Work { at: 0, len: 0 },
    ) else {
        return;
    };
    match exchange(renderer, &ask) {
        Some(Event::ObjectMade {
            status: Status::Ok, ..
        }) => {
            crate::console::println!(
                "  render   renderD{} made object {object} of {PROOF_OBJECT_BYTES} bytes in context {context}",
                renderer.index
            );
        }
        Some(Event::ObjectMade { status, .. }) => {
            crate::console::println!("  render   the device refused an object: {status:?}");
            return;
        }
        _ => return,
    }
    let Ok(ask) = renderer.state.lock().session.drop_object(object) else {
        return;
    };
    match exchange(renderer, &ask) {
        Some(Event::ObjectGone {
            status: Status::Ok, ..
        }) => {
            crate::console::println!(
                "  render   renderD{} gave object {object} back",
                renderer.index
            );
        }
        Some(Event::ObjectGone { status, .. }) => {
            // The device may still hold the backing, so the core keeps the
            // memory rather than handing it out again: `docs/DISPLAY.md`
            // §2.2's rule, which belongs to the core and not to virtio.
            crate::console::println!("  render   the device kept an object: {status:?}");
        }
        _ => {}
    }
}

/// Send `ask` and take the one reply the session is waiting for.
///
/// Only [`prove`] uses this, and only before the renderer is published: it
/// reads the channel itself, which nothing may do once [`serve`] is the one
/// taking replies off it.
fn exchange(renderer: &Renderer, ask: &Message) -> Option<Event> {
    if !send(&renderer.control, ask) {
        return None;
    }
    let deadline = timer::now_nanos().saturating_add(REPLY_PATIENCE_NANOS);
    let reply = receive(&renderer.control, deadline)?;
    let message = Message::decode(&reply.bytes)?;
    let accepted = renderer.state.lock().session.receive(&message);
    match accepted {
        Ok(event) => Some(event),
        Err(refusal) => {
            refuse(&renderer.control, refusal);
            None
        }
    }
}

/// A range of the work VMO, for the node above when it is written.
#[expect(dead_code, reason = "the node that hands these out is the next step")]
pub(crate) const fn whole_work() -> Work {
    Work {
        at: 0,
        len: WORK_BYTES as u32,
    }
}
