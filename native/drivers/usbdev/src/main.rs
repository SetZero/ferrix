//! The Pixel 7's USB device driver process: a ring-3 program that presents a
//! USB serial port (CDC-ACM) on the phone's USB-C port, through its DWC3
//! (`docs/PIXEL7-USB-HANDOVER.md`).
//!
//! Everything that knows anything about USB is `ferrix-dwc3` and
//! `ferrix-usb-device`, tested on the host against a model of the
//! controller. This program is the handles they were written to be wrapped
//! in, as `usbhid` is for the DK board's host:
//!
//! 1. START, as a gadget's: only the device, whose common window is the
//!    controller's registers. devmgr does not wait for it.
//! 2. The controller's memory is one VMO pinned with `PIN_COHERENT` before
//!    it is mapped: the controller does not snoop the caches, so the kernel
//!    maps it past them. What the library's `clean` and `invalidate` still
//!    need on such memory is a barrier, which orders the processor's writes
//!    before the register write that hands them over, and a register read
//!    before the memory reads it announced.
//! 3. One port carries the controller's interrupt. The wait on it ends at
//!    the next tick, when the heartbeat is due.
//! 4. What the port carries, for now: what the host sends is sent back, and
//!    while a program on the host has the port open, a heartbeat line a
//!    second. That proves the controller's path on the phone by itself; the
//!    kernel's log follows once its reader lands (phase 4).
//!
//! What it does is said on standard error, which is the console.
//!
//! The exit status names the step that failed ([`Step`]).

#![no_std]
#![no_main]

use core::fmt::{self, Write as _};
use core::ptr;

use ferrix_blkring::control::{Message as StartMessage, START_BYTES, Start};
use ferrix_dwc3::layout::AREA_BYTES;
use ferrix_dwc3::usb_device::acm::{DATA_IN, DATA_OUT, SerialPort};
use ferrix_dwc3::{Clock, Controller, Dma, Error as UsbError, Parts, Registers};
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::{IoMappingSpec, PACKET_INTERRUPT, TREE_GS201_DWC3};
use ferrix_rt::native::channel::Channel;
use ferrix_rt::native::device::{Device, Interrupt, IoMapping};
use ferrix_rt::native::error::Error;
use ferrix_rt::native::handle::{Deadline, Object, OwnedHandle};
use ferrix_rt::native::pending::Protection;
use ferrix_rt::native::pin::{Pin, PinAccess, device_address};
use ferrix_rt::native::port::{self, Port};
use ferrix_rt::native::vmo::{self, Vmo};
use ferrix_rt::{Bootstrap, Kernel};

ferrix_rt::entry!(main);

/// A page, on every architecture this runs on.
const PAGE: usize = 4096;
/// The controller's memory, in pages.
const AREA_PAGES: usize = AREA_BYTES.div_ceil(PAGE);

/// The port key the interrupt is bound under.
const KEY_INTERRUPT: u64 = 1;

/// How often the loop wakes when nothing happens: the heartbeat's period.
const TICK_NANOS: u64 = 1_000_000_000;

/// The serial number the port reports.
const SERIAL: &str = "ferrix-pixel7";

/// Where a run stopped, as the exit status.
#[derive(Clone, Copy, Debug)]
#[repr(i32)]
enum Step {
    /// No bootstrap channel, or the first message was not START.
    Start = 1,
    /// START named a device that is not the Pixel's USB controller.
    Identity = 2,
    /// The registers could not be mapped.
    Registers = 3,
    /// Memory could not be made, pinned or mapped.
    Memory = 4,
    /// The controller was not as ABL leaves it, or would not come up.
    Controller = 5,
    /// The port, the interrupt or the waits could not be arranged.
    Events = 7,
    /// The controller failed while running.
    Faulted = 8,
    /// The controller would not stop: its memory is kept.
    Wedged = 9,
}

fn main(bootstrap: Bootstrap) -> i32 {
    let Some(boot) = bootstrap else {
        return Step::Start as i32;
    };
    match run(&boot) {
        Ok(()) => 0,
        Err(step) => {
            say(format_args!("usbdev: stopped at {step:?}"));
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

/// A line, formatted into a fixed buffer and cut at its end.
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

/// The controller's register window.
///
/// Every access that could change the controller is logged, and so is
/// every read made before the first of them: the product owner's condition
/// for writing this controller at all (`docs/PIXEL7-USB-HANDOVER.md` §8)
/// is that the record shows the guard's reads and each write. Reads after
/// that -- mostly `GEVNTCOUNT`, at each interrupt -- are not logged.
struct Window {
    _mapping: IoMapping<Kernel>,
    base: usize,
    len: usize,
    /// Whether anything has been written yet.
    wrote: bool,
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
        let value = unsafe { ptr::read_volatile(self.address(offset) as *const u32) };
        if !self.wrote {
            say(format_args!("usbdev: read  {offset:#06x} = {value:#010x}"));
        }
        value
    }

    fn write32(&mut self, offset: u32, value: u32) {
        say(format_args!("usbdev: write {offset:#06x} = {value:#010x}"));
        self.wrote = true;
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
        let bytes = AREA_PAGES * PAGE;
        let vmo = vmo::create(Kernel, bytes).map_err(|_| Step::Memory)?;
        // Pinned before it is mapped: a coherent pin refuses a VMO anything
        // maps, since its mappings are the ones that bypass the caches.
        let pin = device
            .pin(&vmo, 0, bytes, PinAccess::Coherent)
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
            .map(None, bytes, Protection::ReadWrite, 0)
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
            offset + size <= AREA_PAGES * PAGE && offset.is_multiple_of(size),
            "inside the area, aligned"
        );
        self.base + offset
    }
}

impl Dma for Area {
    fn len(&self) -> usize {
        AREA_PAGES * PAGE
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

    fn clean(&mut self, _offset: usize, _len: usize) {
        // Uncached: only the order is owed. The controller is outside the
        // processors' shareability domain, which `dmb ish` orders only
        // within.
        ferrix_rt::device_barrier();
    }

    fn invalidate(&mut self, _offset: usize, _len: usize) {
        ferrix_rt::device_barrier();
    }

    fn device_address(&self, offset: usize) -> Option<u64> {
        let page = self.addresses.get(offset / PAGE)?;
        page.checked_add((offset % PAGE) as u64)
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

type Usb = Controller<Window, Area, Time>;

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

/// Map the controller's window START names.
fn window(start: &Start, device: &Device<Kernel>) -> Result<Window, Step> {
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
    Ok(Window {
        _mapping: mapping,
        base,
        len,
        wrote: false,
    })
}

fn run(boot: &Channel<Kernel>) -> Result<(), Step> {
    let (start, device) = started(boot)?;
    if start.pci_device_id != TREE_GS201_DWC3 {
        return Err(Step::Identity);
    }
    let registers = window(&start, &device)?;
    let memory = Area::new(&device)?;
    let port = port::create(Kernel).map_err(|_| Step::Events)?;
    let interrupt = device.interrupt(0).map_err(|_| Step::Registers)?;
    interrupt
        .bind(&port, KEY_INTERRUPT)
        .map_err(|_| Step::Events)?;
    let clock = Time {
        idle: port::create(Kernel).map_err(|_| Step::Events)?,
    };

    let usb = match Controller::start(Parts {
        registers,
        memory,
        clock,
    }) {
        Ok(usb) => usb,
        Err((error, parts)) => return Err(refused(error, parts)),
    };
    say(format_args!("usbdev: DWC3 running, waiting for a host"));
    let mut driver = Driver {
        usb,
        serial: SerialPort::new(SERIAL),
        port,
        interrupt,
        beats: 0,
        next_beat: 0,
    };
    let ended = driver.serve();
    let Driver { mut usb, .. } = driver;
    if usb.stop().is_err() {
        let _kept_for_good = core::mem::ManuallyDrop::new(usb);
        return Err(Step::Wedged);
    }
    drop(usb.into_parts());
    ended
}

/// Say why the controller would not start, and keep its memory if it may
/// still be writing it.
fn refused(error: UsbError, parts: Parts<Window, Area, Time>) -> Step {
    say(format_args!(
        "usbdev: the controller would not start: {error:?}"
    ));
    match error {
        // Nothing was written: the controller is as it was.
        UsbError::Memory | UsbError::NotDwc3(_) | UsbError::Refused { .. } => {
            drop(parts);
            Step::Controller
        }
        // It may be running, and may write its memory.
        _ => {
            let _kept_for_good = core::mem::ManuallyDrop::new(parts);
            Step::Wedged
        }
    }
}

struct Driver {
    usb: Usb,
    serial: SerialPort,
    port: Port<Kernel>,
    interrupt: Interrupt<Kernel>,
    /// Heartbeats sent.
    beats: u64,
    /// When the next is due.
    next_beat: u64,
}

impl Driver {
    /// Serve interrupts and the heartbeat for as long as the controller
    /// runs.
    fn serve(&mut self) -> Result<(), Step> {
        loop {
            let now = self.usb.clock().now_nanos();
            let deadline = Deadline::At(self.next_beat.max(now.saturating_add(1)));
            match self.port.wait(deadline) {
                Ok(packet) if packet.kind == PACKET_INTERRUPT && packet.key == KEY_INTERRUPT => {
                    let serviced = self.usb.on_interrupt(&mut self.serial);
                    let _ = self.interrupt.ack();
                    match serviced {
                        Ok(notice) => self.act(notice)?,
                        Err(error) => {
                            say(format_args!("usbdev: {error:?}"));
                            return Err(Step::Faulted);
                        }
                    }
                }
                Ok(_) => {}
                Err(Error::TimedOut) => self.beat()?,
                Err(_) => return Err(Step::Events),
            }
        }
    }

    /// Act on what an interrupt found.
    fn act(&mut self, notice: ferrix_dwc3::Notice) -> Result<(), Step> {
        if let Some(speed) = notice.connected {
            say(format_args!("usbdev: connected at {speed:?} speed"));
        }
        if notice.configuration {
            say(format_args!(
                "usbdev: {}",
                if self.serial.is_configured() {
                    "configured: the host sees a serial port"
                } else {
                    "unconfigured"
                }
            ));
        }
        if notice.disconnected {
            say(format_args!("usbdev: the host went away"));
        }
        if notice.received {
            self.echo()?;
        }
        Ok(())
    }

    /// Send back what the host sent.
    fn echo(&mut self) -> Result<(), Step> {
        let mut bytes = [0_u8; 512];
        let count = self
            .usb
            .read(DATA_OUT, &mut bytes)
            .map_err(|_| Step::Faulted)?;
        if let Some(received) = bytes.get(..count)
            && !received.is_empty()
        {
            // What does not fit in the ring is dropped: an echo is best
            // effort, and a host that sends faster than it reads loses it.
            let _taken = self
                .usb
                .write(DATA_IN, received)
                .map_err(|_| Step::Faulted)?;
        }
        Ok(())
    }

    /// Once a second, a line while a program on the host has the port open;
    /// what waits to go is dropped while none has.
    fn beat(&mut self) -> Result<(), Step> {
        let now = self.usb.clock().now_nanos();
        if now < self.next_beat {
            return Ok(());
        }
        self.next_beat = now.saturating_add(TICK_NANOS);
        if !self.serial.is_configured() {
            return Ok(());
        }
        if !self.serial.dtr() {
            self.usb.discard(DATA_IN);
            return Ok(());
        }
        self.beats += 1;
        let mut line = Line::default();
        let _ = write!(
            line,
            "usbdev: heartbeat {} at {} ms\r\n",
            self.beats,
            now / 1_000_000
        );
        // A heartbeat that does not fit is dropped: the next one says how
        // many came before it.
        let _taken = self
            .usb
            .write(DATA_IN, line.as_bytes())
            .map_err(|_| Step::Faulted)?;
        Ok(())
    }
}
