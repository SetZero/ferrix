//! A virtio-blk device on the host, and the memory between it and the driver.
//!
//! The device checks the driver as much as the driver checks it: every
//! out-of-order status write, every feature accepted that was not offered, a
//! queue enabled before `FEATURES_OK`, a chain of the wrong shape — each is
//! recorded in [`Device::protocol_errors`], which the tests require to stay
//! empty. And every device access to an address no pinned page is mapped at
//! counts as a fault on the [`Bus`].

use core::cell::{Cell, RefCell};
use core::mem::ManuallyDrop;
use core::ops::{Deref, DerefMut};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use ferrix_virtio::blk::{
    Config, DeviceConfig, FEATURE_BLK_SIZE, FEATURE_RO, Header, PAGE_SIZE, RequestType,
    STATUS_IOERR, STATUS_OK, STATUS_UNSUPP,
};
use ferrix_virtio::pci::{
    CONFIG_GENERATION, CommonConfig, DEVICE_FEATURE, DEVICE_FEATURE_SELECT, DEVICE_STATUS,
    DRIVER_FEATURE, DRIVER_FEATURE_SELECT, FEATURE_VERSION_1, NO_VECTOR, NUM_QUEUES, QUEUE_DESC,
    QUEUE_DEVICE, QUEUE_DRIVER, QUEUE_ENABLE, QUEUE_MSIX_VECTOR, QUEUE_NOTIFY_OFF, QUEUE_SELECT,
    QUEUE_SIZE, STATUS_DEVICE_NEEDS_RESET, STATUS_DRIVER_OK, STATUS_FAILED, STATUS_FEATURES_OK,
};
use ferrix_virtio::{Descriptor, Layout, QueueMemory, SplitQueueDevice};

use crate::{
    Completion, DevicePages, Driver, ISR_QUEUE, InitFailure, Op, Options, Parts, Request,
    RequestArea, Slot, SubmitError, Teardown, Transport,
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

/// Memory, and the device-address mapping in front of it.
#[derive(Debug)]
pub(super) struct Bus {
    /// Every pinned page, back to back.
    phys: RefCell<Vec<u8>>,
    /// Device page address to page index in `phys`.
    map: RefCell<BTreeMap<u64, usize>>,
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
            phys: RefCell::new(Vec::new()),
            map: RefCell::new(BTreeMap::new()),
            next_consecutive: Cell::new(0x10_0000_0000),
            next_scattered: Cell::new(0x7FFF_0000_0000),
            faults: Cell::new(0),
        })
    }

    /// Pin `count` zeroed pages, at consecutive device addresses or at
    /// addresses no two of which follow on.
    pub(super) fn pin(self: &Rc<Self>, count: usize, scattered: bool) -> Region {
        let mut phys = Vec::new();
        let mut device = Vec::new();
        for _ in 0..count {
            let index = self.phys.borrow().len() / PAGE;
            self.phys.borrow_mut().resize((index + 1) * PAGE, 0);
            let address = if scattered {
                let address = self.next_scattered.get();
                self.next_scattered.set(address - 3 * PAGE_SIZE);
                address
            } else {
                let address = self.next_consecutive.get();
                self.next_consecutive.set(address + PAGE_SIZE);
                address
            };
            let _ = self.map.borrow_mut().insert(address, index);
            phys.push(index);
            device.push(address);
        }
        // A gap, so two pins never follow on from each other either.
        self.next_consecutive
            .set(self.next_consecutive.get() + 16 * PAGE_SIZE);
        Region {
            bus: Rc::clone(self),
            phys,
            device,
        }
    }

    /// Where `address` is in `phys`, if a page is mapped there.
    fn locate(&self, address: u64) -> Option<usize> {
        let page = address & !(PAGE_SIZE - 1);
        let index = *self.map.borrow().get(&page)?;
        Some(index * PAGE + (address - page) as usize)
    }

    /// The device reads a byte.
    pub(super) fn device_read(&self, address: u64) -> u8 {
        match self.locate(address) {
            Some(at) => self.phys.borrow()[at],
            None => {
                self.faults.set(self.faults.get() + 1);
                0
            }
        }
    }

    /// The device writes a byte.
    pub(super) fn device_write(&self, address: u64, value: u8) {
        match self.locate(address) {
            Some(at) => self.phys.borrow_mut()[at] = value,
            None => self.faults.set(self.faults.get() + 1),
        }
    }

    /// The bytes of `len` from `address` that lie in one page: how many.
    fn chunk(address: u64, len: usize) -> usize {
        (PAGE - (address % PAGE_SIZE) as usize).min(len)
    }

    /// The device reads `out.len()` bytes from `address`, a page at a time,
    /// so a test copying megabytes is not millions of lookups under Miri.
    pub(super) fn device_copy_out(&self, address: u64, out: &mut [u8]) {
        let mut done = 0;
        while done < out.len() {
            let at = address.wrapping_add(done as u64);
            let chunk = Self::chunk(at, out.len() - done);
            let target = &mut out[done..done + chunk];
            match self.locate(at) {
                Some(start) => target.copy_from_slice(&self.phys.borrow()[start..start + chunk]),
                None => {
                    self.faults.set(self.faults.get() + 1);
                    target.fill(0);
                }
            }
            done += chunk;
        }
    }

    /// The device writes `bytes` at `address`, a page at a time.
    pub(super) fn device_copy_in(&self, address: u64, bytes: &[u8]) {
        let mut done = 0;
        while done < bytes.len() {
            let at = address.wrapping_add(done as u64);
            let chunk = Self::chunk(at, bytes.len() - done);
            match self.locate(at) {
                Some(start) => self.phys.borrow_mut()[start..start + chunk]
                    .copy_from_slice(&bytes[done..done + chunk]),
                None => self.faults.set(self.faults.get() + 1),
            }
            done += chunk;
        }
    }
}

/// A pinned region, as the driver and the test see it.
#[derive(Clone, Debug)]
pub(super) struct Region {
    /// The bus it is on.
    bus: Rc<Bus>,
    /// Page indices in the bus's memory.
    phys: Vec<usize>,
    /// Device address of each page.
    device: Vec<u64>,
}

impl Region {
    /// Where byte `offset` is in the bus's memory.
    fn at(&self, offset: usize) -> Option<usize> {
        let page = self.phys.get(offset / PAGE)?;
        Some(page * PAGE + offset % PAGE)
    }

    /// Read `len` bytes from `offset`, as the process would, a page at a time.
    pub(super) fn read(&self, offset: usize, len: usize) -> Vec<u8> {
        let mut out = vec![0; len];
        let mut done = 0;
        while done < len {
            let at = offset + done;
            let chunk = (PAGE - at % PAGE).min(len - done);
            if let Some(start) = self.at(at) {
                out[done..done + chunk]
                    .copy_from_slice(&self.bus.phys.borrow()[start..start + chunk]);
            }
            done += chunk;
        }
        out
    }

    /// Write `bytes` at `offset`, as the process would, a page at a time.
    pub(super) fn write(&self, offset: usize, bytes: &[u8]) {
        let mut done = 0;
        while done < bytes.len() {
            let at = offset + done;
            let chunk = (PAGE - at % PAGE).min(bytes.len() - done);
            if let Some(start) = self.at(at) {
                self.bus.phys.borrow_mut()[start..start + chunk]
                    .copy_from_slice(&bytes[done..done + chunk]);
            }
            done += chunk;
        }
    }

    /// One byte.
    fn get(&self, offset: usize) -> u8 {
        self.at(offset)
            .and_then(|at| self.bus.phys.borrow().get(at).copied())
            .unwrap_or(0)
    }

    /// Set one byte.
    fn set(&self, offset: usize, value: u8) {
        if let Some(at) = self.at(offset)
            && let Some(slot) = self.bus.phys.borrow_mut().get_mut(at)
        {
            *slot = value;
        }
    }
}

impl DevicePages for Region {
    fn device_pages(&self) -> &[u64] {
        &self.device
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
// zero, so no access leaves the bus's `Vec`; alignment is never relied on,
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
        self.bus.device_read(self.address(offset))
    }
    fn write_u8(&mut self, offset: usize, value: u8) {
        self.bus.device_write(self.address(offset), value);
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
    /// Add this to every `written`.
    pub(super) extra_written: u32,
    /// Write this status byte instead of the right one.
    pub(super) status: Option<u8>,
    /// Write no status byte at all.
    pub(super) skip_status: bool,
    /// Fail every read or write from this sector on.
    pub(super) fail_from_sector: Option<u64>,
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

/// A virtio-blk device.
pub(super) struct Device {
    bus: Rc<Bus>,
    /// The disk.
    pub(super) disk: Vec<u8>,
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
    /// The logical block size requests must be whole multiples of.
    block_size: u64,
    queue_select: u16,
    /// The request queue's largest size.
    pub(super) queue_max: u16,
    queue: QueueRegisters,
    /// `queue_notify_off`.
    pub(super) notify_off: u16,
    isr: u8,
    ring: Option<SplitQueueDevice<RingView>>,
    taken: Vec<u16>,
    /// Doorbells rung.
    pub(super) notifications: usize,
    /// Flushes served.
    pub(super) flushes: usize,
    /// What to get wrong.
    pub(super) misbehave: Misbehave,
    /// Every way the driver broke the protocol.
    pub(super) protocol_errors: Vec<&'static str>,
    /// Every value written to `device_status`.
    pub(super) status_writes: Vec<u8>,
    /// The data descriptors of every chain served.
    pub(super) chains: Vec<Vec<Descriptor>>,
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
        let block_size = if offered & FEATURE_BLK_SIZE != 0 {
            u64::from(config.blk_size.unwrap_or(512))
        } else {
            512
        };
        Device {
            bus,
            disk: vec![0; (config.capacity * 512) as usize],
            offered,
            accepted: 0,
            device_select: 0,
            driver_select: 0,
            status: 0,
            generation: Cell::new(0),
            config: bytes,
            block_size: block_size.max(512),
            queue_select: 0,
            queue_max: 64,
            queue: QueueRegisters::default(),
            notify_off: 3,
            isr: 0,
            ring: None,
            taken: Vec::new(),
            notifications: 0,
            flushes: 0,
            misbehave: Misbehave::default(),
            protocol_errors: Vec::new(),
            status_writes: Vec::new(),
            chains: Vec::new(),
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

    /// The request queue's registers: size, addresses, vector, enabled.
    pub(super) fn queue(&self) -> (u16, u64, u64, u64, u16, bool) {
        let queue = self.queue;
        (
            queue.size,
            queue.descriptors,
            queue.driver,
            queue.device,
            queue.vector,
            queue.enabled,
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
        self.queue = QueueRegisters {
            size: self.queue_max,
            vector: NO_VECTOR,
            ..QueueRegisters::default()
        };
        self.ring = None;
        self.taken.clear();
        self.isr = 0;
    }

    /// Read a register.
    fn register(&self, offset: u32) -> u32 {
        let selected = self.queue_select == 0;
        match offset {
            DEVICE_FEATURE => match self.device_select {
                0 => self.offered as u32,
                1 => (self.offered >> 32) as u32,
                _ => 0,
            },
            NUM_QUEUES => 1,
            DEVICE_STATUS => u32::from(self.status()),
            CONFIG_GENERATION => {
                if self.misbehave.churn_config {
                    self.generation.set(self.generation.get().wrapping_add(1));
                }
                u32::from(self.generation.get())
            }
            QUEUE_SELECT => u32::from(self.queue_select),
            QUEUE_SIZE if selected => u32::from(self.queue.size),
            QUEUE_MSIX_VECTOR if selected => u32::from(self.queue.vector),
            QUEUE_MSIX_VECTOR => u32::from(NO_VECTOR),
            QUEUE_ENABLE if selected => u32::from(self.queue.enabled),
            QUEUE_NOTIFY_OFF if selected => u32::from(self.notify_off),
            _ => 0,
        }
    }

    /// Write a register.
    fn set_register(&mut self, offset: u32, value: u32) {
        let selected = self.queue_select == 0;
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
            QUEUE_SIZE | QUEUE_MSIX_VECTOR | QUEUE_ENABLE if !selected => {}
            QUEUE_SIZE => {
                self.require_features_ok(features_ok);
                self.queue.size = value as u16;
            }
            QUEUE_MSIX_VECTOR => {
                self.queue.vector = if self.misbehave.drop_vector {
                    NO_VECTOR
                } else {
                    value as u16
                };
            }
            QUEUE_ENABLE => self.enable(value),
            _ => self.set_address(offset, value, features_ok),
        }
    }

    /// Write half of one of the queue's addresses.
    fn set_address(&mut self, offset: u32, value: u32, features_ok: bool) {
        let (slot, base) = match offset {
            o if o == QUEUE_DESC || o == QUEUE_DESC + 4 => {
                (&mut self.queue.descriptors, QUEUE_DESC)
            }
            o if o == QUEUE_DRIVER || o == QUEUE_DRIVER + 4 => {
                (&mut self.queue.driver, QUEUE_DRIVER)
            }
            o if o == QUEUE_DEVICE || o == QUEUE_DEVICE + 4 => {
                (&mut self.queue.device, QUEUE_DEVICE)
            }
            _ => return,
        };
        if offset == base {
            *slot = (*slot & !0xFFFF_FFFF) | u64::from(value);
        } else {
            *slot = (*slot & 0xFFFF_FFFF) | u64::from(value) << 32;
        }
        if self.queue_select != 0 {
            self.error("an address written for a queue that does not exist");
        }
        self.require_features_ok(features_ok);
    }

    /// Complain about queue setup before `FEATURES_OK`.
    fn require_features_ok(&mut self, features_ok: bool) {
        if !features_ok {
            self.error("queue set up before FEATURES_OK");
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
            if !self.queue.enabled {
                self.error("DRIVER_OK before the queue was enabled");
            }
        }
        self.status = value;
    }

    /// `queue_enable` written.
    fn enable(&mut self, value: u32) {
        if value != 1 {
            self.error("queue_enable written with something other than 1");
            return;
        }
        self.require_features_ok(self.status & STATUS_FEATURES_OK != 0);
        let size = self.queue.size;
        let Ok(layout) = Layout::for_size(size) else {
            self.error("queue enabled with a bad size");
            return;
        };
        if size > self.queue_max {
            self.error("queue enabled larger than its maximum");
        }
        let view = RingView {
            bus: Rc::clone(&self.bus),
            layout,
            descriptors: self.queue.descriptors,
            driver: self.queue.driver,
            device: self.queue.device,
        };
        self.ring = Some(SplitQueueDevice::new(layout, view));
        self.queue.enabled = true;
    }

    /// Take every chain the driver has published, without serving any.
    pub(super) fn take_available(&mut self) {
        let driver_ok = self.status & STATUS_DRIVER_OK != 0;
        let Some(ring) = self.ring.as_mut() else {
            return;
        };
        if ring.has_available() && !driver_ok {
            self.protocol_errors
                .push("a chain published before DRIVER_OK");
        }
        for _ in 0..=ring.layout().queue_size {
            match ring.next_chain() {
                Ok(Some(head)) => self.taken.push(head),
                Ok(None) => break,
                Err(_) => {
                    self.protocol_errors.push("the available ring is corrupt");
                    break;
                }
            }
        }
    }

    /// Chains taken and not yet served.
    pub(super) fn taken(&self) -> usize {
        self.taken.len()
    }

    /// Serve and complete the `index`th chain taken.
    pub(super) fn complete(&mut self, index: usize) {
        if index < self.taken.len() {
            let head = self.taken.remove(index);
            self.serve(head);
        }
    }

    /// Take and serve everything, in order. Returns how many.
    pub(super) fn run(&mut self) -> usize {
        self.take_available();
        let count = self.taken.len();
        while !self.taken.is_empty() {
            self.complete(0);
        }
        count
    }

    /// Serve the chain at `head`, as QEMU does.
    fn serve(&mut self, head: u16) {
        let Some(ring) = self.ring.as_ref() else {
            return;
        };
        let mut descriptors = vec![BLANK; usize::from(ring.layout().queue_size)];
        let Ok(count) = ring.read_chain(head, &mut descriptors) else {
            self.error("a chain the device cannot walk");
            return;
        };
        descriptors.truncate(count);
        let (first, last) = (descriptors[0], descriptors[count - 1]);
        if count < 2
            || first.is_device_writable()
            || first.len < 16
            || !last.is_device_writable()
            || last.len < 1
        {
            self.error("a chain without a header and a status");
            return;
        }
        let mut header = [0_u8; 16];
        self.bus.device_copy_out(first.address, &mut header);
        let data = descriptors[1..count - 1].to_vec();
        let status = match Header::decode(header) {
            Ok(Header {
                kind: RequestType::In,
                sector,
            }) => self.read_into(sector, &data),
            Ok(Header {
                kind: RequestType::Out,
                sector,
            }) => self.write_from(sector, &data),
            Ok(Header {
                kind: RequestType::Flush,
                sector,
            }) => {
                if !data.is_empty() || sector != 0 {
                    self.error("a flush with data or a sector");
                }
                self.flushes += 1;
                STATUS_OK
            }
            _ => STATUS_UNSUPP,
        };
        let writable: u32 = data
            .iter()
            .filter(|descriptor| descriptor.is_device_writable())
            .map(|descriptor| descriptor.len)
            .sum();
        self.chains.push(data);

        let status = self.misbehave.status.unwrap_or(status);
        if !self.misbehave.skip_status {
            self.bus.device_write(last.address, status);
        }
        let written = writable + 1 + self.misbehave.extra_written;
        if let Some(ring) = self.ring.as_mut()
            && ring.complete(head, written).is_err()
        {
            self.protocol_errors
                .push("a head the device cannot complete");
        }
        self.isr |= ISR_QUEUE;
    }

    /// Whether `bytes` from `sector` is a request this disk takes.
    fn in_range(&self, sector: u64, bytes: u64) -> bool {
        let sectors = bytes / 512;
        bytes > 0
            && bytes.is_multiple_of(self.block_size)
            && (sector * 512).is_multiple_of(self.block_size)
            && sector + sectors <= self.disk.len() as u64 / 512
            && self
                .misbehave
                .fail_from_sector
                .is_none_or(|from| sector < from)
    }

    /// Serve a read.
    fn read_into(&mut self, sector: u64, data: &[Descriptor]) -> u8 {
        if data
            .iter()
            .any(|descriptor| !descriptor.is_device_writable())
        {
            self.error("a read whose data the device may not write");
            return STATUS_IOERR;
        }
        let bytes: u64 = data
            .iter()
            .map(|descriptor| u64::from(descriptor.len))
            .sum();
        if !self.in_range(sector, bytes) {
            return STATUS_IOERR;
        }
        let mut at = (sector * 512) as usize;
        for descriptor in data {
            let len = descriptor.len as usize;
            self.bus
                .device_copy_in(descriptor.address, &self.disk[at..at + len]);
            at += len;
        }
        STATUS_OK
    }

    /// Serve a write.
    fn write_from(&mut self, sector: u64, data: &[Descriptor]) -> u8 {
        if data.iter().any(Descriptor::is_device_writable) {
            self.error("a write whose data the device may write");
            return STATUS_IOERR;
        }
        let bytes: u64 = data
            .iter()
            .map(|descriptor| u64::from(descriptor.len))
            .sum();
        if self.offered & FEATURE_RO != 0 || !self.in_range(sector, bytes) {
            return STATUS_IOERR;
        }
        let mut at = (sector * 512) as usize;
        for descriptor in data {
            let len = descriptor.len as usize;
            self.bus
                .device_copy_out(descriptor.address, &mut self.disk[at..at + len]);
            at += len;
        }
        STATUS_OK
    }

    /// The ring memory, as the device reaches it.
    fn view(&self) -> RingView {
        self.ring.as_ref().expect("a queue").memory().clone()
    }

    /// Put a used entry naming `id` in the ring without serving anything.
    pub(super) fn forge_used(&mut self, id: u32, written: u32) {
        let mut view = self.view();
        let layout = view.layout;
        let index = view.read_u16(layout.used_ring + 2);
        let slot = usize::from(index % layout.queue_size);
        view.write_u32(layout.used_ring + 4 + slot * 8, id);
        view.write_u32(layout.used_ring + 8 + slot * 8, written);
        view.write_u16(layout.used_ring + 2, index.wrapping_add(1));
        self.isr |= ISR_QUEUE;
    }

    /// Move `used.idx` forward by `by` without writing any entry.
    pub(super) fn jump_used_index(&mut self, by: u16) {
        let mut view = self.view();
        let at = view.layout.used_ring + 2;
        let index = view.read_u16(at);
        view.write_u16(at, index.wrapping_add(by));
    }

    /// Point every descriptor's `next` outside the table.
    pub(super) fn scribble_links(&mut self) {
        let mut view = self.view();
        for index in 0..usize::from(view.layout.queue_size) {
            view.write_u16(index * 16 + 14, 0x7FFF);
        }
    }
}

/// The driver's handle on a [`Device`].
#[derive(Clone, Debug)]
pub(super) struct Handle {
    /// The device.
    pub(super) device: Rc<RefCell<Device>>,
    /// The vector to ask for.
    pub(super) vector: u16,
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
        device.notifications += 1;
        if queue != 0 || notify_off != device.notify_off {
            device.error("a doorbell for the wrong queue");
        }
    }

    fn queue_vector(&self) -> u16 {
        self.vector
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

/// How to build a device, its memory and a driver.
#[derive(Clone, Debug)]
pub(super) struct Setup {
    /// The device's configuration.
    pub(super) config: Config,
    /// Features offered.
    pub(super) offered: u64,
    /// Bytes of configuration block.
    pub(super) config_len: usize,
    /// The request queue's largest size.
    pub(super) queue_max: u16,
    /// Pages for the rings.
    pub(super) ring_pages: usize,
    /// Whether the rings' pages are scattered.
    pub(super) ring_scattered: bool,
    /// Pages for headers and status bytes.
    pub(super) area_pages: usize,
    /// Pages of data.
    pub(super) data_pages: usize,
    /// Whether the data pages are scattered.
    pub(super) data_scattered: bool,
    /// Bookkeeping slots.
    pub(super) slots: usize,
    /// Driver options.
    pub(super) options: Options,
    /// The MSI-X vector to ask for.
    pub(super) vector: u16,
    /// What the device gets wrong.
    pub(super) misbehave: Misbehave,
}

impl Setup {
    /// A disk QEMU would present, less the features nobody here uses.
    pub(super) fn new() -> Self {
        use ferrix_virtio::blk::{
            FEATURE_CONFIG_WCE, FEATURE_DISCARD, FEATURE_FLUSH, FEATURE_MQ, FEATURE_SEG_MAX,
            FEATURE_WRITE_ZEROES,
        };
        use ferrix_virtio::pci::FEATURE_ACCESS_PLATFORM;
        Setup {
            config: Config {
                capacity: 256,
                size_max: None,
                seg_max: Some(62),
                geometry: None,
                blk_size: Some(512),
                topology: None,
                writeback: Some(1),
                num_queues: Some(1),
                discard: None,
                write_zeroes: None,
                secure_erase: None,
                zoned: None,
            },
            offered: FEATURE_VERSION_1
                | FEATURE_ACCESS_PLATFORM
                | FEATURE_SEG_MAX
                | FEATURE_BLK_SIZE
                | FEATURE_FLUSH
                | FEATURE_CONFIG_WCE
                | FEATURE_MQ
                | FEATURE_DISCARD
                | FEATURE_WRITE_ZEROES
                | 1 << 28
                | 1 << 29,
            config_len: 96,
            queue_max: 64,
            ring_pages: 1,
            ring_scattered: true,
            area_pages: 1,
            data_pages: 16,
            data_scattered: true,
            slots: 64,
            options: Options::default(),
            vector: 1,
            misbehave: Misbehave::default(),
        }
    }

    /// Build the device and memory, and try to bring the driver up.
    pub(super) fn try_build(
        &self,
    ) -> (
        Rc<Bus>,
        Rc<RefCell<Device>>,
        Region,
        Result<TestDriver, Failure>,
    ) {
        let bus = Bus::new();
        let mut device = Device::new(Rc::clone(&bus), &self.config, self.offered, self.config_len);
        device.queue_max = self.queue_max;
        device.misbehave = self.misbehave;
        device.status = 0x0F;
        let device = Rc::new(RefCell::new(device));
        let rings = bus.pin(self.ring_pages, self.ring_scattered);
        let area = bus.pin(self.area_pages, true);
        let data = bus.pin(self.data_pages, self.data_scattered);
        let parts = Parts {
            transport: Handle {
                device: Rc::clone(&device),
                vector: self.vector,
            },
            rings,
            area,
            data: data.clone(),
            slots: vec![Slot::EMPTY; self.slots],
        };
        let driver = Driver::init(parts, self.options);
        (bus, device, data, driver)
    }

    /// Build everything, requiring the driver to come up.
    pub(super) fn build(&self) -> Rig {
        let (bus, device, data, driver) = self.try_build();
        let driver = Owned(Some(driver.expect("the driver comes up")));
        Rig {
            bus,
            device,
            data,
            driver,
        }
    }
}

/// A teardown, as the tests see it.
pub(super) type TestTeardown = Teardown<Handle, Region, Region, Region, Vec<Slot>>;

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
///
/// A `Driver` dropped without `shutdown` leaks its memory on purpose; see
/// [`free`] for why the tests do not.
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
    /// The data region, as the process sees it.
    pub(super) data: Region,
    /// The driver.
    pub(super) driver: Owned,
}

impl Rig {
    /// Submit a request.
    pub(super) fn submit(
        &mut self,
        id: u64,
        op: Op,
        sector: u64,
        count: u32,
        data_offset: u64,
    ) -> Result<crate::Accepted, SubmitError> {
        self.driver.submit(&Request {
            id,
            op,
            sector,
            count,
            data_offset,
        })
    }

    /// Let the device serve everything published.
    pub(super) fn run_device(&self) -> usize {
        self.device.borrow_mut().run()
    }

    /// Take every completion there is.
    pub(super) fn drain(&mut self) -> Vec<Completion> {
        let mut all = Vec::new();
        let mut out = [Completion {
            id: 0,
            status: crate::Status::Ok,
            bytes: 0,
        }; 4];
        loop {
            let drained = self.driver.on_interrupt(&mut out).expect("no fault");
            all.extend_from_slice(&out[..drained.completions]);
            if !drained.more {
                return all;
            }
        }
    }

    /// Serve everything and take the completions.
    pub(super) fn complete_all(&mut self) -> Vec<Completion> {
        let _ = self.run_device();
        self.drain()
    }

    /// Require the device saw no protocol error and no fault.
    pub(super) fn assert_clean(&self) {
        assert_eq!(self.device.borrow().protocol_errors, Vec::<&str>::new());
        assert_eq!(
            self.bus.faults.get(),
            0,
            "device accesses outside pinned pages"
        );
    }
}
