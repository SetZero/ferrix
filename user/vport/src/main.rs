//! The virtio-console driver process: a ring-3 program that carries one
//! virtio-serial port's bytes between the device and a Unix socket.
//!
//! `docs/CLIPBOARD.md` §6 is what this is. `ferrix-virtio-console` drives the
//! device -- bring-up, the control conversation of §3.3, and the byte paths
//! in both directions -- and is tested on the host against a fake device.
//! This program is the handles that library was written to be wrapped in, and
//! a socket:
//!
//! 1. The bootstrap channel carries START: the device, and where its virtio
//!    register blocks are. There is no second handle and no kernel end,
//!    because §5 gave the port no kernel representation at all.
//! 2. Each register block is an `IoMapping`; the six queues' rings, the
//!    control area and both data regions are VMOs this process creates, pins
//!    read-write into the device's domain and maps.
//! 3. The driver comes up and the control conversation runs until the port
//!    named `com.redhat.spice.0` is open. Nothing crosses before that.
//! 4. A Unix socket is bound at [`ferrix_vdagent::SOCKET_PATH`] and listens.
//!    Whatever arrives on the port is written to whoever is connected, and
//!    whatever they write goes out on the port. This program understands
//!    nothing of vdagent: it is a pipe with a device on one end.
//!
//! # Why the loop polls
//!
//! It waits on two things that cannot be waited on together: the device's
//! interrupt, which is a native port packet, and the socket, which is a Linux
//! descriptor. The kernel has no bridge between the two tables, so the socket
//! is non-blocking and the port wait carries a deadline
//! ([`POLL_NANOS`]) rather than `Never`. A clipboard is not a data path and a
//! millisecond of latency on a paste is not a cost anyone can feel; a bridge,
//! if one is ever built, turns this back into a blocking wait and changes
//! nothing else.
//!
//! # Why one chunk is in flight at a time
//!
//! The agent frames vdagent into chunks of at most 1032 bytes and the whole
//! clipboard is at most a megabyte, so a second chunk in flight would buy a
//! round trip on a path that makes a few hundred of them while a person is
//! not looking. One chunk means one offset in the transmit region and no
//! allocator for it.
//!
//! The exit status names the step that failed ([`Step`]); a clean end is the
//! device removing the port, which is 0.

#![no_std]
#![no_main]

use core::ptr;
use core::sync::atomic::{Ordering, fence};

use ferrix_blkring::control::{Message as StartMessage, START_BYTES};
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::socket::{AF_UNIX, SOCK_CLOEXEC, SOCK_NONBLOCK, SOCK_STREAM};
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::{DeviceBlock, DeviceInfo, IoMappingSpec};
use ferrix_rt::native::channel::Channel;
use ferrix_rt::native::device::{Device, Interrupt, IoMapping};
use ferrix_rt::native::error::Error;
use ferrix_rt::native::handle::{Deadline, Object, OwnedHandle};
use ferrix_rt::native::linux::{self, Fd};
use ferrix_rt::native::pending::Protection;
use ferrix_rt::native::pin::{Pin, PinAccess, device_address};
use ferrix_rt::native::port::{self, Port};
use ferrix_rt::native::vmo::{self, Vmo};
use ferrix_rt::{Bootstrap, Kernel};
use ferrix_virtio::DeviceConfig;
use ferrix_virtio::QueueMemory;
use ferrix_virtio::pci::{CommonConfig, NO_VECTOR};
use ferrix_virtio_console::{
    Bytes, CONTROL_SLOT, CONTROL_SLOTS, Chunk, DevicePages, Driver, Event, ISR_QUEUE, Options,
    Parts, Port as PortState, QUEUE_COUNT, Slot, SubmitError, Transport,
};

ferrix_rt::entry!(main);

/// A page, on every architecture this runs on.
const PAGE: usize = 4096;

/// virtio-console's modern PCI device id, which `docs/CLIPBOARD.md` §3.1
/// names, and the transitional one beside it.
const VIRTIO_CONSOLE_IDS: [u16; 2] = [0x1043, 0x1003];

/// The most descriptors each queue is asked for.
const QUEUE_SIZE: u16 = 64;

/// Pages of ring memory per queue: a 64-entry split queue's three rings fit
/// in one page, and two leaves room for the alignment the layout asks for.
const QUEUE_PAGES: usize = 2;

/// Pages of control area: [`CONTROL_SLOTS`] slots of [`CONTROL_SLOT`] bytes.
const CONTROL_PAGES: usize = (CONTROL_SLOTS as usize * CONTROL_SLOT).div_ceil(PAGE);

/// Bytes from one receive buffer to the next.
const RECEIVE_STRIDE: u32 = 512;

/// Pages of receive data: half the queue at [`RECEIVE_STRIDE`].
const RECEIVE_PAGES: usize = (QUEUE_SIZE as usize / 2) * RECEIVE_STRIDE as usize / PAGE;

/// Pages of transmit data: one chunk is in flight, and it starts at zero.
const TRANSMIT_PAGES: usize = 1;

/// The largest chunk read from the socket in one go, which is the transmit
/// region: an agent's frame is 1032 bytes, so this is several of them.
const TRANSMIT_BYTES: usize = TRANSMIT_PAGES * PAGE;

/// The largest region any pin covers, which sizes the address array.
const MAX_PAGES: usize = 8;

/// Bookkeeping slots the device library needs: one bank per queue.
const SLOTS: usize = QUEUE_COUNT * QUEUE_SIZE as usize;

/// Events taken from the device in one drain.
const EVENTS: usize = 8;

/// Bytes held on their way to the socket: one drain's worth of receive
/// buffers, so a drain never has more to put down than there is room for.
const STAGING_BYTES: usize = EVENTS * RECEIVE_STRIDE as usize;

/// How long the loop sleeps on the port before looking at the socket again.
const POLL_NANOS: u64 = 1_000_000;

/// Connections waiting to be accepted: the agent is the only client there
/// ever is, and a second would be a bug worth refusing rather than queueing.
const BACKLOG: i32 = 1;

/// The port key the device's interrupt arrives under.
const KEY_INTERRUPT: u64 = 1;

/// Where a run stopped, as the exit status.
#[derive(Clone, Copy, Debug)]
#[repr(i32)]
enum Step {
    /// No bootstrap channel, or the first message was not START.
    Start = 1,
    /// START named a device that is not virtio-console.
    Identity = 2,
    /// A register block could not be mapped, or the interrupt claimed.
    Registers = 3,
    /// A VMO could not be made, pinned or mapped.
    Memory = 4,
    /// The device would not come up.
    Device = 5,
    /// The socket could not be made, bound or listened on.
    Socket = 6,
    /// The device broke the protocol while the port was being carried.
    Carry = 7,
    /// The device would not reset.
    Reset = 8,
}

/// The program.
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
// Memory this process shares with the device
// ---------------------------------------------------------------------------

/// A VMO mapped into this process: a byte range volatile accesses go to.
struct Mapped {
    /// Where the mapping starts.
    base: usize,
    /// How long it is.
    len: usize,
}

impl Mapped {
    /// The byte at `offset`, or zero outside.
    fn read_u8(&self, offset: usize) -> u8 {
        if offset >= self.len {
            return 0;
        }
        // SAFETY: `base..base + len` is a mapping the kernel made for this
        // process and never takes away while the VMO handle lives; the offset
        // was checked. Volatile, because the other side is a device.
        unsafe { ptr::read_volatile((self.base + offset) as *const u8) }
    }

    /// Write the byte at `offset`, or nothing outside.
    fn write_u8(&mut self, offset: usize, value: u8) {
        if offset >= self.len {
            return;
        }
        // SAFETY: as for `read_u8`, and the mapping is writable.
        unsafe { ptr::write_volatile((self.base + offset) as *mut u8, value) }
    }
}

/// A VMO this process made, pinned for the device and mapped for itself.
struct Pinned {
    /// Kept for its life: dropping it unpins, which must never happen while
    /// the device may still write.
    _pin: Pin<Kernel>,
    /// Kept so the mapping stays.
    _vmo: Vmo<Kernel>,
    /// This process's view.
    mapped: Mapped,
    /// The device's view, page by page.
    addresses: [u64; MAX_PAGES],
    /// How many of those are in use.
    pages: usize,
}

impl Pinned {
    /// `pages` pages, pinned into `device`'s domain and mapped read-write.
    fn new(device: &Device<Kernel>, pages: usize) -> Result<Pinned, Step> {
        if pages > MAX_PAGES {
            return Err(Step::Memory);
        }
        let bytes = pages * PAGE;
        let vmo = vmo::create(Kernel, bytes).map_err(|_| Step::Memory)?;
        let pin = device
            .pin(&vmo, 0, bytes, PinAccess::ReadWrite)
            .map_err(|_| Step::Memory)?;
        let mut raw = [[0_u8; 8]; MAX_PAGES];
        let asked = raw.get_mut(..pages).ok_or(Step::Memory)?;
        let got = pin.addresses(asked).map_err(|_| Step::Memory)?;
        if !got.is_complete() || got.pages != pages {
            return Err(Step::Memory);
        }
        let mut addresses = [0_u64; MAX_PAGES];
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

    /// A window onto this region, for the caller that owns its bytes.
    fn window(&self) -> Window {
        Window {
            base: self.mapped.base,
            len: self.mapped.len,
        }
    }
}

impl DevicePages for Pinned {
    fn device_pages(&self) -> &[u64] {
        self.addresses.get(..self.pages).unwrap_or_default()
    }
}

impl Bytes for Pinned {
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

/// A window onto a mapping the driver owns.
///
/// Both data regions are moved into the driver, which holds their VMOs and so
/// their mappings but gives no way to read or write their bytes: it builds
/// descriptors and leaves the payload to whoever asked for it.
#[derive(Clone, Copy)]
struct Window {
    /// Where the mapping starts.
    base: usize,
    /// How long it is.
    len: usize,
}

impl Window {
    /// Copy `bytes` to `offset`, or nothing if they do not fit.
    fn write(&self, offset: u64, bytes: &[u8]) {
        let Ok(at) = usize::try_from(offset) else {
            return;
        };
        if at.saturating_add(bytes.len()) > self.len {
            return;
        }
        for (index, byte) in bytes.iter().enumerate() {
            // SAFETY: the address is inside a mapping the driver holds for as
            // long as this `Window` is used, and the range was checked.
            // Volatile, because the device reads the same bytes.
            unsafe { ptr::write_volatile((self.base + at + index) as *mut u8, *byte) };
        }
    }

    /// Fill `out` from `offset`, or with zeroes if it does not fit.
    fn read(&self, offset: u64, out: &mut [u8]) {
        let Ok(at) = usize::try_from(offset) else {
            return;
        };
        if at.saturating_add(out.len()) > self.len {
            return;
        }
        for (index, byte) in out.iter_mut().enumerate() {
            // SAFETY: as for `write`; the device may be writing the byte at
            // any moment, so it is read volatilely and taken as data.
            *byte = unsafe { ptr::read_volatile((self.base + at + index) as *const u8) };
        }
    }
}

// ---------------------------------------------------------------------------
// The device's registers
// ---------------------------------------------------------------------------

/// One virtio register block: the mapping of the pages holding it, and where
/// in them it starts.
struct Block {
    /// Kept so the mapping stays.
    _mapping: IoMapping<Kernel>,
    /// Where the block itself begins.
    base: usize,
    /// How long it is.
    len: usize,
}

impl Block {
    /// Map the pages `device_info` says hold the block.
    fn map(device: &Device<Kernel>, block: &DeviceBlock) -> Result<Block, Step> {
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

    /// A register, read volatilely.
    fn read<T: Copy>(&self, offset: u32) -> T {
        let offset = offset as usize;
        assert!(
            offset + size_of::<T>() <= self.len && offset.is_multiple_of(size_of::<T>()),
            "a register inside the block, aligned"
        );
        // SAFETY: the block is mapped device memory the kernel gave this
        // process, the offset is inside it and aligned for `T`, and the access
        // is volatile, as a register read must be.
        unsafe { ptr::read_volatile((self.base + offset) as *const T) }
    }

    /// A register, written volatilely.
    fn write<T: Copy>(&mut self, offset: u32, value: T) {
        let offset = offset as usize;
        assert!(
            offset + size_of::<T>() <= self.len && offset.is_multiple_of(size_of::<T>()),
            "a register inside the block, aligned"
        );
        // SAFETY: as for `read`, and the block is writable.
        unsafe { ptr::write_volatile((self.base + offset) as *mut T, value) }
    }
}

/// The four blocks, the notification arithmetic and the interrupt.
struct Registers {
    /// virtio's common configuration.
    common: Block,
    /// The notification area.
    notify: Block,
    /// The ISR status byte.
    isr: Block,
    /// The device-specific configuration.
    device: Block,
    /// What a `queue_notify_off` is multiplied by.
    notify_off_multiplier: u32,
    /// Whether the device has MSI-X entries.
    msix: bool,
    /// The interrupt every queue arrives on.
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

    fn queue_vector(&self, _queue: u16) -> u16 {
        // One entry for every queue: the handler drains all of them whichever
        // rang, and a device with one vector is what QEMU gives without
        // `vectors=`.
        if self.msix { 0 } else { NO_VECTOR }
    }

    fn acknowledge_interrupt(&mut self) -> u8 {
        if self.msix {
            // Re-arm the entry; a failure here leaves the next interrupt
            // undelivered, which the loop's next poll turns into a stall, and
            // nothing better can be done from inside the driver.
            let _ = self.interrupt.ack();
            ISR_QUEUE
        } else {
            let status: u8 = self.isr.read(0);
            let _ = self.interrupt.ack();
            status
        }
    }
}

/// The driver's own type, written once.
type Console = Driver<Registers, Pinned, Pinned, Pinned, [Slot; SLOTS]>;

// ---------------------------------------------------------------------------
// The socket the port is offered through
// ---------------------------------------------------------------------------

/// The listening socket and whoever is connected to it.
struct Offered {
    /// The socket bound at [`ferrix_vdagent::SOCKET_PATH`].
    listener: Fd<Kernel>,
    /// The one client, when there is one.
    client: Option<Fd<Kernel>>,
}

impl Offered {
    /// Bind and listen, clearing a name a previous run left behind.
    fn open() -> Result<Offered, Step> {
        // `ENOENT` is the ordinary case and not a failure: nothing is there.
        let _ = linux::unlink(Kernel, ferrix_vdagent::SOCKET_PATH);
        let listener = linux::socket(
            Kernel,
            AF_UNIX,
            SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC,
            0,
        )
        .map_err(|_| Step::Socket)?;
        listener
            .bind_unix(ferrix_vdagent::SOCKET_PATH)
            .map_err(|_| Step::Socket)?;
        listener.listen(BACKLOG).map_err(|_| Step::Socket)?;
        Ok(Offered {
            listener,
            client: None,
        })
    }

    /// Take a connection if one is waiting and there is not one already.
    fn accept(&mut self) {
        if self.client.is_some() {
            return;
        }
        if let Ok(client) = self.listener.accept(SOCK_NONBLOCK | SOCK_CLOEXEC) {
            self.client = Some(client);
        }
    }

    /// Forget the client, which a closed connection or a closed host end
    /// both mean.
    fn hang_up(&mut self) {
        self.client = None;
    }
}

/// Bytes on their way from the device to the socket.
struct Staging {
    /// The bytes.
    bytes: [u8; STAGING_BYTES],
    /// How many are in it.
    filled: usize,
    /// How many of those have gone.
    sent: usize,
}

impl Staging {
    /// Nothing held.
    const fn new() -> Staging {
        Staging {
            bytes: [0; STAGING_BYTES],
            filled: 0,
            sent: 0,
        }
    }

    /// Whether everything held has gone.
    const fn is_empty(&self) -> bool {
        self.sent >= self.filled
    }

    /// Put `len` bytes from `offset` of `window` at the end.
    fn take(&mut self, window: &Window, offset: u64, len: u32) {
        let len = len as usize;
        let Some(room) = self.bytes.get_mut(self.filled..self.filled + len) else {
            // A drain never offers more than `STAGING_BYTES`, so this is the
            // shape of a bound rather than a case that happens.
            return;
        };
        window.read(offset, room);
        self.filled += len;
    }

    /// Write what is held to `client`, as much as it will take.
    ///
    /// A client that has gone answers an error and is dropped; a client that
    /// is full answers `EAGAIN`, which is the ordinary case and leaves the
    /// rest for the next turn.
    fn flush(&mut self, offered: &mut Offered) {
        while !self.is_empty() {
            let Some(client) = offered.client.as_ref() else {
                return;
            };
            let Some(rest) = self.bytes.get(self.sent..self.filled) else {
                return;
            };
            match client.write(rest) {
                Ok(0) => return,
                Ok(written) => self.sent += written,
                Err(errno) if errno == Errno::EAGAIN => return,
                Err(_) => {
                    offered.hang_up();
                    self.clear();
                    return;
                }
            }
        }
        self.clear();
    }

    /// Forget everything held, which a host that went away means.
    fn clear(&mut self) {
        self.filled = 0;
        self.sent = 0;
    }
}

// ---------------------------------------------------------------------------
// Bringing it up
// ---------------------------------------------------------------------------

/// What START handed over.
struct Started {
    /// The device.
    device: Device<Kernel>,
}

/// Read START off the bootstrap channel.
fn started(boot: &Channel<Kernel>) -> Result<Started, Step> {
    let _ = boot
        .wait_one(Signals::READABLE, Deadline::Never)
        .map_err(|_| Step::Start)?;
    let mut bytes = [0_u8; START_BYTES];
    let mut handles = [Handle::INVALID; 1];
    let received = boot
        .read(&mut bytes, &mut handles)
        .map_err(|_| Step::Start)?;
    if received.handles != 1 {
        return Err(Step::Start);
    }
    let device = Device::from_owned(OwnedHandle::from_raw(Kernel, handles[0]));
    match StartMessage::decode(bytes.get(..received.bytes).unwrap_or_default()) {
        Ok(StartMessage::Start(_)) => Ok(Started { device }),
        _ => Err(Step::Start),
    }
}

/// Map the registers `device_info` describes and claim the interrupt.
fn registers(device: &Device<Kernel>, info: &DeviceInfo) -> Result<Registers, Step> {
    let interrupt = device.interrupt(0).map_err(|_| Step::Registers)?;
    Ok(Registers {
        common: Block::map(device, &info.common)?,
        notify: Block::map(device, &info.notify)?,
        isr: Block::map(device, &info.isr)?,
        device: Block::map(device, &info.device)?,
        notify_off_multiplier: info.notify_off_multiplier,
        msix: info.msix_table_size > 0,
        interrupt,
    })
}

/// The driver, and the two windows it does not hand out.
struct Carrier {
    /// The driver.
    console: Console,
    /// Where bytes to send are written.
    transmit: Window,
    /// Where bytes that arrived are read.
    receive: Window,
}

/// Make the device's memory and bring the device up.
///
/// The interrupt is bound to `port` here, before the registers are moved into
/// the driver: after that the driver owns them, and the interrupt with them.
fn bring_up(
    device: &Device<Kernel>,
    registers: Registers,
    port: &Port<Kernel>,
) -> Result<Carrier, Step> {
    registers
        .interrupt
        .bind(port, KEY_INTERRUPT)
        .map_err(|_| Step::Registers)?;
    // One region per queue, written out: an array has no fallible `map` on
    // stable, and the assertion turns a changed `QUEUE_COUNT` into a compile
    // error rather than a shorter array.
    const _: () = assert!(QUEUE_COUNT == 6, "a ring region for every queue");
    let rings = [
        Pinned::new(device, QUEUE_PAGES)?,
        Pinned::new(device, QUEUE_PAGES)?,
        Pinned::new(device, QUEUE_PAGES)?,
        Pinned::new(device, QUEUE_PAGES)?,
        Pinned::new(device, QUEUE_PAGES)?,
        Pinned::new(device, QUEUE_PAGES)?,
    ];
    let control = Pinned::new(device, CONTROL_PAGES)?;
    let receive_data = Pinned::new(device, RECEIVE_PAGES)?;
    let transmit_data = Pinned::new(device, TRANSMIT_PAGES)?;
    let transmit = transmit_data.window();
    let receive = receive_data.window();
    let parts = Parts {
        transport: registers,
        rings,
        control,
        receive_data,
        transmit_data,
        slots: [Slot::EMPTY; SLOTS],
    };
    let console = Driver::init(
        parts,
        Options {
            max_queue_size: QUEUE_SIZE,
            receive_stride: RECEIVE_STRIDE,
            ..Options::default()
        },
    )
    .map_err(|_| Step::Device)?;
    Ok(Carrier {
        console,
        transmit,
        receive,
    })
}

// ---------------------------------------------------------------------------
// Carrying the bytes
// ---------------------------------------------------------------------------

/// What one turn of the loop found.
#[derive(Clone, Copy, Default)]
struct Turn {
    /// Whether anything moved, so the next turn should not sleep.
    busy: bool,
    /// Whether the device removed the port, which ends the run.
    removed: bool,
}

/// Carry the port's bytes until the device removes it.
fn carry(carrier: &mut Carrier, port: &Port<Kernel>, offered: &mut Offered) -> Result<(), Step> {
    let mut staging = Staging::new();
    let mut sending: Option<u64> = None;
    let mut next_id = 1_u64;
    let mut from_socket = [0_u8; TRANSMIT_BYTES];
    loop {
        let mut turn = Turn::default();
        offered.accept();
        staging.flush(offered);
        drain(carrier, offered, &mut staging, &mut sending, &mut turn)?;
        if turn.removed {
            return Ok(());
        }
        staging.flush(offered);
        push(
            carrier,
            offered,
            &mut sending,
            &mut next_id,
            &mut from_socket,
            &mut turn,
        )?;
        let _ = carrier.console.refill().map_err(|_| Step::Carry)?;
        if turn.busy {
            continue;
        }
        // Nothing moved, so wait for the interrupt -- but only as long as it
        // takes for the socket to be worth looking at again.
        let deadline = linux::monotonic_nanos(Kernel)
            .ok()
            .map_or(Deadline::Never, |now| {
                Deadline::At(now.saturating_add(POLL_NANOS))
            });
        match port.wait(deadline) {
            Ok(_) | Err(Error::TimedOut) => {}
            Err(_) => return Err(Step::Carry),
        }
    }
}

/// Take what the device has done: bytes to the staging buffer, completions to
/// the chunk in flight, and the host's comings and goings to the client.
fn drain(
    carrier: &mut Carrier,
    offered: &mut Offered,
    staging: &mut Staging,
    sending: &mut Option<u64>,
    turn: &mut Turn,
) -> Result<(), Step> {
    // A drain is taken only with the staging buffer empty, because a drain
    // can put down one buffer per event and there is room for exactly that
    // many.
    while staging.is_empty() {
        let mut events = [Event::Sent { id: 0 }; EVENTS];
        let drained = carrier
            .console
            .on_interrupt(&mut events)
            .map_err(|_| Step::Carry)?;
        let taken = events.get(..drained.events).unwrap_or_default();
        if taken.is_empty() && !drained.more {
            return Ok(());
        }
        turn.busy = true;
        for event in taken {
            match *event {
                Event::Received {
                    buffer,
                    offset,
                    len,
                } => {
                    staging.take(&carrier.receive, offset, len);
                    carrier.console.release(buffer).map_err(|_| Step::Carry)?;
                }
                Event::Sent { id } => {
                    if *sending == Some(id) {
                        *sending = None;
                    }
                }
                // A host that went away takes the agent's state with it, and
                // the way to say so to a program holding a socket is to close
                // it. The port may open again, with a new host and a new
                // connection.
                Event::Closed { .. } => {
                    offered.hang_up();
                    staging.clear();
                    *sending = None;
                }
                Event::Removed { .. } => {
                    turn.removed = true;
                    return Ok(());
                }
                Event::Named { .. } | Event::Opened { .. } => {}
            }
        }
        if !drained.more {
            return Ok(());
        }
    }
    Ok(())
}

/// Read what the client has to say and put it on the port.
fn push(
    carrier: &mut Carrier,
    offered: &mut Offered,
    sending: &mut Option<u64>,
    next_id: &mut u64,
    room: &mut [u8; TRANSMIT_BYTES],
    turn: &mut Turn,
) -> Result<(), Step> {
    if sending.is_some() || !matches!(carrier.console.port(), PortState::Open { .. }) {
        return Ok(());
    }
    let Some(client) = offered.client.as_ref() else {
        return Ok(());
    };
    let read = match client.read(room) {
        // The client closed its end. The port stays open for the next one.
        Ok(0) => {
            offered.hang_up();
            return Ok(());
        }
        Ok(read) => read,
        Err(errno) if errno == Errno::EAGAIN => return Ok(()),
        Err(_) => {
            offered.hang_up();
            return Ok(());
        }
    };
    let Some(bytes) = room.get(..read) else {
        return Ok(());
    };
    carrier.transmit.write(0, bytes);
    let id = *next_id;
    let chunk = Chunk {
        id,
        offset: 0,
        len: read as u32,
    };
    match carrier.console.submit(&chunk) {
        Ok(()) => {
            *sending = Some(id);
            *next_id = next_id.wrapping_add(1);
            turn.busy = true;
        }
        // The device broke the protocol as the chunk went out. The driver
        // has failed and the run is over; the next drain says so.
        Err(SubmitError::Broken | SubmitError::Device(_)) => return Err(Step::Carry),
        // Everything else means these bytes did not go and the next ones
        // will: the port closed between the check and the submit, or a
        // queue that cannot in fact be full with one chunk in flight. A
        // pipe whose reader went away drops what it was carrying.
        Err(_) => {}
    }
    Ok(())
}

/// The whole run.
fn run(boot: &Channel<Kernel>) -> Result<(), Step> {
    let Started { device } = started(boot)?;
    let info = device.info().map_err(|_| Step::Identity)?;
    if !VIRTIO_CONSOLE_IDS.contains(&info.device_id) {
        return Err(Step::Identity);
    }
    let registers = registers(&device, &info)?;
    let port = port::create(Kernel).map_err(|_| Step::Registers)?;
    let mut carrier = bring_up(&device, registers, &port)?;
    let mut offered = Offered::open()?;
    let outcome = carry(&mut carrier, &port, &mut offered);
    let reset = finish(carrier);
    outcome.and(reset)
}

/// Reset the device and give its memory back.
fn finish(carrier: Carrier) -> Result<(), Step> {
    let _ = linux::unlink(Kernel, ferrix_vdagent::SOCKET_PATH);
    match carrier.console.shutdown() {
        ferrix_virtio_console::Teardown::Released(_) => Ok(()),
        // The device did not reset and may still write, so its memory is
        // never given back. The exit status is what says so.
        ferrix_virtio_console::Teardown::Wedged(_) => Err(Step::Reset),
    }
}
