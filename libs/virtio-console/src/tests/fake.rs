//! A virtio-console device on the host, and the memory between it and the
//! driver.
//!
//! The device is as sceptical of the driver as the driver is of it: a queue
//! described before `FEATURES_OK`, a chain published before `DRIVER_OK`, a
//! control message written into a chain the driver may not have written, and
//! every access to an address no page is mapped at are each recorded in
//! [`Device::protocol_errors`], which the tests require to stay empty.

use core::cell::{Cell, RefCell};
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use ferrix_virtio::console::{CONTROL_BYTES, CONTROL_RECEIVE_QUEUE, CONTROL_TRANSMIT_QUEUE};
use ferrix_virtio::pci::{
    CommonConfig, DEVICE_FEATURE, DEVICE_FEATURE_SELECT, DEVICE_STATUS, DRIVER_FEATURE,
    DRIVER_FEATURE_SELECT, FEATURE_ACCESS_PLATFORM, FEATURE_VERSION_1, NO_VECTOR, NUM_QUEUES,
    QUEUE_DESC, QUEUE_DEVICE, QUEUE_DRIVER, QUEUE_ENABLE, QUEUE_MSIX_VECTOR, QUEUE_NOTIFY_OFF,
    QUEUE_SELECT, QUEUE_SIZE, STATUS_DEVICE_NEEDS_RESET, STATUS_DRIVER_OK, STATUS_FEATURES_OK,
};
use ferrix_virtio::{
    Descriptor, DeviceConfig, Layout, PAGE_SIZE, QueueMemory, SplitQueueDevice, console,
};

use crate::{Bytes, DevicePages, Driver, ISR_QUEUE, Options, Parts, QUEUE_COUNT, Slot, Transport};

/// A page, as a `usize`.
pub(super) const PAGE: usize = PAGE_SIZE as usize;

/// The features the device offers unless a test says otherwise.
pub(super) const OFFERED: u64 =
    FEATURE_VERSION_1 | FEATURE_ACCESS_PLATFORM | console::FEATURE_MULTIPORT;

/// A pinned region: bytes, and the device address of each of its pages.
///
/// Cloning shares the bytes, which is how the driver and the device end up
/// looking at the same memory.
#[derive(Clone, Debug)]
pub(super) struct Region {
    /// The bytes themselves, shared with every clone.
    bytes: Rc<RefCell<Vec<u8>>>,
    /// Device address of each page, in order.
    pages: Vec<u64>,
}

impl Region {
    /// A region of `pages` pages, whose device addresses start at `base` and
    /// follow on.
    pub(super) fn new(base: u64, pages: usize) -> Self {
        Region {
            bytes: Rc::new(RefCell::new(vec![0; pages * PAGE])),
            pages: (0..pages)
                .map(|index| base + index as u64 * PAGE_SIZE)
                .collect(),
        }
    }

    /// Where device address `address` is in this region, if anywhere.
    fn locate(&self, address: u64) -> Option<usize> {
        self.pages.iter().enumerate().find_map(|(index, page)| {
            let within = address.checked_sub(*page)?;
            (within < PAGE_SIZE).then(|| index * PAGE + within as usize)
        })
    }

    /// One byte, or zero past the end.
    fn get(&self, offset: usize) -> u8 {
        self.bytes.borrow().get(offset).copied().unwrap_or(0)
    }

    /// Write one byte, ignoring one past the end.
    fn set(&self, offset: usize, value: u8) {
        if let Some(slot) = self.bytes.borrow_mut().get_mut(offset) {
            *slot = value;
        }
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
}

impl DevicePages for Region {
    fn device_pages(&self) -> &[u64] {
        &self.pages
    }
}

impl Bytes for Region {
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
// SAFETY: every offset is looked up in the region's own `Vec` and a miss reads
// zero, so no access leaves it; alignment is never relied on, since nothing
// dereferences the rings as a struct; and the driver and the device are
// stepped one after the other by the tests, so the barrier has nothing to
// order.
unsafe impl QueueMemory for Region {
    fn read_u8(&self, offset: usize) -> u8 {
        self.get(offset)
    }

    fn write_u8(&mut self, offset: usize, value: u8) {
        self.set(offset, value);
    }

    fn barrier(&self) {}
}

/// What one queue's registers say.
#[derive(Clone, Copy, Debug, Default)]
struct QueueState {
    /// The largest size the device allows.
    max: u16,
    /// The size the driver set.
    size: u16,
    /// The descriptor table's address.
    descriptors: u64,
    /// The available ring's address.
    driver: u64,
    /// The used ring's address.
    device: u64,
    /// The MSI-X vector the device kept.
    vector: u16,
    /// Whether the driver enabled it.
    enabled: bool,
    /// What the device reports as its notification offset.
    notify_off: u16,
}

/// The device's registers, and the rules it holds the driver to.
#[derive(Debug)]
pub(super) struct Fake {
    /// Features offered.
    offered: u64,
    /// Features the driver accepted.
    pub(super) accepted: u64,
    /// Which half of the feature words is selected.
    device_select: u32,
    /// Likewise for the driver's.
    driver_select: u32,
    /// `device_status`.
    status: u8,
    /// The queue `QUEUE_SELECT` names.
    selected: u16,
    /// Every queue's registers.
    queues: Vec<QueueState>,
    /// `max_nr_ports`, as the configuration block reports it.
    ports: u32,
    /// Bytes the configuration block claims to be, so a test can truncate it.
    config_len: u32,
    /// The vector the driver will be told to use for every queue.
    vector: u16,
    /// Whether the device keeps the vector it is given.
    keeps_vector: bool,
    /// Doorbells rung, as `(queue, notify_off)`.
    pub(super) notifications: Vec<(u16, u16)>,
    /// The ISR byte the next acknowledgement returns.
    pub(super) isr: u8,
    /// Set when the device has given up: `device_status` then reads
    /// `DEVICE_NEEDS_RESET` however the driver last wrote it.
    needs_reset: Rc<Cell<bool>>,
    /// Rules the driver broke.
    pub(super) protocol_errors: Vec<&'static str>,
}

impl Fake {
    /// A device with `ports` ports and a queue maximum of `max`.
    pub(super) fn new(ports: u32, max: u16) -> Self {
        let queues = vec![
            QueueState {
                max,
                ..QueueState::default()
            };
            usize::from(console::queue_count(ports))
        ];
        let notify_offs = queues.len();
        let mut fake = Fake {
            offered: OFFERED,
            accepted: 0,
            device_select: 0,
            driver_select: 0,
            status: 0,
            selected: 0,
            queues,
            ports,
            config_len: console::CONFIG_LEN,
            vector: 1,
            keeps_vector: true,
            notifications: Vec::new(),
            isr: ISR_QUEUE,
            needs_reset: Rc::new(Cell::new(false)),
            protocol_errors: Vec::new(),
        };
        // A distinct offset per queue, so a driver that rings the wrong
        // doorbell is caught by the offset and not only by the number.
        for (index, queue) in fake.queues.iter_mut().enumerate() {
            queue.notify_off = u16::try_from(index).unwrap_or(0);
        }
        let _ = notify_offs;
        fake
    }

    /// Offer exactly `features` and nothing else.
    pub(super) fn offering(mut self, features: u64) -> Self {
        self.offered = features;
        self
    }

    /// Report a configuration block of `len` bytes.
    pub(super) fn with_config_len(mut self, len: u32) -> Self {
        self.config_len = len;
        self
    }

    /// Give up whenever `flag` is set, as a device that has hit a bug of its
    /// own does: the flag is the test's end of it.
    pub(super) fn wedged_by(mut self, flag: Rc<Cell<bool>>) -> Self {
        self.needs_reset = flag;
        self
    }

    /// Refuse whatever vector the driver asks for.
    pub(super) fn refusing_vectors(mut self) -> Self {
        self.keeps_vector = false;
        self
    }

    /// Whether the driver has said it is ready.
    pub(super) fn driver_ok(&self) -> bool {
        self.status & STATUS_DRIVER_OK != 0
    }

    /// One queue's registers.
    fn queue(&self, index: u16) -> Option<&QueueState> {
        self.queues.get(usize::from(index))
    }

    /// Note that the driver broke a rule.
    fn broke(&mut self, rule: &'static str) {
        self.protocol_errors.push(rule);
    }
}

impl CommonConfig for Fake {
    fn read8(&self, offset: u32) -> u8 {
        match offset {
            DEVICE_STATUS if self.needs_reset.get() => self.status | STATUS_DEVICE_NEEDS_RESET,
            DEVICE_STATUS => self.status,
            _ => 0,
        }
    }

    fn read16(&self, offset: u32) -> u16 {
        match offset {
            NUM_QUEUES => u16::try_from(self.queues.len()).unwrap_or(u16::MAX),
            QUEUE_SELECT => self.selected,
            QUEUE_SIZE => self.queue(self.selected).map_or(0, |queue| {
                if queue.size == 0 {
                    queue.max
                } else {
                    queue.size
                }
            }),
            QUEUE_MSIX_VECTOR => self
                .queue(self.selected)
                .map_or(NO_VECTOR, |queue| queue.vector),
            QUEUE_NOTIFY_OFF => self
                .queue(self.selected)
                .map_or(0, |queue| queue.notify_off),
            QUEUE_ENABLE => u16::from(self.queue(self.selected).is_some_and(|queue| queue.enabled)),
            _ => 0,
        }
    }

    fn read32(&self, offset: u32) -> u32 {
        match offset {
            DEVICE_FEATURE => {
                if self.device_select == 0 {
                    self.offered as u32
                } else {
                    (self.offered >> 32) as u32
                }
            }
            _ => 0,
        }
    }

    fn write8(&mut self, offset: u32, value: u8) {
        if offset != DEVICE_STATUS {
            return;
        }
        if value == 0 {
            self.status = 0;
            self.accepted = 0;
            for queue in &mut self.queues {
                *queue = QueueState {
                    max: queue.max,
                    notify_off: queue.notify_off,
                    ..QueueState::default()
                };
            }
            return;
        }
        if value & STATUS_DRIVER_OK != 0
            && !self
                .queues
                .iter()
                .take(QUEUE_COUNT)
                .all(|queue| queue.enabled)
        {
            self.broke("DRIVER_OK before every queue was enabled");
        }
        self.status = value;
    }

    fn write16(&mut self, offset: u32, value: u16) {
        match offset {
            QUEUE_SELECT => self.selected = value,
            QUEUE_SIZE => {
                let selected = self.selected;
                if self.status & STATUS_FEATURES_OK == 0 {
                    self.broke("a queue was sized before FEATURES_OK");
                }
                if let Some(queue) = self.queues.get_mut(usize::from(selected)) {
                    queue.size = value;
                }
            }
            QUEUE_MSIX_VECTOR => {
                let (selected, keeps) = (self.selected, self.keeps_vector);
                if let Some(queue) = self.queues.get_mut(usize::from(selected)) {
                    queue.vector = if keeps { value } else { NO_VECTOR };
                }
            }
            QUEUE_ENABLE => {
                let selected = self.selected;
                if self.status & STATUS_DRIVER_OK != 0 {
                    self.broke("a queue was enabled after DRIVER_OK");
                }
                if let Some(queue) = self.queues.get_mut(usize::from(selected)) {
                    queue.enabled = value != 0;
                }
            }
            _ => {}
        }
    }

    fn write32(&mut self, offset: u32, value: u32) {
        let selected = usize::from(self.selected);
        match offset {
            DEVICE_FEATURE_SELECT => self.device_select = value,
            DRIVER_FEATURE_SELECT => self.driver_select = value,
            DRIVER_FEATURE => {
                let shifted = u64::from(value) << if self.driver_select == 0 { 0 } else { 32 };
                if u64::from(value) != 0 && shifted & !self.offered != 0 {
                    self.broke("a feature was accepted that was not offered");
                }
                self.accepted |= shifted;
            }
            QUEUE_DESC => set_half(
                &mut self.queues,
                selected,
                |queue| &mut queue.descriptors,
                value,
                false,
            ),
            0x24 => set_half(
                &mut self.queues,
                selected,
                |queue| &mut queue.descriptors,
                value,
                true,
            ),
            QUEUE_DRIVER => set_half(
                &mut self.queues,
                selected,
                |queue| &mut queue.driver,
                value,
                false,
            ),
            0x2C => set_half(
                &mut self.queues,
                selected,
                |queue| &mut queue.driver,
                value,
                true,
            ),
            QUEUE_DEVICE => set_half(
                &mut self.queues,
                selected,
                |queue| &mut queue.device,
                value,
                false,
            ),
            0x34 => set_half(
                &mut self.queues,
                selected,
                |queue| &mut queue.device,
                value,
                true,
            ),
            _ => {}
        }
    }
}

/// Write one half of a 64-bit queue register.
fn set_half(
    queues: &mut [QueueState],
    selected: usize,
    field: impl Fn(&mut QueueState) -> &mut u64,
    value: u32,
    high: bool,
) {
    if let Some(queue) = queues.get_mut(selected) {
        let slot = field(queue);
        *slot = if high {
            (*slot & 0xFFFF_FFFF) | (u64::from(value) << 32)
        } else {
            (*slot & !0xFFFF_FFFF) | u64::from(value)
        };
    }
}

impl DeviceConfig for Fake {
    fn config_len(&self) -> u32 {
        self.config_len
    }

    fn config_read8(&self, offset: u32) -> u8 {
        (self.config_read32(offset & !3) >> ((offset & 3) * 8)) as u8
    }

    fn config_read16(&self, offset: u32) -> u16 {
        (self.config_read32(offset & !3) >> ((offset & 3) * 8)) as u16
    }

    fn config_read32(&self, offset: u32) -> u32 {
        match offset {
            console::CONFIG_MAX_NR_PORTS => self.ports,
            _ => 0,
        }
    }
}

impl Transport for Fake {
    fn notify(&mut self, queue: u16, notify_off: u16) {
        if !self.driver_ok() {
            self.broke("a doorbell was rung before DRIVER_OK");
        }
        self.notifications.push((queue, notify_off));
    }

    fn queue_vector(&self, _queue: u16) -> u16 {
        self.vector
    }

    fn acknowledge_interrupt(&mut self) -> u8 {
        self.isr
    }
}

/// Everything the driver and the device share, so that a test can hand the
/// driver its half and keep the other.
pub(super) struct Harness {
    /// Each queue's ring region, by queue number.
    pub(super) rings: [Region; QUEUE_COUNT],
    /// The control area.
    pub(super) control: Region,
    /// Where arriving bytes land.
    pub(super) receive_data: Region,
    /// Where bytes to send are written.
    pub(super) transmit_data: Region,
    /// Set to make the device give up mid-run.
    pub(super) wedge: Rc<Cell<bool>>,
}

impl Harness {
    /// The regions a plain device needs: a page of rings per queue, and a page
    /// each of control area and data.
    pub(super) fn new() -> Self {
        let mut base = 0x10_0000_u64;
        let mut page = |pages: usize| {
            let region = Region::new(base, pages);
            base += pages as u64 * PAGE_SIZE;
            region
        };
        Harness {
            rings: core::array::from_fn(|_| page(2)),
            control: page(1),
            receive_data: page(1),
            transmit_data: page(1),
            wedge: Rc::new(Cell::new(false)),
        }
    }

    /// The driver's half, with `slots` bookkeeping entries.
    pub(super) fn parts(&self, slots: usize) -> Parts<Fake, Region, Region, Region, Vec<Slot>> {
        self.parts_for(Fake::new(31, 64).wedged_by(self.wedge.clone()), slots)
    }

    /// The driver's half, against a device a test has shaped.
    pub(super) fn parts_for(
        &self,
        fake: Fake,
        slots: usize,
    ) -> Parts<Fake, Region, Region, Region, Vec<Slot>> {
        Parts {
            transport: fake,
            rings: self.rings.clone(),
            control: self.control.clone(),
            receive_data: self.receive_data.clone(),
            transmit_data: self.transmit_data.clone(),
            slots: vec![Slot::EMPTY; slots],
        }
    }

    /// Where device address `address` is, in whichever region holds it.
    fn locate(&self, address: u64) -> Option<(&Region, usize)> {
        let regions =
            self.rings
                .iter()
                .chain([&self.control, &self.receive_data, &self.transmit_data]);
        regions
            .into_iter()
            .find_map(|region| region.locate(address).map(|offset| (region, offset)))
    }
}

/// The device half of the queues, once the driver has built them.
pub(super) struct Device<'a> {
    /// The shared memory.
    harness: &'a Harness,
    /// One device-side queue per queue number.
    queues: Vec<SplitQueueDevice<Region>>,
    /// Addresses the device reached that no page is mapped at.
    pub(super) faults: usize,
}

impl<'a> Device<'a> {
    /// Attach to every queue the driver built, which it must already have.
    pub(super) fn new(harness: &'a Harness, queue_size: u16) -> Self {
        let layout = Layout::for_size(queue_size).expect("a queue size the driver chose");
        Device {
            harness,
            queues: harness
                .rings
                .iter()
                .map(|region| SplitQueueDevice::new(layout, region.clone()))
                .collect(),
            faults: 0,
        }
    }

    /// The chain the driver published on `queue`, as its descriptors.
    fn next_chain(&mut self, queue: u16) -> Option<(u16, Vec<Descriptor>)> {
        let ring = self.queues.get_mut(usize::from(queue))?;
        let head = ring.next_chain().expect("the available ring is sound")?;
        let mut descriptors = vec![
            Descriptor {
                address: 0,
                len: 0,
                flags: 0,
                next: 0
            };
            usize::from(ring.layout().queue_size)
        ];
        let count = ring
            .read_chain(head, &mut descriptors)
            .expect("the chain is sound");
        descriptors.truncate(count);
        Some((head, descriptors))
    }

    /// Complete a chain on `queue`, saying `written` bytes went into it.
    fn complete(&mut self, queue: u16, head: u16, written: u32) {
        if let Some(ring) = self.queues.get_mut(usize::from(queue)) {
            ring.complete(head, written).expect("the head is a chain");
        }
    }

    /// Read every byte of a chain the driver published for the device to read.
    fn read_chain(&mut self, descriptors: &[Descriptor]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for descriptor in descriptors {
            match self.harness.locate(descriptor.address) {
                Some((region, offset)) => {
                    bytes.extend(region.read(offset, descriptor.len as usize));
                }
                None => self.faults += 1,
            }
        }
        bytes
    }

    /// Write `bytes` into a chain the driver posted for the device to write,
    /// and say how many went in.
    fn write_chain(&mut self, descriptors: &[Descriptor], bytes: &[u8]) -> u32 {
        let mut written = 0;
        let mut rest = bytes;
        for descriptor in descriptors {
            if rest.is_empty() {
                break;
            }
            let take = rest.len().min(descriptor.len as usize);
            match self.harness.locate(descriptor.address) {
                Some((region, offset)) => {
                    region.write(offset, &rest[..take]);
                    written += take;
                    rest = &rest[take..];
                }
                None => self.faults += 1,
            }
        }
        u32::try_from(written).unwrap_or(u32::MAX)
    }

    /// Take the next control message the driver sent, if it sent one.
    pub(super) fn take_control(&mut self) -> Option<Vec<u8>> {
        let (head, descriptors) = self.next_chain(CONTROL_TRANSMIT_QUEUE)?;
        let bytes = self.read_chain(&descriptors);
        let len = u32::try_from(bytes.len()).unwrap_or(0);
        self.complete(CONTROL_TRANSMIT_QUEUE, head, len);
        Some(bytes)
    }

    /// Every control message the driver has sent and the device has not taken.
    pub(super) fn take_controls(&mut self) -> Vec<Vec<u8>> {
        let mut messages = Vec::new();
        while let Some(message) = self.take_control() {
            messages.push(message);
        }
        messages
    }

    /// Send one control message to the driver.
    ///
    /// Panics if the driver has no buffer posted, which is itself the bug the
    /// caller wants to hear about.
    pub(super) fn send_control(&mut self, port: u32, event: u16, value: u16, name: &[u8]) {
        let mut bytes = Vec::with_capacity(CONTROL_BYTES + name.len());
        bytes.extend(port.to_le_bytes());
        bytes.extend(event.to_le_bytes());
        bytes.extend(value.to_le_bytes());
        bytes.extend(name);
        self.send_control_bytes(&bytes);
    }

    /// Send whatever bytes a test likes as a control message.
    pub(super) fn send_control_bytes(&mut self, bytes: &[u8]) {
        let (head, descriptors) = self
            .next_chain(CONTROL_RECEIVE_QUEUE)
            .expect("a control buffer is posted");
        let written = self.write_chain(&descriptors, bytes);
        self.complete(CONTROL_RECEIVE_QUEUE, head, written);
    }

    /// Complete a control receive chain claiming `written` bytes, whatever was
    /// really put in it.
    pub(super) fn send_control_claiming(&mut self, bytes: &[u8], written: u32) {
        let (head, descriptors) = self
            .next_chain(CONTROL_RECEIVE_QUEUE)
            .expect("a control buffer is posted");
        let _ = self.write_chain(&descriptors, bytes);
        self.complete(CONTROL_RECEIVE_QUEUE, head, written);
    }

    /// Put `bytes` on the port, as the host end would.
    pub(super) fn send_port(&mut self, port: u32, bytes: &[u8]) {
        let queue = console::receive_queue(port);
        let (head, descriptors) = self.next_chain(queue).expect("a receive buffer is posted");
        let written = self.write_chain(&descriptors, bytes);
        self.complete(queue, head, written);
    }

    /// Put `bytes` on the port but claim `written` went in.
    pub(super) fn send_port_claiming(&mut self, port: u32, bytes: &[u8], written: u32) {
        let queue = console::receive_queue(port);
        let (head, descriptors) = self.next_chain(queue).expect("a receive buffer is posted");
        let _ = self.write_chain(&descriptors, bytes);
        self.complete(queue, head, written);
    }

    /// Take what the driver sent out on the port.
    pub(super) fn take_port(&mut self, port: u32) -> Option<Vec<u8>> {
        let queue = console::transmit_queue(port);
        let (head, descriptors) = self.next_chain(queue)?;
        let bytes = self.read_chain(&descriptors);
        let len = u32::try_from(bytes.len()).unwrap_or(0);
        self.complete(queue, head, len);
        Some(bytes)
    }

    /// Complete a chain on the port's transmit queue for a head the driver
    /// never published.
    pub(super) fn complete_unknown(&mut self, port: u32, head: u16) {
        self.complete(console::transmit_queue(port), head, 0);
    }
}

/// A driver over the harness, brought up but with no port open.
pub(super) type Under = Driver<Fake, Region, Region, Region, Vec<Slot>>;

/// Bring a driver up against a plain device.
pub(super) fn brought_up(harness: &Harness) -> Under {
    match Driver::init(harness.parts(QUEUE_COUNT * 64), Options::default()) {
        Ok(driver) => driver,
        Err(failure) => panic!("bring-up failed: {}", failure.error),
    }
}

/// Bring a driver up and walk `docs/CLIPBOARD.md` §3.3 to an open port.
pub(super) fn opened(harness: &Harness) -> (Under, Device<'_>, u32) {
    let mut driver = brought_up(harness);
    let mut device = Device::new(harness, driver.info().queue_size);
    let port = 1;

    // The driver announced itself before anything else.
    let opening = device.take_controls();
    assert_eq!(
        opening.len(),
        1,
        "the driver says DEVICE_READY and nothing else at bring-up"
    );

    let mut events = [crate::Event::Sent { id: 0 }; 8];
    device.send_control(port, console::PORT_ADD, 0, &[]);
    let _ = driver.on_interrupt(&mut events).expect("PORT_ADD is fine");
    device.send_control(port, console::PORT_NAME, 0, console::SPICE_PORT_NAME);
    let _ = driver.on_interrupt(&mut events).expect("PORT_NAME is fine");
    device.send_control(port, console::PORT_OPEN, 1, &[]);
    let _ = driver.on_interrupt(&mut events).expect("PORT_OPEN is fine");

    assert!(driver.port().is_open(), "the port is open after §3.3");
    (driver, device, port)
}
