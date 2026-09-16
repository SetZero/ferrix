//! A virtio-net device on the host, and the memory between it and the driver.
//!
//! The device checks the driver as much as the driver checks it: features
//! accepted that were not offered, a queue set up before `FEATURES_OK`,
//! `DRIVER_OK` before a queue was enabled, a chain published before
//! `DRIVER_OK`, a receive chain the device may not write, a transmit chain it
//! may not read — each is recorded in [`Device::protocol_errors`], which the
//! tests require to stay empty. And every device access to an address no
//! pinned page is mapped at counts as a fault on the [`Bus`].

use core::cell::{Cell, RefCell};
use core::mem::ManuallyDrop;
use core::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use ferrix_virtio::net::{Config, HEADER_LEN, MAC_LEN};
use ferrix_virtio::pci::{
    CONFIG_GENERATION, CommonConfig, DEVICE_FEATURE, DEVICE_FEATURE_SELECT, DEVICE_STATUS,
    DRIVER_FEATURE, DRIVER_FEATURE_SELECT, FEATURE_VERSION_1, NO_VECTOR, NUM_QUEUES, QUEUE_DESC,
    QUEUE_DEVICE, QUEUE_DRIVER, QUEUE_ENABLE, QUEUE_MSIX_VECTOR, QUEUE_NOTIFY_OFF, QUEUE_SELECT,
    QUEUE_SIZE, STATUS_DEVICE_NEEDS_RESET, STATUS_DRIVER_OK, STATUS_FAILED, STATUS_FEATURES_OK,
};
use ferrix_virtio::{Descriptor, DeviceConfig, Layout, PAGE_SIZE, QueueMemory, SplitQueueDevice};

use crate::{
    DevicePages, Driver, Event, ISR_QUEUE, InitFailure, Options, Parts, RECEIVE_QUEUE, RequestArea,
    Slot, TRANSMIT_QUEUE, Teardown, Transport,
};

/// A page, as a `usize`.
pub(super) const PAGE: usize = PAGE_SIZE as usize;

/// An empty descriptor.
pub(super) const BLANK: Descriptor = Descriptor {
    address: 0,
    len: 0,
    flags: 0,
    next: 0,
};

/// A pinned region: bytes, and the device address of each of its pages.
#[derive(Clone, Debug)]
pub(super) struct Region {
    /// The bytes themselves, shared with every clone.
    bytes: Rc<RefCell<Vec<u8>>>,
    /// Device address of each page.
    pages: Vec<u64>,
}

impl Region {
    /// Where byte `offset` is in `bytes`; the region is one flat `Vec`, so
    /// this is the identity, but going through it keeps the page walk honest.
    fn at(&self, offset: usize) -> Option<usize> {
        (offset / PAGE < self.pages.len()).then_some(offset)
    }

    /// Where device address `address` is in this region.
    fn locate(&self, address: u64) -> Option<usize> {
        self.pages.iter().enumerate().find_map(|(index, page)| {
            let within = address.checked_sub(*page)?;
            (within < PAGE_SIZE).then(|| index * PAGE + within as usize)
        })
    }

    /// Read `len` bytes from `offset`, as the process would.
    pub(super) fn read(&self, offset: usize, len: usize) -> Vec<u8> {
        (0..len).map(|index| self.get(offset + index)).collect()
    }

    /// Write `bytes` at `offset`, as the process would.
    pub(super) fn write(&self, offset: usize, bytes: &[u8]) {
        for (index, byte) in bytes.iter().enumerate() {
            self.set(offset + index, *byte);
        }
    }

    /// One byte.
    fn get(&self, offset: usize) -> u8 {
        self.at(offset)
            .and_then(|at| self.bytes.borrow().get(at).copied())
            .unwrap_or(0)
    }

    /// Set one byte.
    fn set(&self, offset: usize, value: u8) {
        if let Some(at) = self.at(offset)
            && let Some(slot) = self.bytes.borrow_mut().get_mut(at)
        {
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

#[expect(
    unsafe_code,
    reason = "AUDIT: QueueMemory is an unsafe trait; this implementation only indexes a Vec through checked lookups"
)]
// SAFETY: every offset is looked up in pages the region owns and a miss reads
// zero, so no access leaves the region's `Vec`; alignment is never relied on,
// since nothing dereferences the table as a struct; and the driver and device
// are stepped one after the other, so the barrier has nothing to order.
unsafe impl QueueMemory for Region {
    fn read_u8(&self, offset: usize) -> u8 {
        self.get(offset)
    }
    fn write_u8(&mut self, offset: usize, value: u8) {
        self.set(offset, value);
    }
    fn barrier(&self) {}
}

/// Every pinned region, and the device-address mapping in front of them.
#[derive(Debug)]
pub(super) struct Bus {
    /// The regions, in the order they were pinned.
    regions: RefCell<Vec<Region>>,
    /// Where the next consecutive pin starts.
    next_consecutive: Cell<u64>,
    /// Where the next scattered page goes; it moves down, three pages a time.
    next_scattered: Cell<u64>,
    /// Device accesses to unmapped addresses.
    pub(super) faults: Cell<usize>,
}

impl Bus {
    /// An empty bus.
    pub(super) fn new() -> Rc<Self> {
        Rc::new(Bus {
            regions: RefCell::new(Vec::new()),
            next_consecutive: Cell::new(0x10_0000_0000),
            next_scattered: Cell::new(0x7FFF_0000_0000),
            faults: Cell::new(0),
        })
    }

    /// Pin `count` zeroed pages, at consecutive device addresses or at
    /// addresses no two of which follow on.
    pub(super) fn pin(self: &Rc<Self>, count: usize, scattered: bool) -> Region {
        let mut pages = Vec::new();
        for _ in 0..count {
            let address = if scattered {
                let address = self.next_scattered.get();
                self.next_scattered.set(address - 3 * PAGE_SIZE);
                address
            } else {
                let address = self.next_consecutive.get();
                self.next_consecutive.set(address + PAGE_SIZE);
                address
            };
            pages.push(address);
        }
        // A gap, so two pins never follow on from each other either.
        self.next_consecutive
            .set(self.next_consecutive.get() + 16 * PAGE_SIZE);
        let region = Region {
            bytes: Rc::new(RefCell::new(vec![0; count * PAGE])),
            pages,
        };
        self.regions.borrow_mut().push(region.clone());
        region
    }

    /// The region `address` is in, and where in it.
    fn find(&self, address: u64) -> Option<(Region, usize)> {
        self.regions
            .borrow()
            .iter()
            .find_map(|region| region.locate(address).map(|at| (region.clone(), at)))
    }

    /// The device reads a byte.
    pub(super) fn read(&self, address: u64) -> u8 {
        match self.find(address) {
            Some((region, at)) => region.get(at),
            None => {
                self.faults.set(self.faults.get() + 1);
                0
            }
        }
    }

    /// The device writes a byte.
    pub(super) fn write(&self, address: u64, value: u8) {
        match self.find(address) {
            Some((region, at)) => region.set(at, value),
            None => self.faults.set(self.faults.get() + 1),
        }
    }

    /// The device reads `out.len()` bytes from `address`.
    pub(super) fn copy_out(&self, address: u64, out: &mut [u8]) {
        for (index, byte) in out.iter_mut().enumerate() {
            *byte = self.read(address + index as u64);
        }
    }

    /// The device writes `bytes` at `address`.
    pub(super) fn copy_in(&self, address: u64, bytes: &[u8]) {
        for (index, byte) in bytes.iter().enumerate() {
            self.write(address + index as u64, *byte);
        }
    }
}

/// The rings as the device reaches them: three areas, each at the device
/// address the driver wrote into the queue's registers.
#[derive(Clone, Debug)]
pub(super) struct RingView {
    /// The bus.
    bus: Rc<Bus>,
    /// The queue's layout.
    layout: Layout,
    /// `queue_desc`.
    descriptors: u64,
    /// `queue_driver`.
    driver: u64,
    /// `queue_device`.
    device: u64,
}

impl RingView {
    /// The device address of ring offset `offset`.
    fn address(&self, offset: usize) -> u64 {
        let layout = &self.layout;
        if offset < layout.available_ring {
            self.descriptors + offset as u64
        } else if offset < layout.used_ring {
            self.driver + (offset - layout.available_ring) as u64
        } else {
            self.device + (offset - layout.used_ring) as u64
        }
    }
}

#[expect(
    unsafe_code,
    reason = "AUDIT: QueueMemory is an unsafe trait; this implementation only indexes a Vec through checked lookups"
)]
// SAFETY: as `Region`'s: every access goes through the bus's checked lookup.
unsafe impl QueueMemory for RingView {
    fn read_u8(&self, offset: usize) -> u8 {
        self.bus.read(self.address(offset))
    }
    fn write_u8(&mut self, offset: usize, value: u8) {
        self.bus.write(self.address(offset), value);
    }
    fn barrier(&self) {}
}

/// Ways the device can be told to misbehave.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Misbehave {
    /// Drop `FEATURES_OK` when the driver sets it.
    pub(super) refuse_features: bool,
    /// Never let `device_status` read zero after a reset.
    pub(super) never_reset: bool,
    /// Keep `NO_VECTOR` whatever the driver asks for.
    pub(super) drop_vector: bool,
    /// Move `config_generation` on every read.
    pub(super) churn_config: bool,
    /// Report `DEVICE_NEEDS_RESET`.
    pub(super) needs_reset: bool,
    /// Complete a received frame claiming fewer bytes than a header.
    pub(super) short_header: bool,
    /// Add this to every `written` a receive completion reports.
    pub(super) extra_written: u32,
    /// Put these flags in every header written to the driver.
    pub(super) header_flags: u8,
    /// Put this `gso_type` in every header written to the driver.
    pub(super) header_gso: u8,
    /// Claim the frame spans this many buffers.
    pub(super) num_buffers: Option<u16>,
}

/// One queue's registers.
#[derive(Clone, Copy, Debug, Default)]
struct QueueRegisters {
    size: u16,
    descriptors: u64,
    driver: u64,
    device: u64,
    vector: u16,
    enabled: bool,
}

/// A virtio-net device.
pub(super) struct Device {
    bus: Rc<Bus>,
    /// Features offered.
    pub(super) offered: u64,
    /// Features the driver wrote.
    pub(super) accepted: u64,
    device_select: u32,
    driver_select: u32,
    status: u8,
    generation: Cell<u8>,
    /// The device-specific configuration block.
    pub(super) config: Vec<u8>,
    queue_select: u16,
    /// The largest either queue may be.
    pub(super) queue_max: u16,
    queues: [QueueRegisters; 2],
    rings: [Option<SplitQueueDevice<RingView>>; 2],
    /// Chains taken off each available ring and not yet served.
    taken: [Vec<u16>; 2],
    /// `queue_notify_off` of each queue.
    pub(super) notify_off: [u16; 2],
    isr: u8,
    /// Doorbells rung, by queue.
    pub(super) notifications: Vec<u16>,
    /// The frames the driver has sent, headers and all.
    pub(super) sent: Vec<(Vec<u8>, Vec<u8>)>,
    /// What to get wrong.
    pub(super) misbehave: Misbehave,
    /// Every way the driver broke the protocol.
    pub(super) protocol_errors: Vec<&'static str>,
    /// Every value written to `device_status`.
    pub(super) status_writes: Vec<u8>,
}

impl core::fmt::Debug for Device {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Device")
            .field("status", &self.status)
            .field("protocol_errors", &self.protocol_errors)
            .finish_non_exhaustive()
    }
}

impl Device {
    /// A device on `bus` with `config`, offering `offered`.
    pub(super) fn new(bus: Rc<Bus>, config: &Config, offered: u64, config_len: usize) -> Self {
        let mut bytes = vec![0; config_len];
        config.encode(&mut bytes);
        Device {
            bus,
            offered,
            accepted: 0,
            device_select: 0,
            driver_select: 0,
            status: 0,
            generation: Cell::new(0),
            config: bytes,
            queue_select: 0,
            queue_max: 16,
            queues: [QueueRegisters::default(); 2],
            rings: [None, None],
            taken: [Vec::new(), Vec::new()],
            notify_off: [3, 5],
            isr: 0,
            notifications: Vec::new(),
            sent: Vec::new(),
            misbehave: Misbehave::default(),
            protocol_errors: Vec::new(),
            status_writes: Vec::new(),
        }
    }

    /// `device_status`, as the driver reads it.
    pub(super) fn status(&self) -> u8 {
        let needs_reset = if self.misbehave.needs_reset {
            STATUS_DEVICE_NEEDS_RESET
        } else {
            0
        };
        self.status | needs_reset
    }

    /// One queue's registers: size, addresses, vector, enabled.
    pub(super) fn queue(&self, queue: u16) -> (u16, u64, u64, u64, u16, bool) {
        let registers = self.queues[usize::from(queue)];
        (
            registers.size,
            registers.descriptors,
            registers.driver,
            registers.device,
            registers.vector,
            registers.enabled,
        )
    }

    /// Note a protocol error.
    fn error(&mut self, what: &'static str) {
        self.protocol_errors.push(what);
    }

    /// Forget everything a driver set up.
    fn reset(&mut self) {
        self.status = 0;
        self.accepted = 0;
        self.queues = [QueueRegisters {
            size: self.queue_max,
            vector: NO_VECTOR,
            ..QueueRegisters::default()
        }; 2];
        self.rings = [None, None];
        self.taken = [Vec::new(), Vec::new()];
        self.isr = 0;
    }

    /// Read a register.
    fn register(&self, offset: u32) -> u32 {
        let selected = usize::from(self.queue_select);
        let queue = self.queues.get(selected);
        match offset {
            DEVICE_FEATURE => match self.device_select {
                0 => self.offered as u32,
                1 => (self.offered >> 32) as u32,
                _ => 0,
            },
            NUM_QUEUES => 2,
            DEVICE_STATUS => u32::from(self.status()),
            CONFIG_GENERATION => {
                if self.misbehave.churn_config {
                    self.generation.set(self.generation.get().wrapping_add(1));
                }
                u32::from(self.generation.get())
            }
            QUEUE_SELECT => u32::from(self.queue_select),
            QUEUE_SIZE => u32::from(queue.map_or(0, |queue| queue.size)),
            QUEUE_MSIX_VECTOR => u32::from(queue.map_or(NO_VECTOR, |queue| queue.vector)),
            QUEUE_ENABLE => u32::from(queue.is_some_and(|queue| queue.enabled)),
            QUEUE_NOTIFY_OFF => {
                u32::from(self.notify_off.get(selected).copied().unwrap_or_default())
            }
            _ => 0,
        }
    }

    /// Write a register.
    fn set_register(&mut self, offset: u32, value: u32) {
        let features_ok = self.status & STATUS_FEATURES_OK != 0;
        match offset {
            DEVICE_FEATURE_SELECT => self.device_select = value,
            DRIVER_FEATURE_SELECT => self.driver_select = value,
            DRIVER_FEATURE => {
                if features_ok {
                    self.error("features written after FEATURES_OK");
                }
                match self.driver_select {
                    0 => self.accepted = (self.accepted & !0xFFFF_FFFF) | u64::from(value),
                    1 => self.accepted = (self.accepted & 0xFFFF_FFFF) | u64::from(value) << 32,
                    _ => {}
                }
            }
            DEVICE_STATUS => self.write_status(value as u8),
            QUEUE_SELECT => self.queue_select = value as u16,
            QUEUE_SIZE => {
                self.require_features_ok(features_ok);
                self.with_queue(|queue| queue.size = value as u16);
            }
            QUEUE_MSIX_VECTOR => {
                let dropped = self.misbehave.drop_vector;
                self.with_queue(|queue| {
                    queue.vector = if dropped { NO_VECTOR } else { value as u16 };
                });
            }
            QUEUE_ENABLE => self.enable(value),
            _ => self.set_address(offset, value, features_ok),
        }
    }

    /// Change the selected queue's registers, if there is such a queue.
    fn with_queue(&mut self, change: impl FnOnce(&mut QueueRegisters)) {
        let selected = usize::from(self.queue_select);
        match self.queues.get_mut(selected) {
            Some(queue) => change(queue),
            None => self.error("a register written for a queue that does not exist"),
        }
    }

    /// Write half of one of the selected queue's addresses.
    fn set_address(&mut self, offset: u32, value: u32, features_ok: bool) {
        let low = |slot: &mut u64| *slot = (*slot & !0xFFFF_FFFF) | u64::from(value);
        let high = |slot: &mut u64| *slot = (*slot & 0xFFFF_FFFF) | u64::from(value) << 32;
        self.with_queue(|queue| match offset {
            o if o == QUEUE_DESC => low(&mut queue.descriptors),
            o if o == QUEUE_DESC + 4 => high(&mut queue.descriptors),
            o if o == QUEUE_DRIVER => low(&mut queue.driver),
            o if o == QUEUE_DRIVER + 4 => high(&mut queue.driver),
            o if o == QUEUE_DEVICE => low(&mut queue.device),
            o if o == QUEUE_DEVICE + 4 => high(&mut queue.device),
            _ => {}
        });
        if matches!(
            offset,
            o if o == QUEUE_DESC
                || o == QUEUE_DESC + 4
                || o == QUEUE_DRIVER
                || o == QUEUE_DRIVER + 4
                || o == QUEUE_DEVICE
                || o == QUEUE_DEVICE + 4
        ) {
            self.require_features_ok(features_ok);
        }
    }

    /// Complain about queue setup before `FEATURES_OK`.
    fn require_features_ok(&mut self, features_ok: bool) {
        if !features_ok {
            self.error("a queue set up before FEATURES_OK");
        }
    }

    /// `device_status` written.
    fn write_status(&mut self, value: u8) {
        self.status_writes.push(value);
        if value == 0 {
            if !self.misbehave.never_reset {
                self.reset();
            }
            return;
        }
        if self.status & !value != 0 {
            self.error("a status bit cleared without a reset");
        }
        let mut value = value;
        let newly_features_ok =
            value & STATUS_FEATURES_OK != 0 && self.status & STATUS_FEATURES_OK == 0;
        if newly_features_ok {
            if self.accepted & !self.offered != 0 {
                self.error("the driver accepted features the device did not offer");
            }
            if self.accepted & FEATURE_VERSION_1 == 0 {
                self.error("FEATURES_OK without VERSION_1");
            }
            if self.misbehave.refuse_features {
                value &= !STATUS_FEATURES_OK;
            }
        }
        let driver_ok = value & STATUS_DRIVER_OK != 0 && self.status & STATUS_DRIVER_OK == 0;
        if driver_ok && value & STATUS_FAILED == 0 {
            if value & STATUS_FEATURES_OK == 0 {
                self.error("DRIVER_OK without FEATURES_OK");
            }
            if !self.queues.iter().all(|queue| queue.enabled) {
                self.error("DRIVER_OK before both queues were enabled");
            }
        }
        self.status = value;
    }

    /// `queue_enable` written on the selected queue.
    fn enable(&mut self, value: u32) {
        if value != 1 {
            self.error("queue_enable written with something other than 1");
            return;
        }
        self.require_features_ok(self.status & STATUS_FEATURES_OK != 0);
        let selected = usize::from(self.queue_select);
        let Some(registers) = self.queues.get(selected).copied() else {
            self.error("a queue enabled that does not exist");
            return;
        };
        let Ok(layout) = Layout::for_size(registers.size) else {
            self.error("a queue enabled with a bad size");
            return;
        };
        if registers.size > self.queue_max {
            self.error("a queue enabled larger than its maximum");
        }
        let view = RingView {
            bus: Rc::clone(&self.bus),
            layout,
            descriptors: registers.descriptors,
            driver: registers.driver,
            device: registers.device,
        };
        if let Some(slot) = self.rings.get_mut(selected) {
            *slot = Some(SplitQueueDevice::new(layout, view));
        }
        if let Some(queue) = self.queues.get_mut(selected) {
            queue.enabled = true;
        }
    }

    /// The chains the driver has published on `queue` and the device has not
    /// taken.
    fn next_chain(&mut self, queue: u16) -> Option<u16> {
        let driver_ok = self.status & STATUS_DRIVER_OK != 0;
        let ring = self.rings.get_mut(usize::from(queue))?.as_mut()?;
        if ring.has_available() && !driver_ok {
            self.protocol_errors
                .push("a chain published before DRIVER_OK");
            return None;
        }
        match ring.next_chain() {
            Ok(head) => head,
            Err(_) => {
                self.protocol_errors
                    .push("an available ring that is corrupt");
                None
            }
        }
    }

    /// Read the chain at `head` of `queue`.
    fn chain(&self, queue: u16, head: u16) -> Vec<Descriptor> {
        let Some(Some(ring)) = self.rings.get(usize::from(queue)) else {
            return Vec::new();
        };
        let mut descriptors = vec![BLANK; usize::from(ring.layout().queue_size)];
        match ring.read_chain(head, &mut descriptors) {
            Ok(count) => descriptors[..count].to_vec(),
            Err(_) => Vec::new(),
        }
    }

    /// Complete the chain at `head` of `queue`.
    fn complete(&mut self, queue: u16, head: u16, written: u32) {
        if let Some(Some(ring)) = self.rings.get_mut(usize::from(queue))
            && ring.complete(head, written).is_err()
        {
            self.protocol_errors
                .push("a head the device cannot complete");
        }
        self.isr |= ISR_QUEUE;
    }

    /// Take every chain the driver has published on `queue`, without serving
    /// any, and say how many are now waiting.
    pub(super) fn take_available(&mut self, queue: u16) -> usize {
        while let Some(head) = self.next_chain(queue) {
            if let Some(parked) = self.taken.get_mut(usize::from(queue)) {
                parked.push(head);
            }
        }
        self.taken.get(usize::from(queue)).map_or(0, Vec::len)
    }

    /// The next chain waiting on `queue`.
    fn next_taken(&mut self, queue: u16) -> Option<u16> {
        let _ = self.take_available(queue);
        let parked = self.taken.get_mut(usize::from(queue))?;
        if parked.is_empty() {
            return None;
        }
        Some(parked.remove(0))
    }

    /// Put `frame` in the next receive buffer the driver posted, and complete
    /// it. Returns whether there was one.
    pub(super) fn deliver(&mut self, frame: &[u8]) -> bool {
        let Some(head) = self.next_taken(RECEIVE_QUEUE) else {
            return false;
        };
        let descriptors = self.chain(RECEIVE_QUEUE, head);
        let Some(first) = descriptors.first().copied() else {
            self.error("a receive chain the device cannot walk");
            return false;
        };
        if !descriptors.iter().all(Descriptor::is_device_writable) {
            self.error("a receive chain the device may not write");
            return false;
        }
        if u64::from(first.len) < u64::from(HEADER_LEN) {
            self.error("a receive chain whose header descriptor is too short");
            return false;
        }

        let mut header = [0_u8; HEADER_LEN as usize];
        header[0] = self.misbehave.header_flags;
        header[1] = self.misbehave.header_gso;
        let buffers = self.misbehave.num_buffers.unwrap_or(1);
        header[10..12].copy_from_slice(&buffers.to_le_bytes());
        self.bus.copy_in(first.address, &header);

        let mut at = 0;
        for descriptor in descriptors.iter().skip(1) {
            if at >= frame.len() {
                break;
            }
            let take = (descriptor.len as usize).min(frame.len() - at);
            self.bus.copy_in(descriptor.address, &frame[at..at + take]);
            at += take;
        }
        if at < frame.len() {
            self.error("a receive buffer too small for the frame");
        }

        let written = if self.misbehave.short_header {
            HEADER_LEN - 1
        } else {
            HEADER_LEN + at as u32 + self.misbehave.extra_written
        };
        self.complete(RECEIVE_QUEUE, head, written);
        true
    }

    /// Serve every frame the driver has published on the transmit queue.
    /// Returns how many.
    pub(super) fn transmit_all(&mut self) -> usize {
        let mut served = 0;
        while let Some(head) = self.next_taken(TRANSMIT_QUEUE) {
            let descriptors = self.chain(TRANSMIT_QUEUE, head);
            if descriptors.iter().any(Descriptor::is_device_writable) {
                self.error("a transmit chain the device may write");
            }
            let mut header = vec![0_u8; 0];
            let mut frame = Vec::new();
            for (index, descriptor) in descriptors.iter().enumerate() {
                let mut bytes = vec![0_u8; descriptor.len as usize];
                self.bus.copy_out(descriptor.address, &mut bytes);
                if index == 0 {
                    header = bytes;
                } else {
                    frame.extend_from_slice(&bytes);
                }
            }
            self.sent.push((header, frame));
            // A read-only chain: virtio 1.2 §5.1.6.2 has the device report
            // nothing written.
            self.complete(TRANSMIT_QUEUE, head, 0);
            served += 1;
        }
        served
    }

    /// Put a used entry naming `id` on `queue` without serving anything.
    pub(super) fn forge_used(&mut self, queue: u16, id: u32, written: u32) {
        let Some(Some(ring)) = self.rings.get(usize::from(queue)) else {
            return;
        };
        let layout = *ring.layout();
        let mut view = ring.memory().clone();
        let index = view.read_u16(layout.used_ring + 2);
        let slot = usize::from(index % layout.queue_size);
        view.write_u32(layout.used_ring + 4 + slot * 8, id);
        view.write_u32(layout.used_ring + 8 + slot * 8, written);
        view.write_u16(layout.used_ring + 2, index.wrapping_add(1));
        self.isr |= ISR_QUEUE;
    }
}

/// The driver's handle on a [`Device`].
#[derive(Clone, Debug)]
pub(super) struct Handle {
    /// The device.
    pub(super) device: Rc<RefCell<Device>>,
    /// The vector to ask for, by queue.
    pub(super) vectors: [u16; 2],
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
    fn notify(&mut self, queue: u16, notify_off: u16) {
        let mut device = self.device.borrow_mut();
        let expected = device.notify_off.get(usize::from(queue)).copied();
        if expected != Some(notify_off) {
            device.error("a doorbell with the wrong notification offset");
        }
        device.notifications.push(queue);
    }

    fn queue_vector(&self, queue: u16) -> u16 {
        self.vectors
            .get(usize::from(queue))
            .copied()
            .unwrap_or(NO_VECTOR)
    }

    fn acknowledge_interrupt(&mut self) -> u8 {
        let mut device = self.device.borrow_mut();
        core::mem::take(&mut device.isr)
    }
}

/// The driver as the tests build it.
pub(super) type TestDriver = Driver<Handle, Region, Region, Region, Vec<Slot>>;

/// A failed initialisation, as the tests see it.
pub(super) type Failure = InitFailure<Handle, Region, Region, Region, Vec<Slot>>;

/// A teardown, as the tests see it.
pub(super) type TestTeardown = Teardown<Handle, Region, Region, Region, Vec<Slot>>;

/// What [`Setup::try_build`] hands back: the bus, the device, the two data
/// regions as the process sees them, and the driver or why it did not come up.
pub(super) type Built = (
    Rc<Bus>,
    Rc<RefCell<Device>>,
    (Region, Region),
    Result<TestDriver, Failure>,
);

/// How to build a device, its memory and a driver.
#[derive(Clone, Debug)]
pub(super) struct Setup {
    /// The device's configuration.
    pub(super) config: Config,
    /// Features offered.
    pub(super) offered: u64,
    /// Bytes of configuration block.
    pub(super) config_len: usize,
    /// The largest either queue may be.
    pub(super) queue_max: u16,
    /// Pages for each queue's rings.
    pub(super) ring_pages: usize,
    /// Pages for the headers.
    pub(super) area_pages: usize,
    /// Pages for received frames.
    pub(super) receive_pages: usize,
    /// Pages for frames to send.
    pub(super) transmit_pages: usize,
    /// Bookkeeping slots.
    pub(super) slots: usize,
    /// Driver options.
    pub(super) options: Options,
    /// The MSI-X vectors to ask for.
    pub(super) vectors: [u16; 2],
    /// What the device gets wrong.
    pub(super) misbehave: Misbehave,
}

/// The address a device would be given.
pub(super) const MAC: [u8; MAC_LEN] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

impl Setup {
    /// A device QEMU would present, less the features nobody here uses.
    pub(super) fn new() -> Self {
        use ferrix_virtio::net::{
            FEATURE_CSUM, FEATURE_CTRL_VQ, FEATURE_GUEST_CSUM, FEATURE_HOST_TSO4, FEATURE_MAC,
            FEATURE_MRG_RXBUF, FEATURE_MTU, FEATURE_STATUS, STATUS_LINK_UP,
        };
        use ferrix_virtio::pci::FEATURE_ACCESS_PLATFORM;
        Setup {
            config: Config {
                mac: Some(MAC),
                status: Some(STATUS_LINK_UP),
                max_virtqueue_pairs: None,
                mtu: Some(1500),
                speed: None,
                duplex: None,
            },
            offered: FEATURE_VERSION_1
                | FEATURE_ACCESS_PLATFORM
                | FEATURE_MAC
                | FEATURE_STATUS
                | FEATURE_MTU
                | FEATURE_CSUM
                | FEATURE_GUEST_CSUM
                | FEATURE_HOST_TSO4
                | FEATURE_MRG_RXBUF
                | FEATURE_CTRL_VQ,
            config_len: 24,
            queue_max: 16,
            ring_pages: 1,
            area_pages: 1,
            receive_pages: 4,
            transmit_pages: 4,
            slots: 64,
            options: Options::default(),
            vectors: [1, 2],
            misbehave: Misbehave::default(),
        }
    }

    /// Build the device and memory, and try to bring the driver up: the bus,
    /// the device, the two data regions as the process sees them, and the
    /// driver.
    pub(super) fn try_build(&self) -> Built {
        let bus = Bus::new();
        let mut device = Device::new(Rc::clone(&bus), &self.config, self.offered, self.config_len);
        device.queue_max = self.queue_max;
        device.misbehave = self.misbehave;
        device.status = 0x0F;
        let device = Rc::new(RefCell::new(device));
        let parts = Parts {
            transport: Handle {
                device: Rc::clone(&device),
                vectors: self.vectors,
            },
            receive_rings: bus.pin(self.ring_pages, true),
            transmit_rings: bus.pin(self.ring_pages, true),
            area: bus.pin(self.area_pages, true),
            receive_data: bus.pin(self.receive_pages, true),
            transmit_data: bus.pin(self.transmit_pages, true),
            slots: vec![Slot::EMPTY; self.slots],
        };
        let regions = (parts.receive_data.clone(), parts.transmit_data.clone());
        let driver = Driver::init(parts, self.options);
        (bus, device, regions, driver)
    }

    /// Build everything, requiring the driver to come up.
    pub(super) fn build(&self) -> Rig {
        let (bus, device, (receive, transmit), driver) = self.try_build();
        let driver = Owned(Some(driver.expect("the driver comes up")));
        Rig {
            bus,
            device,
            receive,
            transmit,
            driver,
        }
    }
}

/// Drop a teardown's parts, even a wedged one's.
///
/// A wedged teardown keeps its memory in `ManuallyDrop` because a real device
/// that did not reset may still write to it. This device is test memory that
/// writes nothing once the test stops stepping it, so its memory is freed —
/// otherwise Miri reports the driver's deliberate leak as a test's.
pub(super) fn free(teardown: TestTeardown) {
    match teardown {
        Teardown::Released(_) => {}
        Teardown::Wedged(released) => {
            let _ = ManuallyDrop::into_inner(released);
        }
    }
}

/// A test's driver, shut down when the test is done with it.
#[derive(Debug)]
pub(super) struct Owned(Option<TestDriver>);

impl Owned {
    /// Shut the driver down now and hand back what that returns.
    pub(super) fn shutdown(mut self) -> TestTeardown {
        self.0.take().expect("a driver").shutdown()
    }
}

impl Deref for Owned {
    type Target = TestDriver;
    fn deref(&self) -> &TestDriver {
        self.0.as_ref().expect("a driver")
    }
}

impl DerefMut for Owned {
    fn deref_mut(&mut self) -> &mut TestDriver {
        self.0.as_mut().expect("a driver")
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        let Some(driver) = self.0.take() else {
            return;
        };
        if std::thread::panicking() {
            // A failing assertion may hold the device borrowed; shutting down
            // now would panic again and lose the first message.
            let _ = ManuallyDrop::new(driver);
            return;
        }
        free(driver.shutdown());
    }
}

/// A device, its memory and a driver, up.
#[derive(Debug)]
pub(super) struct Rig {
    /// The bus.
    pub(super) bus: Rc<Bus>,
    /// The device.
    pub(super) device: Rc<RefCell<Device>>,
    /// The receive data region, as the process sees it.
    pub(super) receive: Region,
    /// The transmit data region, as the process sees it.
    pub(super) transmit: Region,
    /// The driver.
    pub(super) driver: Owned,
}

impl Rig {
    /// Have the device deliver a frame.
    pub(super) fn deliver(&self, frame: &[u8]) -> bool {
        self.device.borrow_mut().deliver(frame)
    }

    /// Take every event there is, releasing nothing.
    pub(super) fn drain(&mut self) -> Vec<Event> {
        let mut all = Vec::new();
        let mut out = [Event::Sent { id: u64::MAX }; 4];
        loop {
            let drained = self.driver.on_interrupt(&mut out).expect("no fault");
            all.extend_from_slice(&out[..drained.events]);
            if !drained.more {
                return all;
            }
        }
    }

    /// Require the device saw no protocol error and no fault.
    pub(super) fn assert_clean(&self) {
        assert_eq!(
            self.device.borrow().protocol_errors,
            Vec::<&str>::new(),
            "the driver broke the protocol"
        );
        assert_eq!(
            self.bus.faults.get(),
            0,
            "device accesses outside pinned pages"
        );
        assert_eq!(self.driver.fault(), None, "the driver faulted");
    }
}
