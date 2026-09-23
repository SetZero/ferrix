//! The STM32MP15 DK board's HDMI driver process: a ring-3 program that
//! serves one card to the kernel's display core (`docs/DISPLAY.md` §6).
//!
//! Everything that decides a register value is `ferrix-stm32-display`,
//! tested on the host. This program is the handles it was written to be
//! wrapped in:
//!
//! 1. START, as the virtio-gpu driver's: the device, the driver's end of the
//!    display control channel, and the device's two register windows -- the
//!    LTDC's where virtio's common block would be, the HDMI bridge's I2C
//!    controller's where its device block would be.
//! 2. The bridge is found on the bus and its monitor's EDID read, which says
//!    whether to speak HDMI or DVI; the LTDC starts 720p60 with its layer
//!    off; the bridge is told the mode and its TMDS output turned on. HELLO
//!    then offers the one scanout, and READY brings the card VMO back.
//! 3. ATTACH pins a buffer's range read-only and requires it to be one run
//!    of addresses, which the kernel makes it for this device. SCANOUT points
//!    the layer at it; FLUSH asks for a reload at the next vertical blanking
//!    and FLIPPED goes out when the reload's interrupt comes, so a
//!    compositor's page flips are paced by the screen. DETACH waits for a
//!    buffer on screen to leave it before it unpins.
//! 4. STOP, or the core closing its end, turns the controller off -- nothing
//!    is read from memory after -- and TMDS with it.
//!
//! The exit status names the step that failed ([`Step`]), 0 a clean STOP.

#![no_std]
#![no_main]

use core::ptr;

use ferrix_blkring::control::{Block as StartBlock, Message as StartMessage, START_BYTES, Start};
use ferrix_displayctl::message::{
    Attach, Hello, MAX_BUFFER_PAGES as MAX_PAGES, MAX_BYTES, MAX_SCANOUTS, Message, PORT_RIGHTS,
    Rect, ScanoutMode, Status, VERSION,
};
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::rights::Requested;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::{
    IoMappingSpec, PACKET_INTERRUPT, PACKET_SIGNAL, PACKET_USER, TREE_STM32_HDMI,
};
use ferrix_rt::native::channel::{Channel, ReadError};
use ferrix_rt::native::device::{Device, Interrupt, IoMapping};
use ferrix_rt::native::error::Error;
use ferrix_rt::native::handle::{Deadline, Object, OwnedHandle};
use ferrix_rt::native::pending::Protection;
use ferrix_rt::native::pin::{Pin, PinAccess, device_address};
use ferrix_rt::native::port::{self, Port};
use ferrix_rt::native::vmo::{self, Vmo};
use ferrix_rt::{Bootstrap, Kernel};
use ferrix_stm32_display::i2c::{I2c, TIMING_100KHZ_AT_64MHZ};
use ferrix_stm32_display::ltdc::{Frame, Ltdc};
use ferrix_stm32_display::mode::Mode;
use ferrix_stm32_display::sii9022::{self, Bridge};
use ferrix_stm32_display::{Budget, Registers, edid};

ferrix_rt::entry!(main);

/// A page.
const PAGE: usize = 4096;

/// The most pages one buffer may have, as the protocol says.
const MAX_BUFFER_PAGES: usize = MAX_PAGES as usize;

/// Buffers pinned at once: a compositor double- or triple-buffers.
const MAX_BUFFERS: usize = 8;

/// Flushes waiting for their reload at once. The core waits for each
/// FLIPPED before it sends the next FLUSH, so one is all there ever is.
const MAX_PENDING: usize = 8;

/// Port keys.
const KEY_INTERRUPT: u64 = 1;
const KEY_CONTROL: u64 = 2;

/// How long a buffer leaving the screen may take to go: one frame is
/// 16.7 ms, and a register read is well under a microsecond.
const LEAVE_BUDGET: Budget = Budget(2_000_000);

/// The mode the board runs, the one its pixel clock was set for.
const MODE: Mode = Mode::CEA_720P60;

/// Where a run stopped, as the exit status.
#[derive(Clone, Copy, Debug)]
#[repr(i32)]
enum Step {
    /// No bootstrap channel, or the first message was not START.
    Start = 1,
    /// START named a device that is not a DK board's HDMI output.
    Identity = 2,
    /// A register window could not be mapped.
    Registers = 3,
    /// Memory could not be made or mapped.
    Memory = 4,
    /// The LTDC is not a version this driver knows.
    Controller = 5,
    /// HELLO could not be sent, or READY did not come.
    Hello = 6,
    /// The port, the interrupt or the waits could not be arranged.
    Events = 7,
    /// The core sent something this driver cannot act on.
    Control = 8,
    /// Nothing answered at the bridge's address: unpowered, in reset, or
    /// the bus's pins are not the controller's.
    BridgeSilent = 20,
    /// Something answered, but not a `SiI902x`.
    BridgeChip = 21,
    /// The bridge would not take the mode.
    BridgeMode = 22,
    /// Another bus failure talking to the bridge.
    BridgeBus = 23,
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
// Registers
// ---------------------------------------------------------------------------

/// One register window, mapped.
struct Window {
    _mapping: IoMapping<Kernel>,
    base: usize,
    len: usize,
}

impl Window {
    fn map(device: &Device<Kernel>, block: &StartBlock) -> Result<Window, Step> {
        let len = block.length as usize;
        if block.offset != 0 || len == 0 || !len.is_multiple_of(PAGE) {
            return Err(Step::Registers);
        }
        let mapping = device
            .io_mapping(IoMappingSpec {
                phys: block.phys,
                len: len as u64,
            })
            .map_err(|_| Step::Registers)?;
        let base = mapping.map(None).map_err(|_| Step::Registers)?;
        Ok(Window {
            _mapping: mapping,
            base,
            len,
        })
    }

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

/// Room for a buffer's page addresses as the pin query writes them: 64 KiB,
/// too much for a stack.
struct Scratch {
    _vmo: Vmo<Kernel>,
    base: usize,
}

impl Scratch {
    fn new() -> Result<Scratch, Step> {
        let bytes = (MAX_BUFFER_PAGES * 8).div_ceil(PAGE) * PAGE;
        let vmo = vmo::create(Kernel, bytes).map_err(|_| Step::Memory)?;
        let base = vmo
            .map(None, bytes, Protection::ReadWrite, 0)
            .map_err(|_| Step::Memory)?;
        Ok(Scratch { _vmo: vmo, base })
    }

    fn raw(&mut self, pages: usize) -> &mut [[u8; 8]] {
        // SAFETY: `base` starts a zero-filled private mapping of
        // `MAX_BUFFER_PAGES` eight-byte arrays, which need no alignment;
        // `pages` is capped to that; `&mut self` keeps the slice unique.
        unsafe {
            core::slice::from_raw_parts_mut(self.base as *mut [u8; 8], pages.min(MAX_BUFFER_PAGES))
        }
    }
}

// ---------------------------------------------------------------------------
// Bring-up
// ---------------------------------------------------------------------------

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
    let [device, control] = handles;
    let device = Device::from_owned(OwnedHandle::from_raw(Kernel, device));
    let control = Channel::from_owned(OwnedHandle::from_raw(Kernel, control));
    match StartMessage::decode(bytes.get(..received.bytes).unwrap_or_default()) {
        Ok(StartMessage::Start(start)) => Ok(Started {
            start,
            device,
            control,
        }),
        _ => Err(Step::Start),
    }
}

/// Find the bridge and ask the monitor what it speaks: HDMI when its EDID
/// has an HDMI vendor block, DVI when it has none or cannot be read.
fn bridge(i2c: I2c<Window>) -> Result<(Bridge<I2c<Window>>, bool), Step> {
    use sii9022::{BridgeError, BusError};
    let mut bridge = Bridge::probe(i2c, sii9022::ADDRESS).map_err(|error| match error {
        BridgeError::Bus(BusError::Nack) => Step::BridgeSilent,
        BridgeError::Chip(_) => Step::BridgeChip,
        _ => Step::BridgeBus,
    })?;
    let mut base = [0_u8; edid::BLOCK_BYTES];
    let mut extension = [0_u8; edid::BLOCK_BYTES];
    let hdmi = bridge.read_edid(0, &mut base).is_ok()
        && edid::is_base(&base)
        && edid::extensions(&base) > 0
        && bridge.read_edid(1, &mut extension).is_ok()
        && edid::is_hdmi(&extension);
    Ok((bridge, hdmi))
}

/// Say HELLO for the one scanout and take READY's card VMO.
fn introduce(
    control: &Channel<Kernel>,
    port: &Port<Kernel>,
    location: u32,
) -> Result<Vmo<Kernel>, Step> {
    let mut modes = [ScanoutMode::default(); MAX_SCANOUTS];
    if let Some(first) = modes.first_mut() {
        *first = ScanoutMode {
            width: u32::from(MODE.hdisplay),
            height: u32::from(MODE.vdisplay),
            enabled: true,
        };
    }
    let hello = Hello {
        version: VERSION,
        scanouts: 1,
        location,
        modes,
        virgl: false,
        capsets: 0,
        capset: 0,
        capset_bytes: 0,
    };
    let share = port
        .as_owned()
        .duplicate(Requested::Exactly(PORT_RIGHTS))
        .map_err(|_| Step::Hello)?;
    control
        .write_with(Message::Hello(hello).encode().as_bytes(), [share])
        .map_err(|_| Step::Hello)?;
    let _ = control
        .wait_one(Signals::READABLE, Deadline::Never)
        .map_err(|_| Step::Hello)?;
    let mut bytes = [0_u8; MAX_BYTES];
    let mut handles = [Handle::INVALID; 2];
    let received = control
        .read(&mut bytes, &mut handles)
        .map_err(|_| Step::Hello)?;
    let [card, core_port] = handles;
    match Message::decode(bytes.get(..received.bytes).unwrap_or_default()) {
        Ok(Message::Ready(_)) if received.handles == 2 => {
            drop(OwnedHandle::from_raw(Kernel, core_port));
            Ok(Vmo::from_owned(OwnedHandle::from_raw(Kernel, card)))
        }
        _ => Err(Step::Control),
    }
}

fn run(boot: &Channel<Kernel>) -> Result<(), Step> {
    let Started {
        start,
        device,
        control,
    } = started(boot)?;
    if start.pci_device_id != TREE_STM32_HDMI {
        return Err(Step::Identity);
    }
    let ltdc_window = Window::map(&device, &start.common)?;
    let i2c_window = Window::map(&device, &start.device)?;
    let interrupt = device.interrupt(0).map_err(|_| Step::Events)?;
    let port = port::create(Kernel).map_err(|_| Step::Events)?;
    interrupt
        .bind(&port, KEY_INTERRUPT)
        .map_err(|_| Step::Events)?;

    let mut ltdc = Ltdc::new(ltdc_window).map_err(|_| Step::Controller)?;
    let (mut bridge, hdmi) = bridge(I2c::new(i2c_window, TIMING_100KHZ_AT_64MHZ))?;
    // The bridge learns the mode with its output off, the controller starts
    // sending it, and only then does the monitor see a signal.
    bridge.set_mode(&MODE, hdmi).map_err(|_| Step::BridgeMode)?;
    ltdc.start(&MODE).map_err(|_| Step::Controller)?;
    bridge.enable().map_err(|_| Step::BridgeBus)?;

    let card = match introduce(&control, &port, start.location) {
        Ok(card) => card,
        Err(step) => {
            ltdc.stop();
            let _ = bridge.disable();
            return Err(step);
        }
    };
    control
        .wait_async(&port, Signals::READABLE | Signals::PEER_CLOSED, KEY_CONTROL)
        .map_err(|_| Step::Events)?;

    let mut serving = Serving {
        ltdc,
        interrupt,
        card,
        device,
        control,
        port,
        scratch: Scratch::new()?,
        buffers: [const { None }; MAX_BUFFERS],
        shown: None,
        pending: [0; MAX_PENDING],
        waiting: 0,
    };
    let ended = serving.serve();
    serving.ltdc.stop();
    let _ = bridge.disable();
    if matches!(ended, Ok(true)) {
        let _ = serving.control.write(Message::Stopped.encode().as_bytes());
    }
    ended.map(drop)
}

// ---------------------------------------------------------------------------
// Serving
// ---------------------------------------------------------------------------

/// A buffer the device may be shown.
struct Buffer {
    id: u32,
    _pin: Pin<Kernel>,
    /// Bus address of its first byte.
    address: u32,
    stride: u32,
    width: u32,
    height: u32,
}

/// The serve loop's state.
struct Serving {
    ltdc: Ltdc<Window>,
    interrupt: Interrupt<Kernel>,
    card: Vmo<Kernel>,
    device: Device<Kernel>,
    control: Channel<Kernel>,
    port: Port<Kernel>,
    scratch: Scratch,
    buffers: [Option<Buffer>; MAX_BUFFERS],
    /// The buffer on screen, or `None` with the layer off.
    shown: Option<u32>,
    /// Flush sequences waiting for the next reload, oldest first.
    pending: [u64; MAX_PENDING],
    waiting: usize,
}

impl Serving {
    /// Serve until STOP (`true`) or the core closing its end (`false`).
    fn serve(&mut self) -> Result<bool, Step> {
        loop {
            let packet = self.port.wait(Deadline::Never).map_err(|_| Step::Events)?;
            match (packet.kind, packet.key) {
                (PACKET_INTERRUPT, KEY_INTERRUPT) => {
                    let events = self.ltdc.take_events();
                    let _ = self.interrupt.ack();
                    if events.reloaded {
                        self.flipped()?;
                    }
                }
                (PACKET_SIGNAL | PACKET_USER, KEY_CONTROL) => {
                    if let Some(stopped) = self.take_messages()? {
                        return Ok(stopped);
                    }
                    self.control
                        .wait_async(
                            &self.port,
                            Signals::READABLE | Signals::PEER_CLOSED,
                            KEY_CONTROL,
                        )
                        .map_err(|_| Step::Events)?;
                }
                _ => {}
            }
        }
    }

    /// Answer every flush waiting for a reload: one has happened.
    fn flipped(&mut self) -> Result<(), Step> {
        let waiting = self.waiting;
        self.waiting = 0;
        for &sequence in self.pending.iter().take(waiting) {
            self.reply(Message::Flipped {
                sequence,
                status: Status::Ok,
            })?;
        }
        Ok(())
    }

    fn reply(&self, message: Message) -> Result<(), Step> {
        self.control
            .write(message.encode().as_bytes())
            .map_err(|_| Step::Control)
    }

    /// Take the messages the core has sent until none is left. `Some` when
    /// the run is over.
    fn take_messages(&mut self) -> Result<Option<bool>, Step> {
        loop {
            let mut bytes = [0_u8; MAX_BYTES];
            match self.control.read(&mut bytes, &mut []) {
                Ok(received) => {
                    let message = Message::decode(bytes.get(..received.bytes).unwrap_or_default())
                        .map_err(|_| Step::Control)?;
                    match message {
                        Message::Stop => return Ok(Some(true)),
                        Message::Refused(_) => return Err(Step::Control),
                        other => self.act(other)?,
                    }
                }
                Err(ReadError::Failed(Error::PeerClosed)) => return Ok(Some(false)),
                Err(ReadError::Failed(Error::ShouldWait)) => return Ok(None),
                Err(_) => return Err(Step::Control),
            }
        }
    }

    /// Do what one message asks, and answer it.
    fn act(&mut self, message: Message) -> Result<(), Step> {
        match message {
            Message::Attach(attach) => {
                let status = self.attach(&attach);
                self.reply(Message::Attached {
                    buffer: attach.buffer,
                    status,
                })
            }
            // Pixels on a GPU this card does not have.
            Message::AttachObject(attach) => self.reply(Message::Attached {
                buffer: attach.buffer,
                status: Status::Invalid,
            }),
            Message::Scanout { buffer, rect, .. } => {
                self.scanout(buffer, rect);
                Ok(())
            }
            Message::Flush {
                buffer, sequence, ..
            } => self.flush(buffer, sequence),
            Message::Detach { buffer } => {
                let status = self.detach(buffer);
                self.reply(Message::Detached { buffer, status })
            }
            _ => Err(Step::Control),
        }
    }

    /// Pin a buffer's range and require it to be one run of addresses below
    /// 4 GiB, which is all the LTDC reads.
    fn attach(&mut self, attach: &Attach) -> Status {
        if attach.validate(self.card_bytes()).is_err() {
            return Status::Invalid;
        }
        let Some(slot) = self.buffers.iter().position(Option::is_none) else {
            return Status::OutOfMemory;
        };
        let (Ok(offset), Ok(length)) = (
            usize::try_from(attach.offset),
            usize::try_from(attach.length),
        ) else {
            return Status::Invalid;
        };
        let pages = length / PAGE;
        let Ok(pin) = self
            .device
            .pin(&self.card, offset, length, PinAccess::ReadOnly)
        else {
            return Status::PinFailed;
        };
        let raw = self.scratch.raw(pages);
        let complete = pin
            .addresses(raw)
            .is_ok_and(|got| got.pages == pages && got.written == pages);
        let first = raw.first().map_or(0, |bytes| device_address(*bytes));
        let contiguous = raw
            .iter()
            .enumerate()
            .all(|(index, bytes)| device_address(*bytes) == first + (index * PAGE) as u64);
        let Ok(address) = u32::try_from(first) else {
            return Status::PinFailed;
        };
        if !complete || !contiguous || u64::from(address) + attach.length > 1 << 32 {
            return Status::PinFailed;
        }
        if let Some(held) = self.buffers.get_mut(slot) {
            *held = Some(Buffer {
                id: attach.buffer,
                _pin: pin,
                address,
                stride: attach.stride,
                width: attach.width,
                height: attach.height,
            });
        }
        Status::Ok
    }

    fn card_bytes(&self) -> u64 {
        self.card.size().unwrap_or(0)
    }

    fn buffer(&self, id: u32) -> Option<&Buffer> {
        self.buffers.iter().flatten().find(|buffer| buffer.id == id)
    }

    /// Point the layer at `rect` of a buffer, from the next frame; buffer 0
    /// takes it off the screen. A buffer that is not the mode's size, or a
    /// rectangle that does not start it at the screen's corner, is left off
    /// the screen: the core does not wait for an answer to SCANOUT.
    fn scanout(&mut self, id: u32, rect: Rect) {
        if id == 0 {
            self.ltdc.hide();
            self.shown = None;
            return;
        }
        let Some(buffer) = self.buffer(id) else {
            return;
        };
        let start = u64::from(rect.y) * u64::from(buffer.stride) + u64::from(rect.x) * 4;
        let Ok(address) = u32::try_from(u64::from(buffer.address) + start) else {
            return;
        };
        let frame = Frame {
            address,
            pitch: buffer.stride,
            width: rect.width.min(buffer.width),
            height: rect.height.min(buffer.height),
        };
        if self.ltdc.show(&frame).is_ok() {
            self.shown = Some(id);
        }
    }

    /// Ask for a reload at the next vertical blanking and answer when it
    /// comes; a buffer not on screen has nothing to wait for.
    fn flush(&mut self, buffer: u32, sequence: u64) -> Result<(), Step> {
        if self.shown != Some(buffer) {
            return self.reply(Message::Flipped {
                sequence,
                status: Status::Ok,
            });
        }
        let Some(slot) = self.pending.get_mut(self.waiting) else {
            return Err(Step::Control);
        };
        *slot = sequence;
        self.waiting += 1;
        self.ltdc.request_reload();
        Ok(())
    }

    /// Unpin a buffer, once the LTDC has stopped reading it.
    fn detach(&mut self, id: u32) -> Status {
        if self.shown == Some(id) {
            self.ltdc.hide();
            self.shown = None;
        }
        // A buffer taken off the screen is read until the reload happens.
        if !LEAVE_BUDGET.wait(|| !self.ltdc.reload_pending()) {
            return Status::DeviceRefused;
        }
        match self
            .buffers
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|buffer| buffer.id == id))
        {
            Some(slot) => {
                *slot = None;
                Status::Ok
            }
            None => Status::Invalid,
        }
    }
}
