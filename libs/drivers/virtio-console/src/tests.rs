//! The driver against a device played by the test.
//!
//! The device's side of a virtqueue is `ferrix_virtio::SplitQueueDevice`, the
//! same code the kernel uses when it serves a userspace driver, so what these
//! tests drive the driver with is a real ring and not a mock of one. The
//! control messages are written out by hand from virtio 1.2 §5.3.6, not built
//! with `Control::write`, so the driver is checked against the specification
//! rather than against its own encoder.

extern crate std;

use std::cell::RefCell;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use ferrix_virtio::pci::{
    DEVICE_FEATURE, DEVICE_FEATURE_SELECT, DEVICE_STATUS, DRIVER_FEATURE, DRIVER_FEATURE_SELECT,
    FEATURE_ACCESS_PLATFORM, FEATURE_VERSION_1, NO_VECTOR, QUEUE_ENABLE, QUEUE_NOTIFY_OFF,
    QUEUE_SELECT, QUEUE_SIZE as QUEUE_SIZE_REGISTER, STATUS_DRIVER_OK,
};
use ferrix_virtio::{Descriptor, Layout, SplitQueueDevice};

use super::*;

/// Bytes of memory each queue's rings get: one page, as a driver gives them.
const RING: usize = RING_BYTES;

/// Memory shared with the "device", which is this test.
///
/// A handle, not a buffer: the driver takes one and the test keeps another
/// onto the same bytes, so the test can play the device's end of a real
/// queue over the memory the driver is using rather than over a copy.
#[derive(Debug, Clone)]
struct Memory {
    bytes: Rc<RefCell<Vec<u8>>>,
    pages: Rc<[u64]>,
}

impl Memory {
    /// `bytes` of memory, pinned at device addresses that are deliberately
    /// not the host ones: an address the driver computes is turned back into
    /// an offset here, so a wrong page shows up as a wrong offset.
    fn new(bytes: usize, base: u64) -> Memory {
        let pages = bytes.div_ceil(PAGE_SIZE as usize).max(1);
        Memory {
            bytes: Rc::new(RefCell::new(vec![0; bytes])),
            pages: (0..pages)
                .map(|page| 0x4000_0000 + base + page as u64 * PAGE_SIZE)
                .collect(),
        }
    }

    /// The offset a device address names, for the test's own writes.
    fn offset_of(&self, address: u64) -> usize {
        let page = self
            .pages
            .iter()
            .position(|at| address >= *at && address < at + PAGE_SIZE)
            .expect("an address in this memory");
        page * PAGE_SIZE as usize + (address - self.pages[page]) as usize
    }

    /// Write `bytes` at `offset`, as the device would.
    fn put(&self, offset: usize, bytes: &[u8]) {
        let mut memory = self.bytes.borrow_mut();
        for (index, byte) in bytes.iter().enumerate() {
            if let Some(slot) = memory.get_mut(offset + index) {
                *slot = *byte;
            }
        }
    }

    /// Read `len` bytes from `offset`, as the device would.
    fn take(&self, offset: usize, len: usize) -> Vec<u8> {
        let memory = self.bytes.borrow();
        (0..len)
            .map(|at| memory.get(offset + at).copied().unwrap_or(0))
            .collect()
    }
}

impl DevicePages for Memory {
    fn device_pages(&self) -> &[u64] {
        &self.pages
    }
}

impl Area for Memory {
    fn read_u8(&self, offset: usize) -> u8 {
        self.bytes.borrow().get(offset).copied().unwrap_or(0)
    }
    fn write_u8(&mut self, offset: usize, value: u8) {
        if let Some(slot) = self.bytes.borrow_mut().get_mut(offset) {
            *slot = value;
        }
    }
}

// SAFETY: a `Vec` the test owns for as long as the driver and the device
// side both hold handles to it, stepped one after the other and never
// concurrently.
unsafe impl QueueMemory for Memory {
    fn read_u8(&self, offset: usize) -> u8 {
        self.bytes.borrow().get(offset).copied().unwrap_or(0)
    }
    fn write_u8(&mut self, offset: usize, value: u8) {
        if let Some(slot) = self.bytes.borrow_mut().get_mut(offset) {
            *slot = value;
        }
    }
    fn barrier(&self) {}
}

/// The device: a configuration block, the status register, and what the
/// driver notified.
#[derive(Debug)]
struct Device {
    common: Vec<u8>,
    config: Vec<u8>,
    /// Queues the driver enabled, in the order it enabled them.
    enabled: Vec<u16>,
    /// The queue last notified.
    notified: Vec<u16>,
    /// The size the driver asked each queue to be.
    sizes: Vec<(u16, u16)>,
    /// What the driver wrote to the feature register, both halves.
    features: u64,
}

impl Device {
    fn new(ports: u32) -> Device {
        let mut config = vec![0_u8; console::CONFIG_LEN as usize];
        config[4..8].copy_from_slice(&ports.to_le_bytes());
        Device {
            common: vec![0; 0x40],
            config,
            enabled: Vec::new(),
            notified: Vec::new(),
            sizes: Vec::new(),
            features: 0,
        }
    }

    fn selected(&self) -> u16 {
        u16::from_le_bytes([
            self.common[QUEUE_SELECT as usize],
            self.common[QUEUE_SELECT as usize + 1],
        ])
    }
}

impl CommonConfig for Device {
    fn read8(&self, offset: u32) -> u8 {
        self.common.get(offset as usize).copied().unwrap_or(0)
    }
    fn read16(&self, offset: u32) -> u16 {
        // Every queue this device has can hold 64 entries, which is more
        // than the driver asks for.
        if offset == QUEUE_SIZE_REGISTER {
            return 64;
        }
        if offset == QUEUE_NOTIFY_OFF {
            return self.selected();
        }
        u16::from(self.read8(offset)) | u16::from(self.read8(offset + 1)) << 8
    }
    fn read32(&self, offset: u32) -> u32 {
        // Everything this driver needs, offered: version 1, access platform
        // and multiport.
        if offset == DEVICE_FEATURE {
            let half = u32::from_le_bytes([self.common[DEVICE_FEATURE_SELECT as usize], 0, 0, 0]);
            let offered = console::FEATURE_MULTIPORT | FEATURE_VERSION_1 | FEATURE_ACCESS_PLATFORM;
            return if half == 0 {
                offered as u32
            } else {
                (offered >> 32) as u32
            };
        }
        u32::from(self.read16(offset)) | u32::from(self.read16(offset + 2)) << 16
    }
    fn write8(&mut self, offset: u32, value: u8) {
        if let Some(slot) = self.common.get_mut(offset as usize) {
            *slot = value;
        }
        // A reset is acknowledged at once, and FEATURES_OK is always kept.
    }
    fn write16(&mut self, offset: u32, value: u16) {
        for (index, byte) in value.to_le_bytes().into_iter().enumerate() {
            self.write8(offset + index as u32, byte);
        }
        if offset == QUEUE_ENABLE && value == 1 {
            let queue = self.selected();
            self.enabled.push(queue);
        }
        if offset == QUEUE_SIZE_REGISTER {
            let queue = self.selected();
            self.sizes.push((queue, value));
        }
    }
    fn write32(&mut self, offset: u32, value: u32) {
        for (index, byte) in value.to_le_bytes().into_iter().enumerate() {
            self.write8(offset + index as u32, byte);
        }
        if offset == DRIVER_FEATURE {
            let half = self.common[DRIVER_FEATURE_SELECT as usize];
            if half == 0 {
                self.features |= u64::from(value);
            } else {
                self.features |= u64::from(value) << 32;
            }
        }
    }
}

impl DeviceConfig for Device {
    fn config_len(&self) -> u32 {
        self.config.len() as u32
    }
    fn config_read8(&self, offset: u32) -> u8 {
        self.config.get(offset as usize).copied().unwrap_or(0xff)
    }
    fn config_read16(&self, offset: u32) -> u16 {
        u16::from(self.config_read8(offset)) | u16::from(self.config_read8(offset + 1)) << 8
    }
    fn config_read32(&self, offset: u32) -> u32 {
        u32::from(self.config_read16(offset)) | u32::from(self.config_read16(offset + 2)) << 16
    }
}

impl Transport for Device {
    fn notify(&mut self, queue: u16, _notify_off: u16) {
        self.notified.push(queue);
    }
    fn queue_vector(&self) -> u16 {
        NO_VECTOR
    }
    fn acknowledge_interrupt(&mut self) -> u8 {
        1
    }
}

/// A driver over a device declaring `ports` ports, brought up.
fn start(ports: u32) -> Driver<Device, Memory, Memory> {
    Driver::init(
        Parts {
            transport: Device::new(ports),
            control_rx_rings: Memory::new(RING, 0),
            control_tx_rings: Memory::new(RING, 0x1_0000),
            port_rx_rings: Memory::new(RING, 0x2_0000),
            port_tx_rings: Memory::new(RING, 0x3_0000),
            control_area: Memory::new(CONTROL_AREA_BYTES, 0x4_0000),
            port_area: Memory::new(AREA_BYTES, 0x5_0000),
        },
        Options::default(),
    )
    .expect("a device that offers everything comes up")
}

/// The layout every queue here has.
fn layout() -> Layout {
    Layout::for_size(QUEUE_SIZE).expect("a power of two")
}

/// The bring-up is the order virtio 1.2 §3.1.1 fixes, and the queues are the
/// four of §5.3.2 -- the control pair and port 1's, which are 2, 3, 4 and 5
/// and never 0 and 1.
#[test]
fn the_four_queues_are_described_and_the_device_started() {
    let driver = start(2);
    let device = &driver.transport;
    assert_eq!(
        device.enabled,
        vec![2, 3, 4, 5],
        "the control pair and port 1's pair, in that order"
    );
    assert!(
        device.sizes.iter().all(|(_, size)| *size == QUEUE_SIZE),
        "every queue is asked to be the driver's size"
    );
    assert_eq!(
        device.features,
        console::FEATURE_MULTIPORT | FEATURE_VERSION_1 | FEATURE_ACCESS_PLATFORM,
        "multiport is accepted, and nothing the driver does not want"
    );
    assert!(
        device.common[DEVICE_STATUS as usize] & STATUS_DRIVER_OK != 0,
        "DRIVER_OK is set once the queues are described"
    );
}

/// A device that does not offer multiport is refused: a port that cannot be
/// named is not one an agent may take for the host's clipboard.
#[test]
fn a_device_without_multiport_is_refused() {
    struct Plain(Device);
    // Everything delegates but the feature read, which offers version 1 only.
    impl CommonConfig for Plain {
        fn read8(&self, offset: u32) -> u8 {
            self.0.read8(offset)
        }
        fn read16(&self, offset: u32) -> u16 {
            self.0.read16(offset)
        }
        fn read32(&self, offset: u32) -> u32 {
            if offset == DEVICE_FEATURE {
                let half = self.0.common[DEVICE_FEATURE_SELECT as usize];
                let offered = FEATURE_VERSION_1;
                return if half == 0 {
                    offered as u32
                } else {
                    (offered >> 32) as u32
                };
            }
            self.0.read32(offset)
        }
        fn write8(&mut self, offset: u32, value: u8) {
            self.0.write8(offset, value);
        }
        fn write16(&mut self, offset: u32, value: u16) {
            self.0.write16(offset, value);
        }
        fn write32(&mut self, offset: u32, value: u32) {
            self.0.write32(offset, value);
        }
    }
    impl DeviceConfig for Plain {
        fn config_len(&self) -> u32 {
            self.0.config_len()
        }
        fn config_read8(&self, offset: u32) -> u8 {
            self.0.config_read8(offset)
        }
        fn config_read16(&self, offset: u32) -> u16 {
            self.0.config_read16(offset)
        }
        fn config_read32(&self, offset: u32) -> u32 {
            self.0.config_read32(offset)
        }
    }
    impl Transport for Plain {
        fn notify(&mut self, queue: u16, off: u16) {
            self.0.notify(queue, off);
        }
        fn queue_vector(&self) -> u16 {
            NO_VECTOR
        }
        fn acknowledge_interrupt(&mut self) -> u8 {
            1
        }
    }

    let failed = Driver::init(
        Parts {
            transport: Plain(Device::new(2)),
            control_rx_rings: Memory::new(RING, 0),
            control_tx_rings: Memory::new(RING, 0x1_0000),
            port_rx_rings: Memory::new(RING, 0x2_0000),
            port_tx_rings: Memory::new(RING, 0x3_0000),
            control_area: Memory::new(CONTROL_AREA_BYTES, 0x4_0000),
            port_area: Memory::new(AREA_BYTES, 0x5_0000),
        },
        Options::default(),
    );
    assert!(
        matches!(
            failed,
            Err(ConsoleError::Transport(
                TransportError::MissingFeatures { .. }
            ))
        ),
        "a console without multiport is not a device this driver drives"
    );
}

/// A device with no ports, or more than this crate drives, is refused at the
/// configuration block.
#[test]
fn an_impossible_port_count_is_refused_at_init() {
    for ports in [0, 32] {
        let failed = Driver::init(
            Parts {
                transport: Device::new(ports),
                control_rx_rings: Memory::new(RING, 0),
                control_tx_rings: Memory::new(RING, 0x1_0000),
                port_rx_rings: Memory::new(RING, 0x2_0000),
                port_tx_rings: Memory::new(RING, 0x3_0000),
                control_area: Memory::new(CONTROL_AREA_BYTES, 0x4_0000),
                port_area: Memory::new(AREA_BYTES, 0x5_0000),
            },
            Options::default(),
        );
        assert!(
            matches!(failed, Err(ConsoleError::Protocol(_))),
            "{ports} ports is refused"
        );
    }
}

// ---------------------------------------------------------------------------
// The control conversation and the port's bytes
// ---------------------------------------------------------------------------

/// A started driver, with the test holding the memory of every queue and the
/// device's end of each, so it can play the device.
///
/// The device ends are kept rather than made per call: a `SplitQueueDevice`
/// remembers where it is in the available ring, and a fresh one starts at
/// the beginning and hands back chains that were consumed long ago.
struct Harness {
    driver: Driver<Device, Memory, Memory>,
    control_rx: SplitQueueDevice<Memory>,
    control_tx: SplitQueueDevice<Memory>,
    port_rx: SplitQueueDevice<Memory>,
    port_tx: SplitQueueDevice<Memory>,
    control_area: Memory,
    port_area: Memory,
}

impl Harness {
    fn new(ports: u32) -> Harness {
        let control_rx = Memory::new(RING, 0);
        let control_tx = Memory::new(RING, 0x1_0000);
        let port_rx = Memory::new(RING, 0x2_0000);
        let port_tx = Memory::new(RING, 0x3_0000);
        let control_area = Memory::new(CONTROL_AREA_BYTES, 0x4_0000);
        let port_area = Memory::new(AREA_BYTES, 0x5_0000);
        let driver = Driver::init(
            Parts {
                transport: Device::new(ports),
                control_rx_rings: control_rx.clone(),
                control_tx_rings: control_tx.clone(),
                port_rx_rings: port_rx.clone(),
                port_tx_rings: port_tx.clone(),
                control_area: control_area.clone(),
                port_area: port_area.clone(),
            },
            Options::default(),
        )
        .expect("the device comes up");
        Harness {
            driver,
            control_rx: SplitQueueDevice::new(layout(), control_rx),
            control_tx: SplitQueueDevice::new(layout(), control_tx),
            port_rx: SplitQueueDevice::new(layout(), port_rx),
            port_tx: SplitQueueDevice::new(layout(), port_tx),
            control_area,
            port_area,
        }
    }

    /// Take the next chain the driver published on a device-readable queue,
    /// and give back what it holds: what the driver sent.
    fn sent(device: &mut SplitQueueDevice<Memory>, area: &Memory) -> Option<Vec<u8>> {
        let head = device.next_chain().expect("a sound ring")?;
        let mut chain = [EMPTY; 4];
        let count = device.read_chain(head, &mut chain).expect("a sound chain");
        let mut bytes = Vec::new();
        for entry in chain.iter().take(count) {
            bytes.extend(area.take(area.offset_of(entry.address), entry.len as usize));
        }
        device.complete(head, 0).expect("a sound completion");
        Some(bytes)
    }

    /// Fill the next buffer the driver posted on a device-writable queue
    /// with `bytes`, as the device would.
    fn deliver(device: &mut SplitQueueDevice<Memory>, area: &Memory, bytes: &[u8]) {
        let head = device
            .next_chain()
            .expect("a sound ring")
            .expect("the driver posted a buffer");
        let mut chain = [EMPTY; 4];
        let count = device.read_chain(head, &mut chain).expect("a sound chain");
        assert_eq!(count, 1, "one buffer per receive descriptor");
        area.put(area.offset_of(chain[0].address), bytes);
        device
            .complete(head, bytes.len() as u32)
            .expect("a sound completion");
    }

    /// What the driver sent on the control queue, if anything.
    fn control_sent(&mut self) -> Option<Vec<u8>> {
        Harness::sent(&mut self.control_tx, &self.control_area)
    }

    /// Give the driver a control message.
    fn control(&mut self, bytes: &[u8]) {
        Harness::deliver(&mut self.control_rx, &self.control_area, bytes);
    }

    /// Give the driver bytes on the port.
    fn arrives(&mut self, bytes: &[u8]) {
        Harness::deliver(&mut self.port_rx, &self.port_area, bytes);
    }

    /// What the driver sent on the port, if anything.
    fn port_sent(&mut self) -> Option<Vec<u8>> {
        Harness::sent(&mut self.port_tx, &self.port_area)
    }
}

/// An empty descriptor, for a chain a read fills in.
const EMPTY: Descriptor = Descriptor {
    address: 0,
    len: 0,
    flags: 0,
    next: 0,
};

/// A control message, written by hand from virtio 1.2 §5.3.6: `id`, `event`,
/// `value`, four bytes and two and two.
fn control(port: u32, event: u16, value: u16) -> Vec<u8> {
    let mut bytes = port.to_le_bytes().to_vec();
    bytes.extend(event.to_le_bytes());
    bytes.extend(value.to_le_bytes());
    bytes
}

/// A `PORT_NAME` message: the header, then the name.
fn named(port: u32, name: &[u8]) -> Vec<u8> {
    let mut bytes = control(port, 7, 0);
    bytes.extend(name);
    bytes.push(0);
    bytes
}

/// The driver says `DEVICE_READY` as soon as it is up, and before that it has
/// posted the buffers the answer will land in.
#[test]
fn the_driver_announces_itself_once_it_is_up() {
    let mut harness = Harness::new(2);
    assert_eq!(
        harness.control_sent(),
        Some(control(0xffff_ffff, 0, 1)),
        "BAD_ID, DEVICE_READY, 1"
    );
    // Every control receive buffer is posted: the device may answer at once.
    let mut posted = 0;
    while harness
        .control_rx
        .next_chain()
        .expect("a sound ring")
        .is_some()
    {
        posted += 1;
    }
    assert_eq!(posted, QUEUE_SIZE, "a buffer for every slot");
}

/// The whole opening exchange of §3.3: the device adds a port, the driver
/// says it is ready, the device names it, and the driver opens the one whose
/// name it was looking for.
#[test]
fn the_named_port_is_found_and_opened() {
    let mut harness = Harness::new(2);
    let _ = harness.control_sent();

    harness.control(&control(1, 1, 0)); // PORT_ADD for port 1
    assert_eq!(
        harness.driver.poll(),
        Ok(None),
        "an add is answered, not news"
    );
    assert_eq!(
        harness.control_sent(),
        Some(control(1, 3, 1)),
        "PORT_READY for port 1"
    );

    harness.control(&named(1, console::SPICE_PORT_NAME));
    assert_eq!(
        harness.driver.poll(),
        Ok(Some(Event::Found { port: 1 })),
        "the port answering to the name is taken"
    );
    assert_eq!(
        harness.control_sent(),
        Some(control(1, 6, 1)),
        "PORT_OPEN for port 1: this end is open"
    );
    assert_eq!(harness.driver.port(), Some(1));
    assert!(
        !harness.driver.port_open(),
        "the host end has not opened yet"
    );

    harness.control(&control(1, 6, 1)); // PORT_OPEN from the device
    assert_eq!(harness.driver.poll(), Ok(Some(Event::Open { open: true })));
    assert!(harness.driver.port_open(), "now both ends are open");
}

/// A port with another name is answered so the device is not left waiting,
/// and then ignored: the number is the device's to choose and the name is
/// what identifies the port.
#[test]
fn a_port_with_another_name_is_answered_and_ignored() {
    let mut harness = Harness::new(3);
    let _ = harness.control_sent();

    harness.control(&control(1, 1, 0));
    assert_eq!(harness.driver.poll(), Ok(None));
    assert_eq!(harness.control_sent(), Some(control(1, 3, 1)), "answered");

    harness.control(&named(1, b"org.qemu.guest_agent.0"));
    assert_eq!(harness.driver.poll(), Ok(None), "not the port wanted");
    assert_eq!(harness.control_sent(), None, "and not opened");
    assert_eq!(harness.driver.port(), None);

    // The one after it is, and its number is not 1.
    harness.control(&control(2, 1, 0));
    assert_eq!(harness.driver.poll(), Ok(None));
    assert_eq!(harness.control_sent(), Some(control(2, 3, 1)));
    harness.control(&named(2, console::SPICE_PORT_NAME));
    assert_eq!(
        harness.driver.poll(),
        Ok(Some(Event::Found { port: 2 })),
        "a port is found by its name, whatever its number"
    );
}

/// Bytes the device writes on the port come back out of `read`, and the
/// buffer is posted again so the device never runs out.
#[test]
fn what_arrives_on_the_port_is_read_back() {
    let mut harness = opened();
    harness.arrives(b"from the host");

    assert_eq!(harness.driver.poll(), Ok(Some(Event::Data)));
    let mut out = [0_u8; 64];
    assert_eq!(harness.driver.read(&mut out), Ok(Some(13)));
    assert_eq!(&out[..13], b"from the host");
    assert_eq!(harness.driver.read(&mut out), Ok(None), "nothing more");

    // And again, which only works if the buffer went back.
    harness.arrives(b"and again");
    assert_eq!(harness.driver.read(&mut out), Ok(Some(9)));
    assert_eq!(&out[..9], b"and again");
}

/// A device that claims to have written more than the buffer holds is
/// clamped, not believed.
#[test]
fn a_lying_completion_length_is_clamped() {
    let mut harness = opened();
    let head = harness
        .port_rx
        .next_chain()
        .expect("a sound ring")
        .expect("a posted buffer");
    harness
        .port_rx
        .complete(head, 0xffff_ffff)
        .expect("a completion");
    let mut out = [0_u8; CHUNK];
    assert_eq!(
        harness.driver.read(&mut out),
        Ok(Some(CHUNK)),
        "clamped to the buffer that was posted"
    );
}

/// What `write` takes goes out on the port's transmit queue.
#[test]
fn what_is_written_goes_out_on_the_port() {
    let mut harness = opened();
    assert_eq!(harness.driver.write(b"to the host"), Ok(11));
    assert_eq!(harness.port_sent(), Some(b"to the host".to_vec()));
}

/// A write longer than a buffer is taken a buffer at a time, and the caller
/// is told how much was taken: a chunk is never split by this driver's own
/// choice of buffer size.
#[test]
fn a_write_longer_than_a_buffer_is_taken_in_pieces() {
    let mut harness = opened();
    let long = vec![b'x'; CHUNK + 10];
    assert_eq!(harness.driver.write(&long), Ok(CHUNK));
    assert_eq!(harness.port_sent().map(|sent| sent.len()), Some(CHUNK));
}

/// Nothing is sent on a port whose host end is not open: a closed port
/// carries nothing, and a driver that queued into it would be writing into a
/// queue nobody drains.
#[test]
fn a_closed_port_carries_nothing() {
    let mut nothing = Harness::new(2);
    assert_eq!(nothing.driver.write(b"nowhere"), Ok(0), "no port yet");

    let mut harness = opened();
    harness.control(&control(1, 6, 0)); // PORT_OPEN, value 0: the host went
    assert_eq!(harness.driver.poll(), Ok(Some(Event::Open { open: false })));
    assert!(!harness.driver.port_open());
    assert_eq!(harness.driver.write(b"nowhere"), Ok(0));
}

/// The port being removed closes it, and whoever was using it is told.
#[test]
fn a_removed_port_closes() {
    let mut harness = opened();
    harness.control(&control(1, 2, 0)); // PORT_REMOVE
    assert_eq!(harness.driver.poll(), Ok(Some(Event::Open { open: false })));
    assert_eq!(harness.driver.port(), None);
}

/// A control message about a port the device never declared is refused: it
/// names queues the device does not have.
#[test]
fn a_control_message_about_an_undeclared_port_is_refused() {
    let mut harness = Harness::new(2);
    harness.control(&control(7, 1, 0));
    assert!(
        matches!(harness.driver.poll(), Err(ConsoleError::Protocol(_))),
        "a device with two ports has no port 7"
    );
}

/// An event this crate does not know is ignored rather than refused.
#[test]
fn an_unknown_control_event_is_ignored() {
    let mut harness = Harness::new(2);
    let _ = harness.control_sent();
    harness.control(&control(1, 9, 3));
    assert_eq!(harness.driver.poll(), Ok(None));
    assert_eq!(harness.control_sent(), None, "and answered with nothing");
}

/// A driver with its port open on both ends, which most of the tests above
/// start from.
fn opened() -> Harness {
    let mut harness = Harness::new(2);
    let _ = harness.control_sent();
    harness.control(&control(1, 1, 0));
    let _ = harness.driver.poll();
    let _ = harness.control_sent();
    harness.control(&named(1, console::SPICE_PORT_NAME));
    let _ = harness.driver.poll();
    let _ = harness.control_sent();
    harness.control(&control(1, 6, 1));
    let _ = harness.driver.poll();
    assert!(harness.driver.port_open(), "the port is open on both ends");
    harness
}
