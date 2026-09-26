//! Fuzz the virtio-blk driver against a device the input controls.
//!
//! From stage 10 the driver runs in a user process and believes nothing the
//! device says: its registers, its configuration, the used ring, the status
//! bytes, and any byte of the rings it cares to scribble on through DMA. This
//! target gives all of that to the fuzzer.
//!
//! # The input
//!
//! Two flag bytes choose what the device offers and how it misbehaves at
//! bring-up (refusing features, never resetting, a configuration that keeps
//! changing, dropping the queue's vector); then a configuration-block length
//! and its first 24 bytes, raw; then the device's and the driver's largest
//! queue size. Everything after is a script of actions, each a byte and its
//! arguments: submit a read, write or flush; take the available ring; complete
//! a taken chain with a chosen status byte and an honest or chosen `written`;
//! forge a used entry; scribble a byte anywhere in the rings; take completions
//! into a slice of chosen length; change the configuration and re-read it; set
//! or clear `DEVICE_NEEDS_RESET`. Bytes past the end read as zero.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **A completion names an accepted request, and each at most once.**
//! 2. **The driver's count of requests in flight is exactly** those accepted
//!    and not completed.
//! 3. **Once the driver reports a fault it accepts nothing more.**
//! 4. **Every accepted request is answered exactly once**: by a completion, or
//!    by the abandoned list after shutdown — whether the reset finished or not.

#![no_main]

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::mem::ManuallyDrop;
use std::rc::Rc;

use ferrix_virtio::blk::{
    DeviceConfig, FEATURE_BLK_SIZE, FEATURE_FLUSH, FEATURE_RO, FEATURE_SEG_MAX, FEATURE_SIZE_MAX,
};
use ferrix_virtio::pci::{
    CONFIG_GENERATION, CommonConfig, DEVICE_FEATURE, DEVICE_FEATURE_SELECT, DEVICE_STATUS,
    DRIVER_FEATURE, DRIVER_FEATURE_SELECT, FEATURE_ACCESS_PLATFORM, FEATURE_VERSION_1, NO_VECTOR,
    QUEUE_ENABLE, QUEUE_MSIX_VECTOR, QUEUE_NOTIFY_OFF, QUEUE_SELECT, QUEUE_SIZE,
    STATUS_DEVICE_NEEDS_RESET, STATUS_FEATURES_OK,
};
use ferrix_virtio::{Descriptor, Layout, QueueMemory, SplitQueueDevice};
use ferrix_virtio_blk::{
    Accepted, Completion, DevicePages, Driver, ISR_QUEUE, Op, Options, Parts, Request,
    RequestArea, Slot, Status, SubmitError, Teardown, Transport,
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

/// The device: registers, configuration, and the device half of the queue.
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
    size: u16,
    vector: u16,
    isr: u8,
    rings: Region,
    area: Region,
    ring: Option<SplitQueueDevice<Region>>,
    taken: Vec<u16>,
}

impl Device {
    fn register(&self, offset: u32) -> u32 {
        match offset {
            DEVICE_FEATURE => match self.device_select {
                0 => self.offered as u32,
                1 => (self.offered >> 32) as u32,
                _ => 0,
            },
            DEVICE_STATUS => {
                u32::from(self.status | if self.needs_reset { STATUS_DEVICE_NEEDS_RESET } else { 0 })
            }
            CONFIG_GENERATION => {
                if self.churn {
                    self.generation.set(self.generation.get().wrapping_add(1));
                }
                u32::from(self.generation.get())
            }
            QUEUE_SIZE if self.queue_select == 0 => u32::from(self.size),
            QUEUE_MSIX_VECTOR => u32::from(self.vector),
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
                        self.size = self.queue_max;
                        self.ring = None;
                        self.taken.clear();
                    }
                    return;
                }
                if self.refuse_features {
                    value &= !STATUS_FEATURES_OK;
                }
                self.status = value;
            }
            QUEUE_SELECT => self.queue_select = value as u16,
            QUEUE_SIZE => self.size = value as u16,
            QUEUE_MSIX_VECTOR => {
                self.vector = if self.drop_vector { NO_VECTOR } else { value as u16 };
            }
            QUEUE_ENABLE => {
                if let Ok(layout) = Layout::for_size(self.size)
                    && layout.total_size <= self.rings.bytes.borrow().len()
                {
                    self.ring = Some(SplitQueueDevice::new(layout, self.rings.clone()));
                }
            }
            _ => {}
        }
    }
}

/// The driver's handle on the device.
#[derive(Clone)]
struct Handle {
    device: Rc<RefCell<Device>>,
    vector: u16,
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
        self.device.borrow_mut().set_register(offset, u32::from(value));
    }
    fn write16(&mut self, offset: u32, value: u16) {
        self.device.borrow_mut().set_register(offset, u32::from(value));
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
    fn queue_vector(&self) -> u16 {
        self.vector
    }
    fn acknowledge_interrupt(&mut self) -> u8 {
        std::mem::take(&mut self.device.borrow_mut().isr)
    }
}

type FuzzDriver = Driver<Handle, Region, Region, Region, Vec<Slot>>;

type FuzzReleased = ferrix_virtio_blk::Released<Handle, Region, Region, Region, Vec<Slot>>;

/// The parts a teardown hands back, whether the device reset or not.
///
/// A wedged teardown keeps its memory in `ManuallyDrop` because a real device
/// that did not reset may still write to it, and a real process must leak it.
/// This device is a struct in this process that cannot write anything once
/// the input ends, so the harness frees the memory rather than have the leak
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
            usize::from(driver.requests_in_flight()),
            self.accepted.len() - self.completed.len(),
            "the driver's in-flight count is what was accepted and not completed"
        );
    }
}

const BLANK: Descriptor = Descriptor {
    address: 0,
    len: 0,
    flags: 0,
    next: 0,
};

/// Bring a driver up against the device the input describes.
fn build(input: &mut Input<'_>) -> (Rc<RefCell<Device>>, Result<FuzzDriver, ()>) {
    let flags = input.u8();
    let more = input.u8();
    let mut offered = FEATURE_ACCESS_PLATFORM;
    for (bit, feature) in [(0, FEATURE_VERSION_1), (1, FEATURE_RO), (2, FEATURE_SIZE_MAX)] {
        if (flags >> bit) & 1 == u8::from(bit != 0) {
            offered |= feature;
        }
    }
    for (bit, feature) in [(0, FEATURE_FLUSH), (1, FEATURE_BLK_SIZE), (2, FEATURE_SEG_MAX)] {
        if (more >> bit) & 1 == 0 {
            offered |= feature;
        }
    }
    let config_len = usize::from(input.u8()) % 97;
    let mut config = vec![0; config_len];
    for index in 0..24 {
        let byte = input.u8();
        if let Some(slot) = config.get_mut(index) {
            *slot = byte;
        }
    }
    let queue_max = input.u16();
    let max_queue_size = input.u16();

    let rings = Region::new(4, 0x1_0000_0000, if more & 8 != 0 { 3 * PAGE as u64 } else { PAGE as u64 });
    let area = Region::new(2, 0x2_0000_0000, 5 * PAGE as u64);
    let data = Region::new(8, 0x3_0000_0000, if flags & 0x80 != 0 { 7 * PAGE as u64 } else { PAGE as u64 });
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
        size: queue_max,
        vector: NO_VECTOR,
        isr: 0,
        rings: rings.clone(),
        area: area.clone(),
        ring: None,
        taken: Vec::new(),
    }));
    let parts = Parts {
        transport: Handle {
            device: Rc::clone(&device),
            vector: if more & 0x10 != 0 { 1 } else { NO_VECTOR },
        },
        rings,
        area,
        data,
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
    let op = match input.u8() % 3 {
        0 => Op::Read,
        1 => Op::Write,
        _ => Op::Flush,
    };
    let request = Request {
        id: ledger.next_id,
        op,
        sector: u64::from(input.u16()),
        count: u32::from(input.u8()),
        data_offset: u64::from(input.u16()),
    };
    ledger.next_id += 1;
    let was_broken = driver.fault().is_some();
    match driver.submit(&request) {
        Ok(Accepted::Queued { .. }) | Err(SubmitError::Device(_)) => {
            assert!(!was_broken, "a failed driver accepted a request");
            assert!(ledger.accepted.insert(request.id));
        }
        Ok(Accepted::Completed(completion)) => {
            assert!(!was_broken);
            assert_eq!(completion.id, request.id);
        }
        Err(SubmitError::Broken) => assert!(was_broken),
        Err(_) => assert!(!was_broken),
    }
}

fn complete(input: &mut Input<'_>, device: &mut Device) {
    let index = usize::from(input.u8());
    let status = input.u8();
    let honest = input.u8() == 0;
    let chosen = if honest { 0 } else { input.u32() };
    if device.taken.is_empty() {
        return;
    }
    let head = device.taken.remove(index % device.taken.len());
    let Some(ring) = device.ring.as_mut() else {
        return;
    };
    let mut chain = vec![BLANK; usize::from(ring.layout().queue_size)];
    let mut written = chosen;
    if let Ok(count) = ring.read_chain(head, &mut chain)
        && let Some(last) = chain.get(count.saturating_sub(1))
    {
        if honest {
            written = chain
                .iter()
                .take(count)
                .filter(|d| d.is_device_writable())
                .map(|d| d.len)
                .fold(0_u32, u32::saturating_add);
        }
        let byte = if status < 0xF0 { status % 3 } else { status };
        if let Some(at) = device.area.locate(last.address) {
            device.area.set(at, byte);
        }
    }
    let _ = ring.complete(head, written);
    device.isr |= ISR_QUEUE;
}

fn act(input: &mut Input<'_>, device: &Rc<RefCell<Device>>, driver: &mut FuzzDriver, ledger: &mut Ledger) {
    match input.u8() % 8 {
        0 => submit(input, driver, ledger),
        1 => {
            let mut device = device.borrow_mut();
            let device = &mut *device;
            if let Some(ring) = device.ring.as_mut() {
                for _ in 0..64 {
                    match ring.next_chain() {
                        Ok(Some(head)) => device.taken.push(head),
                        _ => break,
                    }
                }
            }
        }
        2 => complete(input, &mut device.borrow_mut()),
        3 => {
            let id = u32::from(input.u16());
            let written = input.u32();
            let mut device = device.borrow_mut();
            if let Some(ring) = device.ring.as_ref() {
                let layout = *ring.layout();
                let mut rings = device.rings.clone();
                let index = rings.read_u16(layout.used_ring + 2);
                let slot = usize::from(index % layout.queue_size);
                rings.write_u32(layout.used_ring + 4 + slot * 8, id);
                rings.write_u32(layout.used_ring + 8 + slot * 8, written);
                rings.write_u16(layout.used_ring + 2, index.wrapping_add(1));
                device.isr |= ISR_QUEUE;
            }
        }
        4 => {
            let offset = usize::from(input.u16());
            let value = input.u8();
            device.borrow().rings.set(offset, value);
        }
        5 => {
            let mut out = vec![
                Completion {
                    id: u64::MAX,
                    status: Status::Ok,
                    bytes: 0,
                };
                usize::from(input.u8() % 9)
            ];
            if let Ok(drained) = driver.on_interrupt(&mut out) {
                assert!(drained.completions <= out.len());
                for completion in &out[..drained.completions] {
                    assert!(ledger.accepted.contains(&completion.id), "a completion nobody asked for");
                    assert!(ledger.completed.insert(completion.id), "a request completed twice");
                }
            }
        }
        6 => {
            let offset = usize::from(input.u8());
            let value = input.u8();
            if let Some(slot) = device.borrow_mut().config.get_mut(offset) {
                *slot = value;
            }
            let _ = driver.refresh_config();
        }
        _ => {
            let mut device = device.borrow_mut();
            device.needs_reset = !device.needs_reset;
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
    for _ in 0..MAX_ACTIONS {
        if input.0.is_empty() {
            break;
        }
        act(&mut input, &device, &mut driver, &mut ledger);
        ledger.check(&driver);
    }

    let released = released(driver.shutdown());
    let abandoned: Vec<u64> = released.abandoned().collect();
    let mut answered = ledger.completed.clone();
    for id in abandoned {
        assert!(ledger.accepted.contains(&id), "an abandoned request nobody asked for");
        assert!(answered.insert(id), "a request both completed and abandoned");
    }
    assert_eq!(answered, ledger.accepted, "every accepted request is answered");
});
