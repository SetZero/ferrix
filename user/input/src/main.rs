//! The virtio-input driver process: a ring-3 program that serves one input
//! device to the kernel's input core.
//!
//! Everything that knows anything is a library. `ferrix-virtio-input` drives
//! the device and turns its events into the messages the core reads, and
//! `ferrix-inputctl` is the conversation with the core; both are tested on
//! the host, and the driver's own logic is fuzzed against a real session.
//! This program is the handles those libraries were written to be wrapped in
//! (`docs/INPUT.md` §3.2):
//!
//! 1. The bootstrap channel carries START, as for the display: the device,
//!    the driver's end of the input control channel, and where the device's
//!    virtio register blocks are.
//! 2. Each register block is an `IoMapping`; the event queue's rings and its
//!    buffers are VMOs this process creates, pins read-write and maps.
//! 3. The configuration queries give the device's name, ids, property and
//!    code bitmaps and each axis's range, which HELLO carries with a port.
//!    READY brings the node's number and starts the device.
//! 4. One port carries every event: the device's interrupt and the control
//!    channel becoming readable. Every interrupt drains the used ring, posts
//!    the buffers back before a single event is forwarded, and hands the
//!    core whatever messages the batch made.
//! 5. STOP, or the core closing its end, ends it: the device is reset and
//!    the memory released only if the reset finished.
//!
//! The exit status names the step that failed ([`Step`]), 0 a clean STOP.

#![no_std]
#![no_main]

use core::mem::ManuallyDrop;
use core::ptr;
use core::sync::atomic::{Ordering, fence};

use ferrix_blkring::control::{Block as StartBlock, Message as StartMessage, START_BYTES, Start};
use ferrix_inputctl::message::{MAX_BYTES, Message, PORT_RIGHTS};
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
use ferrix_virtio::input::{ConfigSelect, DeviceConfig};
use ferrix_virtio::pci::{CommonConfig, NO_VECTOR};
use ferrix_virtio_input::{
    AREA_BYTES, Control, DevicePages, Driver, EventArea, ISR_QUEUE, Options, Parts, Teardown,
    Transport,
};

ferrix_rt::entry!(main);

/// A page, on every architecture this runs on.
const PAGE: usize = 4096;

/// Pages of queue memory: one 64-entry split queue's rings fit in one page,
/// and two leaves room for the alignment the layout asks for.
const QUEUE_PAGES: usize = 2;

/// Pages of event buffers: 64 buffers of eight bytes.
const AREA_PAGES: usize = AREA_BYTES.div_ceil(PAGE);

/// virtio-input's PCI device id, which `docs/DEVMGR.md` names.
const VIRTIO_INPUT_ID: u16 = 0x1052;

/// Port keys.
const KEY_INTERRUPT: u64 = 1;
const KEY_CONTROL: u64 = 2;

/// Where a run stopped, as the exit status.
#[derive(Clone, Copy, Debug)]
#[repr(i32)]
enum Step {
    /// No bootstrap channel, or the first message was not START.
    Start = 1,
    /// START named a device that is not virtio-input.
    Identity = 2,
    /// A register block could not be mapped.
    Registers = 3,
    /// Memory could not be made, pinned or mapped.
    Memory = 4,
    /// The device would not come up, or would not describe itself.
    Device = 5,
    /// HELLO could not be sent, or READY did not come.
    Hello = 6,
    /// The port, the interrupt or the waits could not be arranged.
    Events = 7,
    /// The device broke the protocol.
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
    addresses: [u64; QUEUE_PAGES],
    pages: usize,
}

impl Pinned {
    fn new(device: &Device<Kernel>, pages: usize) -> Result<Pinned, Step> {
        let bytes = pages * PAGE;
        let vmo = vmo::create(Kernel, bytes).map_err(|_| Step::Memory)?;
        let pin = device
            .pin(&vmo, 0, bytes, PinAccess::ReadWrite)
            .map_err(|_| Step::Memory)?;
        let mut raw = [[0_u8; 8]; QUEUE_PAGES];
        let asked = raw.get_mut(..pages).ok_or(Step::Memory)?;
        let got = pin.addresses(asked).map_err(|_| Step::Memory)?;
        if !got.is_complete() || got.pages != pages {
            return Err(Step::Memory);
        }
        let mut addresses = [0_u64; QUEUE_PAGES];
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

impl EventArea for Pinned {
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

// ---------------------------------------------------------------------------
// The device's registers
// ---------------------------------------------------------------------------

/// One of the device's virtio register blocks, mapped.
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

/// The device as `ferrix-virtio-input` drives it.
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

impl ConfigSelect for Registers {
    fn config_write8(&mut self, offset: u32, value: u8) {
        self.device.write(offset, value);
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
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

type Input = Driver<Registers, Pinned, Pinned>;

/// What START gave.
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

/// Send HELLO and wait for the core's answer.
///
/// The port goes with HELLO as the display's does: the core holds it and the
/// driver keeps nothing of it, so a core that goes away closes it and this
/// process hears.
fn introduce(
    driver: &mut Input,
    port: &Port<Kernel>,
    control: &Channel<Kernel>,
    location: u32,
) -> Result<u32, Step> {
    let hello = driver.hello(location);
    let port_share = port
        .as_owned()
        .duplicate(Requested::Exactly(PORT_RIGHTS))
        .map_err(|_| Step::Hello)?;
    control
        .write_with(Message::Hello(hello).encode().as_bytes(), [port_share])
        .map_err(|_| Step::Hello)?;
    let node = ready(driver, control)?;
    control
        .wait_async(port, Signals::READABLE | Signals::PEER_CLOSED, KEY_CONTROL)
        .map_err(|_| Step::Events)?;
    Ok(node)
}

/// Take the core's answer to HELLO: READY starts the device, and anything
/// else ends the driver.
fn ready(driver: &mut Input, control: &Channel<Kernel>) -> Result<u32, Step> {
    let _ = control
        .wait_one(Signals::READABLE, Deadline::Never)
        .map_err(|_| Step::Hello)?;
    let mut bytes = [0_u8; MAX_BYTES];
    let mut handles = [Handle::INVALID; 1];
    let received = control
        .read(&mut bytes, &mut handles)
        .map_err(|_| Step::Hello)?;
    for handle in handles.iter().take(received.handles) {
        // The core's port is for a later iteration -- the status queue the
        // LED row needs -- and is closed here.
        drop(OwnedHandle::from_raw(Kernel, *handle));
    }
    let decoded = Message::decode(bytes.get(..received.bytes).unwrap_or_default())
        .map_err(|_| Step::Control)?;
    // `on_control` is what sets `DRIVER_OK` and posts the buffers, so the
    // device delivers nothing until the core has judged it.
    match driver.on_control(&decoded) {
        Ok(Control::Started { node }) => Ok(node),
        Ok(Control::Refused(_) | Control::Stop | Control::Status) => Err(Step::Control),
        Err(_) => Err(Step::Faulted),
    }
}

fn run(boot: &Channel<Kernel>) -> Result<(), Step> {
    let Started {
        start,
        device,
        control,
    } = started(boot)?;
    if start.pci_device_id != VIRTIO_INPUT_ID {
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
        Options::default(),
    ) {
        Ok(driver) => driver,
        Err(failure) => {
            drop(failure);
            return Err(Step::Device);
        }
    };

    if let Err(step) = introduce(&mut driver, &port, &control, start.location) {
        // Nothing has been posted yet; the device goes back to reset, unless
        // it will not, when its memory stays.
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

    let ended = serve(&mut driver, &port, &control);
    match driver.shutdown() {
        Teardown::Released(released) => {
            drop(released);
            if matches!(ended, Ok(true)) {
                let _ = control.write(Message::Stopped.encode().as_bytes());
            }
            ended.map(drop)
        }
        Teardown::Wedged(kept) => {
            // The device may still write into the event buffers: they stay.
            let _kept_for_good = ManuallyDrop::new(kept);
            Err(Step::Wedged)
        }
    }
}

/// Serve the device until the core stops it or goes away.
///
/// `Ok(true)` when the core asked for a stop, which is answered with STOPPED.
fn serve(driver: &mut Input, port: &Port<Kernel>, control: &Channel<Kernel>) -> Result<bool, Step> {
    loop {
        let packet = port.wait(Deadline::Never).map_err(|_| Step::Events)?;
        match (packet.kind, packet.key) {
            (PACKET_INTERRUPT, KEY_INTERRUPT) => forward(driver, control)?,
            (PACKET_SIGNAL, KEY_CONTROL) => {
                // A one-shot wait: asked for again after every packet, as
                // the display's driver asks.
                if let Some(stop) = take_control(driver, control)? {
                    return Ok(stop);
                }
                control
                    .wait_async(port, Signals::READABLE | Signals::PEER_CLOSED, KEY_CONTROL)
                    .map_err(|_| Step::Events)?;
            }
            // A packet for something this loop did not ask for: waited for
            // again, not acted on.
            _ => {}
        }
    }
}

/// Drain the device and hand the core whatever messages the batch made.
fn forward(driver: &mut Input, control: &Channel<Kernel>) -> Result<(), Step> {
    loop {
        let drained = driver.on_interrupt().map_err(|_| Step::Faulted)?;
        while let Some(events) = driver.pop_events() {
            let encoded = Message::Events(events).encode();
            control
                .write(encoded.as_bytes())
                .map_err(|_| Step::Control)?;
        }
        // `Drained.more` means the batch was full and the used ring still
        // holds completions: taking messages made room, so go again.
        if !drained.more {
            return Ok(());
        }
    }
}

/// Read what the core said, if anything.
///
/// `Some(true)` for a STOP, `Some(false)` for the core going away or
/// refusing, and `None` when there was nothing to read.
fn take_control(driver: &mut Input, control: &Channel<Kernel>) -> Result<Option<bool>, Step> {
    let mut bytes = [0_u8; MAX_BYTES];
    let mut handles = [Handle::INVALID; 1];
    let received = match control.read(&mut bytes, &mut handles) {
        Ok(received) => received,
        // The core let go of its end: the run is over, and nothing is
        // answered.
        Err(ReadError::Failed(Error::PeerClosed)) => return Ok(Some(false)),
        // Readable without a message: another read took it, or the signal
        // was for the close that has not landed yet.
        Err(ReadError::Failed(Error::ShouldWait)) => return Ok(None),
        Err(_) => return Err(Step::Control),
    };
    for handle in handles.iter().take(received.handles) {
        drop(OwnedHandle::from_raw(Kernel, *handle));
    }
    let decoded = Message::decode(bytes.get(..received.bytes).unwrap_or_default())
        .map_err(|_| Step::Control)?;
    match driver.on_control(&decoded) {
        Ok(Control::Stop) => Ok(Some(true)),
        Ok(Control::Refused(_)) => Ok(Some(false)),
        Ok(Control::Started { .. } | Control::Status) => Ok(None),
        Err(_) => Err(Step::Faulted),
    }
}
