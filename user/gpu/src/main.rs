//! The virtio-gpu driver process: a ring-3 program that serves one card to
//! the kernel's display core.
//!
//! Everything that knows anything is a library. `ferrix-virtio-gpu` drives
//! the device and turns the core's requests into device commands, and
//! `ferrix-displayctl` is the conversation with the core; both are tested on
//! the host. This program is the handles those libraries were written to be
//! wrapped in (`docs/DISPLAY.md` §2.2):
//!
//! 1. The bootstrap channel carries START, as for the block driver: the
//!    device, the driver's end of the display control channel, and where the
//!    device's virtio register blocks are.
//! 2. Each register block is an `IoMapping`; the rings and the command area
//!    are VMOs this process creates, pins read-write and maps.
//! 3. `GET_DISPLAY_INFO` gives each scanout's preferred mode, which HELLO
//!    carries with a port. READY brings the card VMO back, `READ | TRANSFER`
//!    only: this process pins its ranges read-only as buffers' backing and
//!    can neither write nor map the pixels.
//! 4. One port carries every event: the device's interrupt and the control
//!    channel becoming readable. Requests queue in the pipeline, which says
//!    what to do next: pin, submit a command, unpin, or reply.
//! 5. STOP, or the core closing its end, ends it: the device is reset and
//!    the pins released only if the reset finished.
//!
//! The exit status names the step that failed ([`Step`]), 0 a clean STOP.

#![no_std]
#![no_main]

use core::mem::ManuallyDrop;
use core::ptr;
use core::sync::atomic::{Ordering, fence};

use ferrix_blkring::control::{Block as StartBlock, Message as StartMessage, START_BYTES, Start};
use ferrix_displayctl::message::{
    Hello, MAX_BUFFER_PAGES as MAX_PAGES, MAX_BYTES, MAX_DIMENSION, MAX_SCANOUTS, Message,
    PORT_RIGHTS, ScanoutMode, VERSION,
};
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::rights::Requested;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::{IoMappingSpec, PACKET_INTERRUPT, PACKET_SIGNAL};
use ferrix_rt::native::channel::{Channel, ReadError};
use ferrix_rt::native::device::{Device, Interrupt, IoMapping};
use ferrix_rt::native::error::Error;
use ferrix_rt::native::handle::{Deadline, Object, OwnedHandle};
use ferrix_rt::native::pending::Protection;
use ferrix_rt::native::pin::{Pin, PinAccess, device_address};
use ferrix_rt::native::port::{self, Port};
use ferrix_rt::native::vmo::{self, Vmo};
use ferrix_rt::{Bootstrap, Kernel};
use ferrix_virtio::QueueMemory;
use ferrix_virtio::gpu::{self, Command, DeviceConfig, DeviceError as Refusal, MemEntry, Response};
use ferrix_virtio::pci::{CommonConfig, NO_VECTOR};
use ferrix_virtio_gpu::pipeline::{Pipeline, Request, Step as Next};
use ferrix_virtio_gpu::{
    CAPSET_ROOM, CommandArea, DevicePages, Driver, ISR_QUEUE, Options, Parts, Teardown, Transport,
};

ferrix_rt::entry!(main);

/// A page, on every architecture this runs on.
const PAGE: usize = 4096;

/// Pages of queue memory: the control queue's rings fit in one.
const QUEUE_PAGES: usize = 2;

/// The most pages one buffer may have, as the protocol says.
const MAX_BUFFER_PAGES: usize = MAX_PAGES as usize;

/// Bytes of `RESOURCE_ATTACH_BACKING` before its entries.
const ATTACH_BACKING_HEADER: usize = 32;

/// Bytes of one backing entry on the wire.
const ENTRY_BYTES: usize = 16;

/// Pages of command area: the largest request is a backing list of one
/// entry per page, with the response after it. Sized for the worst case,
/// pages no two of which the device sees side by side, so no ATTACH the
/// protocol allows is too large to submit.
const AREA_PAGES: usize =
    (ATTACH_BACKING_HEADER + MAX_BUFFER_PAGES * ENTRY_BYTES + ferrix_virtio_gpu::RESPONSE_BYTES)
        .div_ceil(PAGE);

/// Buffers pinned at once.
const MAX_PINS: usize = 32;

/// virtio-gpu's modern PCI device id.
const VIRTIO_GPU_ID: u16 = 0x1050;

/// Port keys.
const KEY_INTERRUPT: u64 = 1;
const KEY_CONTROL: u64 = 2;

/// Where a run stopped, as the exit status.
#[derive(Clone, Copy, Debug)]
#[repr(i32)]
enum Step {
    /// No bootstrap channel, or the first message was not START.
    Start = 1,
    /// START named a device that is not virtio-gpu.
    Identity = 2,
    /// A register block could not be mapped.
    Registers = 3,
    /// Memory could not be made, pinned or mapped.
    Memory = 4,
    /// The device would not come up, or would not say its displays.
    Device = 5,
    /// HELLO could not be sent, or READY did not come.
    Hello = 6,
    /// The port, the interrupt or the waits could not be arranged.
    Events = 7,
    /// The device broke the protocol, or the pipeline was out of step.
    Faulted = 8,
    /// The device would not reset: its memory is kept.
    Wedged = 9,
    /// The control channel failed, or the core refused this driver.
    Control = 10,
}

fn main(bootstrap: Bootstrap) -> i32 {
    let Some(boot) = bootstrap else {
        return Step::Start as i32;
    };
    match run(&boot) {
        Ok(()) => 0,
        Err(step) => step as i32,
    }
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

/// A VMO mapped into this process.
struct Mapped {
    base: usize,
    len: usize,
}

impl Mapped {
    fn read_u8(&self, offset: usize) -> u8 {
        assert!(offset < self.len, "a read inside the mapping");
        // SAFETY: a mapping the kernel made for this process that lives as
        // long as its VMO handle; the offset was checked; volatile, since
        // the other side is a device.
        unsafe { ptr::read_volatile((self.base + offset) as *const u8) }
    }

    fn write_u8(&mut self, offset: usize, value: u8) {
        assert!(offset < self.len, "a write inside the mapping");
        // SAFETY: as for `read_u8`, and the mapping is writable.
        unsafe { ptr::write_volatile((self.base + offset) as *mut u8, value) }
    }
}

/// A VMO this process made, pinned read-write for the device and mapped.
struct Pinned {
    _pin: Pin<Kernel>,
    _vmo: Vmo<Kernel>,
    mapped: Mapped,
    addresses: [u64; AREA_PAGES],
    pages: usize,
}

impl Pinned {
    fn new(device: &Device<Kernel>, pages: usize) -> Result<Pinned, Step> {
        let bytes = pages * PAGE;
        let vmo = vmo::create(Kernel, bytes).map_err(|_| Step::Memory)?;
        let pin = device
            .pin(&vmo, 0, bytes, PinAccess::ReadWrite)
            .map_err(|_| Step::Memory)?;
        let mut raw = [[0_u8; 8]; AREA_PAGES];
        let asked = raw.get_mut(..pages).ok_or(Step::Memory)?;
        let got = pin.addresses(asked).map_err(|_| Step::Memory)?;
        if !got.is_complete() || got.pages != pages {
            return Err(Step::Memory);
        }
        let mut addresses = [0_u64; AREA_PAGES];
        for (slot, bytes) in addresses.iter_mut().zip(raw.iter().take(pages)) {
            *slot = device_address(*bytes);
        }
        let base = vmo
            .map(None, bytes, Protection::ReadWrite, 0)
            .map_err(|_| Step::Memory)?;
        Ok(Pinned {
            _pin: pin,
            _vmo: vmo,
            mapped: Mapped { base, len: bytes },
            addresses,
            pages,
        })
    }
}

impl DevicePages for Pinned {
    fn device_pages(&self) -> &[u64] {
        self.addresses.get(..self.pages).unwrap_or_default()
    }
}

impl CommandArea for Pinned {
    fn read_u8(&self, offset: usize) -> u8 {
        self.mapped.read_u8(offset)
    }

    fn write_u8(&mut self, offset: usize, value: u8) {
        self.mapped.write_u8(offset, value);
    }
}

// SAFETY: one VMO, pinned so the device sees the pages `device_pages` names
// and mapped so this process sees `mapped`, both until the `Pinned` is
// dropped, which the driver's teardown rule forbids while the device may
// write. Every access is volatile and `barrier` is a full fence.
unsafe impl QueueMemory for Pinned {
    fn read_u8(&self, offset: usize) -> u8 {
        self.mapped.read_u8(offset)
    }

    fn write_u8(&mut self, offset: usize, value: u8) {
        self.mapped.write_u8(offset, value);
    }

    fn barrier(&self) {
        fence(Ordering::SeqCst);
    }
}

/// Scratch for pinning a buffer: its pages' addresses as the pin query writes
/// them, and the backing entries made from them. A mapped VMO, since both
/// are too large for a stack.
struct Scratch {
    _vmo: Vmo<Kernel>,
    raw: usize,
    entries: usize,
}

impl Scratch {
    fn new() -> Result<Scratch, Step> {
        let raw_bytes = MAX_BUFFER_PAGES * 8;
        let entry_bytes = MAX_BUFFER_PAGES * size_of::<MemEntry>();
        let bytes = (raw_bytes + entry_bytes).div_ceil(PAGE) * PAGE;
        let vmo = vmo::create(Kernel, bytes).map_err(|_| Step::Memory)?;
        let base = vmo
            .map(None, bytes, Protection::ReadWrite, 0)
            .map_err(|_| Step::Memory)?;
        let entries = (base + raw_bytes).next_multiple_of(align_of::<MemEntry>());
        if entries + entry_bytes > base + bytes {
            return Err(Step::Memory);
        }
        Ok(Scratch {
            _vmo: vmo,
            raw: base,
            entries,
        })
    }

    /// Room for `pages` addresses as the pin query writes them.
    fn raw(&mut self, pages: usize) -> &mut [[u8; 8]] {
        // SAFETY: `raw` starts a zero-filled private mapping of at least
        // `MAX_BUFFER_PAGES` eight-byte arrays, which have no alignment
        // requirement; `pages` is capped to that; the `&mut self` borrow
        // keeps the slice unique.
        unsafe {
            core::slice::from_raw_parts_mut(self.raw as *mut [u8; 8], pages.min(MAX_BUFFER_PAGES))
        }
    }

    /// The backing entries, all [`MAX_BUFFER_PAGES`] of them.
    fn entries(&self) -> &[MemEntry] {
        // SAFETY: `entries` is aligned for `MemEntry` inside a zero-filled
        // mapping holding `MAX_BUFFER_PAGES` of them, and all-zero is a valid
        // `MemEntry`; shared, as `&self` is.
        unsafe { core::slice::from_raw_parts(self.entries as *const MemEntry, MAX_BUFFER_PAGES) }
    }

    fn entries_mut(&mut self) -> &mut [MemEntry] {
        // SAFETY: as for `entries`, unique through `&mut self`.
        unsafe { core::slice::from_raw_parts_mut(self.entries as *mut MemEntry, MAX_BUFFER_PAGES) }
    }
}

// ---------------------------------------------------------------------------
// The device's registers
// ---------------------------------------------------------------------------

/// One virtio register block.
struct Block {
    _mapping: IoMapping<Kernel>,
    base: usize,
    len: usize,
}

impl Block {
    fn map(device: &Device<Kernel>, block: &StartBlock) -> Result<Block, Step> {
        let offset = block.offset as usize;
        let len = block.length as usize;
        let end = offset.checked_add(len).ok_or(Step::Registers)?;
        let pages = end.div_ceil(PAGE).max(1) * PAGE;
        let mapping = device
            .io_mapping(IoMappingSpec {
                phys: block.phys,
                len: pages as u64,
            })
            .map_err(|_| Step::Registers)?;
        let base = mapping.map(None).map_err(|_| Step::Registers)?;
        Ok(Block {
            _mapping: mapping,
            base: base + offset,
            len,
        })
    }

    fn read<T: Copy>(&self, offset: u32) -> T {
        let offset = offset as usize;
        assert!(
            offset + size_of::<T>() <= self.len && offset.is_multiple_of(size_of::<T>()),
            "a register inside the block, aligned"
        );
        // SAFETY: mapped device memory the kernel gave this process, the
        // offset inside it and aligned for `T`, read volatile.
        unsafe { ptr::read_volatile((self.base + offset) as *const T) }
    }

    fn write<T: Copy>(&mut self, offset: u32, value: T) {
        let offset = offset as usize;
        assert!(
            offset + size_of::<T>() <= self.len && offset.is_multiple_of(size_of::<T>()),
            "a register inside the block, aligned"
        );
        // SAFETY: as for `read`, and the mapping is writable.
        unsafe { ptr::write_volatile((self.base + offset) as *mut T, value) }
    }
}

/// The device as `ferrix-virtio-gpu` drives it.
struct Registers {
    common: Block,
    notify: Block,
    isr: Block,
    device: Block,
    notify_off_multiplier: u32,
    msix: bool,
    interrupt: Interrupt<Kernel>,
}

impl CommonConfig for Registers {
    fn read8(&self, offset: u32) -> u8 {
        self.common.read(offset)
    }
    fn read16(&self, offset: u32) -> u16 {
        self.common.read(offset)
    }
    fn read32(&self, offset: u32) -> u32 {
        self.common.read(offset)
    }
    fn write8(&mut self, offset: u32, value: u8) {
        self.common.write(offset, value);
    }
    fn write16(&mut self, offset: u32, value: u16) {
        self.common.write(offset, value);
    }
    fn write32(&mut self, offset: u32, value: u32) {
        self.common.write(offset, value);
    }
}

impl DeviceConfig for Registers {
    fn config_len(&self) -> u32 {
        self.device.len as u32
    }
    fn config_read8(&self, offset: u32) -> u8 {
        self.device.read(offset)
    }
    fn config_read16(&self, offset: u32) -> u16 {
        self.device.read(offset)
    }
    fn config_read32(&self, offset: u32) -> u32 {
        self.device.read(offset)
    }
}

impl Transport for Registers {
    fn notify(&mut self, queue: u16, notify_off: u16) {
        let at = u32::from(notify_off).saturating_mul(self.notify_off_multiplier);
        self.notify.write(at, queue);
    }

    fn queue_vector(&self) -> u16 {
        if self.msix { 0 } else { NO_VECTOR }
    }

    fn acknowledge_interrupt(&mut self) -> u8 {
        if self.msix {
            let _ = self.interrupt.ack();
            ISR_QUEUE
        } else {
            let status: u8 = self.isr.read(0);
            let _ = self.interrupt.ack();
            status
        }
    }

    fn config_write32(&mut self, offset: u32, value: u32) {
        self.device.write(offset, value);
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

type Gpu = Driver<Registers, Pinned, Pinned>;

/// What START handed over.
struct Started {
    start: Start,
    device: Device<Kernel>,
    control: Channel<Kernel>,
}

fn started(boot: &Channel<Kernel>) -> Result<Started, Step> {
    let _ = boot
        .wait_one(Signals::READABLE, Deadline::Never)
        .map_err(|_| Step::Start)?;
    let mut bytes = [0_u8; START_BYTES];
    let mut handles = [Handle::INVALID; 2];
    let received = boot
        .read(&mut bytes, &mut handles)
        .map_err(|_| Step::Start)?;
    if received.handles != 2 {
        return Err(Step::Start);
    }
    let device = Device::from_owned(OwnedHandle::from_raw(Kernel, handles[0]));
    let control = Channel::from_owned(OwnedHandle::from_raw(Kernel, handles[1]));
    match StartMessage::decode(bytes.get(..received.bytes).unwrap_or_default()) {
        Ok(StartMessage::Start(start)) => Ok(Started {
            start,
            device,
            control,
        }),
        _ => Err(Step::Start),
    }
}

/// The render half of the card: `libs/renderctl`'s HELLO, and then the one
/// context the core asks for.
///
/// A card has two conversations (`docs/GPU.md` §3.3) and this driver serves
/// both, as one Linux driver serves `card0` and `renderD128`. What is here
/// is the *adapter*: renderctl's messages in, virtio-gpu's 3D commands out.
/// A second driver for another GPU writes this function against its own
/// device and nothing above it changes.
fn render(
    driver: &mut Gpu,
    port: &Port<Kernel>,
    device: &Device<Kernel>,
    location: u32,
) -> Result<(), Step> {
    use ferrix_renderctl::message::{
        self as rc, MAX_BYTES as RENDER_MAX_BYTES, Message as RenderMessage,
    };

    // A card with no 3D behind it has no render node to serve.
    if driver.info().features & gpu::FEATURE_VIRGL == 0 {
        return Ok(());
    }
    let Ok(control) = device.render_control() else {
        return Ok(());
    };
    let Some(name) = rc::Hello::named("virtio_gpu") else {
        return Ok(());
    };
    let hello = RenderMessage::Hello(rc::Hello {
        version: rc::VERSION,
        location,
        name,
        // Fences are encoded but nothing waits on one yet, so they are not
        // offered: a feature bit is a promise the core would hold us to.
        features: rc::features::SUBMIT,
        // Which capability set this device's streams are in, asked for
        // afresh: the display's HELLO read it too, and a driver that kept
        // one number in two places would eventually disagree with itself.
        capset: match run_command(driver, port, &Command::GetCapsetInfo { index: 0 })? {
            Ok(Response::CapsetInfo(info)) => info.id,
            _ => 0,
        },
        object_max: MAX_OBJECT_BYTES,
    });
    let share = port
        .as_owned()
        .duplicate(Requested::Exactly(PORT_RIGHTS))
        .map_err(|_| Step::Hello)?;
    control
        .write_with(hello.encode().as_bytes(), [share])
        .map_err(|_| Step::Hello)?;

    // READY, and then whatever the core asks. Only MAKE_CTX today, which is
    // what says the 3D commands work against the device rather than against
    // QEMU's header.
    let mut bytes = [0_u8; RENDER_MAX_BYTES];
    let mut handles = [Handle::INVALID; 2];
    // READY's work VMO, kept for as long as the conversation lasts: an
    // object's description and a command buffer are ranges of it, and the
    // core writes them there rather than into a message. The core's port,
    // which comes beside it, is for fences and is closed until something
    // waits on one.
    let mut work: Option<Vmo<Kernel>> = None;
    loop {
        let _ = control
            .wait_one(Signals::READABLE | Signals::PEER_CLOSED, Deadline::Never)
            .map_err(|_| Step::Events)?;
        let Ok(received) = control.read(&mut bytes, &mut handles) else {
            return Ok(());
        };
        let taken = RenderMessage::decode(bytes.get(..received.bytes).unwrap_or_default())
            .is_some_and(|message| matches!(message, RenderMessage::Ready(_)))
            && received.handles == 2;
        for (index, handle) in handles.iter_mut().enumerate().take(received.handles) {
            let owned = OwnedHandle::from_raw(Kernel, *handle);
            match (taken, index) {
                (true, 0) => work = Some(Vmo::from_owned(owned)),
                _ => drop(owned),
            }
            *handle = Handle::INVALID;
        }
        let Some(message) = RenderMessage::decode(bytes.get(..received.bytes).unwrap_or_default())
        else {
            return Ok(());
        };
        let answer = match message {
            RenderMessage::Ready(_) => continue,
            RenderMessage::Refused(_) => return Ok(()),
            RenderMessage::MakeContext { context, capset } => {
                make_context(driver, port, context, capset)?
            }
            RenderMessage::MakeObject(make) => make_object(driver, port, work.as_ref(), &make)?,
            RenderMessage::DropObject { object } => drop_object(driver, port, object)?,
            RenderMessage::Stop => RenderMessage::Stopped,
            _ => return Ok(()),
        };
        let stopping = matches!(answer, RenderMessage::Stopped);
        control
            .write(answer.encode().as_bytes())
            .map_err(|_| Step::Control)?;
        if stopping {
            return Ok(());
        }
    }
}

/// Make one rendering context on the device.
///
/// virtio's own shape: the id rides in the header, and `context_init`
/// carries the capability set.
fn make_context(
    driver: &mut Gpu,
    port: &Port<Kernel>,
    context: u32,
    capset: u32,
) -> Result<ferrix_renderctl::message::Message, Step> {
    let made = run_command_in(
        driver,
        port,
        gpu::Context {
            id: context,
            ..gpu::Context::NONE
        },
        &Command::CtxCreate {
            capset: u8::try_from(capset).unwrap_or(0),
            name: "ferrix",
        },
    )?;
    Ok(ferrix_renderctl::message::Message::ContextMade {
        context,
        status: status_of(made),
    })
}

/// Give one object back to the device.
fn drop_object(
    driver: &mut Gpu,
    port: &Port<Kernel>,
    object: u32,
) -> Result<ferrix_renderctl::message::Message, Step> {
    let gone = run_command(
        driver,
        port,
        &Command::ResourceUnref {
            resource_id: object,
        },
    )?;
    Ok(ferrix_renderctl::message::Message::ObjectGone {
        object,
        status: status_of(gone),
    })
}

/// Make one object on the device: the resource itself, and the context's
/// right to name it.
///
/// This is the device-specific half of the adapter, and the whole of what a
/// second GPU rewrites (`docs/GPU.md` §3.3). The core said how many bytes it
/// wants and nothing else; what a resource *is* on this device is known
/// here and nowhere above.
fn make_object(
    driver: &mut Gpu,
    port: &Port<Kernel>,
    work: Option<&Vmo<Kernel>>,
    make: &ferrix_renderctl::message::MakeObject,
) -> Result<ferrix_renderctl::message::Message, Step> {
    use ferrix_renderctl::message::Message as RenderMessage;

    // An empty description is the core asking for a plain buffer; a
    // description of its own will be the render node's, once a program can
    // write one.
    let (target, format, bind) = described(work, make.describe);
    let made = run_command_in(
        driver,
        port,
        gpu::Context {
            id: make.context,
            ..gpu::Context::NONE
        },
        &Command::ResourceCreate3d {
            resource_id: make.object,
            target,
            format,
            bind,
            size: gpu::Box3d {
                x: 0,
                y: 0,
                z: 0,
                width: u32::try_from(make.bytes).unwrap_or(u32::MAX),
                height: 1,
                depth: 1,
            },
            array_size: 1,
            last_level: 0,
            samples: 0,
            flags: 0,
        },
    )?;
    // A resource a context may name has to be given to it: a 3D command
    // naming one that was not is refused by the device.
    let attached = match (&made, make.context) {
        (Ok(_), context) if context != 0 => run_command_in(
            driver,
            port,
            gpu::Context {
                id: context,
                ..gpu::Context::NONE
            },
            &Command::CtxAttachResource {
                resource_id: make.object,
            },
        )?,
        _ => Ok(Response::NoData),
    };
    Ok(RenderMessage::ObjectMade {
        object: make.object,
        status: status_of(made.and(attached)),
    })
}

/// The largest object this driver will make: what one virtio-gpu resource
/// may reasonably be, and far inside what the protocol allows.
const MAX_OBJECT_BYTES: u64 = 64 * 1024 * 1024;

/// virgl's `PIPE_BUFFER`, from Mesa's `p_defines.h`: a resource with no
/// shape, which is what bytes on their way to a shader are.
const PIPE_BUFFER: u32 = 0;

/// virgl's `VIRGL_FORMAT_R8_UNORM`, from virglrenderer's `virgl_hw.h`: one
/// byte a pixel, which is how a buffer's bytes are counted.
const VIRGL_FORMAT_R8_UNORM: u32 = 64;

/// virgl's `VIRGL_BIND_VERTEX_BUFFER`, from the same header. virgl numbers
/// some of its bind bits differently from Mesa's `PIPE_BIND_*`, so they are
/// taken from virgl's header and not from gallium's.
const VIRGL_BIND_VERTEX_BUFFER: u32 = 1 << 4;

/// What to make of a `MAKE_OBJ`'s description: the target, format and bind
/// words for the device.
///
/// An empty description is the core saying "so many bytes, and the rest is
/// yours", which is a plain buffer. A description of its own is four
/// little-endian words -- target, format, bind, and a reserved zero -- which
/// is what the render node will write once a program can ask for a texture.
/// Anything shorter is treated as empty rather than half-read.
fn described(
    work: Option<&Vmo<Kernel>>,
    describe: ferrix_renderctl::message::Work,
) -> (u32, u32, u32) {
    const DESCRIBED_BYTES: usize = 16;
    let plain = (PIPE_BUFFER, VIRGL_FORMAT_R8_UNORM, VIRGL_BIND_VERTEX_BUFFER);
    if describe.len as usize != DESCRIBED_BYTES {
        return plain;
    }
    let Some(work) = work else {
        return plain;
    };
    // Three words and a reserved zero, each read as its own four bytes:
    // indexing one buffer would be four places this could panic on a
    // description the node wrote wrong.
    let mut word = [0_u8; 4];
    let mut at = u64::from(describe.at);
    let mut next = || {
        let read = work.read(&mut word, at).is_ok();
        at = at.saturating_add(4);
        read.then(|| u32::from_le_bytes(word))
    };
    match (next(), next(), next()) {
        (Some(target), Some(format), Some(bind)) => (target, format, bind),
        _ => plain,
    }
}

/// A device's answer, as the render protocol says it.
fn status_of(answer: Result<Response, Refusal>) -> ferrix_renderctl::message::Status {
    use ferrix_renderctl::message::Status;
    match answer {
        Ok(_) => Status::Ok,
        Err(Refusal::OutOfMemory) => Status::OutOfMemory,
        Err(Refusal::InvalidParameter | Refusal::InvalidResourceId | Refusal::InvalidContextId) => {
            Status::Invalid
        }
        Err(_) => Status::DeviceRefused,
    }
}

/// [`run_command`] for a command that belongs to a context.
fn run_command_in(
    driver: &mut Gpu,
    port: &Port<Kernel>,
    context: gpu::Context,
    command: &Command<'_>,
) -> Result<Result<Response, Refusal>, Step> {
    driver
        .submit_in(context, command)
        .map_err(|_| Step::Device)?;
    loop {
        let packet = port.wait(Deadline::Never).map_err(|_| Step::Events)?;
        if (packet.kind, packet.key) != (PACKET_INTERRUPT, KEY_INTERRUPT) {
            continue;
        }
        if let (Some(done), _) = driver.on_interrupt().map_err(|_| Step::Device)? {
            return Ok(done.result);
        }
    }
}

/// Submit a command and wait for its outcome, before the pipeline runs.
fn run_command(
    driver: &mut Gpu,
    port: &Port<Kernel>,
    command: &Command<'_>,
) -> Result<Result<Response, Refusal>, Step> {
    driver.submit(command).map_err(|_| Step::Device)?;
    loop {
        let packet = port.wait(Deadline::Never).map_err(|_| Step::Events)?;
        if (packet.kind, packet.key) != (PACKET_INTERRUPT, KEY_INTERRUPT) {
            continue;
        }
        if let (Some(done), _) = driver.on_interrupt().map_err(|_| Step::Device)? {
            return Ok(done.result);
        }
    }
}

/// HELLO from what `GET_DISPLAY_INFO` said.
fn hello(driver: &mut Gpu, port: &Port<Kernel>, location: u32) -> Result<Hello, Step> {
    let Ok(Response::DisplayInfo(scanouts)) = run_command(driver, port, &Command::GetDisplayInfo)?
    else {
        return Err(Step::Device);
    };
    let count = (driver.info().config.num_scanouts as usize).min(MAX_SCANOUTS);
    let mut modes = [ScanoutMode::default(); MAX_SCANOUTS];
    for (mode, scanout) in modes.iter_mut().zip(scanouts.iter()).take(count) {
        let fits = scanout.rect.width <= MAX_DIMENSION && scanout.rect.height <= MAX_DIMENSION;
        if scanout.enabled && fits {
            *mode = ScanoutMode {
                width: scanout.rect.width,
                height: scanout.rect.height,
                enabled: true,
            };
        }
    }
    // What the card can do in 3D, which is the device's answer and not the
    // driver's wish: `VIRTIO_GPU_F_VIRGL` is granted or it is not, and the
    // capability sets are only worth walking when it was. The first set's
    // id is what a renderer would go on -- `CAPSET_VIRGL2` on anything
    // recent -- and one is enough to say which renderer is behind the card.
    let virgl = driver.info().features & gpu::FEATURE_VIRGL != 0;
    let capsets = if virgl {
        u16::try_from(driver.info().config.num_capsets).unwrap_or(u16::MAX)
    } else {
        0
    };
    let first = match capsets {
        0 => None,
        _ => match run_command(driver, port, &Command::GetCapsetInfo { index: 0 })? {
            Ok(Response::CapsetInfo(info)) => Some(info),
            // A device that will not say what its first set is has one this
            // driver cannot use; the card is still a scanout.
            _ => None,
        },
    };
    // And the set itself, which is the blob a renderer reads to find out
    // what the host can do. Asked for at the size the device named, and
    // only when that fits the response buffer: a set larger than the buffer
    // is one this driver cannot fetch, and asking for it anyway would have
    // the device write past the end of it.
    let mut capset_bytes = 0;
    if let Some(info) = first
        && info.max_size <= CAPSET_ROOM
        && let Ok(Response::Capset { len }) = run_command(
            driver,
            port,
            &Command::GetCapset {
                capset_id: info.id,
                capset_version: info.max_version,
                max_size: info.max_size,
            },
        )?
    {
        capset_bytes = u32::try_from(len).unwrap_or(0);
    }
    let capset = first.map_or(0, |info| info.id);
    Ok(Hello {
        version: VERSION,
        scanouts: u16::try_from(count).map_err(|_| Step::Device)?,
        location,
        modes,
        virgl,
        capsets: if capset == 0 { 0 } else { capsets },
        capset,
        capset_bytes,
    })
}

/// Wait for READY and take the card VMO from it.
fn ready(control: &Channel<Kernel>) -> Result<Vmo<Kernel>, Step> {
    let _ = control
        .wait_one(Signals::READABLE, Deadline::Never)
        .map_err(|_| Step::Hello)?;
    let mut bytes = [0_u8; MAX_BYTES];
    let mut handles = [Handle::INVALID; 2];
    let received = control
        .read(&mut bytes, &mut handles)
        .map_err(|_| Step::Hello)?;
    match Message::decode(bytes.get(..received.bytes).unwrap_or_default()) {
        Ok(Message::Ready(_)) if received.handles == 2 => {
            // The core's port is for later; it is closed here.
            drop(OwnedHandle::from_raw(Kernel, handles[1]));
            Ok(Vmo::from_owned(OwnedHandle::from_raw(Kernel, handles[0])))
        }
        _ => Err(Step::Control),
    }
}

/// Say HELLO, take READY's card VMO, and start listening to the core.
fn introduce(
    driver: &mut Gpu,
    port: &Port<Kernel>,
    control: &Channel<Kernel>,
    location: u32,
) -> Result<(Scratch, Vmo<Kernel>), Step> {
    let scratch = Scratch::new()?;
    let hello = hello(driver, port, location)?;
    let port_share = port
        .as_owned()
        .duplicate(Requested::Exactly(PORT_RIGHTS))
        .map_err(|_| Step::Hello)?;
    control
        .write_with(Message::Hello(hello).encode().as_bytes(), [port_share])
        .map_err(|_| Step::Hello)?;
    let card = ready(control)?;
    control
        .wait_async(port, Signals::READABLE | Signals::PEER_CLOSED, KEY_CONTROL)
        .map_err(|_| Step::Events)?;
    Ok((scratch, card))
}

fn run(boot: &Channel<Kernel>) -> Result<(), Step> {
    let Started {
        start,
        device,
        control,
    } = started(boot)?;
    if start.pci_device_id != VIRTIO_GPU_ID {
        return Err(Step::Identity);
    }

    let interrupt = device.interrupt(0).map_err(|_| Step::Registers)?;
    let registers = Registers {
        common: Block::map(&device, &start.common)?,
        notify: Block::map(&device, &start.notify)?,
        isr: Block::map(&device, &start.isr)?,
        device: Block::map(&device, &start.device)?,
        notify_off_multiplier: start.notify_off_multiplier,
        msix: start.msix_table_size > 0,
        interrupt,
    };
    let rings = Pinned::new(&device, QUEUE_PAGES)?;
    let area = Pinned::new(&device, AREA_PAGES)?;
    let port = port::create(Kernel).map_err(|_| Step::Events)?;
    registers
        .interrupt
        .bind(&port, KEY_INTERRUPT)
        .map_err(|_| Step::Events)?;
    let mut driver = match Driver::init(
        Parts {
            transport: registers,
            rings,
            area,
        },
        Options {
            // Ask the card whether there is a GPU behind it. A 2D card says
            // no and is driven as it always was; a `virtio-gpu-gl` says yes
            // and brings virglrenderer up on the host.
            want_3d: true,
            ..Options::default()
        },
    ) {
        Ok(driver) => driver,
        Err(failure) => {
            drop(failure);
            return Err(Step::Device);
        }
    };

    let introduced = introduce(&mut driver, &port, &control, start.location);
    let (scratch, card) = match introduced {
        Ok(introduced) => introduced,
        Err(step) => {
            // Nothing is pinned for a buffer yet; the device goes back to
            // reset, unless it will not, when its memory stays.
            return match driver.shutdown() {
                Teardown::Released(released) => {
                    drop(released);
                    Err(step)
                }
                Teardown::Wedged(kept) => {
                    let _kept_for_good = kept;
                    Err(Step::Wedged)
                }
            };
        }
    };

    // The card's other conversation, which is where the GPU is. A card with
    // no 3D behind it says nothing and serves the display as before.
    render(&mut driver, &port, &device, start.location)?;

    let mut serving = Serving {
        driver,
        pipeline: Pipeline::new(),
        pins: [const { None }; MAX_PINS],
        scratch,
        card,
        device,
        control,
        port,
        paused: false,
    };
    let ended = serving.serve();
    let Serving {
        driver,
        pins,
        control,
        ..
    } = serving;
    match driver.shutdown() {
        Teardown::Released(released) => {
            drop(released);
            drop(pins);
            if matches!(ended, Ok(true)) {
                let _ = control.write(Message::Stopped.encode().as_bytes());
            }
            ended.map(drop)
        }
        Teardown::Wedged(kept) => {
            // The device may still read the buffers' pages: the pins stay.
            let _kept_for_good = (kept, ManuallyDrop::new(pins));
            Err(Step::Wedged)
        }
    }
}

/// The serve loop's state.
struct Serving {
    driver: Gpu,
    pipeline: Pipeline,
    pins: [Option<(u32, Pin<Kernel>)>; MAX_PINS],
    scratch: Scratch,
    card: Vmo<Kernel>,
    device: Device<Kernel>,
    control: Channel<Kernel>,
    port: Port<Kernel>,
    /// Whether reading the control channel stopped because the pipeline was
    /// full: its wait is not armed until there is room again.
    paused: bool,
}

impl Serving {
    /// Serve until STOP (`true`) or the core closing its end (`false`).
    fn serve(&mut self) -> Result<bool, Step> {
        loop {
            self.pump()?;
            if self.paused && !self.pipeline.is_full() {
                self.paused = false;
                if let Some(stopped) = self.listen()? {
                    return Ok(stopped);
                }
                continue;
            }
            let packet = self.port.wait(Deadline::Never).map_err(|_| Step::Events)?;
            match (packet.kind, packet.key) {
                (PACKET_INTERRUPT, KEY_INTERRUPT) => {
                    if let (Some(done), _) =
                        self.driver.on_interrupt().map_err(|_| Step::Faulted)?
                    {
                        self.pipeline.done(done.result).map_err(|_| Step::Faulted)?;
                    }
                }
                (PACKET_SIGNAL, KEY_CONTROL) => {
                    if let Some(stopped) = self.listen()? {
                        return Ok(stopped);
                    }
                }
                _ => {}
            }
        }
    }

    /// Take what the core has sent, and wait for more unless the pipeline
    /// is full. `Some` when the run is over.
    fn listen(&mut self) -> Result<Option<bool>, Step> {
        if let Some(stopped) = self.take_messages()? {
            return Ok(Some(stopped));
        }
        if !self.paused {
            self.control
                .wait_async(
                    &self.port,
                    Signals::READABLE | Signals::PEER_CLOSED,
                    KEY_CONTROL,
                )
                .map_err(|_| Step::Events)?;
        }
        Ok(None)
    }

    /// Take the messages the core has sent, until none is left or the
    /// pipeline has no room. `Some` when the run is over.
    fn take_messages(&mut self) -> Result<Option<bool>, Step> {
        loop {
            if self.pipeline.is_full() {
                // The rest wait in the channel, which the core bounds, until
                // a request finishes.
                self.paused = true;
                return Ok(None);
            }
            let mut bytes = [0_u8; MAX_BYTES];
            match self.control.read(&mut bytes, &mut []) {
                Ok(received) => {
                    match Message::decode(bytes.get(..received.bytes).unwrap_or_default()) {
                        Ok(Message::Stop) => return Ok(Some(true)),
                        Ok(Message::Refused(_)) => return Err(Step::Control),
                        Ok(message) => {
                            let request = Request::from_message(&message).ok_or(Step::Control)?;
                            self.pipeline.push(request).map_err(|_| Step::Control)?;
                        }
                        Err(_) => return Err(Step::Control),
                    }
                }
                Err(ReadError::Failed(Error::PeerClosed)) => return Ok(Some(false)),
                Err(ReadError::Failed(Error::ShouldWait)) => return Ok(None),
                Err(_) => return Err(Step::Control),
            }
        }
    }

    /// Do what the pipeline says until it waits on the device or has nothing.
    fn pump(&mut self) -> Result<(), Step> {
        loop {
            match self.pipeline.next(self.scratch.entries()) {
                Next::Pin {
                    buffer,
                    offset,
                    length,
                } => {
                    let result = self.pin(buffer, offset, length);
                    self.pipeline.pinned(result).map_err(|_| Step::Faulted)?;
                }
                Next::Submit(command) => {
                    if self.driver.submit(&command).is_err() {
                        return Err(Step::Faulted);
                    }
                    return Ok(());
                }
                Next::Unpin { buffer } => self.unpin(buffer),
                Next::Reply(message) => {
                    self.control
                        .write(message.encode().as_bytes())
                        .map_err(|_| Step::Control)?;
                }
                Next::Wait | Next::Idle => return Ok(()),
            }
        }
    }

    /// Close `buffer`'s pin: the device no longer holds its pages.
    fn unpin(&mut self, buffer: u32) {
        if let Some(slot) = self
            .pins
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|(held, _)| *held == buffer))
        {
            *slot = None;
        }
    }

    /// Pin `length` bytes of the card from `offset` read-only and make the
    /// backing entries: how many, or `Err` when it cannot be done.
    fn pin(&mut self, buffer: u32, offset: u64, length: u64) -> Result<usize, ()> {
        let offset = usize::try_from(offset).map_err(drop)?;
        let length = usize::try_from(length).map_err(drop)?;
        let pages = length / PAGE;
        if pages == 0 || pages > MAX_BUFFER_PAGES {
            return Err(());
        }
        // An id the device may still hold pages under is never pinned twice:
        // the core never reuses one, so this is a broken core.
        if self.pins.iter().flatten().any(|(held, _)| *held == buffer) {
            return Err(());
        }
        let slot = self.pins.iter().position(Option::is_none).ok_or(())?;
        let pin = self
            .device
            .pin(&self.card, offset, length, PinAccess::ReadOnly)
            .map_err(drop)?;
        let got = pin.addresses(self.scratch.raw(pages)).map_err(drop)?;
        if !got.is_complete() || got.pages != pages {
            return Err(());
        }
        // One entry per run of device-consecutive pages, as
        // `backing_entries` makes them, without a second array of addresses.
        let page = PAGE as u64;
        let mut count = 0usize;
        for index in 0..pages {
            let bytes = self.scratch.raw(pages).get(index).copied().ok_or(())?;
            let address = device_address(bytes);
            if !address.is_multiple_of(page) {
                return Err(());
            }
            let entries = self.scratch.entries_mut();
            let joined = count
                .checked_sub(1)
                .and_then(|last| entries.get_mut(last))
                .filter(|entry| {
                    entry.addr.checked_add(u64::from(entry.length)) == Some(address)
                        && entry.length.checked_add(PAGE as u32).is_some()
                });
            if let Some(entry) = joined {
                entry.length += PAGE as u32;
            } else {
                let fresh = entries.get_mut(count).ok_or(())?;
                *fresh = MemEntry {
                    addr: address,
                    length: PAGE as u32,
                };
                count += 1;
            }
        }
        if let Some(held) = self.pins.get_mut(slot) {
            *held = Some((buffer, pin));
        }
        Ok(count)
    }
}
