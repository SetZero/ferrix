//! A virtio-snd device on the host, doing what QEMU 9.2.4's does.
//!
//! `hw/audio/virtio-snd.c`, as `docs/AUDIO.md` §3.3 reads it: two streams by
//! default, output first; every stream configured and prepared at realize,
//! so `PCM_INFO` answers at once with `channels_max` the count currently set;
//! control requests answered as the doorbell rings; transmit buffers held
//! until the backend has consumed all of each ([`Device::consume`]), then
//! completed with status OK and the buffer's own size as `latency_bytes`;
//! `PCM_RELEASE` completing every buffer it holds before it answers; no
//! events at all. [`Misbehave`] is what a device might do instead. Every way
//! the driver breaks the protocol goes to `protocol_errors`.

use core::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use ferrix_virtio::pci::{
    CommonConfig, DEVICE_FEATURE, DEVICE_FEATURE_SELECT, DEVICE_STATUS, DRIVER_FEATURE,
    DRIVER_FEATURE_SELECT, FEATURE_ACCESS_PLATFORM, FEATURE_VERSION_1, NO_VECTOR, NUM_QUEUES,
    QUEUE_DESC, QUEUE_DEVICE, QUEUE_DRIVER, QUEUE_ENABLE, QUEUE_MSIX_VECTOR, QUEUE_NOTIFY_OFF,
    QUEUE_SELECT, QUEUE_SIZE, STATUS_DEVICE_NEEDS_RESET, STATUS_DRIVER_OK, STATUS_FEATURES_OK,
};
use ferrix_virtio::snd::DeviceConfig;
use ferrix_virtio::{Descriptor, Layout, PAGE_SIZE, QueueMemory, SplitQueueDevice};

use crate::{DevicePages, ISR_QUEUE, Scratch, Transport};

pub(super) const PAGE: usize = PAGE_SIZE as usize;

/// Pinned pages, reached by device address.
#[derive(Debug, Default)]
pub(super) struct Bus {
    pages: RefCell<BTreeMap<u64, Vec<u8>>>,
    next: RefCell<u64>,
}

impl Bus {
    pub(super) fn new() -> Rc<Self> {
        let bus = Self::default();
        *bus.next.borrow_mut() = 0x10_0000_0000;
        Rc::new(bus)
    }

    /// Pin `count` pages, consecutive or each apart from the last.
    pub(super) fn pin(self: &Rc<Self>, count: usize, scattered: bool) -> Region {
        let mut device = Vec::new();
        for _ in 0..count {
            let address = *self.next.borrow();
            let _ = self.pages.borrow_mut().insert(address, vec![0; PAGE]);
            device.push(address);
            *self.next.borrow_mut() += if scattered { 3 * PAGE_SIZE } else { PAGE_SIZE };
        }
        *self.next.borrow_mut() += 16 * PAGE_SIZE;
        Region {
            bus: Rc::clone(self),
            device,
        }
    }

    pub(super) fn read(&self, address: u64) -> Option<u8> {
        let page = address & !(PAGE_SIZE - 1);
        self.pages
            .borrow()
            .get(&page)
            .and_then(|bytes| bytes.get((address - page) as usize).copied())
    }

    pub(super) fn write(&self, address: u64, value: u8) -> bool {
        let page = address & !(PAGE_SIZE - 1);
        match self.pages.borrow_mut().get_mut(&page) {
            Some(bytes) => {
                bytes[(address - page) as usize] = value;
                true
            }
            None => false,
        }
    }
}

/// A pinned region as the driver sees it.
#[derive(Clone, Debug)]
pub(super) struct Region {
    bus: Rc<Bus>,
    pub(super) device: Vec<u64>,
}

impl Region {
    /// Write `bytes` at `offset`, as the core writes samples into its buffer.
    pub(super) fn fill(&self, offset: usize, bytes: &[u8]) {
        for (index, byte) in bytes.iter().enumerate() {
            let at = offset + index;
            let page = self.device[at / PAGE];
            let _ = self.bus.write(page + (at % PAGE) as u64, *byte);
        }
    }
}

impl DevicePages for Region {
    fn device_pages(&self) -> &[u64] {
        &self.device
    }
}

impl Scratch for Region {
    fn read_u8(&self, offset: usize) -> u8 {
        self.device
            .get(offset / PAGE)
            .and_then(|page| self.bus.read(page + (offset % PAGE) as u64))
            .unwrap_or(0)
    }
    fn write_u8(&mut self, offset: usize, value: u8) {
        if let Some(page) = self.device.get(offset / PAGE) {
            let _ = self.bus.write(page + (offset % PAGE) as u64, value);
        }
    }
}

#[expect(
    unsafe_code,
    reason = "AUDIT: QueueMemory is an unsafe trait; this implementation only indexes Vecs through checked lookups"
)]
// SAFETY: every access is a checked lookup of a page the bus owns and a miss
// reads zero; nothing is dereferenced as a struct; driver and device are
// stepped one after the other, so the barrier has nothing to order.
unsafe impl QueueMemory for Region {
    fn read_u8(&self, offset: usize) -> u8 {
        Scratch::read_u8(self, offset)
    }
    fn write_u8(&mut self, offset: usize, value: u8) {
        Scratch::write_u8(self, offset, value);
    }
    fn barrier(&self) {}
}

/// The device's view of one queue's rings.
#[derive(Debug)]
struct RingView {
    bus: Rc<Bus>,
    layout: Layout,
    descriptors: u64,
    driver: u64,
    device: u64,
}

impl RingView {
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
    reason = "AUDIT: QueueMemory is an unsafe trait; every access goes through the bus's checked lookup"
)]
// SAFETY: as `Region`'s.
unsafe impl QueueMemory for RingView {
    fn read_u8(&self, offset: usize) -> u8 {
        self.bus.read(self.address(offset)).unwrap_or(0)
    }
    fn write_u8(&mut self, offset: usize, value: u8) {
        let _ = self.bus.write(self.address(offset), value);
    }
    fn barrier(&self) {}
}

/// What the device gets wrong.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Misbehave {
    /// Complete each transmit buffer claiming this many bytes written.
    pub(super) written: Option<u32>,
    /// Complete each transmit buffer with this status instead of OK.
    pub(super) status: Option<u32>,
    /// Never answer a control request.
    pub(super) silent: bool,
    /// Answer `SET_PARAMS` with `NOT_SUPP`.
    pub(super) refuse_params: bool,
    /// Complete the held buffers newest first.
    pub(super) reversed: bool,
    /// Set `DEVICE_NEEDS_RESET`.
    pub(super) needs_reset: bool,
}

/// One queue's registers and rings.
#[derive(Debug, Default)]
struct Queue {
    size: u16,
    vector: u16,
    addresses: [u64; 3],
    ring: Option<SplitQueueDevice<RingView>>,
}

/// A transmit buffer the device holds.
#[derive(Clone, Debug)]
struct Held {
    head: u16,
    stream: u32,
    data: Vec<(u64, u32)>,
    status: u64,
}

/// A control request the device answered.
pub(super) type Request = (u32, Vec<u8>);

/// The device.
#[derive(Debug)]
pub(super) struct Device {
    bus: Rc<Bus>,
    offered: u64,
    accepted: u64,
    device_select: u32,
    driver_select: u32,
    pub(super) status: u8,
    queue_select: u16,
    queues: [Queue; 4],
    pub(super) streams: u32,
    /// Per stream: its channels as set, whether started, prepared.
    pub(super) channels: Vec<u8>,
    pub(super) started: Vec<bool>,
    pub(super) prepared: Vec<bool>,
    held: VecDeque<Held>,
    /// Every control request, its code and bytes, in order.
    pub(super) requests: Vec<Request>,
    /// Every sample byte the backend consumed, in order.
    pub(super) played: Vec<u8>,
    pub(super) misbehave: Misbehave,
    pub(super) protocol_errors: Vec<&'static str>,
}

impl Device {
    pub(super) fn new(bus: Rc<Bus>) -> Self {
        Self {
            bus,
            offered: FEATURE_VERSION_1 | FEATURE_ACCESS_PLATFORM,
            accepted: 0,
            device_select: 0,
            driver_select: 0,
            status: 0,
            queue_select: 0,
            queues: Default::default(),
            streams: 2,
            channels: vec![2, 2],
            started: vec![false, false],
            prepared: vec![true, true],
            held: VecDeque::new(),
            requests: Vec::new(),
            played: Vec::new(),
            misbehave: Misbehave::default(),
            protocol_errors: Vec::new(),
        }
    }

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
            NUM_QUEUES => 4,
            DEVICE_STATUS => {
                u32::from(self.status)
                    | if self.misbehave.needs_reset && self.status != 0 {
                        u32::from(STATUS_DEVICE_NEEDS_RESET)
                    } else {
                        0
                    }
            }
            QUEUE_SELECT => u32::from(self.queue_select),
            QUEUE_SIZE => self.selected().map_or(0, |queue| u32::from(queue.size)),
            QUEUE_MSIX_VECTOR => self.selected().map_or(0, |queue| u32::from(queue.vector)),
            QUEUE_ENABLE => self
                .selected()
                .map_or(0, |queue| u32::from(queue.ring.is_some())),
            QUEUE_NOTIFY_OFF => u32::from(self.queue_select) + 5,
            _ => 0,
        }
    }

    fn reset(&mut self) {
        self.status = 0;
        for queue in &mut self.queues {
            *queue = Queue {
                size: 64,
                vector: NO_VECTOR,
                ..Queue::default()
            };
        }
        self.held.clear();
    }

    fn set_register(&mut self, offset: u32, value: u32) {
        let index = usize::from(self.queue_select);
        match offset {
            DEVICE_FEATURE_SELECT => self.device_select = value,
            DRIVER_FEATURE_SELECT => self.driver_select = value,
            DRIVER_FEATURE => match self.driver_select {
                0 => self.accepted = (self.accepted & !0xFFFF_FFFF) | u64::from(value),
                _ => self.accepted = (self.accepted & 0xFFFF_FFFF) | u64::from(value) << 32,
            },
            DEVICE_STATUS if value == 0 => self.reset(),
            DEVICE_STATUS => self.status = value as u8,
            QUEUE_SELECT => self.queue_select = value as u16,
            QUEUE_SIZE => {
                if let Some(queue) = self.queues.get_mut(index) {
                    queue.size = value as u16;
                }
            }
            QUEUE_MSIX_VECTOR => {
                if let Some(queue) = self.queues.get_mut(index) {
                    queue.vector = value as u16;
                }
            }
            QUEUE_ENABLE => self.enable(),
            _ => {
                let Some(queue) = self.queues.get_mut(index) else {
                    return;
                };
                for (slot, base) in [QUEUE_DESC, QUEUE_DRIVER, QUEUE_DEVICE]
                    .into_iter()
                    .enumerate()
                {
                    if offset == base {
                        queue.addresses[slot] =
                            (queue.addresses[slot] & !0xFFFF_FFFF) | u64::from(value);
                    } else if offset == base + 4 {
                        queue.addresses[slot] =
                            (queue.addresses[slot] & 0xFFFF_FFFF) | u64::from(value) << 32;
                    }
                }
            }
        }
    }

    fn enable(&mut self) {
        if self.status & STATUS_FEATURES_OK == 0 {
            self.protocol_errors
                .push("a queue enabled before FEATURES_OK");
        }
        let bus = Rc::clone(&self.bus);
        let Some(queue) = self.queues.get_mut(usize::from(self.queue_select)) else {
            return;
        };
        let layout = Layout::for_size(queue.size).expect("a valid size");
        queue.ring = Some(SplitQueueDevice::new(
            layout,
            RingView {
                bus,
                layout,
                descriptors: queue.addresses[0],
                driver: queue.addresses[1],
                device: queue.addresses[2],
            },
        ));
    }

    fn chain(&mut self, queue: usize, head: u16) -> Vec<Descriptor> {
        let mut descriptors = [Descriptor {
            address: 0,
            len: 0,
            flags: 0,
            next: 0,
        }; 8];
        let ring = self.queues[queue].ring.as_mut().expect("a ring");
        match ring.read_chain(head, &mut descriptors) {
            Ok(count) => descriptors[..count].to_vec(),
            Err(_) => {
                self.protocol_errors.push("an unreadable chain");
                Vec::new()
            }
        }
    }

    fn read_bytes(&self, address: u64, len: u32) -> Vec<u8> {
        (0..u64::from(len))
            .map(|offset| self.bus.read(address + offset).unwrap_or(0))
            .collect()
    }

    fn write_bytes(&self, address: u64, bytes: &[u8]) {
        for (offset, byte) in bytes.iter().enumerate() {
            let _ = self.bus.write(address + offset as u64, *byte);
        }
    }

    /// The doorbell of `queue`.
    fn ring(&mut self, queue: u16) {
        if self.status & STATUS_DRIVER_OK == 0 {
            self.protocol_errors.push("a doorbell before DRIVER_OK");
        }
        match queue {
            0 => self.control(),
            2 => self.transmit(),
            _ => self
                .protocol_errors
                .push("a doorbell for a queue nothing uses"),
        }
    }

    fn heads(&mut self, queue: usize) -> Vec<u16> {
        let mut heads = Vec::new();
        let Some(ring) = self.queues[queue].ring.as_mut() else {
            self.protocol_errors
                .push("a doorbell for a queue not enabled");
            return heads;
        };
        loop {
            match ring.next_chain() {
                Ok(Some(head)) => heads.push(head),
                Ok(None) => return heads,
                Err(_) => {
                    self.protocol_errors.push("a corrupt available ring");
                    return heads;
                }
            }
        }
    }

    fn control(&mut self) {
        if self.misbehave.silent {
            return;
        }
        for head in self.heads(0) {
            let chain = self.chain(0, head);
            let [asked, answer] = chain[..] else {
                self.protocol_errors
                    .push("a control chain not of two buffers");
                continue;
            };
            if asked.is_device_writable() || !answer.is_device_writable() {
                self.protocol_errors
                    .push("a control chain the wrong way round");
            }
            let request = self.read_bytes(asked.address, asked.len);
            let code = u32::from_le_bytes(request[..4].try_into().expect("a code"));
            self.requests.push((code, request.clone()));
            let response = self.answer(code, &request);
            let len = response.len().min(answer.len as usize);
            self.write_bytes(answer.address, &response[..len]);
            let ring = self.queues[0].ring.as_mut().expect("a ring");
            ring.complete(head, len as u32).expect("completes");
        }
    }

    fn word(request: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(request[at..at + 4].try_into().expect("a word"))
    }

    fn answer(&mut self, code: u32, request: &[u8]) -> Vec<u8> {
        let ok = 0x8000_u32.to_le_bytes().to_vec();
        let bad = 0x8001_u32.to_le_bytes().to_vec();
        match code {
            0x0100 => {
                let (start, count) = (Self::word(request, 4), Self::word(request, 8));
                let mut response = ok;
                for stream in start..start + count {
                    let mut entry = [0_u8; 32];
                    // S8 U8 S16 U16 S32 U32 FLOAT, and all fourteen rates.
                    let formats: u64 = (1 << 3)
                        | (1 << 4)
                        | (1 << 5)
                        | (1 << 6)
                        | (1 << 17)
                        | (1 << 18)
                        | (1 << 19);
                    entry[8..16].copy_from_slice(&formats.to_le_bytes());
                    entry[16..24].copy_from_slice(&0x3fff_u64.to_le_bytes());
                    entry[24] = u8::from(stream >= self.streams / 2 + self.streams % 2);
                    entry[25] = 1;
                    entry[26] = self.channels.get(stream as usize).copied().unwrap_or(2);
                    response.extend_from_slice(&entry);
                }
                response
            }
            0x0101 => {
                if self.misbehave.refuse_params {
                    return 0x8002_u32.to_le_bytes().to_vec();
                }
                let stream = Self::word(request, 4) as usize;
                if let Some(channels) = self.channels.get_mut(stream) {
                    *channels = request[20];
                }
                ok
            }
            0x0102 => {
                let stream = Self::word(request, 4) as usize;
                if let Some(prepared) = self.prepared.get_mut(stream) {
                    *prepared = true;
                }
                ok
            }
            0x0103 => {
                let stream = Self::word(request, 4);
                // Every buffer it holds comes back before the answer.
                let (flushed, kept): (Vec<Held>, Vec<Held>) =
                    self.held.drain(..).partition(|held| held.stream == stream);
                self.held = kept.into();
                for held in flushed {
                    self.finish(&held);
                }
                if let Some(prepared) = self.prepared.get_mut(stream as usize) {
                    *prepared = false;
                }
                ok
            }
            0x0104 | 0x0105 => {
                let stream = Self::word(request, 4) as usize;
                if let Some(started) = self.started.get_mut(stream) {
                    *started = code == 0x0104;
                }
                ok
            }
            _ => bad,
        }
    }

    fn transmit(&mut self) {
        for head in self.heads(2) {
            let chain = self.chain(2, head);
            if chain.len() < 3 {
                self.protocol_errors.push("a transmit chain without data");
                continue;
            }
            let header = chain[0];
            let status = chain[chain.len() - 1];
            if header.len != 4 || header.is_device_writable() {
                self.protocol_errors
                    .push("a transmit header that is not four readable bytes");
            }
            if status.len != 8 || !status.is_device_writable() {
                self.protocol_errors
                    .push("a transmit status that is not eight writable bytes");
            }
            let data: Vec<(u64, u32)> = chain[1..chain.len() - 1]
                .iter()
                .map(|descriptor| {
                    if descriptor.is_device_writable() {
                        self.protocol_errors.push("samples the device may write");
                    }
                    (descriptor.address, descriptor.len)
                })
                .collect();
            let stream = u32::from_le_bytes(
                self.read_bytes(header.address, 4)
                    .try_into()
                    .expect("four bytes"),
            );
            self.held.push_back(Held {
                head,
                stream,
                data,
                status: status.address,
            });
        }
    }

    fn finish(&mut self, held: &Held) {
        let size: u32 = held.data.iter().map(|(_, len)| len).sum();
        let status = self.misbehave.status.unwrap_or(0x8000);
        let mut bytes = status.to_le_bytes().to_vec();
        bytes.extend_from_slice(&size.to_le_bytes());
        self.write_bytes(held.status, &bytes);
        let written = self.misbehave.written.unwrap_or(8);
        let ring = self.queues[2].ring.as_mut().expect("a ring");
        ring.complete(held.head, written).expect("completes");
    }

    /// The backend consumes up to `buffers` whole buffers of started streams:
    /// what `virtio_snd_pcm_out_cb` does as the audio clock runs.
    pub(super) fn consume(&mut self, buffers: usize) -> usize {
        let mut done = 0;
        while done < buffers {
            let next = if self.misbehave.reversed {
                self.held.pop_back()
            } else {
                self.held.pop_front()
            };
            let Some(held) = next else {
                break;
            };
            if !self
                .started
                .get(held.stream as usize)
                .copied()
                .unwrap_or(false)
            {
                self.held.push_front(held);
                break;
            }
            for (address, len) in &held.data {
                let bytes = self.read_bytes(*address, *len);
                self.played.extend_from_slice(&bytes);
            }
            self.finish(&held);
            done += 1;
        }
        done
    }

    /// Buffers the device holds.
    pub(super) fn holding(&self) -> usize {
        self.held.len()
    }

    /// The data descriptors of the buffers it holds.
    pub(super) fn held_data(&self) -> Vec<Vec<(u64, u32)>> {
        self.held.iter().map(|held| held.data.clone()).collect()
    }
}

/// The driver's handle on the device.
#[derive(Clone, Debug)]
pub(super) struct Handle {
    pub(super) device: Rc<RefCell<Device>>,
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
        16
    }
    fn config_read8(&self, offset: u32) -> u8 {
        self.config_read32(offset & !3).to_le_bytes()[(offset % 4) as usize]
    }
    fn config_read16(&self, offset: u32) -> u16 {
        u16::from_le_bytes([self.config_read8(offset), self.config_read8(offset + 1)])
    }
    fn config_read32(&self, offset: u32) -> u32 {
        match offset {
            4 => self.device.borrow().streams,
            _ => 0,
        }
    }
}

impl Transport for Handle {
    fn notify(&mut self, queue: u16, notify_off: u16) {
        if notify_off != queue + 5 {
            self.device
                .borrow_mut()
                .protocol_errors
                .push("a doorbell with another queue's offset");
        }
        self.device.borrow_mut().ring(queue);
    }
    fn queue_vector(&self, queue: u16) -> u16 {
        queue + 1
    }
    fn acknowledge_interrupt(&mut self) -> u8 {
        ISR_QUEUE
    }
    fn spin(&mut self) {}
}
