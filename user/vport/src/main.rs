//! The virtio-serial driver process: one named port, carried on a Unix
//! socket.
//!
//! `docs/CLIPBOARD.md` §6. Everything that knows anything is a library:
//! `ferrix-virtio::console` is the protocol and `ferrix-virtio-console` the
//! order of the conversation, both tested on the host. This program is the
//! handles those were written to be wrapped in, and a relay:
//!
//! 1. The bootstrap channel carries START -- the device, and where its
//!    virtio register blocks are. Unlike every other driver here there is no
//!    control channel, because there is no kernel subsystem to serve: this
//!    driver publishes nothing and answers to no core.
//! 2. Each register block is an `IoMapping`; the four queues' rings and their
//!    buffers are VMOs this process creates, pins and maps.
//! 3. The device is brought up and the control conversation walked until the
//!    port named `com.redhat.spice.0` is open at both ends.
//! 4. A Unix socket is bound to an **abstract** name, so there is no
//!    directory to make and no stale file to unlink. Whatever arrives on the
//!    port is written to whoever is connected, and whatever they write goes
//!    out on the port.
//!
//! # Why this program makes Linux calls
//!
//! It is a native program and it binds a socket, which sounds like two kinds
//! of program at once. It is not: the kernel gives every process a descriptor
//! table and a namespace, and picks the ABI by the system call number's
//! range, so `ferrix_rt::linux` is as available here as the native calls are.
//! `docs/CLIPBOARD.md` §5 argues it at length; this is the program it was
//! argued for.
//!
//! # What it is not
//!
//! It understands nothing of vdagent, or of clipboards. It is a pipe with a
//! device on one end, and everything that knows what the bytes mean is in
//! `compositor/vdagent` on the other side of the socket.
//!
//! The exit status names the step that failed ([`Step`]).

#![no_std]
#![no_main]

use core::ptr;
use core::sync::atomic::{Ordering, fence};

use ferrix_blkring::control::{Block as StartBlock, Message as StartMessage, START_BYTES, Start};
use ferrix_linux_abi::errno::Errno;
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::{IoMappingSpec, PACKET_INTERRUPT};
use ferrix_rt::linux;
use ferrix_rt::native::channel::Channel;
use ferrix_rt::native::device::{Device, Interrupt, IoMapping};
use ferrix_rt::native::handle::{Deadline, Object, OwnedHandle};
use ferrix_rt::native::pending::Protection;
use ferrix_rt::native::pin::{Pin, PinAccess, device_address};
use ferrix_rt::native::port::{self, Port};
use ferrix_rt::native::vmo::{self, Vmo};
use ferrix_rt::{Bootstrap, Kernel};
use ferrix_virtio::console::{self, DeviceConfig};
use ferrix_virtio::pci::{CommonConfig, NO_VECTOR};
use ferrix_virtio::{PAGE_SIZE, QueueMemory};
use ferrix_virtio_console::{
    AREA_BYTES, Area, CHUNK, CONTROL_AREA_BYTES, DevicePages, Driver, Event, Options, Parts,
    RING_BYTES, Transport,
};

ferrix_rt::entry!(main);

/// A page, on every architecture this runs on.
const PAGE: usize = PAGE_SIZE as usize;

/// The most pages any one of this driver's regions takes: the port's buffers.
const MAX_PAGES: usize = AREA_BYTES.div_ceil(PAGE);

/// virtio-console's modern and transitional PCI device ids, which
/// `docs/DEVMGR.md` names.
const VIRTIO_CONSOLE_IDS: [u16; 2] = [0x1043, 0x1003];

/// The abstract name the port is served on, which `compositor/vdagent`
/// connects to. `docs/CLIPBOARD.md` §6.
const SOCKET_NAME: &[u8] = b"ferrix-vport0";

/// How many connections may be waiting.
const BACKLOG: usize = 4;

/// How long a wait for the device's interrupt lasts before the socket is
/// looked at anyway.
///
/// The device's interrupt wakes a native port and a socket's readability does
/// not, so the two are not waited on together: the wait has a deadline and
/// the socket is drained each time round. Five milliseconds is far below what
/// a person notices in a paste and far above the cost of going round.
const TICK_NANOS: u64 = 5_000_000;

/// The port key the device's interrupt arrives on.
const KEY_INTERRUPT: u64 = 1;

/// Where a run stopped, as the exit status.
#[derive(Clone, Copy, Debug)]
#[repr(i32)]
enum Step {
    /// No bootstrap channel, or the first message was not START.
    Start = 1,
    /// START named a device that is not virtio-console.
    Identity = 2,
    /// A register block could not be mapped.
    Registers = 3,
    /// Memory could not be made, pinned or mapped.
    Memory = 4,
    /// The device would not come up.
    Device = 5,
    /// The port, or the interrupt, could not be arranged.
    Events = 6,
    /// The socket could not be made, bound or listened on.
    Socket = 7,
    /// The device broke the protocol.
    Faulted = 8,
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

/// A VMO this process made, pinned read-write for the device and mapped.
struct Pinned {
    _pin: Pin<Kernel>,
    _vmo: Vmo<Kernel>,
    base: usize,
    len: usize,
    addresses: [u64; MAX_PAGES],
    pages: usize,
}

impl Pinned {
    fn new(device: &Device<Kernel>, bytes: usize) -> Result<Pinned, Step> {
        let pages = bytes.div_ceil(PAGE).max(1);
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
            base,
            len: bytes,
            addresses,
            pages,
        })
    }

    fn read(&self, offset: usize) -> u8 {
        assert!(offset < self.len, "a read inside the mapping");
        // SAFETY: a mapping the kernel made for this process that lives as
        // long as its VMO handle; the offset was checked; volatile, since the
        // device writes the same memory.
        unsafe { ptr::read_volatile((self.base + offset) as *const u8) }
    }

    fn write(&mut self, offset: usize, value: u8) {
        assert!(offset < self.len, "a write inside the mapping");
        // SAFETY: as for `read`, and the mapping is writable.
        unsafe { ptr::write_volatile((self.base + offset) as *mut u8, value) }
    }
}

impl DevicePages for Pinned {
    fn device_pages(&self) -> &[u64] {
        self.addresses.get(..self.pages).unwrap_or_default()
    }
}

impl Area for Pinned {
    fn read_u8(&self, offset: usize) -> u8 {
        self.read(offset)
    }
    fn write_u8(&mut self, offset: usize, value: u8) {
        self.write(offset, value);
    }
}

// SAFETY: one VMO, pinned so the device sees the pages `device_pages` names
// and mapped so this process sees them, both until the `Pinned` is dropped --
// which happens only after the device has been reset. Every access is
// volatile and `barrier` is a full fence.
unsafe impl QueueMemory for Pinned {
    fn read_u8(&self, offset: usize) -> u8 {
        self.read(offset)
    }
    fn write_u8(&mut self, offset: usize, value: u8) {
        self.write(offset, value);
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

/// The device, as `ferrix-virtio-console` drives it.
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
            1
        } else {
            let status: u8 = self.isr.read(0);
            let _ = self.interrupt.ack();
            status
        }
    }
}

// ---------------------------------------------------------------------------
// The socket
// ---------------------------------------------------------------------------

/// The listening socket, and whoever is connected to it.
struct Relay {
    listener: usize,
    client: Option<usize>,
}

impl Relay {
    /// A non-blocking listener bound to the abstract name.
    fn bind() -> Result<Relay, Step> {
        let listener = linux::socket(
            linux::AF_UNIX,
            linux::SOCK_STREAM | linux::SOCK_NONBLOCK,
            0,
        )
        .map_err(|_| Step::Socket)?;
        let (address, len) =
            linux::sockaddr_un_abstract(SOCKET_NAME).ok_or(Step::Socket)?;
        let _ = linux::bind(listener, &address, len).map_err(|_| Step::Socket)?;
        let _ = linux::listen(listener, BACKLOG).map_err(|_| Step::Socket)?;
        Ok(Relay {
            listener,
            client: None,
        })
    }

    /// Take a connection if one is waiting and there is no client already.
    ///
    /// One at a time: the port is one stream and two readers of it would each
    /// get half of every message.
    fn accept(&mut self) {
        if self.client.is_some() {
            return;
        }
        if let Ok(client) = linux::accept4(self.listener, linux::SOCK_NONBLOCK) {
            self.client = Some(client);
        }
    }

    /// Give up on the client, if the error says it has gone.
    fn drop_client(&mut self) {
        if let Some(client) = self.client.take() {
            let _ = linux::close(client);
        }
    }

    /// Read what the client has written, if anything.
    fn take(&mut self, out: &mut [u8]) -> Option<usize> {
        let client = self.client?;
        match linux::read(client, out) {
            Ok(0) => {
                // End of stream: the agent has gone, and the next one to
                // connect starts afresh.
                self.drop_client();
                None
            }
            Ok(read) => Some(read),
            Err(Errno::EAGAIN) => None,
            Err(_) => {
                self.drop_client();
                None
            }
        }
    }

    /// Write `bytes` to the client, dropping them if there is none or if it
    /// will not take them.
    ///
    /// Dropping rather than blocking, deliberately: the device's queue must
    /// keep being drained whatever the agent is doing, and a clipboard whose
    /// driver is stuck writing to a wedged program is a machine that stops
    /// pasting. A vdagent message that is cut short is refused at the other
    /// end by `libs/vdagent`'s reassembler, which is where that belongs.
    fn give(&mut self, bytes: &[u8]) {
        let Some(client) = self.client else {
            return;
        };
        let mut sent = 0;
        while sent < bytes.len() {
            match linux::write(client, bytes.get(sent..).unwrap_or_default()) {
                Ok(0) | Err(Errno::EAGAIN) => return,
                Ok(wrote) => sent += wrote,
                Err(_) => {
                    self.drop_client();
                    return;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// What START gave.
struct Started {
    start: Start,
    device: Device<Kernel>,
}

/// Read START off the bootstrap channel.
///
/// One handle, not two: there is no control channel, because there is no
/// subsystem to serve.
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

    let waiter = port::create(Kernel).map_err(|_| Step::Events)?;
    registers
        .interrupt
        .bind(&waiter, KEY_INTERRUPT)
        .map_err(|_| Step::Events)?;

    let parts = Parts {
        transport: registers,
        control_rx_rings: Pinned::new(&device, RING_BYTES)?,
        control_tx_rings: Pinned::new(&device, RING_BYTES)?,
        port_rx_rings: Pinned::new(&device, RING_BYTES)?,
        port_tx_rings: Pinned::new(&device, RING_BYTES)?,
        control_area: Pinned::new(&device, CONTROL_AREA_BYTES)?,
        port_area: Pinned::new(&device, AREA_BYTES)?,
    };
    let mut driver = Driver::init(parts, Options::default()).map_err(|_| Step::Device)?;
    let mut relay = Relay::bind()?;

    serve(&mut driver, &mut relay, &waiter)
}

/// The relay: the device's interrupt on one side, the socket on the other.
fn serve(
    driver: &mut Driver<Registers, Pinned, Pinned>,
    relay: &mut Relay,
    waiter: &Port<Kernel>,
) -> Result<(), Step> {
    let mut buffer = [0_u8; CHUNK];
    loop {
        // A deadline rather than a wait without one: the socket's readability
        // does not wake a native port, so the loop goes round on its own and
        // looks (see `TICK_NANOS`).
        let deadline = linux::monotonic_nanos()
            .map(|now| Deadline::At(now.saturating_add(TICK_NANOS)))
            .unwrap_or(Deadline::Never);
        match waiter.wait(deadline) {
            Ok(packet) if packet.key == KEY_INTERRUPT => {
                let _ = driver.transport().acknowledge_interrupt();
            }
            // A deadline that passed, which is the ordinary case.
            Ok(_) | Err(_) => {}
        }

        relay.accept();

        // What the device has to say, and what it has delivered.
        while let Some(event) = driver.poll().map_err(|_| Step::Faulted)? {
            match event {
                Event::Data | Event::Open { .. } | Event::Found { .. } => {}
            }
        }
        while let Some(len) = driver.read(&mut buffer).map_err(|_| Step::Faulted)? {
            relay.give(buffer.get(..len).unwrap_or_default());
        }

        // And what the agent has to send, while the port will take it.
        while driver.port_open() {
            let Some(read) = relay.take(&mut buffer) else {
                break;
            };
            let mut sent = 0;
            while sent < read {
                let taken = driver
                    .write(buffer.get(sent..read).unwrap_or_default())
                    .map_err(|_| Step::Faulted)?;
                if taken == 0 {
                    // The queue is full: go round, let the device drain it,
                    // and send the rest then. Nothing is lost -- `buffer`
                    // holds it -- but nothing may be sent out of order
                    // either, so this waits rather than dropping.
                    break;
                }
                sent += taken;
            }
            if sent < read {
                break;
            }
        }
    }
}
