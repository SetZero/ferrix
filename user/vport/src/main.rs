//! The virtio-console port driver: a named virtio-serial port on one end, a
//! Unix socket on the other, and nothing in between that understands either.
//!
//! `ferrix-virtio-console` is the device: the order of the bring-up, the
//! control conversation, and which buffer goes where. It is tested on the
//! host against a real virtqueue and knows nothing of handles. This program
//! is the handles that library was written to be wrapped in, and a socket.
//!
//! `docs/CLIPBOARD.md` §6 is the specification. Two things about it are worth
//! saying here, because neither is obvious from the code:
//!
//! **It understands nothing of vdagent.** Everything that arrives on the port
//! is written to whoever is connected and everything written there goes out
//! on the port. The protocol is `compositor/vdagent`'s, one process further
//! out, so this driver has no opinion about the clipboard at all.
//!
//! **It publishes to no kernel subsystem**, because there is none to publish
//! to: §5 of that document is why the port needs no kernel representation.
//! `devmgr` therefore starts it with a kind of its own and does not wait for
//! a PUBLISHED that will never come.
//!
//! # Waiting on two things at once
//!
//! An interrupt arrives on a native port and a socket is a Linux descriptor,
//! and no call in this kernel waits on both. So the socket is non-blocking
//! and the wait on the port carries a deadline: each turn of the loop either
//! an interrupt arrived or [`TICK_NANOS`] passed, and either way the socket
//! is serviced before the next wait. A clipboard is a human-speed thing and
//! a tick of ten milliseconds is far below what a person can notice.

#![no_std]
#![no_main]

use core::ptr;
use core::sync::atomic::{Ordering, fence};

use ferrix_blkring::control::{Block as StartBlock, Message as StartMessage, START_BYTES, Start};
use ferrix_linux_abi::errno::Errno;
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::types::IoMappingSpec;
use ferrix_rt::linux;
use ferrix_rt::native::channel::Channel;
use ferrix_rt::native::device::{Device, Interrupt, IoMapping};
use ferrix_rt::native::error::Error;
use ferrix_rt::native::handle::{Deadline, Object, OwnedHandle};
use ferrix_rt::native::pending::Protection;
use ferrix_rt::native::pin::{Pin, PinAccess, device_address};
use ferrix_rt::native::port::{self, Port};
use ferrix_rt::native::vmo::{self, Vmo};
use ferrix_rt::{Bootstrap, Kernel};
use ferrix_virtio::pci::{CommonConfig, NO_VECTOR};
use ferrix_virtio::{DeviceConfig, QueueMemory};
use ferrix_virtio_console::{
    AREA_BYTES, Area, CHUNK, CONTROL_AREA_BYTES, ConsoleError, DevicePages, Driver, Event, Options,
    Parts, RING_BYTES, Transport,
};

ferrix_rt::entry!(main);

/// A page, on every architecture this runs on.
const PAGE: usize = 4096;

/// Pages for one queue's rings.
const RING_PAGES: usize = RING_BYTES.div_ceil(PAGE);
/// Pages for the control queues' buffers.
const CONTROL_PAGES: usize = CONTROL_AREA_BYTES.div_ceil(PAGE);
/// Pages for the port's buffers.
const PORT_PAGES: usize = AREA_BYTES.div_ceil(PAGE);
/// The largest of them, which is what a [`Pinned`]'s address array must hold.
const MAX_PAGES: usize = if PORT_PAGES > CONTROL_PAGES {
    PORT_PAGES
} else {
    CONTROL_PAGES
};

/// virtio-console's modern PCI device id, and the transitional one
/// (`docs/CLIPBOARD.md` §3.1).
const VIRTIO_CONSOLE_IDS: [u16; 2] = [0x1043, 0x1003];

/// The socket the agent connects to. `/tmp` is in the initramfs and writable
/// (`xtask/src/initramfs.rs`), which `/run` is not.
const SOCKET_PATH: &[u8] = b"/tmp/vport\0";

/// The path without its NUL, for `bind`, which takes a length instead.
const SOCKET_NAME: &[u8] = b"/tmp/vport";

/// How long a turn of the loop waits for an interrupt before servicing the
/// socket anyway. Ten milliseconds.
const TICK_NANOS: u64 = 10_000_000;

/// The port key the device's interrupt arrives on.
const KEY_INTERRUPT: u64 = 1;

/// How many connections may be waiting. One: there is one agent.
const BACKLOG: usize = 1;

/// The ISR bit that means a queue was used, which is bit 0 for every virtio
/// device (virtio 1.2 §4.1.4.5). Each driver crate names its own; this one
/// has no core to take it from.
const ISR_QUEUE: u8 = 1;

/// `EAGAIN`, which on a non-blocking descriptor means "not now", not "no".
const EAGAIN: Errno = Errno::EAGAIN;

/// Where a run stopped, as the exit status.
#[derive(Clone, Copy, Debug)]
#[repr(i32)]
enum Step {
    /// No bootstrap channel, or the first message was not START.
    Start = 1,
    /// START named a device that is not a virtio-console.
    Identity = 2,
    /// A register block could not be mapped.
    Registers = 3,
    /// Memory could not be made, pinned or mapped.
    Memory = 4,
    /// The device would not come up, or broke the protocol.
    Device = 5,
    /// The port, the interrupt or the waits could not be arranged.
    Events = 6,
    /// The socket could not be made, bound or listened on.
    Socket = 7,
    /// The device declared no port with the name asked for.
    NoPort = 8,
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
        // long as its VMO handle; the offset was checked; volatile, since the
        // other side is a device.
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
    addresses: [u64; MAX_PAGES],
    pages: usize,
}

impl Pinned {
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
}

impl DevicePages for Pinned {
    fn device_pages(&self) -> &[u64] {
        self.addresses.get(..self.pages).unwrap_or_default()
    }
}

impl Area for Pinned {
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

/// The device as `ferrix-virtio-console` drives it.
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
            let _acked = self.interrupt.ack();
            ISR_QUEUE
        } else {
            let status: u8 = self.isr.read(0);
            let _acked = self.interrupt.ack();
            status
        }
    }
}

// ---------------------------------------------------------------------------
// The socket
// ---------------------------------------------------------------------------

/// Bytes on their way somewhere, and how far along they are.
///
/// Both ends of this driver take what they are given and may take less than
/// all of it: the port's transmit queue has a fixed number of buffers and a
/// socket has a fixed pipe. So a direction is a buffer, a length and how much
/// of it has gone, and a turn of the loop moves what it can.
struct Staged {
    bytes: [u8; CHUNK],
    len: usize,
    at: usize,
}

impl Staged {
    const fn new() -> Staged {
        Staged {
            bytes: [0; CHUNK],
            len: 0,
            at: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.at >= self.len
    }

    fn rest(&self) -> &[u8] {
        self.bytes.get(self.at..self.len).unwrap_or_default()
    }

    fn advance(&mut self, by: usize) {
        self.at = self.at.saturating_add(by).min(self.len);
        if self.is_empty() {
            self.len = 0;
            self.at = 0;
        }
    }

    fn fill(&mut self, len: usize) {
        self.len = len.min(CHUNK);
        self.at = 0;
    }
}

/// The listening socket, and the one client there may be.
struct Listener {
    fd: usize,
    client: Option<usize>,
}

impl Listener {
    /// Bind `SOCKET_PATH` and listen on it, non-blocking.
    fn bind() -> Result<Listener, Step> {
        // A path left behind by a previous boot of this process would make
        // `bind` fail with EADDRINUSE, and there is nothing else that could
        // own this name. ENOENT is the ordinary answer and is not an error.
        let _removed = linux::unlink(SOCKET_PATH);
        let fd = linux::socket(linux::AF_UNIX, linux::SOCK_STREAM | linux::SOCK_NONBLOCK, 0)
            .map_err(|_| Step::Socket)?;
        let (address, len) = linux::sockaddr_un(SOCKET_NAME).ok_or(Step::Socket)?;
        let _bound = linux::bind(fd, &address, len).map_err(|_| Step::Socket)?;
        let _listening = linux::listen(fd, BACKLOG).map_err(|_| Step::Socket)?;
        Ok(Listener { fd, client: None })
    }

    /// Take a waiting connection, if there is one and nobody is connected.
    ///
    /// One at a time: a second agent would see half a conversation, so the
    /// one that is here keeps the port until it goes away.
    fn accept(&mut self) {
        if self.client.is_some() {
            return;
        }
        if let Ok(fd) = linux::accept4(self.fd, linux::SOCK_NONBLOCK) {
            self.client = Some(fd);
        }
    }

    /// Forget the client, closing it.
    fn drop_client(&mut self) {
        if let Some(fd) = self.client.take() {
            let _closed = linux::close(fd);
        }
    }

    /// Read what the client has sent, or `None` when it has sent nothing.
    ///
    /// A read of zero is the other end gone, which closes this side too.
    fn read(&mut self, into: &mut [u8]) -> Option<usize> {
        let fd = self.client?;
        match linux::read(fd, into) {
            Ok(0) => {
                self.drop_client();
                None
            }
            Ok(read) => Some(read),
            Err(errno) if errno == EAGAIN => None,
            Err(_) => {
                self.drop_client();
                None
            }
        }
    }

    /// Send what is staged, answering how much went.
    fn write(&mut self, bytes: &[u8]) -> usize {
        let Some(fd) = self.client else {
            return 0;
        };
        match linux::write(fd, bytes) {
            Ok(written) => written,
            Err(errno) if errno == EAGAIN => 0,
            Err(_) => {
                self.drop_client();
                0
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

type Console = Driver<Registers, Pinned, Pinned>;

/// What START gave.
struct Started {
    start: Start,
    device: Device<Kernel>,
}

fn started(boot: &Channel<Kernel>) -> Result<Started, Step> {
    let _signalled = boot
        .wait_one(
            ferrix_native_abi::signals::Signals::READABLE,
            Deadline::Never,
        )
        .map_err(|_| Step::Start)?;
    let mut bytes = [0_u8; START_BYTES];
    let mut handles = [Handle::INVALID; 2];
    let received = boot
        .read(&mut bytes, &mut handles)
        .map_err(|_| Step::Start)?;
    if received.handles == 0 {
        return Err(Step::Start);
    }
    let device = Device::from_owned(OwnedHandle::from_raw(Kernel, handles[0]));
    // A second handle would be a control channel, which this driver has no
    // subsystem to talk to; it is closed rather than kept.
    for handle in handles.iter().take(received.handles).skip(1) {
        drop(OwnedHandle::from_raw(Kernel, *handle));
    }
    match StartMessage::decode(bytes.get(..received.bytes).unwrap_or_default()) {
        Ok(StartMessage::Start(start)) => Ok(Started { start, device }),
        _ => Err(Step::Start),
    }
}

fn run(boot: &Channel<Kernel>) -> Result<(), Step> {
    let Started { start, device } = started(boot)?;
    if !VIRTIO_CONSOLE_IDS.contains(&start.pci_device_id) {
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

    let parts = Parts {
        transport: registers,
        control_rx_rings: Pinned::new(&device, RING_PAGES)?,
        control_tx_rings: Pinned::new(&device, RING_PAGES)?,
        port_rx_rings: Pinned::new(&device, RING_PAGES)?,
        port_tx_rings: Pinned::new(&device, RING_PAGES)?,
        control_area: Pinned::new(&device, CONTROL_PAGES)?,
        port_area: Pinned::new(&device, PORT_PAGES)?,
    };

    let waits = port::create(Kernel).map_err(|_| Step::Events)?;
    parts
        .transport
        .interrupt
        .bind(&waits, KEY_INTERRUPT)
        .map_err(|_| Step::Events)?;

    let mut driver = Driver::init(parts, Options::default()).map_err(step_of)?;
    let mut listener = Listener::bind()?;
    pump(&mut driver, &mut listener, &waits)
}

/// The loop: an interrupt or a tick, then the socket, for as long as the port
/// is there.
fn pump(driver: &mut Console, listener: &mut Listener, waits: &Port<Kernel>) -> Result<(), Step> {
    let mut to_port = Staged::new();
    let mut to_client = Staged::new();
    loop {
        let deadline = linux::monotonic_nanos()
            .map_err(|_| Step::Events)?
            .saturating_add(TICK_NANOS);
        match waits.wait(Deadline::At(deadline)) {
            Ok(_) => {}
            Err(Error::TimedOut) => {}
            Err(_) => return Err(Step::Events),
        }

        // The device is drained every turn and not only when a packet named
        // an interrupt. A virtio driver may not depend on an interrupt
        // arriving: one raised between `init` and the first wait is one this
        // loop would otherwise sit through for ever, and since `poll` is also
        // what acknowledges the interrupt, a missed one latches and no
        // further interrupt comes. The tick costs nothing that matters here
        // and makes the bring-up depend on the conversation rather than on
        // the timing of its first completion.
        if drain(driver)? {
            // The host end went away: whoever was connected is told by the
            // socket closing, which is the only way this driver can say it.
            listener.drop_client();
            to_port = Staged::new();
            to_client = Staged::new();
        }

        listener.accept();

        // The client's bytes out to the host.
        if to_port.is_empty()
            && listener.client.is_some()
            && let Some(read) = listener.read(&mut to_port.bytes)
        {
            to_port.fill(read);
        }
        if !to_port.is_empty() && driver.port_open() {
            let sent = driver.write(to_port.rest()).map_err(step_of)?;
            to_port.advance(sent);
        }

        // The host's bytes in to the client.
        if to_client.is_empty()
            && driver.port_open()
            && let Some(read) = driver.read(&mut to_client.bytes).map_err(step_of)?
        {
            to_client.fill(read);
        }
        if !to_client.is_empty() {
            let written = listener.write(to_client.rest());
            to_client.advance(written);
        }
    }
}

/// Which status a failure of the device's is.
fn step_of(failure: ConsoleError) -> Step {
    match failure {
        // The device said it had added every port and none answered to the
        // name, which is a device without the port this driver exists for
        // rather than a device that misbehaved.
        ConsoleError::NoSuchPort => Step::NoPort,
        _ => Step::Device,
    }
}

/// Answer everything the device has said, and report whether the port closed.
fn drain(driver: &mut Console) -> Result<bool, Step> {
    let mut closed = false;
    while let Some(event) = driver.poll().map_err(step_of)? {
        match event {
            Event::Found { .. } | Event::Data => {}
            Event::Open { open } => {
                if !open {
                    closed = true;
                }
            }
        }
    }
    Ok(closed)
}
