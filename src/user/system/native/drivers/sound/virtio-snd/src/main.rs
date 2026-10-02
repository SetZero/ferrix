//! The virtio-snd driver process: a ring-3 program that serves one sound
//! card to the kernel's audio core.
//!
//! Everything that knows anything is a library. `ferrix-virtio-snd` drives
//! the device and turns its completions into the messages the core reads, and
//! `ferrix-sndctl` is the conversation with the core; both are tested on the
//! host. This program is the handles those libraries were written to be
//! wrapped in (`docs/AUDIO.md` §3.2 and §3.3):
//!
//! 1. The bootstrap channel carries START, as for input: the device, the
//!    driver's end of the sound control channel, and where the device's
//!    virtio register blocks are.
//! 2. Each register block is an `IoMapping`; the control and transmit
//!    queues' rings and the driver's scratch memory are VMOs this process
//!    creates, pins read-write and maps.
//! 3. The device is brought up and asked its streams, which HELLO carries
//!    with a port. READY brings the core's buffer for each published stream,
//!    which this process pins read-only and never maps: the device reads the
//!    samples straight from the core's pages.
//! 4. One port carries every event: the device's interrupt and the control
//!    channel becoming readable. Each SUBMIT posts a buffer; each interrupt
//!    takes the completions and hands the core the ELAPSED they come to.
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
use ferrix_sndctl::message::{MAX_BYTES, MAX_PUBLISHED, Message, PORT_RIGHTS, Ready};
use ferrix_virtio::QueueMemory;
use ferrix_virtio::pci::{CommonConfig, NO_VECTOR};
use ferrix_virtio::snd::DeviceConfig;
use ferrix_virtio_snd::{
    Control, DevicePages, Driver, ISR_QUEUE, MAX_BUFFER_PAGES, Options, Parts, SCRATCH_BYTES,
    Scratch, Teardown, Transport, WATCH_NANOS,
};

ferrix_rt::entry!(main);

/// A page, on every architecture this runs on.
const PAGE: usize = 4096;

/// Pages of queue memory: a 64-entry split queue's rings fit in one page, and
/// two leave room for the alignment the layout asks for.
const QUEUE_PAGES: usize = 2;

/// Pages of scratch memory: one.
const SCRATCH_PAGES: usize = SCRATCH_BYTES.div_ceil(PAGE);

/// virtio-snd's PCI device id, `0x1040` plus 25.
const VIRTIO_SND_ID: u16 = 0x1059;

/// How long a wait for a control request's answer sleeps between looks.
const NAP_NANOS: u64 = 100_000;

/// Looks before a control request is given up on: five seconds of naps.
/// QEMU answers `PCM_START`, `STOP` and `PREPARE` only once its audio
/// backend has, and a `PipeWire` stream opened or closed on a loaded host can
/// take far longer than the spin this replaced, which gave up after a few
/// milliseconds and took the driver down with it: the likeliest reading of
/// the driver dying under Chrome playing a video on 2026-09-26.
const CONTROL_POLLS: u32 = 50_000;

/// Port keys.
const KEY_INTERRUPT: u64 = 1;
const KEY_CONTROL: u64 = 2;

/// Where a run stopped, as the exit status.
#[derive(Clone, Copy, Debug)]
#[repr(i32)]
enum Step {
    /// No bootstrap channel, or the first message was not START.
    Start = 1,
    /// START named a device that is not virtio-snd.
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

    /// The little-endian `u16` at `offset`, in one access: the other side may
    /// be writing it now, and two byte reads could take one byte from before
    /// its store and one from after.
    fn read_u16(&self, offset: usize) -> u16 {
        assert!(
            offset.checked_add(2).is_some_and(|end| end <= self.len),
            "a u16 inside the mapping"
        );
        let address = self.base + offset;
        assert!(address.is_multiple_of(2), "a u16 on its own alignment");
        // SAFETY: as for `read_u8`, and the two bytes are one aligned `u16`.
        u16::from_le(unsafe { ptr::read_volatile(address as *const u16) })
    }

    /// Write the little-endian `u16` at `offset`, in one access: the other
    /// side may read it at any moment, and must never see one byte changed.
    fn write_u16(&mut self, offset: usize, value: u16) {
        assert!(
            offset.checked_add(2).is_some_and(|end| end <= self.len),
            "a u16 inside the mapping"
        );
        let address = self.base + offset;
        assert!(address.is_multiple_of(2), "a u16 on its own alignment");
        // SAFETY: as for `write_u8`, and the two bytes are one aligned `u16`.
        unsafe { ptr::write_volatile(address as *mut u16, value.to_le()) }
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

/// Pin `pages` pages of `vmo` for `device`, and say where the device sees
/// each.
fn pin_pages(
    device: &Device<Kernel>,
    vmo: &Vmo<Kernel>,
    pages: usize,
    access: PinAccess,
    addresses: &mut [u64],
) -> Result<Pin<Kernel>, Step> {
    let pin = device
        .pin(vmo, 0, pages * PAGE, access)
        .map_err(|_| Step::Memory)?;
    let mut raw = [[0_u8; 8]; MAX_BUFFER_PAGES];
    let asked = raw.get_mut(..pages).ok_or(Step::Memory)?;
    let got = pin.addresses(asked).map_err(|_| Step::Memory)?;
    if !got.is_complete() || got.pages != pages {
        return Err(Step::Memory);
    }
    for (slot, bytes) in addresses.iter_mut().zip(raw.iter().take(pages)) {
        *slot = device_address(*bytes);
    }
    Ok(pin)
}

impl Pinned {
    fn new(device: &Device<Kernel>, pages: usize) -> Result<Pinned, Step> {
        let bytes = pages * PAGE;
        let vmo = vmo::create(Kernel, bytes).map_err(|_| Step::Memory)?;
        let mut addresses = [0_u64; QUEUE_PAGES];
        let slots = addresses.get_mut(..pages).ok_or(Step::Memory)?;
        let pin = pin_pages(device, &vmo, pages, PinAccess::ReadWrite, slots)?;
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

impl Scratch for Pinned {
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
// Each `u16` is one access, as the trait requires of the ring's indices.
unsafe impl QueueMemory for Pinned {
    fn read_u8(&self, offset: usize) -> u8 {
        self.mapped.read_u8(offset)
    }

    fn write_u8(&mut self, offset: usize, value: u8) {
        self.mapped.write_u8(offset, value);
    }

    fn read_u16(&self, offset: usize) -> u16 {
        self.mapped.read_u16(offset)
    }

    fn write_u16(&mut self, offset: usize, value: u16) {
        self.mapped.write_u16(offset, value);
    }

    fn barrier(&self) {
        fence(Ordering::SeqCst);
    }
}

/// A published stream's buffer: the core's VMO, pinned read-only for the
/// device and never mapped here.
struct Buffer {
    _pin: Pin<Kernel>,
    _vmo: Vmo<Kernel>,
    addresses: [u64; MAX_BUFFER_PAGES],
    pages: usize,
}

impl Buffer {
    fn pin(device: &Device<Kernel>, vmo: Vmo<Kernel>, bytes: u32) -> Result<Buffer, Step> {
        let pages = (bytes as usize).div_ceil(PAGE);
        if pages == 0 || pages > MAX_BUFFER_PAGES {
            return Err(Step::Control);
        }
        let mut addresses = [0_u64; MAX_BUFFER_PAGES];
        let slots = addresses.get_mut(..pages).ok_or(Step::Memory)?;
        let pin = pin_pages(device, &vmo, pages, PinAccess::ReadOnly, slots)?;
        Ok(Buffer {
            _pin: pin,
            _vmo: vmo,
            addresses,
            pages,
        })
    }

    fn pages(&self) -> &[u64] {
        self.addresses.get(..self.pages).unwrap_or_default()
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

/// The device as `ferrix-virtio-snd` drives it.
struct Registers {
    common: Block,
    notify: Block,
    isr: Block,
    device: Block,
    notify_off_multiplier: u32,
    msix: bool,
    interrupt: Interrupt<Kernel>,
    /// A port nothing is bound to, which a nap waits on until its deadline.
    nap: Port<Kernel>,
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

    /// Every queue interrupts through the first table entry, as input's do.
    fn queue_vector(&self, _queue: u16) -> u16 {
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

    /// Sleep a little rather than spin: the device's answer comes from
    /// QEMU's main loop, which a spinning processor only takes time from.
    fn spin(&mut self) {
        match ferrix_rt::linux::monotonic_nanos() {
            Ok(now) => {
                let _ = self.nap.wait(Deadline::At(now.saturating_add(NAP_NANOS)));
            }
            Err(_) => core::hint::spin_loop(),
        }
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

type Snd = Driver<Registers, Pinned, Pinned>;

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

/// Send HELLO and wait for READY, pinning the buffers it brings.
fn introduce(
    driver: &mut Snd,
    device: &Device<Kernel>,
    port: &Port<Kernel>,
    control: &Channel<Kernel>,
    location: u32,
) -> Result<[Option<Buffer>; MAX_PUBLISHED], Step> {
    let hello = driver.hello(location);
    let port_share = port
        .as_owned()
        .duplicate(Requested::Exactly(PORT_RIGHTS))
        .map_err(|_| Step::Hello)?;
    control
        .write_with(Message::Hello(hello).encode().as_bytes(), [port_share])
        .map_err(|_| Step::Hello)?;
    let buffers = ready(driver, device, control)?;
    control
        .wait_async(port, Signals::READABLE | Signals::PEER_CLOSED, KEY_CONTROL)
        .map_err(|_| Step::Events)?;
    Ok(buffers)
}

/// Take the core's answer to HELLO: READY and its buffers configure the
/// streams, and anything else ends the driver.
fn ready(
    driver: &mut Snd,
    device: &Device<Kernel>,
    control: &Channel<Kernel>,
) -> Result<[Option<Buffer>; MAX_PUBLISHED], Step> {
    let _ = control
        .wait_one(Signals::READABLE, Deadline::Never)
        .map_err(|_| Step::Hello)?;
    let mut bytes = [0_u8; MAX_BYTES];
    let mut handles = [Handle::INVALID; 1 + MAX_PUBLISHED];
    let received = control
        .read(&mut bytes, &mut handles)
        .map_err(|_| Step::Hello)?;
    let mut owned = handles
        .iter()
        .take(received.handles)
        .map(|handle| OwnedHandle::from_raw(Kernel, *handle));
    // The core's port comes first; nothing here writes to it yet.
    drop(owned.next());
    let decoded = Message::decode(bytes.get(..received.bytes).unwrap_or_default())
        .map_err(|_| Step::Control)?;
    let ready: Ready = match decoded {
        Message::Ready(ready) => ready,
        _ => return Err(Step::Control),
    };
    let mut buffers: [Option<Buffer>; MAX_PUBLISHED] = [None, None];
    for (slot, stream) in buffers
        .iter_mut()
        .zip(ready.streams.iter())
        .take(ready.published as usize)
    {
        let vmo = Vmo::from_owned(owned.next().ok_or(Step::Control)?);
        *slot = Some(Buffer::pin(device, vmo, stream.buffer_bytes)?);
    }
    let mut pages: [&[u64]; MAX_PUBLISHED] = [&[], &[]];
    for (slot, buffer) in pages.iter_mut().zip(buffers.iter()) {
        if let Some(buffer) = buffer {
            *slot = buffer.pages();
        }
    }
    let published = pages.get(..ready.published as usize).ok_or(Step::Control)?;
    driver
        .on_ready(&ready, published)
        .map_err(|_| Step::Faulted)?;
    Ok(buffers)
}

/// Send the core every message the driver has for it.
fn flush(driver: &mut Snd, control: &Channel<Kernel>) -> Result<(), Step> {
    while let Some(message) = driver.pop_message() {
        control
            .write(message.encode().as_bytes())
            .map_err(|_| Step::Control)?;
    }
    Ok(())
}

fn run(boot: &Channel<Kernel>) -> Result<(), Step> {
    let Started {
        start,
        device,
        control,
    } = started(boot)?;
    if start.pci_device_id != VIRTIO_SND_ID {
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
        nap: port::create(Kernel).map_err(|_| Step::Events)?,
    };
    let control_rings = Pinned::new(&device, QUEUE_PAGES)?;
    let tx_rings = Pinned::new(&device, QUEUE_PAGES)?;
    let scratch = Pinned::new(&device, SCRATCH_PAGES)?;
    let port = port::create(Kernel).map_err(|_| Step::Events)?;
    registers
        .interrupt
        .bind(&port, KEY_INTERRUPT)
        .map_err(|_| Step::Events)?;
    let mut driver = match Driver::init(
        Parts {
            transport: registers,
            control: control_rings,
            tx: tx_rings,
            scratch,
        },
        Options {
            control_polls: CONTROL_POLLS,
            ..Options::default()
        },
    ) {
        Ok(driver) => driver,
        Err(failure) => {
            drop(failure);
            return Err(Step::Device);
        }
    };

    let buffers = match introduce(&mut driver, &device, &port, &control, start.location) {
        Ok(buffers) => buffers,
        Err(step) => {
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

    let ended = serve(&mut driver, &port, &control);
    match driver.shutdown() {
        Teardown::Released(released) => {
            drop(released);
            drop(buffers);
            if matches!(ended, Ok(true)) {
                let _ = control.write(Message::Stopped.encode().as_bytes());
            }
            ended.map(drop)
        }
        Teardown::Wedged(kept) => {
            // The device may still read the buffers and write the scratch
            // memory: both stay.
            let _kept_for_good = ManuallyDrop::new(kept);
            let _buffers_for_good = ManuallyDrop::new(buffers);
            Err(Step::Wedged)
        }
    }
}

/// Serve the device until the core stops it or goes away.
///
/// `Ok(true)` when the core asked for a stop, which is answered with STOPPED.
fn serve(driver: &mut Snd, port: &Port<Kernel>, control: &Channel<Kernel>) -> Result<bool, Step> {
    loop {
        let packet = match port.wait(watch(driver)) {
            Ok(packet) => packet,
            Err(Error::TimedOut) => {
                driver.check_needs_reset().map_err(|_| Step::Faulted)?;
                continue;
            }
            Err(_) => return Err(Step::Events),
        };
        match (packet.kind, packet.key) {
            (PACKET_INTERRUPT, KEY_INTERRUPT) => {
                let _ = driver.on_interrupt().map_err(|_| Step::Faulted)?;
                flush(driver, control)?;
            }
            (PACKET_SIGNAL, KEY_CONTROL) => {
                if let Some(stop) = take_control(driver, control)? {
                    return Ok(stop);
                }
                control
                    .wait_async(port, Signals::READABLE | Signals::PEER_CLOSED, KEY_CONTROL)
                    .map_err(|_| Step::Events)?;
            }
            _ => {}
        }
    }
}

/// Follow every message the core has sent, until there is none left.
///
/// `Some(true)` for a STOP, `Some(false)` for the core going away or
/// refusing, and `None` once nothing is left to read.
fn take_control(driver: &mut Snd, control: &Channel<Kernel>) -> Result<Option<bool>, Step> {
    loop {
        let mut bytes = [0_u8; MAX_BYTES];
        let mut handles = [Handle::INVALID; 1];
        let received = match control.read(&mut bytes, &mut handles) {
            Ok(received) => received,
            Err(ReadError::Failed(Error::PeerClosed)) => return Ok(Some(false)),
            Err(ReadError::Failed(Error::ShouldWait)) => return Ok(None),
            Err(_) => return Err(Step::Control),
        };
        for handle in handles.iter().take(received.handles) {
            drop(OwnedHandle::from_raw(Kernel, *handle));
        }
        let decoded = Message::decode(bytes.get(..received.bytes).unwrap_or_default())
            .map_err(|_| Step::Control)?;
        match driver.on_control(&decoded) {
            Ok(Control::Stop) => return Ok(Some(true)),
            Ok(Control::Refused(_)) => return Ok(Some(false)),
            Ok(Control::Followed) => flush(driver, control)?,
            Err(_) => return Err(Step::Faulted),
        }
    }
}

/// How long to wait for the device: for good with nothing in flight, and
/// otherwise [`WATCH_NANOS`], after which the driver looks at the device
/// status itself -- an interrupt that brought completions does not
/// (`Driver::on_interrupt`), so a reset announced in one could otherwise
/// leave the core waiting for ELAPSED that never comes.
fn watch(driver: &Snd) -> Deadline {
    if driver.in_flight() == 0 {
        return Deadline::Never;
    }
    match ferrix_rt::linux::monotonic_nanos() {
        Ok(now) => Deadline::At(now.saturating_add(WATCH_NANOS)),
        Err(_) => Deadline::Never,
    }
}
