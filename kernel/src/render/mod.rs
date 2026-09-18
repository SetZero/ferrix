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

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::convert::Infallible;
use core::sync::atomic::{AtomicUsize, Ordering};

use ferrix_blkring::identity::Location;
use ferrix_native_abi::rights::Rights;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::CHANNEL_MAX_HANDLES;
use ferrix_renderctl::message::{MAX_BYTES, Message, Ready, Refusal, Status, VERSION, Work};
use ferrix_renderctl::session::{Event, Session};

use crate::device::DeviceNode;
use crate::object::channel::{ChannelMessage, Endpoint, ReadError};
use crate::object::{Object, Transfer};
use crate::sched;
use crate::sync::SpinLock;
use crate::timer;

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

/// Why a control channel could not be made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CreateError {
    /// The device already has one.
    InUse,
    /// No memory for the channel, or no stack for the task.
    NoMemory,
}

/// A device whose render channel has been made, waiting for its task.
struct Start {
    id: usize,
    control: Arc<Endpoint>,
    device: Arc<DeviceNode>,
    location: Option<Location>,
}

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
static NEXT_RENDERER: AtomicUsize = AtomicUsize::new(128);
static STARTING: SpinLock<Vec<Start>> = SpinLock::new(Vec::new());
static CLAIMED: SpinLock<Vec<Arc<DeviceNode>>> = SpinLock::new(Vec::new());

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

/// One renderer's task.
fn run(id: usize) {
    let Some(start) = take_start(id) else {
        return;
    };
    if let Some((mut session, renderer)) = take_up(&start) {
        prove(&start, &mut session, renderer);
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
fn take_up(start: &Start) -> Option<(Session, u32)> {
    let deadline = timer::now_nanos().saturating_add(HELLO_PATIENCE_NANOS);
    let message = receive(&start.control, deadline)?;
    match accept(start, &message) {
        Ok(session) => Some(session),
        Err(refusal) => {
            crate::console::println!("  render   a driver was refused: {refusal}");
            refuse(&start.control, refusal);
            None
        }
    }
}

fn accept(start: &Start, message: &ChannelMessage) -> Result<(Session, u32), Refusal> {
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
    let renderer = NEXT_RENDERER.fetch_add(1, Ordering::Relaxed);
    let renderer = u32::try_from(renderer).unwrap_or(128);
    // READY without the work VMO or the core's port yet: nothing submits a
    // command buffer until the node above is written, and a handle handed
    // over now would be one the driver could not be told the meaning of.
    // The driver's HELLO is what this conversation is for today.
    if !send(
        &start.control,
        &Message::Ready(Ready {
            renderer,
            work_bytes: 0,
        }),
    ) {
        return Err(Refusal::Malformed);
    }
    crate::console::println!(
        "  render   renderD{renderer} is `{}`, version {VERSION}, capset {}, objects to {} MiB",
        session.name(),
        session.capset(),
        session.object_limit() / (1024 * 1024),
    );
    Ok((session, renderer))
}

/// Make one context and take it away again, which is the whole of what the
/// core does with a renderer until the node above it exists.
///
/// It is not ceremony: it is the only thing that says the conversation
/// works end to end against a real device, and it is what the boot log
/// reports. `docs/GPU.md` §3.2 -- the commands under it were tested against
/// QEMU's header, and this is them against QEMU.
fn prove(start: &Start, session: &mut Session, renderer: u32) {
    let context = 1;
    let Ok(ask) = session.make_context(context, session.capset()) else {
        return;
    };
    if !send(&start.control, &ask) {
        return;
    }
    let deadline = timer::now_nanos().saturating_add(REPLY_PATIENCE_NANOS);
    let Some(reply) = receive(&start.control, deadline) else {
        crate::console::println!("  render   the driver did not answer in time");
        return;
    };
    let Some(message) = Message::decode(&reply.bytes) else {
        refuse(&start.control, Refusal::Malformed);
        return;
    };
    match session.receive(&message) {
        Ok(Event::ContextMade { status, .. }) => {
            if status == Status::Ok {
                crate::console::println!(
                    "  render   renderD{renderer} made context {context} on the device"
                );
            } else {
                crate::console::println!("  render   the device refused a context: {status:?}");
            }
        }
        Ok(_) => {}
        Err(refusal) => refuse(&start.control, refusal),
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
