//! The STM32MP15 board's USB host driver process: a ring-3 program that
//! finds the keyboards and mice behind the board's EHCI controller and
//! serves each to the kernel's input core (`docs/INPUT.md` §7).
//!
//! Everything that knows anything about USB is `ferrix-usb-host`, tested on
//! the host against a model of the controller and the board's bus. This
//! program is the handles it was written to be wrapped in:
//!
//! 1. START, as a bus host's: only the device, whose common window is the
//!    EHCI controller's registers. devmgr does not wait for it.
//! 2. The controller's memory is one VMO pinned with `PIN_COHERENT` before
//!    it is mapped: the controller does not snoop the caches, so the kernel
//!    maps it past them, and it is seen alike from both sides.
//! 3. One port carries every event: the controller's interrupt, and each
//!    input control channel becoming readable. The wait on it ends at the
//!    next port poll, which is when keyboards and mice are found and lost.
//! 4. Each keyboard or mouse found gets an input control channel of its own
//!    (`device.input_control()`, which a USB host's node allows up to eight
//!    times), a HELLO, and its reports as EVENTS; one unplugged has its
//!    channel closed, which the core hears as the device going.
//!
//! What it finds is said on standard error, which is the console.
//!
//! The exit status names the step that failed ([`Step`]).

#![no_std]
#![no_main]

use core::fmt::{self, Write as _};
use core::ptr;

use ferrix_blkring::control::{Message as StartMessage, START_BYTES, Start};
use ferrix_inputctl::message::{Events, MAX_BYTES, Message, PORT_RIGHTS};
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::rights::Requested;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::{IoMappingSpec, PACKET_INTERRUPT, PACKET_SIGNAL, TREE_STM32_USBH};
use ferrix_rt::native::channel::{Channel, ReadError};
use ferrix_rt::native::device::{Device, Interrupt, IoMapping};
use ferrix_rt::native::error::Error;
use ferrix_rt::native::handle::{Deadline, Object, OwnedHandle};
use ferrix_rt::native::pending::Protection;
use ferrix_rt::native::pin::{Pin, PinAccess, device_address};
use ferrix_rt::native::port::{self, Port};
use ferrix_rt::native::vmo::{self, Vmo};
use ferrix_rt::{Bootstrap, Kernel};
use ferrix_usb_host::bus::{Bus, MAX_FUNCTIONS, Note, Output};
use ferrix_usb_host::ehci::AREA_BYTES;
use ferrix_usb_host::hid::Kind;
use ferrix_usb_host::{Clock, Dma, Parts, Registers};

ferrix_rt::entry!(main);

/// A page, on every architecture this runs on.
const PAGE: usize = 4096;
/// The controller's memory, in pages.
const AREA_PAGES: usize = AREA_BYTES / PAGE;

/// Port keys: the interrupt, then one per function's control channel.
const KEY_INTERRUPT: u64 = 1;
const KEY_FIRST_CONTROL: u64 = 16;

/// What the bootstrap channel is told once the first enumeration settled:
/// devmgr reads only that something came.
const SETTLED: &[u8] = b"settled";

/// How long the core has to answer a HELLO.
const READY_PATIENCE: u64 = 2_000_000_000;

/// Where a run stopped, as the exit status.
#[derive(Clone, Copy, Debug)]
#[repr(i32)]
enum Step {
    /// No bootstrap channel, or the first message was not START.
    Start = 1,
    /// START named a device that is not a USB host.
    Identity = 2,
    /// The registers could not be mapped.
    Registers = 3,
    /// Memory could not be made, pinned or mapped.
    Memory = 4,
    /// The controller would not come up.
    Controller = 5,
    /// The port, the interrupt or the waits could not be arranged.
    Events = 7,
    /// The controller halted with a system error.
    Faulted = 8,
    /// The controller would not halt: its memory is kept.
    Wedged = 9,
}

fn main(bootstrap: Bootstrap) -> i32 {
    let Some(boot) = bootstrap else {
        return Step::Start as i32;
    };
    match run(&boot) {
        Ok(()) => 0,
        Err(step) => {
            say(format_args!("usbhid: stopped at {step:?}"));
            step as i32
        }
    }
}

// ---------------------------------------------------------------------------
// The console
// ---------------------------------------------------------------------------

/// A line on standard error, which a native process has open on the
/// console.
fn say(arguments: fmt::Arguments<'_>) {
    let mut line = Line::default();
    let _ = line.write_fmt(arguments);
    let _ = line.write_str("\n");
    let _ = ferrix_rt::linux::write(2, line.as_bytes());
}

struct Line {
    bytes: [u8; 160],
    len: usize,
}

impl Default for Line {
    fn default() -> Self {
        Line {
            bytes: [0; 160],
            len: 0,
        }
    }
}

impl Line {
    fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or(&[])
    }
}

impl fmt::Write for Line {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for &byte in text.as_bytes() {
            if let Some(slot) = self.bytes.get_mut(self.len) {
                *slot = byte;
                self.len += 1;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Registers, memory, time
// ---------------------------------------------------------------------------

/// The controller's register page.
struct Window {
    _mapping: IoMapping<Kernel>,
    base: usize,
    len: usize,
}

impl Window {
    fn address(&self, offset: u32) -> usize {
        let offset = offset as usize;
        assert!(
            offset + 4 <= self.len && offset.is_multiple_of(4),
            "a register inside the window, aligned"
        );
        self.base + offset
    }
}

impl Registers for Window {
    fn read32(&self, offset: u32) -> u32 {
        // SAFETY: mapped device memory the kernel gave this process, the
        // offset inside it and aligned, read volatile.
        unsafe { ptr::read_volatile(self.address(offset) as *const u32) }
    }

    fn write32(&mut self, offset: u32, value: u32) {
        // SAFETY: as for `read32`, and the mapping is writable.
        unsafe { ptr::write_volatile(self.address(offset) as *mut u32, value) }
    }
}

/// The controller's memory: pinned coherent, then mapped.
struct Area {
    _pin: Pin<Kernel>,
    _vmo: Vmo<Kernel>,
    base: usize,
    addresses: [u64; AREA_PAGES],
}

impl Area {
    fn new(device: &Device<Kernel>) -> Result<Area, Step> {
        let vmo = vmo::create(Kernel, AREA_BYTES).map_err(|_| Step::Memory)?;
        // Pinned before it is mapped: a coherent pin refuses a VMO anything
        // maps, since its mappings are the ones that bypass the caches.
        let pin = device
            .pin(&vmo, 0, AREA_BYTES, PinAccess::Coherent)
            .map_err(|_| Step::Memory)?;
        let mut raw = [[0_u8; 8]; AREA_PAGES];
        let got = pin.addresses(&mut raw).map_err(|_| Step::Memory)?;
        if !got.is_complete() || got.pages != AREA_PAGES {
            return Err(Step::Memory);
        }
        let mut addresses = [0_u64; AREA_PAGES];
        for (slot, bytes) in addresses.iter_mut().zip(raw.iter()) {
            *slot = device_address(*bytes);
        }
        let base = vmo
            .map(None, AREA_BYTES, Protection::ReadWrite, 0)
            .map_err(|_| Step::Memory)?;
        Ok(Area {
            _pin: pin,
            _vmo: vmo,
            base,
            addresses,
        })
    }

    fn at(&self, offset: usize, size: usize) -> usize {
        assert!(
            offset + size <= AREA_BYTES && offset.is_multiple_of(size),
            "inside the area, aligned"
        );
        self.base + offset
    }
}

impl Dma for Area {
    fn len(&self) -> usize {
        AREA_BYTES
    }

    fn read32(&self, offset: usize) -> u32 {
        // SAFETY: a mapping of this process's own pinned VMO that lives as
        // long as `self`; the offset was checked; volatile, since the other
        // side is a device.
        unsafe { ptr::read_volatile(self.at(offset, 4) as *const u32) }
    }

    fn write32(&mut self, offset: usize, value: u32) {
        // SAFETY: as for `read32`, and the mapping is writable.
        unsafe { ptr::write_volatile(self.at(offset, 4) as *mut u32, value) }
    }

    fn read8(&self, offset: usize) -> u8 {
        // SAFETY: as for `read32`.
        unsafe { ptr::read_volatile(self.at(offset, 1) as *const u8) }
    }

    fn write8(&mut self, offset: usize, value: u8) {
        // SAFETY: as for `write32`.
        unsafe { ptr::write_volatile(self.at(offset, 1) as *mut u8, value) }
    }

    fn device_address(&self, offset: usize) -> Option<u32> {
        let page = self.addresses.get(offset / PAGE)?;
        u32::try_from(page + (offset % PAGE) as u64).ok()
    }

    fn barrier(&self) {
        // The controller is outside the processors' shareability domain,
        // which `dmb ish` orders only within.
        ferrix_rt::device_barrier();
    }
}

/// Time: the monotonic clock, and sleeps on a port nothing is bound to.
struct Time {
    idle: Port<Kernel>,
}

impl Clock for Time {
    fn now_nanos(&self) -> u64 {
        ferrix_rt::linux::monotonic_nanos().unwrap_or(0)
    }

    fn sleep_nanos(&mut self, nanos: u64) {
        let until = self.now_nanos().saturating_add(nanos);
        let _ = self.idle.wait(Deadline::At(until));
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

type Host = Bus<Window, Area, Time>;

fn started(boot: &Channel<Kernel>) -> Result<(Start, Device<Kernel>), Step> {
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
        Ok(StartMessage::Start(start)) => Ok((start, device)),
        _ => Err(Step::Start),
    }
}

fn run(boot: &Channel<Kernel>) -> Result<(), Step> {
    let (start, device) = started(boot)?;
    if start.pci_device_id != TREE_STM32_USBH {
        return Err(Step::Identity);
    }
    let len = start.common.length as usize;
    if start.common.offset != 0 || len == 0 || !len.is_multiple_of(PAGE) {
        return Err(Step::Registers);
    }
    let mapping = device
        .io_mapping(IoMappingSpec {
            phys: start.common.phys,
            len: len as u64,
        })
        .map_err(|_| Step::Registers)?;
    let base = mapping.map(None).map_err(|_| Step::Registers)?;
    let registers = Window {
        _mapping: mapping,
        base,
        len,
    };
    let memory = Area::new(&device)?;
    let port = port::create(Kernel).map_err(|_| Step::Events)?;
    let interrupt = device.interrupt(0).map_err(|_| Step::Registers)?;
    interrupt
        .bind(&port, KEY_INTERRUPT)
        .map_err(|_| Step::Events)?;
    let clock = Time {
        idle: port::create(Kernel).map_err(|_| Step::Events)?,
    };

    let bus = match Bus::start(Parts {
        registers,
        memory,
        clock,
    }) {
        Ok(bus) => bus,
        Err((error, parts)) => {
            say(format_args!(
                "usbhid: the controller would not start: {error:?}"
            ));
            // Unresponsive may mean still running: its memory stays.
            if matches!(error, ferrix_usb_host::ehci::Error::Unresponsive) {
                let _kept_for_good = core::mem::ManuallyDrop::new(parts);
                return Err(Step::Wedged);
            }
            return Err(Step::Controller);
        }
    };
    say(format_args!("usbhid: EHCI running"));
    let mut driver = Driver {
        bus,
        device,
        port,
        interrupt,
        location: start.location,
        channels: [const { None }; MAX_FUNCTIONS],
    };
    let ended = driver.serve(boot);
    let Driver { bus, .. } = driver;
    match bus.shutdown() {
        Ok(parts) => {
            drop(parts);
            ended
        }
        Err(parts) => {
            let _kept_for_good = core::mem::ManuallyDrop::new(parts);
            Err(Step::Wedged)
        }
    }
}

struct Driver {
    bus: Host,
    device: Device<Kernel>,
    port: Port<Kernel>,
    interrupt: Interrupt<Kernel>,
    /// START's location word, which each HELLO repeats.
    location: u32,
    /// Each function's input control channel, once the core took it.
    channels: [Option<Channel<Kernel>>; MAX_FUNCTIONS],
}

impl Driver {
    /// Poll, forward, and wait, for as long as the controller runs.
    ///
    /// Once what was plugged in at boot is found and introduced, the
    /// bootstrap channel is told: devmgr waits for that before its REPORT,
    /// which the kernel starts init at, so a compositor that reads
    /// `/dev/input` once finds the keyboard (`docs/INPUT.md` §7.3).
    fn serve(&mut self, boot: &Channel<Kernel>) -> Result<(), Step> {
        self.bus.poll();
        self.forward();
        let _ = boot.write(SETTLED);
        loop {
            self.forward();
            let deadline = Deadline::At(self.bus.next_poll());
            match self.port.wait(deadline) {
                Ok(packet) if packet.kind == PACKET_INTERRUPT && packet.key == KEY_INTERRUPT => {
                    let serviced = self.bus.on_interrupt();
                    let _ = self.interrupt.ack();
                    if serviced.is_err() {
                        return Err(Step::Faulted);
                    }
                }
                Ok(packet) if packet.kind == PACKET_SIGNAL && packet.key >= KEY_FIRST_CONTROL => {
                    let function =
                        usize::try_from(packet.key - KEY_FIRST_CONTROL).unwrap_or(usize::MAX);
                    self.take_control(function);
                }
                Ok(_) => {}
                Err(Error::TimedOut) => self.bus.poll(),
                Err(_) => return Err(Step::Events),
            }
        }
    }

    /// Hand on everything the bus has, and say what it found.
    fn forward(&mut self) {
        while let Some(note) = self.bus.next_note() {
            tell(note);
        }
        while let Some(output) = self.bus.next_output() {
            match output {
                Output::Attached(function) => self.introduce(function),
                Output::Events(function) => {
                    let channel = self.channels.get(function).and_then(Option::as_ref);
                    if let (Some(channel), Some(events)) = (channel, Events::new(self.bus.events()))
                    {
                        let _ = channel.write(Message::Events(events).encode().as_bytes());
                    }
                }
                Output::Detached(function) => {
                    // Closing the channel is how the core hears it went.
                    if let Some(slot) = self.channels.get_mut(function) {
                        *slot = None;
                    }
                    say(format_args!("usbhid: function {function} unplugged"));
                }
            }
        }
    }

    /// Give `function` an input control channel and introduce it; a function
    /// the core will not take is left without one, and its reports go
    /// nowhere.
    fn introduce(&mut self, function: usize) {
        let Some(hello) = self.bus.hello(function, self.location) else {
            return;
        };
        let kind = self.bus.kind(function).map_or("input", Kind::word);
        let name = core::str::from_utf8(hello.name.as_bytes()).unwrap_or("?");
        let Ok(control) = self.device.input_control() else {
            say(format_args!(
                "usbhid: no input channel for the {kind} {name}"
            ));
            return;
        };
        let Ok(share) = self
            .port
            .as_owned()
            .duplicate(Requested::Exactly(PORT_RIGHTS))
        else {
            return;
        };
        if control
            .write_with(Message::Hello(hello).encode().as_bytes(), [share])
            .is_err()
        {
            return;
        }
        match ready(&control) {
            Some(node) => {
                say(format_args!("usbhid: {kind} {name} is event{node}"));
                let key = KEY_FIRST_CONTROL + function as u64;
                if control
                    .wait_async(&self.port, Signals::READABLE | Signals::PEER_CLOSED, key)
                    .is_ok()
                    && let Some(slot) = self.channels.get_mut(function)
                {
                    *slot = Some(control);
                }
            }
            None => say(format_args!("usbhid: the core refused the {kind} {name}")),
        }
    }

    /// Read what the core said on `function`'s channel: STATUS lights the
    /// device's LEDs, STOP is answered and the channel closed, and the core
    /// going away closes it too.
    fn take_control(&mut self, function: usize) {
        let Some(Some(channel)) = self.channels.get(function) else {
            return;
        };
        let mut bytes = [0_u8; MAX_BYTES];
        let mut handles = [Handle::INVALID; 1];
        let keep = match channel.read(&mut bytes, &mut handles) {
            Ok(received) => {
                for handle in handles.iter().take(received.handles) {
                    drop(OwnedHandle::from_raw(Kernel, *handle));
                }
                match Message::decode(bytes.get(..received.bytes).unwrap_or_default()) {
                    Ok(Message::Stop) => {
                        let _ = channel.write(Message::Stopped.encode().as_bytes());
                        false
                    }
                    Ok(Message::Status(leds)) => {
                        if let Err(error) = self.bus.set_leds(function, leds.as_slice()) {
                            say(format_args!(
                                "usbhid: function {function}'s LEDs were not set: {error:?}"
                            ));
                        }
                        true
                    }
                    _ => true,
                }
            }
            Err(ReadError::Failed(Error::ShouldWait)) => true,
            Err(_) => false,
        };
        let key = KEY_FIRST_CONTROL + function as u64;
        let armed = keep
            && channel
                .wait_async(&self.port, Signals::READABLE | Signals::PEER_CLOSED, key)
                .is_ok();
        if !armed && let Some(slot) = self.channels.get_mut(function) {
            *slot = None;
        }
    }
}

/// Wait for the core's answer to HELLO: the node's number on READY.
fn ready(control: &Channel<Kernel>) -> Option<u32> {
    let until = ferrix_rt::linux::monotonic_nanos()
        .ok()?
        .saturating_add(READY_PATIENCE);
    let _ = control
        .wait_one(Signals::READABLE, Deadline::At(until))
        .ok()?;
    let mut bytes = [0_u8; MAX_BYTES];
    let mut handles = [Handle::INVALID; 1];
    let received = control.read(&mut bytes, &mut handles).ok()?;
    for handle in handles.iter().take(received.handles) {
        // The core's port is for the LED row, later.
        drop(OwnedHandle::from_raw(Kernel, *handle));
    }
    match Message::decode(bytes.get(..received.bytes).unwrap_or_default()) {
        Ok(Message::Ready(ready)) => Some(ready.node),
        _ => None,
    }
}

/// Say a note on the console.
fn tell(note: Note) {
    match note {
        Note::Device {
            address,
            speed,
            vendor,
            product,
            hub_ports,
            functions,
        } => match hub_ports {
            Some(ports) => say(format_args!(
                "usbhid: device {address} {vendor:04x}:{product:04x} {speed:?} speed, a hub of {ports} ports"
            )),
            None => say(format_args!(
                "usbhid: device {address} {vendor:04x}:{product:04x} {speed:?} speed, {functions} input functions"
            )),
        },
        Note::Gone { address } => say(format_args!("usbhid: device {address} gone")),
        Note::Stopped { function } => say(format_args!(
            "usbhid: function {function}'s pipe kept failing and is stopped until it is plugged in again"
        )),
        Note::Failed { parent, error } => {
            say(format_args!(
                "usbhid: a device at {parent:?} could not be set up: {error:?}"
            ));
        }
        Note::Handed { port } => say(format_args!(
            "usbhid: root port {port} has a full- or low-speed device, which EHCI hands to its companion"
        )),
    }
}
