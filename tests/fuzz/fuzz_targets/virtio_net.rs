//! Fuzz the virtio-net driver against a device the input controls.
//!
//! From stage 10 the driver runs in a user process and believes nothing the
//! device says: its registers, its configuration, the used rings of both
//! queues, the twelve header bytes before every received frame, and any byte
//! of either ring it cares to scribble on through DMA. This target gives all
//! of that to the fuzzer.
//!
//! # The input
//!
//! Two flag bytes choose what the device offers and how it misbehaves at
//! bring-up (refusing features, never resetting, a configuration that keeps
//! changing, dropping a queue's vector); then a configuration-block length and
//! its first 24 bytes, raw — which is where the MAC, the link status and the
//! advised MTU come from; then the device's and the driver's largest queue
//! size. Everything after is a script of actions, each a byte and its
//! arguments: submit a frame of a chosen offset and length; take either
//! available ring; write a frame into a receive buffer with a header and a
//! `written` the fuzzer chose; complete a transmit chain with a chosen
//! `written`; forge a used entry on either queue; scribble a byte anywhere in
//! either ring; take events into a slice of chosen length; release a buffer;
//! refill; re-read the configuration; set or clear `DEVICE_NEEDS_RESET`. Bytes
//! past the end read as zero.
//!
//! # The properties
//!
//! Not panicking is the floor, and it carries most of the weight here: a
//! `written` the device invented, a header shorter than the negotiated length
//! and a descriptor id the driver never handed out must each come back as a
//! `DeviceError` rather than as a read past the end of a buffer. Beyond it:
//!
//! 1. **An event names an accepted frame, and each at most once.**
//! 2. **The driver's count of frames in flight is exactly** those accepted and
//!    not answered.
//! 3. **Once the driver reports a fault it accepts nothing more.**
//! 4. **Every accepted frame is answered exactly once**: by an `Event::Sent`,
//!    or by the abandoned list after shutdown — whether the reset finished or
//!    not.

#![no_main]

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::mem::ManuallyDrop;
use std::rc::Rc;

use ferrix_virtio::pci::{
    CONFIG_GENERATION, CommonConfig, DEVICE_FEATURE, DEVICE_FEATURE_SELECT, DEVICE_STATUS,
    DRIVER_FEATURE, DRIVER_FEATURE_SELECT, FEATURE_ACCESS_PLATFORM, FEATURE_VERSION_1, NO_VECTOR,
    QUEUE_ENABLE, QUEUE_MSIX_VECTOR, QUEUE_NOTIFY_OFF, QUEUE_SELECT, QUEUE_SIZE,
    STATUS_DEVICE_NEEDS_RESET, STATUS_FEATURES_OK,
};
use ferrix_virtio::net::{
    FEATURE_MAC, FEATURE_MRG_RXBUF, FEATURE_MTU, FEATURE_SPEED_DUPLEX, FEATURE_STATUS, HEADER_LEN,
};
use ferrix_virtio::{Descriptor, DeviceConfig, Layout, QueueMemory, SplitQueueDevice};
use ferrix_virtio_net::{
    DevicePages, Driver, Event, Frame, ISR_QUEUE, Options, Parts, RECEIVE_QUEUE, RequestArea, Slot,
    SubmitError, TRANSMIT_QUEUE, Teardown, Transport,
};
use libfuzzer_sys::fuzz_target;

/// Bytes a page.
const PAGE: usize = 4096;

/// The most actions one input runs.
const MAX_ACTIONS: usize = 4096;

/// The fuzzer's bytes, read front to back; zero once they run out.
struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn u8(&mut self) -> u8 {
        match self.0.split_first() {
            Some((first, rest)) => {
                self.0 = rest;
                *first
            }
            None => 0,
        }
    }

    fn u16(&mut self) -> u16 {
        u16::from_le_bytes([self.u8(), self.u8()])
    }

    fn u32(&mut self) -> u32 {
        u32::from_le_bytes([self.u8(), self.u8(), self.u8(), self.u8()])
    }
}

/// A pinned region: bytes, and each page's device address.
#[derive(Clone)]
struct Region {
    bytes: Rc<RefCell<Vec<u8>>>,
    pages: Vec<u64>,
}

impl Region {
    /// `count` pages from `base`, `step` apart.
    fn new(count: usize, base: u64, step: u64) -> Self {
        Region {
            bytes: Rc::new(RefCell::new(vec![0; count * PAGE])),
            pages: (0..count as u64).map(|page| base + page * step).collect(),
        }
    }

    /// Where device address `address` is in this region.
    fn locate(&self, address: u64) -> Option<usize> {
        self.pages.iter().enumerate().find_map(|(index, page)| {
            let within = address.checked_sub(*page)?;
            (within < PAGE as u64).then(|| index * PAGE + within as usize)
        })
    }

    fn get(&self, offset: usize) -> u8 {
        self.bytes.borrow().get(offset).copied().unwrap_or(0)
    }

    fn set(&self, offset: usize, value: u8) {
        if let Some(slot) = self.bytes.borrow_mut().get_mut(offset) {
            *slot = value;
        }
    }

    /// Write `bytes` at device address `address`, dropping what is not mapped.
    fn device_write(&self, address: u64, bytes: &[u8]) {
        for (index, byte) in bytes.iter().enumerate() {
            if let Some(at) = self.locate(address.wrapping_add(index as u64)) {
                self.set(at, *byte);
            }
        }
    }
}

impl DevicePages for Region {
    fn device_pages(&self) -> &[u64] {
        &self.pages
    }
}

impl RequestArea for Region {
    fn read_u8(&self, offset: usize) -> u8 {
        self.get(offset)
    }
    fn write_u8(&mut self, offset: usize, value: u8) {
        self.set(offset, value);
    }
}

// SAFETY: every access is a checked lookup into a `Vec` and a miss reads zero;
// the target is single-threaded, so the barrier has nothing to order.
unsafe impl QueueMemory for Region {
    fn read_u8(&self, offset: usize) -> u8 {
        self.get(offset)
    }
    fn write_u8(&mut self, offset: usize, value: u8) {
        self.set(offset, value);
    }
    fn barrier(&self) {}
}

/// One queue's registers and its end of the ring.
struct Queue {
    size: u16,
    vector: u16,
    rings: Region,
    ring: Option<SplitQueueDevice<Region>>,
    taken: Vec<u16>,
}

/// The device: registers, configuration, and both queues.
struct Device {
    offered: u64,
    device_select: u32,
    status: u8,
    generation: Cell<u8>,
    churn: bool,
    refuse_features: bool,
    never_reset: bool,
    drop_vector: bool,
    needs_reset: bool,
    config: Vec<u8>,
    queue_select: u16,
    queue_max: u16,
    isr: u8,
    queues: [Queue; 2],
    receive_data: Region,
}

impl Device {
    fn selected(&self) -> Option<&Queue> {
        self.queues.get(usize::from(self.queue_select))
    }

    fn register(&self, offset: u32) -> u32 {
        match offset {
            DEVICE_FEATURE => match self.device_select {
                0 => self.offered as u32,
                1 => (self.offered >> 32) as u32,
                _ => 0,
            },
            DEVICE_STATUS => u32::from(
                self.status
                    | if self.needs_reset {
                        STATUS_DEVICE_NEEDS_RESET
                    } else {
                        0
                    },
            ),
            CONFIG_GENERATION => {
                if self.churn {
                    self.generation.set(self.generation.get().wrapping_add(1));
                }
                u32::from(self.generation.get())
            }
            QUEUE_SIZE => u32::from(self.selected().map_or(0, |queue| queue.size)),
            QUEUE_MSIX_VECTOR => u32::from(self.selected().map_or(NO_VECTOR, |queue| queue.vector)),
            QUEUE_NOTIFY_OFF => 0,
            _ => 0,
        }
    }

    fn set_register(&mut self, offset: u32, value: u32) {
        match offset {
            DEVICE_FEATURE_SELECT => self.device_select = value,
            DRIVER_FEATURE_SELECT | DRIVER_FEATURE => {}
            DEVICE_STATUS => {
                let mut value = value as u8;
                if value == 0 {
                    if !self.never_reset {
                        self.status = 0;
                        let max = self.queue_max;
                        for queue in &mut self.queues {
                            queue.size = max;
                            queue.ring = None;
                            queue.taken.clear();
                        }
                    }
                    return;
                }
                if self.refuse_features {
                    value &= !STATUS_FEATURES_OK;
                }
                self.status = value;
            }
            QUEUE_SELECT => self.queue_select = value as u16,
            QUEUE_SIZE => {
                if let Some(queue) = self.queues.get_mut(usize::from(self.queue_select)) {
                    queue.size = value as u16;
                }
            }
            QUEUE_MSIX_VECTOR => {
                let dropped = self.drop_vector;
                if let Some(queue) = self.queues.get_mut(usize::from(self.queue_select)) {
                    queue.vector = if dropped { NO_VECTOR } else { value as u16 };
                }
            }
            QUEUE_ENABLE => {
                if let Some(queue) = self.queues.get_mut(usize::from(self.queue_select))
                    && let Ok(layout) = Layout::for_size(queue.size)
                    && layout.total_size <= queue.rings.bytes.borrow().len()
                {
                    queue.ring = Some(SplitQueueDevice::new(layout, queue.rings.clone()));
                }
            }
            _ => {}
        }
    }

    /// Take every chain the driver has published on `queue`.
    fn take_available(&mut self, queue: usize) {
        if let Some(queue) = self.queues.get_mut(queue)
            && let Some(ring) = queue.ring.as_mut()
        {
            for _ in 0..256 {
                match ring.next_chain() {
                    Ok(Some(head)) => queue.taken.push(head),
                    _ => break,
                }
            }
        }
    }

    /// The descriptors of the chain at `head` of `queue`.
    fn chain(&self, queue: usize, head: u16) -> Vec<Descriptor> {
        let Some(ring) = self.queues.get(queue).and_then(|queue| queue.ring.as_ref()) else {
            return Vec::new();
        };
        let mut descriptors = vec![BLANK; usize::from(ring.layout().queue_size)];
        match ring.read_chain(head, &mut descriptors) {
            Ok(count) => descriptors[..count].to_vec(),
            Err(_) => Vec::new(),
        }
    }

    /// Complete the chain at `head` of `queue`.
    fn complete(&mut self, queue: usize, head: u16, written: u32) {
        if let Some(ring) = self.queues.get_mut(queue).and_then(|queue| queue.ring.as_mut()) {
            let _ = ring.complete(head, written);
        }
        self.isr |= ISR_QUEUE;
    }
}

const BLANK: Descriptor = Descriptor {
    address: 0,
    len: 0,
    flags: 0,
    next: 0,
};

/// The driver's handle on the device.
#[derive(Clone)]
struct Handle {
    device: Rc<RefCell<Device>>,
    vectors: [u16; 2],
}

impl CommonConfig for Handle {
    fn read8(&self, offset: u32) -> u8 {
        self.device.borrow().register(offset) as u8
    }
    fn read16(&self, offset: u32) -> u16 {
        self.device.borrow().register(offset) as u16
    }
    fn read32(&self, offset: u32) -> u32 {
        self.device.borrow().register(offset)
    }
    fn write8(&mut self, offset: u32, value: u8) {
        self.device
            .borrow_mut()
            .set_register(offset, u32::from(value));
    }
    fn write16(&mut self, offset: u32, value: u16) {
        self.device
            .borrow_mut()
            .set_register(offset, u32::from(value));
    }
    fn write32(&mut self, offset: u32, value: u32) {
        self.device.borrow_mut().set_register(offset, value);
    }
}

impl DeviceConfig for Handle {
    fn config_len(&self) -> u32 {
        self.device.borrow().config.as_slice().config_len()
    }
    fn config_read8(&self, offset: u32) -> u8 {
        self.device.borrow().config.as_slice().config_read8(offset)
    }
    fn config_read16(&self, offset: u32) -> u16 {
        self.device.borrow().config.as_slice().config_read16(offset)
    }
    fn config_read32(&self, offset: u32) -> u32 {
        self.device.borrow().config.as_slice().config_read32(offset)
    }
}

impl Transport for Handle {
    fn notify(&mut self, _queue: u16, _notify_off: u16) {}
    fn queue_vector(&self, queue: u16) -> u16 {
        self.vectors
            .get(usize::from(queue))
            .copied()
            .unwrap_or(NO_VECTOR)
    }
    fn acknowledge_interrupt(&mut self) -> u8 {
        std::mem::take(&mut self.device.borrow_mut().isr)
    }
}

type FuzzDriver = Driver<Handle, Region, Region, Region, Vec<Slot>>;

type FuzzReleased = ferrix_virtio_net::Released<Handle, Region, Region, Region, Vec<Slot>>;

/// The parts a teardown hands back, whether the device reset or not.
///
/// A wedged teardown keeps its memory in `ManuallyDrop` because a real device
/// that did not reset may still write to it, and a real process must leak it.
/// This device is a struct in this process that cannot write anything once the
/// input ends, so the harness frees the memory rather than have the leak
/// sanitizer report the driver's deliberate leak as a bug.
fn released(teardown: Teardown<Handle, Region, Region, Region, Vec<Slot>>) -> FuzzReleased {
    match teardown {
        Teardown::Released(released) => released,
        Teardown::Wedged(released) => ManuallyDrop::into_inner(released),
    }
}

/// What has been accepted and answered.
#[derive(Default)]
struct Ledger {
    accepted: BTreeSet<u64>,
    completed: BTreeSet<u64>,
    next_id: u64,
}

impl Ledger {
    fn check(&self, driver: &FuzzDriver) {
        assert_eq!(
            usize::from(driver.frames_in_flight()),
            self.accepted.len() - self.completed.len(),
            "the driver's in-flight count is what was accepted and not answered"
        );
    }
}

/// Bring a driver up against the device the input describes.
fn build(input: &mut Input<'_>) -> (Rc<RefCell<Device>>, Result<FuzzDriver, ()>) {
    let flags = input.u8();
    let more = input.u8();
    let mut offered = FEATURE_ACCESS_PLATFORM;
    // `VERSION_1` and `MAC` are required, so they are offered unless the input
    // asks for the refusal path; the rest are the optional ones.
    for (bit, feature) in [(0, FEATURE_VERSION_1), (1, FEATURE_MAC)] {
        if (flags >> bit) & 1 == 0 {
            offered |= feature;
        }
    }
    for (bit, feature) in [
        (2, FEATURE_STATUS),
        (3, FEATURE_MTU),
        (4, FEATURE_SPEED_DUPLEX),
        (5, FEATURE_MRG_RXBUF),
    ] {
        if (more >> bit) & 1 == 0 {
            offered |= feature;
        }
    }
    let config_len = usize::from(input.u8()) % 25;
    let mut config = vec![0; config_len];
    for index in 0..24 {
        let byte = input.u8();
        if let Some(slot) = config.get_mut(index) {
            *slot = byte;
        }
    }
    let queue_max = input.u16();
    let max_queue_size = input.u16();

    let step = if more & 0x40 != 0 {
        3 * PAGE as u64
    } else {
        PAGE as u64
    };
    let receive_rings = Region::new(2, 0x1_0000_0000, step);
    let transmit_rings = Region::new(2, 0x2_0000_0000, step);
    let area = Region::new(2, 0x3_0000_0000, 5 * PAGE as u64);
    let receive_data = Region::new(8, 0x4_0000_0000, step);
    let transmit_data = Region::new(8, 0x5_0000_0000, step);
    let device = Rc::new(RefCell::new(Device {
        offered,
        device_select: 0,
        status: 0x0F,
        generation: Cell::new(0),
        churn: flags & 0x20 != 0,
        refuse_features: flags & 0x08 != 0,
        never_reset: flags & 0x10 != 0,
        drop_vector: flags & 0x40 != 0,
        needs_reset: false,
        config,
        queue_select: 0,
        queue_max,
        isr: 0,
        queues: [
            Queue {
                size: queue_max,
                vector: NO_VECTOR,
                rings: receive_rings.clone(),
                ring: None,
                taken: Vec::new(),
            },
            Queue {
                size: queue_max,
                vector: NO_VECTOR,
                rings: transmit_rings.clone(),
                ring: None,
                taken: Vec::new(),
            },
        ],
        receive_data: receive_data.clone(),
    }));
    let parts = Parts {
        transport: Handle {
            device: Rc::clone(&device),
            vectors: if more & 0x10 != 0 {
                [1, 2]
            } else {
                [NO_VECTOR, NO_VECTOR]
            },
        },
        receive_rings,
        transmit_rings,
        area,
        receive_data,
        transmit_data,
        slots: vec![Slot::EMPTY; 64],
    };
    let options = Options {
        reset_polls: 4,
        max_queue_size,
        config_attempts: 3,
    };
    match Driver::init(parts, options) {
        Ok(driver) => (device, Ok(driver)),
        Err(failure) => {
            // Released or wedged, the parts come back and nothing was accepted.
            let released = released(failure.teardown);
            assert_eq!(released.abandoned().count(), 0);
            (device, Err(()))
        }
    }
}

fn submit(input: &mut Input<'_>, driver: &mut FuzzDriver, ledger: &mut Ledger) {
    let frame = Frame {
        id: ledger.next_id,
        offset: u64::from(input.u16()),
        len: u32::from(input.u16()),
    };
    ledger.next_id += 1;
    let was_broken = driver.fault().is_some();
    match driver.submit(&frame) {
        Ok(()) => {
            assert!(!was_broken, "a failed driver accepted a frame");
            assert!(ledger.accepted.insert(frame.id));
        }
        Err(SubmitError::Broken) => assert!(was_broken),
        // Every other refusal, the device's protocol breaks included, leaves
        // the frame untracked: the error was the answer.
        Err(_) => assert!(!was_broken),
    }
}

/// Write a frame the input chose into the next receive buffer, and complete it
/// with a `written` the input chose.
fn deliver(input: &mut Input<'_>, device: &mut Device) {
    let index = usize::from(input.u8());
    let len = usize::from(input.u8()) * 8;
    let honest = input.u8() & 1 == 0;
    let chosen = input.u32();
    let header: Vec<u8> = (0..HEADER_LEN).map(|_| input.u8()).collect();
    let byte = input.u8();

    device.take_available(RECEIVE_QUEUE as usize);
    let Some(queue) = device.queues.get_mut(RECEIVE_QUEUE as usize) else {
        return;
    };
    if queue.taken.is_empty() {
        return;
    }
    let head = queue.taken.remove(index % queue.taken.len());
    let descriptors = device.chain(RECEIVE_QUEUE as usize, head);
    let mut written = chosen;
    let mut put = 0;
    for (at, descriptor) in descriptors.iter().enumerate() {
        let bytes: Vec<u8> = if at == 0 {
            header.clone()
        } else {
            vec![byte; (descriptor.len as usize).min(len.saturating_sub(put))]
        };
        if at > 0 {
            put += bytes.len();
        }
        device.receive_data.device_write(descriptor.address, &bytes);
    }
    if honest {
        written = u32::from(HEADER_LEN) + put as u32;
    }
    device.complete(RECEIVE_QUEUE as usize, head, written);
}

/// Complete a transmit chain with a `written` the input chose.
fn complete_sent(input: &mut Input<'_>, device: &mut Device) {
    let index = usize::from(input.u8());
    let honest = input.u8() & 1 == 0;
    let chosen = input.u32();
    device.take_available(TRANSMIT_QUEUE as usize);
    let Some(queue) = device.queues.get_mut(TRANSMIT_QUEUE as usize) else {
        return;
    };
    if queue.taken.is_empty() {
        return;
    }
    let head = queue.taken.remove(index % queue.taken.len());
    let written = if honest { 0 } else { chosen };
    device.complete(TRANSMIT_QUEUE as usize, head, written);
}

/// Put a used entry the driver was never given on one of the queues.
fn forge(input: &mut Input<'_>, device: &mut Device) {
    let queue = usize::from(input.u8() & 1);
    let id = u32::from(input.u16());
    let written = input.u32();
    let Some(ring) = device.queues.get(queue).and_then(|queue| queue.ring.as_ref()) else {
        return;
    };
    let layout = *ring.layout();
    let mut rings = ring.memory().clone();
    let index = rings.read_u16(layout.used_ring + 2);
    let slot = usize::from(index % layout.queue_size);
    rings.write_u32(layout.used_ring + 4 + slot * 8, id);
    rings.write_u32(layout.used_ring + 8 + slot * 8, written);
    rings.write_u16(layout.used_ring + 2, index.wrapping_add(1));
    device.isr |= ISR_QUEUE;
}

fn drain(input: &mut Input<'_>, driver: &mut FuzzDriver, ledger: &mut Ledger) -> Vec<u16> {
    let mut out = vec![Event::Sent { id: u64::MAX }; usize::from(input.u8() % 9)];
    let mut buffers = Vec::new();
    let Ok(drained) = driver.on_interrupt(&mut out) else {
        return buffers;
    };
    assert!(drained.events <= out.len());
    for event in &out[..drained.events] {
        match *event {
            Event::Sent { id } => {
                assert!(
                    ledger.accepted.contains(&id),
                    "a frame nobody asked to send"
                );
                assert!(ledger.completed.insert(id), "a frame answered twice");
            }
            Event::Received { buffer, offset, len } => {
                // The frame the driver reports must lie inside the receive
                // region it was given, however the device completed it.
                assert!(
                    offset + u64::from(len) <= 8 * PAGE as u64,
                    "a frame outside the receive region"
                );
                buffers.push(buffer);
            }
        }
    }
    buffers
}

fn act(
    input: &mut Input<'_>,
    device: &Rc<RefCell<Device>>,
    driver: &mut FuzzDriver,
    ledger: &mut Ledger,
    held: &mut Vec<u16>,
) {
    match input.u8() % 9 {
        0 => submit(input, driver, ledger),
        1 => device.borrow_mut().take_available(usize::from(input.u8() & 1)),
        2 => deliver(input, &mut device.borrow_mut()),
        3 => complete_sent(input, &mut device.borrow_mut()),
        4 => forge(input, &mut device.borrow_mut()),
        5 => {
            let queue = usize::from(input.u8() & 1);
            let offset = usize::from(input.u16());
            let value = input.u8();
            if let Some(queue) = device.borrow().queues.get(queue) {
                queue.rings.set(offset, value);
            }
        }
        6 => held.extend(drain(input, driver, ledger)),
        7 => {
            // Release a buffer the driver handed over, or one it did not.
            let buffer = if held.is_empty() || input.u8() & 3 == 0 {
                input.u16()
            } else {
                held.remove(usize::from(input.u8()) % held.len())
            };
            let _ = driver.release(buffer);
            let _ = driver.refill();
        }
        _ => {
            let offset = usize::from(input.u8());
            let value = input.u8();
            let mut device = device.borrow_mut();
            if let Some(slot) = device.config.get_mut(offset) {
                *slot = value;
            }
            device.needs_reset = input.u8() & 1 != 0;
            drop(device);
            let _ = driver.refresh_config();
        }
    }
}

fuzz_target!(|bytes: &[u8]| {
    let mut input = Input(bytes);
    let (device, driver) = build(&mut input);
    let Ok(mut driver) = driver else {
        return;
    };
    let mut ledger = Ledger::default();
    let mut held = Vec::new();
    for _ in 0..MAX_ACTIONS {
        if input.0.is_empty() {
            break;
        }
        act(&mut input, &device, &mut driver, &mut ledger, &mut held);
        ledger.check(&driver);
    }

    let released = released(driver.shutdown());
    let abandoned: Vec<u64> = released.abandoned().collect();
    let mut answered = ledger.completed.clone();
    for id in abandoned {
        assert!(
            ledger.accepted.contains(&id),
            "an abandoned frame nobody asked to send"
        );
        assert!(answered.insert(id), "a frame both answered and abandoned");
    }
    assert_eq!(answered, ledger.accepted, "every accepted frame is answered");
});
