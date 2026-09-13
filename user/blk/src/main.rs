//! The virtio-blk driver process: a ring-3 program that serves one disk to
//! the kernel over the block ring.
//!
//! Everything that knows anything is a library. `ferrix-virtio-blk` drives
//! the device, `ferrix-blkring` speaks the ring, and `ferrix-blkserve` joins
//! the two; all three are tested on the host. This program is the handles
//! those libraries were written to be wrapped in, and nothing else:
//!
//! 1. The bootstrap channel carries START (`docs/BLOCK-RING.md` §6): the
//!    device handle, the driver's end of the ring's control channel, and
//!    where the device's virtio register blocks are. A driver that gets
//!    anything else first exits.
//! 2. Each register block is an `IoMapping` of the pages holding it, and the
//!    [`Transport`] the device library asks for is volatile reads and writes
//!    into those mappings.
//! 3. The device's memory — its rings, the request headers and status bytes,
//!    and the data region the kernel fills and drains — is VMOs this process
//!    creates, pins into the device's IOMMU domain with `VMO_PIN`, and maps
//!    into itself with `vmo_map`. Device addresses come only from the pin's
//!    address query; the [`DevicePages`] the library sees are those.
//! 4. The block ring is one more VMO, mapped here and sent to the kernel in
//!    HELLO with the data VMO and a port the kernel rings. READY brings the
//!    kernel's completion port back.
//! 5. One port carries every event: the kernel's bell (`BELL_SUBMIT`), the
//!    device's interrupt, and the control channel becoming readable. The
//!    serve loop decides what each means.
//! 6. STOP ends it: the device is reset, whatever it abandoned is completed
//!    `IoError` on the ring, STOPPED goes back, and the process exits. A
//!    device that will not reset keeps its memory forever — the pins are
//!    never released — and the process exits without STOPPED, which tells
//!    the kernel to reset through its own means.
//!
//! The exit status is the diagnosis: 0 is a clean STOP, and every other
//! number names the step that failed (see [`Step`]), so a boot check that
//! started the process can say what went wrong without a console.

#![no_std]
#![no_main]

use core::ptr;
use core::sync::atomic::{Ordering, fence};

use ferrix_blkring::RingMemory;
use ferrix_blkring::bell::{BELL_SUBMIT, Doorbell, Wait};
use ferrix_blkring::control::{MAX_BYTES, Message, PORT_RIGHTS, START_BYTES, Start, VMO_RIGHTS};
use ferrix_blkring::driver::DriverSide;
use ferrix_blkring::geometry::{Device as Geometry, DeviceFlags};
use ferrix_blkring::identity::{DiskName, Identity, Location, SERIAL_BYTES};
use ferrix_blkring::layout::{RingLayout, Status as RingStatus};
use ferrix_blkserve::{Fault, Serve};
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::rights::Requested;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::{IoMappingSpec, PACKET_INTERRUPT, PACKET_SIGNAL, PACKET_USER};
use ferrix_rt::native::channel::Channel;
use ferrix_rt::native::channel::ReadError;
use ferrix_rt::native::device::{Device, Interrupt, IoMapping};
use ferrix_rt::native::error::Error;
use ferrix_rt::native::handle::{Deadline, Object, OwnedHandle};
use ferrix_rt::native::pending::Protection;
use ferrix_rt::native::pin::{Pin, PinAccess, device_address};
use ferrix_rt::native::port::{self, Port};
use ferrix_rt::native::vmo::{self, Vmo};
use ferrix_rt::{Bootstrap, Kernel};
use ferrix_virtio::QueueMemory;
use ferrix_virtio::blk::DeviceConfig;
use ferrix_virtio::pci::{CommonConfig, NO_VECTOR};
use ferrix_virtio_blk::{
    Completion, DevicePages, Driver, ISR_QUEUE, Options, Parts, RequestArea, Slot, Status,
    Teardown, Transport,
};

ferrix_rt::entry!(main);

/// A page, on every architecture this runs on.
const PAGE: usize = 4096;

/// Submission and completion entries in the ring: what the kernel may have
/// outstanding, and what the serve loop may hold while the device's queue is
/// full.
const ENTRIES: u32 = 64;

/// Bytes in the ring VMO: one page holds 64 entries of each kind and the
/// header with room to spare.
const RING_BYTES: usize = PAGE;

/// The most descriptors the device queue is asked for.
const QUEUE_SIZE: u16 = 128;

/// Pages of queue memory: the rings for [`QUEUE_SIZE`] descriptors fit in
/// one page; two leaves room for a larger queue if the device offers one.
const QUEUE_PAGES: usize = 2;

/// Pages of request area: a header and a status byte per in-flight chain.
const AREA_PAGES: usize = 1;

/// Pages in the data region the kernel fills and drains: 512 KiB, four of
/// the largest requests at a time.
const DATA_PAGES: usize = 128;

/// The most sectors one request may carry: 128 KiB at 512-byte sectors, and
/// fewer if the device's segment limits say so.
const MAX_SECTORS_CAP: u32 = 256;

/// PCI device identifiers a virtio-blk function has: modern and transitional.
const VIRTIO_BLK_IDS: [u16; 2] = [0x1042, 0x1001];

/// Port keys of this process's own: the ring's are 1 and 2.
const KEY_INTERRUPT: u64 = 3;
const KEY_CONTROL: u64 = 4;

/// Where a run stopped, as the exit status.
#[derive(Clone, Copy, Debug)]
#[repr(i32)]
enum Step {
    /// No bootstrap channel, or the first message was not START.
    Start = 1,
    /// START named a device that is not virtio-blk, or a name devmgr would
    /// not give.
    Identity = 2,
    /// A register block could not be mapped.
    Registers = 3,
    /// Memory could not be made, pinned or mapped.
    Memory = 4,
    /// The device would not come up.
    Device = 5,
    /// The ring could not be set up, or HELLO was not accepted.
    Ring = 6,
    /// The port, the interrupt or the waits could not be arranged.
    Events = 7,
    /// The serve loop stopped on a corrupt ring or a broken device.
    Faulted = 8,
    /// STOP came, but the device would not reset: its memory is kept.
    Wedged = 9,
    /// The control channel closed or failed.
    Control = 10,
}

fn main(bootstrap: Bootstrap) -> i32 {
    let Some(control_boot) = bootstrap else {
        return Step::Start as i32;
    };
    match run(&control_boot) {
        Ok(()) => 0,
        Err(step) => step as i32,
    }
}

// ---------------------------------------------------------------------------
// Memory this process shares with the device or the kernel
// ---------------------------------------------------------------------------

/// A VMO mapped into this process: a byte range volatile accesses go to.
struct Mapped {
    base: usize,
    len: usize,
}

impl Mapped {
    fn read_u8(&self, offset: usize) -> u8 {
        assert!(offset < self.len, "a read inside the mapping");
        // SAFETY: `base..base + len` is a mapping the kernel made for this
        // process and never takes away while the VMO handle lives; the
        // offset was checked. Volatile, because the other side of the
        // mapping is a device or the kernel.
        unsafe { ptr::read_volatile((self.base + offset) as *const u8) }
    }

    fn write_u8(&mut self, offset: usize, value: u8) {
        assert!(offset < self.len, "a write inside the mapping");
        // SAFETY: as for `read_u8`, and the mapping is writable.
        unsafe { ptr::write_volatile((self.base + offset) as *mut u8, value) }
    }
}

/// A VMO this process made, pinned for the device and mapped for itself.
struct Pinned {
    /// Kept for its life: dropping it unpins, which must never happen while
    /// the device may still write.
    _pin: Pin<Kernel>,
    _vmo: Vmo<Kernel>,
    mapped: Mapped,
    addresses: [u64; DATA_PAGES],
    pages: usize,
}

impl Pinned {
    /// `pages` pages, pinned into `device`'s domain and mapped read-write.
    fn new(device: &Device<Kernel>, pages: usize) -> Result<Pinned, Step> {
        let bytes = pages * PAGE;
        let vmo = vmo::create(Kernel, bytes).map_err(|_| Step::Memory)?;
        let pin = device
            .pin(&vmo, 0, bytes, PinAccess::ReadWrite)
            .map_err(|_| Step::Memory)?;
        let mut raw = [[0_u8; 8]; DATA_PAGES];
        let asked = raw.get_mut(..pages).ok_or(Step::Memory)?;
        let got = pin.addresses(asked).map_err(|_| Step::Memory)?;
        if !got.is_complete() || got.pages != pages {
            return Err(Step::Memory);
        }
        let mut addresses = [0_u64; DATA_PAGES];
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

impl Pinned {
    /// A handle to the VMO with exactly the rights HELLO's data VMO carries.
    fn share(&self) -> Result<OwnedHandle<Kernel>, Step> {
        self._vmo
            .as_owned()
            .duplicate(Requested::Exactly(VMO_RIGHTS))
            .map_err(|_| Step::Ring)
    }
}

impl DevicePages for Pinned {
    fn device_pages(&self) -> &[u64] {
        self.addresses.get(..self.pages).unwrap_or_default()
    }
}

impl RequestArea for Pinned {
    fn read_u8(&self, offset: usize) -> u8 {
        self.mapped.read_u8(offset)
    }

    fn write_u8(&mut self, offset: usize, value: u8) {
        self.mapped.write_u8(offset, value);
    }
}

// SAFETY: the memory is one VMO, pinned so the device's view of it is the
// pages `device_pages` names, mapped so this process's view is `mapped`, and
// both stay put until the `Pinned` is dropped, which the driver's teardown
// rule forbids while the device may write. Every access is volatile and
// `barrier` is a full fence, so what the device sees is what was written.
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

/// The ring VMO as the ring library reads it: mapped here, shared with the
/// kernel, never pinned.
struct Ring {
    _vmo: Vmo<Kernel>,
    mapped: Mapped,
}

impl RingMemory for Ring {
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

/// One virtio register block: the mapping of the pages holding it, and where
/// in them it starts.
struct Block {
    _mapping: IoMapping<Kernel>,
    base: usize,
    len: usize,
}

impl Block {
    /// Map the pages START says hold the block.
    fn map(device: &Device<Kernel>, block: &ferrix_blkring::control::Block) -> Result<Block, Step> {
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
        // SAFETY: the block is mapped device memory the kernel gave this
        // process, the offset is inside it and aligned for `T`, and the
        // access is volatile, as a register read must be.
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

/// The device as `ferrix-virtio-blk` drives it.
struct Registers {
    common: Block,
    notify: Block,
    isr: Block,
    device: Block,
    notify_off_multiplier: u32,
    /// The request queue's MSI-X entry, or the line.
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
            // Re-arm the entry; a failure here leaves the next interrupt
            // undelivered, which the serve loop's next wait shows as a hang,
            // and nothing better can be done from inside the driver.
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

/// What START handed over.
struct Started {
    start: Start,
    device: Device<Kernel>,
    control: Channel<Kernel>,
}

/// Read START off the bootstrap channel.
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
    match Message::decode(bytes.get(..received.bytes).unwrap_or_default()) {
        Ok(Message::Start(start)) => Ok(Started {
            start,
            device,
            control,
        }),
        _ => Err(Step::Start),
    }
}

/// The disk's identity as HELLO carries it: START's location and name, and
/// the serial the device reported, if it did.
fn identity(start: &Start, serial: [u8; SERIAL_BYTES]) -> Result<Identity, Step> {
    let name = DiskName::new(start.name).ok_or(Step::Identity)?;
    Ok(Identity {
        location: Location(start.location),
        serial,
        name,
    })
}

/// The most sectors a request may carry on this device with this data
/// region: what fits the device's segment limit, page by page, with one
/// page of slack for a region that does not start on a page.
fn max_sectors(max_segments: u32, block_size: u32) -> u32 {
    let pages = max_segments.saturating_sub(1).clamp(1, DATA_PAGES as u32);
    let bytes = pages.saturating_mul(PAGE as u32);
    (bytes / block_size.max(1)).clamp(1, MAX_SECTORS_CAP)
}

fn run(boot: &Channel<Kernel>) -> Result<(), Step> {
    let Started {
        start,
        device,
        control,
    } = started(boot)?;
    if !VIRTIO_BLK_IDS.contains(&start.pci_device_id) {
        return Err(Step::Identity);
    }

    // The registers, and the interrupt they will raise.
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

    let (driver, data_share, port, geometry) = bring_up(&device, registers)?;

    // The ring, and HELLO with the ring VMO, the data VMO and the port.
    let ring_vmo = vmo::create(Kernel, RING_BYTES).map_err(|_| Step::Ring)?;
    let ring_share = ring_vmo
        .as_owned()
        .duplicate(Requested::Exactly(VMO_RIGHTS))
        .map_err(|_| Step::Ring)?;
    let port_share = port
        .as_owned()
        .duplicate(Requested::Exactly(PORT_RIGHTS))
        .map_err(|_| Step::Ring)?;
    let ring_base = ring_vmo
        .map(None, RING_BYTES, Protection::ReadWrite, 0)
        .map_err(|_| Step::Ring)?;
    let ring = Ring {
        _vmo: ring_vmo,
        mapped: Mapped {
            base: ring_base,
            len: RING_BYTES,
        },
    };
    let layout = RingLayout::standard(ENTRIES).map_err(|_| Step::Ring)?;
    let side =
        DriverSide::new(ring, RING_BYTES as u64, layout, geometry).map_err(|_| Step::Ring)?;
    let hello = Message::Hello(side.hello(&identity(&start, [0; SERIAL_BYTES])?)).encode();
    control
        .write_with(hello.as_bytes(), [ring_share, data_share, port_share])
        .map_err(|_| Step::Ring)?;
    let kernel_port = ready(&control)?;

    // The control channel's readiness is the last event source.
    control
        .wait_async(&port, Signals::READABLE, KEY_CONTROL)
        .map_err(|_| Step::Events)?;

    // Serve until STOP, then take the device down.
    let mut serve: Loop = Serve::new(side, driver);
    let ended = serve_until(&mut serve, &port, &kernel_port, &control);
    let stopped = matches!(ended, Ok(Ended::Stop));
    teardown(serve, &control, &kernel_port, stopped)?;
    ended.map(|_| ())
}

/// The device's memory, its bring-up, and what HELLO needs to know: the
/// driver, the data VMO's handle for HELLO, the port every event arrives on
/// and the disk's geometry as the ring announces it.
fn bring_up(
    device: &Device<Kernel>,
    registers: Registers,
) -> Result<(Disk, OwnedHandle<Kernel>, Port<Kernel>, Geometry), Step> {
    // The device's memory. The data VMO's handle for HELLO is taken before
    // the driver owns the memory.
    let rings = Pinned::new(device, QUEUE_PAGES)?;
    let area = Pinned::new(device, AREA_PAGES)?;
    let data = Pinned::new(device, DATA_PAGES)?;
    let data_share = data.share()?;
    let slots = [Slot::EMPTY; QUEUE_SIZE as usize];

    // The port every event arrives on, with the interrupt bound to it before
    // the driver owns that too.
    let port = port::create(Kernel).map_err(|_| Step::Events)?;
    registers
        .interrupt
        .bind(&port, KEY_INTERRUPT)
        .map_err(|_| Step::Events)?;

    // Bring the device up.
    let options = Options {
        max_queue_size: QUEUE_SIZE,
        ..Options::default()
    };
    let driver = match Driver::init(
        Parts {
            transport: registers,
            rings,
            area,
            data,
            slots,
        },
        options,
    ) {
        Ok(driver) => driver,
        Err(failure) => {
            // Dropping the failure drops the parts only if the reset after
            // it finished: a wedged device's memory is in `ManuallyDrop` and
            // stays pinned for good, which is the library's rule.
            drop(failure);
            return Err(Step::Device);
        }
    };
    let info = *driver.info();
    let block_size = info.limits.block_size;
    let sectors = max_sectors(info.limits.max_segments, block_size);
    let capacity = info.limits.capacity / u64::from(block_size / 512).max(1);
    let mut flags = DeviceFlags::default();
    if info.read_only {
        flags = flags.union(DeviceFlags::READ_ONLY);
    }
    if info.flush {
        flags = flags.union(DeviceFlags::FLUSH);
    }
    let geometry = Geometry::new(
        block_size,
        capacity,
        sectors,
        flags,
        (DATA_PAGES * PAGE) as u64,
    )
    .map_err(|_| Step::Ring)?;

    Ok((driver, data_share, port, geometry))
}

/// The device as the loop drives it.
type Disk = Driver<Registers, Pinned, Pinned, Pinned, [Slot; QUEUE_SIZE as usize]>;

/// The loop, with a pending store as deep as the ring.
type Loop = Serve<Ring, Disk, { ENTRIES as usize }>;

/// Why the loop ended.
enum Ended {
    /// The kernel said STOP.
    Stop,
    /// The kernel's end of the control channel went away.
    ControlGone,
}

/// Wait for READY, and take the kernel's completion port from it.
fn ready(control: &Channel<Kernel>) -> Result<Port<Kernel>, Step> {
    let _ = control
        .wait_one(Signals::READABLE, Deadline::Never)
        .map_err(|_| Step::Ring)?;
    let mut bytes = [0_u8; MAX_BYTES];
    let mut handles = [Handle::INVALID; 1];
    let received = control
        .read(&mut bytes, &mut handles)
        .map_err(|_| Step::Ring)?;
    match Message::decode(bytes.get(..received.bytes).unwrap_or_default()) {
        Ok(Message::Ready) if received.handles == 1 => {
            Ok(Port::from_owned(OwnedHandle::from_raw(Kernel, handles[0])))
        }
        _ => Err(Step::Ring),
    }
}

/// Ring the kernel's completion port, if the loop says to. A port that is
/// full has a bell queued already, and a bell is only a hint, so a refusal
/// changes nothing.
fn ring_bell(kernel_port: &Port<Kernel>, bell: Option<Doorbell>) {
    if let Some(bell) = bell {
        let _ = kernel_port.queue(bell.key(), bell.packet().data);
    }
}

/// The loop: consume, serve, sleep, until STOP or a fault.
fn serve_until(
    serve: &mut Loop,
    port: &Port<Kernel>,
    kernel_port: &Port<Kernel>,
    control: &Channel<Kernel>,
) -> Result<Ended, Step> {
    let mut out = [Completion {
        id: 0,
        status: Status::Ok,
        bytes: 0,
    }; 32];
    let faulted = |_: Fault| Step::Faulted;
    ring_bell(kernel_port, serve.on_bell().map_err(faulted)?);
    loop {
        match serve.before_sleep().map_err(faulted)? {
            Wait::Pending(_) => {
                ring_bell(kernel_port, serve.on_bell().map_err(faulted)?);
                continue;
            }
            Wait::Sleep => {}
        }
        let packet = port.wait(Deadline::Never).map_err(|_| Step::Events)?;
        match (packet.kind, packet.key) {
            (PACKET_USER, BELL_SUBMIT) => {
                ring_bell(kernel_port, serve.on_bell().map_err(faulted)?);
            }
            (PACKET_INTERRUPT, KEY_INTERRUPT) => {
                ring_bell(kernel_port, serve.on_interrupt(&mut out).map_err(faulted)?);
            }
            (PACKET_SIGNAL, KEY_CONTROL) => {
                let mut bytes = [0_u8; MAX_BYTES];
                match control.read(&mut bytes, &mut []) {
                    Ok(received) => {
                        if let Ok(Message::Stop) =
                            Message::decode(bytes.get(..received.bytes).unwrap_or_default())
                        {
                            return Ok(Ended::Stop);
                        }
                        // Anything else on the control channel after READY
                        // is not the kernel's; the registration is one-shot,
                        // so arm it again.
                        control
                            .wait_async(port, Signals::READABLE, KEY_CONTROL)
                            .map_err(|_| Step::Events)?;
                    }
                    Err(ReadError::Failed(Error::PeerClosed)) => return Ok(Ended::ControlGone),
                    Err(_) => {
                        control
                            .wait_async(port, Signals::READABLE, KEY_CONTROL)
                            .map_err(|_| Step::Events)?;
                    }
                }
            }
            _ => {}
        }
    }
}

/// Take the device down: reset it, complete on the ring whatever the reset
/// abandoned, and say STOPPED if STOP was asked. A device that will not
/// reset keeps every page pinned for good and the answer is the exit status.
fn teardown(
    serve: Loop,
    control: &Channel<Kernel>,
    kernel_port: &Port<Kernel>,
    stopped: bool,
) -> Result<(), Step> {
    let (mut side, driver) = serve.into_parts();
    match driver.shutdown() {
        Teardown::Released(released) => {
            for id in released.abandoned() {
                let _ = side.complete(id, RingStatus::IoError, 0);
            }
            ring_bell(kernel_port, side.publish());
            drop(released);
            if stopped {
                control
                    .write(Message::Stopped.encode().as_bytes())
                    .map_err(|_| Step::Control)?;
            }
            Ok(())
        }
        Teardown::Wedged(kept) => {
            // `ManuallyDrop`: letting the wrapper go leaks the memory, pins
            // and all, which is what a device that may still write requires.
            let _kept_for_good = kept;
            Err(Step::Wedged)
        }
    }
}
