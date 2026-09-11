//! Tests for the split virtqueue.
//!
//! Three groups of property are worth the trouble here, and they are not the
//! ones a happy-path test covers.
//!
//! The first is the free-running index. `avail.idx` and `used.idx` count
//! everything ever published and wrap at 65536; only the *slot* is reduced
//! modulo the queue size. A driver that masks the index instead works
//! perfectly for 65536 requests and then quietly starts handing the device the
//! wrong slot, which on a disk queue means a read completing into another
//! request's buffer. `wraps_past_sixty_five_thousand` drives more requests than
//! that through a four-entry queue.
//!
//! The second is that a completion frees the *whole* chain. Freeing only the
//! head leaks every interior descriptor, and the queue dies of exhaustion an
//! hour into a run with no clue as to why. Every round trip here asserts the
//! free count comes back to where it started.
//!
//! The third is that the device is hostile. It can put any `u16` in the used
//! ring and move `used.idx` anywhere; none of that may panic or read out of
//! bounds, so those cases forge ring contents directly through
//! [`Shared::poke_u32`] rather than going through [`SplitQueueDevice`], which
//! is too well behaved to produce them.

extern crate std;

use core::cell::{Cell, RefCell};
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use super::*;

/// One memory access, for the tests that assert ordering.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Event {
    /// A byte was read at this offset.
    Read(usize),
    /// A byte was written at this offset.
    Write(usize),
    /// [`QueueMemory::barrier`] was called.
    Barrier,
}

/// A `Vec<u8>` standing in for the DMA buffer, shared between the driver and
/// the device the way the real thing is, and able to record what happens to it.
///
/// Recording is off by default: the wrapping tests do millions of byte accesses
/// and have no interest in any of them.
#[derive(Clone, Debug)]
struct Shared {
    /// The bytes themselves.
    bytes: Rc<RefCell<Vec<u8>>>,
    /// Accesses recorded while `logging` is set.
    log: Rc<RefCell<Vec<Event>>>,
    /// Whether to record.
    logging: Rc<Cell<bool>>,
}

impl Shared {
    /// A zeroed block of `size` bytes.
    fn new(size: usize) -> Self {
        Self {
            bytes: Rc::new(RefCell::new(vec![0_u8; size])),
            log: Rc::new(RefCell::new(Vec::new())),
            logging: Rc::new(Cell::new(false)),
        }
    }

    /// Start recording, discarding anything recorded before.
    fn start_log(&self) {
        self.log.borrow_mut().clear();
        self.logging.set(true);
    }

    /// Stop recording and return what was recorded.
    fn take_log(&self) -> Vec<Event> {
        self.logging.set(false);
        self.log.borrow().clone()
    }

    /// Note one access if recording is on.
    fn record(&self, event: Event) {
        if self.logging.get() {
            self.log.borrow_mut().push(event);
        }
    }

    /// Read a `u16` without going through the queue or the log.
    fn peek_u16(&self, offset: usize) -> u16 {
        let bytes = self.bytes.borrow();
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    }

    /// Write a `u16` behind the queue's back, as a misbehaving device would.
    fn poke_u16(&self, offset: usize, value: u16) {
        let mut bytes = self.bytes.borrow_mut();
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    /// Write a `u32` behind the queue's back, as a misbehaving device would.
    fn poke_u32(&self, offset: usize, value: u32) {
        let mut bytes = self.bytes.borrow_mut();
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// Forge a used ring entry and index without a device having earned it.
    fn forge_used(&self, layout: &Layout, slot: u16, id: u32, written: u32, used_idx: u16) {
        let entry = layout.used_entry(slot);
        self.poke_u32(entry, id);
        self.poke_u32(entry + 4, written);
        self.poke_u16(layout.used_idx(), used_idx);
    }
}

// SAFETY: the bytes are a `Vec` allocated at `Layout::total_size` for the
// layout every queue in these tests is built with, so every offset the crate
// passes is in range; `Vec<u8>`'s allocation is at least pointer-aligned and
// the tests never rely on the descriptor table's 16-byte alignment being real,
// since nothing here dereferences it as a struct. The driver and device halves
// share the handle deliberately — that is the situation the crate is written
// for — and the tests step them one at a time, so no access races.
unsafe impl QueueMemory for Shared {
    fn read_u8(&self, offset: usize) -> u8 {
        self.record(Event::Read(offset));
        self.bytes.borrow()[offset]
    }

    fn write_u8(&mut self, offset: usize, value: u8) {
        self.record(Event::Write(offset));
        self.bytes.borrow_mut()[offset] = value;
    }

    fn barrier(&self) {
        self.record(Event::Barrier);
    }
}

/// A driver and a device over one block of memory, plus a handle on the memory.
fn queue_pair(queue_size: u16) -> (Layout, Shared, SplitQueue<Shared>, SplitQueueDevice<Shared>) {
    let layout = Layout::for_size(queue_size).unwrap();
    let memory = Shared::new(layout.total_size);
    let driver = SplitQueue::new(layout, memory.clone());
    let device = SplitQueueDevice::new(layout, memory.clone());
    (layout, memory, driver, device)
}

/// Take a chain through the device and back: available ring, used ring,
/// completion.
fn round_trip(
    driver: &mut SplitQueue<Shared>,
    device: &mut SplitQueueDevice<Shared>,
    written: u32,
) -> Completion {
    let head = device.next_chain().unwrap().unwrap();
    device.complete(head, written).unwrap();
    driver.take_used().unwrap().unwrap()
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

#[test]
fn layout_places_the_three_areas_where_the_specification_says() {
    let layout = Layout::for_size(4).unwrap();
    assert_eq!(
        layout.descriptor_table, 0,
        "the descriptor table starts the block"
    );
    assert_eq!(
        layout.available_ring, 64,
        "the available ring follows 4 * 16 bytes of descriptors"
    );
    // The available ring ends at 64 + 6 + 8 = 78, which is padded to 80 for the
    // used ring's four-byte alignment.
    assert_eq!(
        layout.used_ring, 80,
        "the used ring is padded to a four-byte boundary"
    );
    assert_eq!(
        layout.total_size,
        80 + 6 + 32,
        "the used ring is flags, idx, ring and avail_event"
    );
}

#[test]
fn layout_of_a_single_descriptor_queue_needs_no_padding() {
    let layout = Layout::for_size(1).unwrap();
    assert_eq!(layout.available_ring, 16, "one descriptor is sixteen bytes");
    assert_eq!(
        layout.used_ring, 24,
        "16 + 6 + 2 is already four-byte aligned, so no padding is inserted"
    );
    assert_eq!(
        layout.total_size,
        24 + 6 + 8,
        "one used element of eight bytes"
    );
}

#[test]
fn layout_alignments_hold_for_every_legal_queue_size() {
    let mut queue_size: u16 = 1;
    loop {
        let layout = Layout::for_size(queue_size).unwrap();
        assert_eq!(
            layout.descriptor_table % 16,
            0,
            "virtio 1.2 §2.7 requires a 16-byte aligned descriptor table, size {queue_size}"
        );
        assert_eq!(
            layout.available_ring % 2,
            0,
            "virtio 1.2 §2.7 requires a 2-byte aligned available ring, size {queue_size}"
        );
        assert_eq!(
            layout.used_ring % 4,
            0,
            "virtio 1.2 §2.7 requires a 4-byte aligned used ring, size {queue_size}"
        );
        assert!(
            layout.used_ring >= layout.available_ring + 6 + queue_size as usize * 2,
            "the used ring must start past the end of the available ring, size {queue_size}"
        );
        assert!(
            layout.total_size >= layout.used_ring + 6 + queue_size as usize * 8,
            "the block must cover the whole used ring, size {queue_size}"
        );
        if queue_size == MAX_QUEUE_SIZE {
            break;
        }
        queue_size *= 2;
    }
}

#[test]
fn layout_sizes_a_realistic_queue() {
    let layout = Layout::for_size(128).unwrap();
    assert_eq!(layout.available_ring, 2048, "128 descriptors of 16 bytes");
    // The available ring is `flags`, `idx`, 128 two-byte entries and
    // `used_event`: 4 + 256 + 2 = 262 bytes, ending at 2310. The used ring is
    // four-byte aligned, so it starts at 2312.
    assert_eq!(
        layout.used_ring, 2312,
        "2048 + 262 = 2310, rounded up to the used ring's four-byte alignment"
    );
    assert_eq!(
        layout.total_size,
        layout.used_ring + 6 + 1024,
        "used ring of 128 eight-byte entries"
    );
}

#[test]
fn layout_rejects_a_zero_queue_size() {
    assert_eq!(
        Layout::for_size(0),
        Err(QueueError::BadQueueSize),
        "a queue with no descriptors cannot carry a request"
    );
}

#[test]
fn layout_rejects_a_non_power_of_two_queue_size() {
    for size in [3_u16, 5, 100, 1000, 4095] {
        assert_eq!(
            Layout::for_size(size),
            Err(QueueError::BadQueueSize),
            "slot = idx % queue_size only survives the 65536 wrap for powers of two, size {size}"
        );
    }
}

#[test]
fn layout_rejects_an_oversized_queue() {
    assert_eq!(
        Layout::for_size(MAX_QUEUE_SIZE),
        Layout::for_size(32768),
        "32768 is the largest legal queue and must be accepted"
    );
    assert!(
        Layout::for_size(MAX_QUEUE_SIZE).is_ok(),
        "the maximum queue size is legal"
    );
    assert_eq!(
        Layout::for_size(65535),
        Err(QueueError::BadQueueSize),
        "above 32768 the free-running index can no longer map onto slots"
    );
}

// ---------------------------------------------------------------------------
// The round trip
// ---------------------------------------------------------------------------

#[test]
fn a_chain_goes_out_and_comes_back() {
    let (_, _, mut driver, mut device) = queue_pair(8);
    let before = driver.free_descriptors();

    let head = driver
        .add_chain(&[Buffer::readable(0x1000, 16), Buffer::writable(0x2000, 512)])
        .unwrap();

    assert!(
        device.has_available(),
        "the device must see the chain the driver published"
    );
    assert!(!driver.has_used(), "nothing has completed yet");

    let completion = round_trip(&mut driver, &mut device, 512);

    assert_eq!(
        completion.head, head,
        "the completion names the head the driver published"
    );
    assert_eq!(
        completion.written, 512,
        "the completion carries the byte count the device gave"
    );
    assert_eq!(
        driver.free_descriptors(),
        before,
        "both descriptors of the chain must come back, not just the head"
    );
}

#[test]
fn the_available_ring_carries_the_head_index() {
    let (layout, memory, mut driver, _) = queue_pair(8);
    let head = driver.add_chain(&[Buffer::readable(0x3000, 8)]).unwrap();

    assert_eq!(
        memory.peek_u16(layout.available_entry(0)),
        head,
        "the driver publishes the head descriptor index, not the buffer address"
    );
    assert_eq!(
        memory.peek_u16(layout.available_idx()),
        1,
        "one chain has been published"
    );
}

#[test]
fn a_one_descriptor_chain_has_no_next_flag() {
    let (_, _, mut driver, _) = queue_pair(8);
    let head = driver
        .add_chain(&[Buffer::writable(0xdead_0000, 42)])
        .unwrap();

    let descriptor = driver.descriptor(head).unwrap();
    assert_eq!(
        descriptor.address, 0xdead_0000,
        "the buffer address goes in the descriptor"
    );
    assert_eq!(
        descriptor.len, 42,
        "the buffer length goes in the descriptor"
    );
    assert!(
        !descriptor.has_next(),
        "a chain of one must not claim to continue"
    );
    assert!(
        descriptor.is_device_writable(),
        "a writable buffer sets VIRTQ_DESC_F_WRITE"
    );
    assert_eq!(
        descriptor.flags, DESC_F_WRITE,
        "no other flag bit belongs on a live descriptor"
    );
}

#[test]
fn a_two_descriptor_chain_links_head_to_tail() {
    let (_, _, mut driver, _) = queue_pair(8);
    let head = driver
        .add_chain(&[Buffer::readable(0x10, 4), Buffer::writable(0x20, 8)])
        .unwrap();

    let first = driver.descriptor(head).unwrap();
    assert!(
        first.has_next(),
        "the head of a two-descriptor chain continues"
    );
    assert!(
        !first.is_device_writable(),
        "the first buffer is device-readable"
    );
    assert_eq!(
        first.flags, DESC_F_NEXT,
        "a readable head carries NEXT and nothing else"
    );

    let second = driver.descriptor(first.next).unwrap();
    assert!(!second.has_next(), "the tail ends the chain");
    assert!(
        second.is_device_writable(),
        "the second buffer is device-writable"
    );
    assert_eq!(
        second.flags, DESC_F_WRITE,
        "a writable tail carries WRITE and nothing else"
    );
    assert_eq!(second.address, 0x20, "the tail describes the second buffer");
}

#[test]
fn a_long_chain_sets_next_on_all_but_the_last() {
    let (_, _, mut driver, _) = queue_pair(16);
    let buffers = [
        Buffer::readable(0x1000, 16),
        Buffer::readable(0x2000, 32),
        Buffer::writable(0x3000, 64),
        Buffer::writable(0x4000, 128),
        Buffer::writable(0x5000, 1),
    ];
    let head = driver.add_chain(&buffers).unwrap();

    let mut index = head;
    for (position, buffer) in buffers.iter().enumerate() {
        let descriptor = driver.descriptor(index).unwrap();
        assert_eq!(
            descriptor.address, buffer.address,
            "descriptor {position} has its own address"
        );
        assert_eq!(
            descriptor.len, buffer.len,
            "descriptor {position} has its own length"
        );
        assert_eq!(
            descriptor.is_device_writable(),
            buffer.device_writable,
            "descriptor {position} keeps its direction"
        );
        let last = position + 1 == buffers.len();
        assert_eq!(
            descriptor.has_next(),
            !last,
            "only the last descriptor clears NEXT, at {position}"
        );
        index = descriptor.next;
    }
}

#[test]
fn the_device_reads_the_chain_the_driver_wrote() {
    let (_, _, mut driver, mut device) = queue_pair(8);
    let head = driver
        .add_chain(&[
            Buffer::readable(0xaaaa, 16),
            Buffer::writable(0xbbbb, 512),
            Buffer::writable(0xcccc, 1),
        ])
        .unwrap();

    assert_eq!(
        device.next_chain().unwrap(),
        Some(head),
        "the device pops the head the driver sent"
    );

    let mut chain = [Descriptor {
        address: 0,
        len: 0,
        flags: 0,
        next: 0,
    }; 8];
    let count = device.read_chain(head, &mut chain).unwrap();

    assert_eq!(count, 3, "the device sees all three buffers of the chain");
    assert_eq!(
        chain[0].address, 0xaaaa,
        "the first buffer is the request header"
    );
    assert_eq!(chain[1].len, 512, "the second buffer is the payload");
    assert!(
        !chain[0].is_device_writable(),
        "the header is read by the device"
    );
    assert!(
        chain[2].is_device_writable(),
        "the status byte is written by the device"
    );
}

#[test]
fn several_chains_complete_in_the_order_the_device_chose() {
    let (_, _, mut driver, mut device) = queue_pair(8);
    let first = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();
    let second = driver.add_chain(&[Buffer::readable(0x2, 2)]).unwrap();
    assert_ne!(first, second, "two live chains must not share a descriptor");

    assert_eq!(
        device.next_chain().unwrap(),
        Some(first),
        "the available ring is first in first out"
    );
    assert_eq!(
        device.next_chain().unwrap(),
        Some(second),
        "and then the second chain"
    );
    assert_eq!(
        device.next_chain().unwrap(),
        None,
        "there is no third chain"
    );

    // A device may complete out of order, and the used ring says which is
    // which precisely so that it can.
    device.complete(second, 2).unwrap();
    device.complete(first, 1).unwrap();

    assert_eq!(
        driver.take_used().unwrap().unwrap().head,
        second,
        "completions arrive as sent"
    );
    assert_eq!(
        driver.take_used().unwrap().unwrap().head,
        first,
        "and then the first chain"
    );
    assert_eq!(
        driver.free_descriptors(),
        8,
        "both chains are back on the free list"
    );
}

#[test]
fn take_used_reports_nothing_when_the_device_is_idle() {
    let (_, _, mut driver, _) = queue_pair(8);
    assert!(!driver.has_used(), "a fresh queue has no completions");
    assert_eq!(
        driver.take_used().unwrap(),
        None,
        "and taking one yields nothing"
    );

    let _ = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();
    assert!(
        !driver.has_used(),
        "publishing a chain does not complete it"
    );
    assert_eq!(
        driver.take_used().unwrap(),
        None,
        "the device has not answered yet"
    );
}

// ---------------------------------------------------------------------------
// The free-running index
// ---------------------------------------------------------------------------

#[test]
fn the_available_index_counts_chains_not_slots() {
    let (layout, memory, mut driver, mut device) = queue_pair(4);
    for _ in 0..6 {
        let _ = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();
        let _ = round_trip(&mut driver, &mut device, 1);
    }

    assert_eq!(
        driver.available_index(),
        6,
        "avail.idx counts every chain ever published; masking it by the queue size is the bug"
    );
    assert_eq!(
        memory.peek_u16(layout.available_idx()),
        6,
        "and that is what lands in memory"
    );
    assert_eq!(
        device.used_index(),
        6,
        "used.idx counts the same way on the device side"
    );
}

#[test]
fn wraps_past_sixty_five_thousand() {
    let (layout, memory, mut driver, mut device) = queue_pair(4);

    // More than 65536 requests, so the free-running indices wrap and the slot
    // arithmetic has to keep working across the discontinuity.
    let requests: u32 = 70_000;
    for request in 0..requests {
        let head = driver
            .add_chain(&[Buffer::readable(u64::from(request), 4)])
            .unwrap();
        let completion = round_trip(&mut driver, &mut device, 4);
        assert_eq!(
            completion.head, head,
            "request {request} completed as the wrong chain"
        );
        assert_eq!(
            completion.written, 4,
            "request {request} lost its byte count"
        );
    }

    let expected = (requests % 65536) as u16;
    assert_eq!(
        driver.available_index(),
        expected,
        "avail.idx wraps at 65536 and nowhere else"
    );
    assert_eq!(
        memory.peek_u16(layout.available_idx()),
        expected,
        "and so does the copy in memory"
    );
    assert_eq!(device.used_index(), expected, "used.idx wraps the same way");
    assert_eq!(
        driver.free_descriptors(),
        4,
        "70000 requests must leak no descriptor"
    );
}

#[test]
fn wraps_with_chains_in_flight() {
    let (_, _, mut driver, mut device) = queue_pair(4);

    // Two outstanding at a time, so the slot the driver reads is never the slot
    // the device just wrote, which is where an off-by-one in the wrap hides.
    for round in 0..33_000_u32 {
        let first = driver
            .add_chain(&[Buffer::readable(u64::from(round), 1)])
            .unwrap();
        let second = driver
            .add_chain(&[Buffer::writable(u64::from(round), 2)])
            .unwrap();

        assert_eq!(
            device.next_chain().unwrap(),
            Some(first),
            "round {round} lost the first chain"
        );
        assert_eq!(
            device.next_chain().unwrap(),
            Some(second),
            "round {round} lost the second chain"
        );
        device.complete(first, 1).unwrap();
        device.complete(second, 2).unwrap();

        assert_eq!(
            driver.take_used().unwrap().unwrap().head,
            first,
            "round {round} first completion"
        );
        assert_eq!(
            driver.take_used().unwrap().unwrap().head,
            second,
            "round {round} second completion"
        );
    }

    assert_eq!(
        driver.available_index(),
        (66_000_u32 % 65536) as u16,
        "66000 chains, wrapped"
    );
    assert_eq!(
        driver.free_descriptors(),
        4,
        "nothing leaked across the wrap"
    );
}

// ---------------------------------------------------------------------------
// Exhaustion and rejection
// ---------------------------------------------------------------------------

#[test]
fn a_full_queue_reports_exhaustion_and_recovers() {
    let (_, _, mut driver, mut device) = queue_pair(4);

    let mut heads = Vec::new();
    for _ in 0..4 {
        heads.push(driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap());
    }
    assert_eq!(
        driver.free_descriptors(),
        0,
        "four single-descriptor chains use all four"
    );

    assert_eq!(
        driver.add_chain(&[Buffer::readable(0x2, 1)]),
        Err(QueueError::OutOfDescriptors),
        "a full queue must refuse the chain rather than reuse a live descriptor"
    );
    assert_eq!(
        driver.available_index(),
        4,
        "a refused chain must not be published"
    );

    let head = device.next_chain().unwrap().unwrap();
    device.complete(head, 1).unwrap();
    let _ = driver.take_used().unwrap().unwrap();

    assert_eq!(
        driver.free_descriptors(),
        1,
        "completing one chain frees exactly one descriptor"
    );
    assert!(
        driver.add_chain(&[Buffer::readable(0x3, 1)]).is_ok(),
        "and the queue accepts again"
    );
}

#[test]
fn a_partial_chain_is_refused_before_anything_is_written() {
    let (_, _, mut driver, _) = queue_pair(4);
    let _ = driver
        .add_chain(&[Buffer::readable(0x1, 1), Buffer::readable(0x2, 2)])
        .unwrap();
    assert_eq!(
        driver.free_descriptors(),
        2,
        "two of four descriptors are in flight"
    );

    assert_eq!(
        driver.add_chain(&[Buffer::readable(0x3, 3); 3]),
        Err(QueueError::OutOfDescriptors),
        "a chain longer than the free list must be refused whole"
    );
    assert_eq!(
        driver.free_descriptors(),
        2,
        "a refused chain must not consume the descriptors it could have had"
    );
    assert_eq!(
        driver.available_index(),
        1,
        "and must not appear in the available ring"
    );
}

#[test]
fn a_chain_longer_than_the_queue_is_refused_outright() {
    let (_, _, mut driver, _) = queue_pair(4);
    assert_eq!(
        driver.add_chain(&[Buffer::readable(0x1, 1); 5]),
        Err(QueueError::ChainTooLong),
        "five buffers can never fit a four-descriptor queue, however long the caller waits"
    );
}

#[test]
fn an_empty_chain_is_refused() {
    let (_, _, mut driver, _) = queue_pair(4);
    assert_eq!(
        driver.add_chain(&[]),
        Err(QueueError::EmptyChain),
        "there is no head index to publish for a chain of nothing"
    );
    assert_eq!(driver.free_descriptors(), 4, "and nothing is consumed");
}

#[test]
fn a_chain_exactly_the_size_of_the_queue_fits() {
    let (_, _, mut driver, mut device) = queue_pair(4);
    let head = driver.add_chain(&[Buffer::writable(0x1, 1); 4]).unwrap();
    assert_eq!(
        driver.free_descriptors(),
        0,
        "the chain uses every descriptor"
    );

    let completion = round_trip(&mut driver, &mut device, 4);
    assert_eq!(
        completion.head, head,
        "the whole-queue chain completes normally"
    );
    assert_eq!(
        driver.free_descriptors(),
        4,
        "and gives every descriptor back"
    );
}

// ---------------------------------------------------------------------------
// A hostile device
// ---------------------------------------------------------------------------

#[test]
fn a_used_entry_naming_a_descriptor_out_of_range_is_an_error() {
    let (layout, memory, mut driver, _) = queue_pair(128);
    let _ = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();

    memory.forge_used(&layout, 0, 60_000, 4, 1);

    assert_eq!(
        driver.take_used(),
        Err(QueueError::DescriptorOutOfRange),
        "descriptor 60000 does not exist in a 128-entry queue and must not be indexed"
    );
    assert_eq!(
        driver.free_descriptors(),
        127,
        "a rejected completion frees nothing"
    );
}

#[test]
fn a_used_entry_naming_a_free_descriptor_is_an_error() {
    let (layout, memory, mut driver, _) = queue_pair(8);
    let _ = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();

    // Descriptor 7 is still on the free list; a device claiming to have
    // finished it is either confused or trying to make the driver free the same
    // descriptor twice.
    memory.forge_used(&layout, 0, 7, 4, 1);

    assert_eq!(
        driver.take_used(),
        Err(QueueError::NotAChainHead),
        "a descriptor on the free list was never handed to the device"
    );
    assert_eq!(
        driver.free_descriptors(),
        7,
        "and it is not freed a second time"
    );
}

#[test]
fn a_used_entry_naming_a_chain_interior_is_an_error() {
    let (layout, memory, mut driver, _) = queue_pair(8);
    let head = driver
        .add_chain(&[
            Buffer::readable(0x1, 1),
            Buffer::readable(0x2, 2),
            Buffer::readable(0x3, 3),
        ])
        .unwrap();
    let interior = driver.descriptor(head).unwrap().next;

    memory.forge_used(&layout, 0, u32::from(interior), 4, 1);

    assert_eq!(
        driver.take_used(),
        Err(QueueError::NotAChainHead),
        "only the head the driver published names a request"
    );
    assert_eq!(
        driver.free_descriptors(),
        5,
        "the live chain keeps all three descriptors"
    );
}

#[test]
fn a_used_entry_for_an_already_completed_chain_is_an_error() {
    let (layout, memory, mut driver, mut device) = queue_pair(8);
    let head = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();
    let _ = round_trip(&mut driver, &mut device, 1);

    // The same head again: a double completion would free the descriptor twice
    // and corrupt the free list into a cycle.
    memory.forge_used(&layout, 1, u32::from(head), 1, 2);

    assert_eq!(
        driver.take_used(),
        Err(QueueError::NotAChainHead),
        "a chain already completed must not be freed again"
    );
    assert_eq!(
        driver.free_descriptors(),
        8,
        "the free list is unchanged by the rejection"
    );
}

#[test]
fn a_used_index_that_jumps_backwards_is_an_error() {
    let (layout, memory, mut driver, mut device) = queue_pair(8);
    let _ = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();
    let head = device.next_chain().unwrap().unwrap();
    device.complete(head, 1).unwrap();
    let _ = driver.take_used().unwrap().unwrap();

    memory.poke_u16(layout.used_idx(), 0xfff0);

    assert_eq!(
        driver.take_used(),
        Err(QueueError::UsedIndexJumped),
        "a backwards used.idx means the driver can no longer tell what it has seen"
    );
}

#[test]
fn a_used_index_that_jumps_absurdly_far_is_an_error() {
    let (layout, memory, mut driver, _) = queue_pair(8);
    let _ = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();

    memory.poke_u16(layout.used_idx(), 30_000);

    assert_eq!(
        driver.take_used(),
        Err(QueueError::UsedIndexJumped),
        "more than a queue's worth of outstanding completions cannot exist"
    );
}

#[test]
fn a_used_index_exactly_a_queue_ahead_is_still_accepted() {
    let (layout, memory, mut driver, _) = queue_pair(4);
    let head = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();

    // Four is the most the queue can hold, so it is legal, not a jump; the
    // check must be `>` and not `>=`.
    memory.forge_used(&layout, 0, u32::from(head), 7, 4);

    assert_eq!(
        driver.take_used().unwrap().map(|done| done.written),
        Some(7),
        "a full queue's worth of completions is the boundary case, not an error"
    );
}

#[test]
fn a_descriptor_cycle_is_bounded_rather_than_followed() {
    let (layout, memory, mut driver, _) = queue_pair(8);
    let head = driver
        .add_chain(&[Buffer::readable(0x1, 1), Buffer::readable(0x2, 2)])
        .unwrap();
    let tail = driver.descriptor(head).unwrap().next;

    // The tail already points back at the head as driver-private bookkeeping;
    // setting NEXT on it turns that into a loop the device could have written.
    memory.poke_u16(layout.descriptor(tail) + DESC_FLAGS, DESC_F_NEXT);
    memory.forge_used(&layout, 0, u32::from(head), 4, 1);

    assert_eq!(
        driver.take_used(),
        Err(QueueError::ChainCycle),
        "a chain walk must be bounded by the queue size rather than trusting NEXT"
    );
}

#[test]
fn a_descriptor_chain_leaving_the_table_is_an_error() {
    let (layout, memory, mut driver, _) = queue_pair(8);
    let head = driver
        .add_chain(&[Buffer::readable(0x1, 1), Buffer::readable(0x2, 2)])
        .unwrap();

    memory.poke_u16(layout.descriptor(head) + DESC_NEXT, 40_000);
    memory.forge_used(&layout, 0, u32::from(head), 4, 1);

    assert_eq!(
        driver.take_used(),
        Err(QueueError::DescriptorOutOfRange),
        "a next link past the end of the table must be caught before it is followed"
    );
}

#[test]
fn the_device_rejects_an_available_entry_out_of_range() {
    let (layout, memory, mut driver, mut device) = queue_pair(8);
    let _ = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();

    // The driver side of the ring is the untrusted one when the device is the
    // Ferrix kernel and the driver is a userspace process.
    memory.poke_u16(layout.available_entry(0), 9);

    assert_eq!(
        device.next_chain(),
        Err(QueueError::DescriptorOutOfRange),
        "descriptor 9 does not exist in an eight-entry queue"
    );
}

#[test]
fn the_device_rejects_an_available_index_that_jumps() {
    let (layout, memory, _driver, mut device) = queue_pair(8);
    memory.poke_u16(layout.available_idx(), 4_000);

    assert_eq!(
        device.next_chain(),
        Err(QueueError::AvailableIndexJumped),
        "a driver cannot have more chains outstanding than the queue holds"
    );
}

#[test]
fn the_device_bounds_a_chain_walk() {
    let (layout, memory, mut driver, device) = queue_pair(8);
    let head = driver
        .add_chain(&[Buffer::readable(0x1, 1), Buffer::readable(0x2, 2)])
        .unwrap();
    let tail = driver.descriptor(head).unwrap().next;
    memory.poke_u16(layout.descriptor(tail) + DESC_FLAGS, DESC_F_NEXT);

    let mut chain = [Descriptor {
        address: 0,
        len: 0,
        flags: 0,
        next: 0,
    }; 8];
    assert_eq!(
        device.read_chain(head, &mut chain),
        Err(QueueError::ChainCycle),
        "the device must not follow a loop the driver wrote into the table"
    );
}

#[test]
fn the_device_refuses_to_overrun_the_callers_slice() {
    let (_, _, mut driver, device) = queue_pair(8);
    let head = driver
        .add_chain(&[Buffer::readable(0x1, 1), Buffer::readable(0x2, 2)])
        .unwrap();

    let mut chain = [Descriptor {
        address: 0,
        len: 0,
        flags: 0,
        next: 0,
    }; 1];
    assert_eq!(
        device.read_chain(head, &mut chain),
        Err(QueueError::OutputTooSmall),
        "a two-descriptor chain does not fit a one-element slice"
    );
}

#[test]
fn the_device_refuses_to_complete_a_descriptor_it_does_not_have() {
    let (_, _, _driver, mut device) = queue_pair(8);
    assert_eq!(
        device.complete(8, 4),
        Err(QueueError::DescriptorOutOfRange),
        "descriptor 8 is one past the end of an eight-entry queue"
    );
    assert_eq!(device.used_index(), 0, "and nothing is published for it");
}

#[test]
fn reading_a_descriptor_out_of_range_is_an_error() {
    let (_, _, driver, _) = queue_pair(8);
    assert_eq!(
        driver.descriptor(8),
        Err(QueueError::DescriptorOutOfRange),
        "the eighth index of an eight-entry queue does not exist"
    );
}

#[test]
fn a_rejected_completion_leaves_the_queue_usable() {
    let (layout, memory, mut driver, device) = queue_pair(8);
    let live = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();
    memory.forge_used(&layout, 0, 60_000, 4, 1);

    assert!(driver.take_used().is_err(), "the forged entry is rejected");

    // The driver did not consume the bad entry, so a reset-and-retry is the
    // caller's decision; meanwhile the honest completion still works once the
    // ring says something sensible again.
    memory.forge_used(&layout, 0, u32::from(live), 11, 1);
    let completion = driver.take_used().unwrap().unwrap();

    assert_eq!(
        completion.head, live,
        "the real chain still completes after the rejection"
    );
    assert_eq!(
        driver.free_descriptors(),
        8,
        "and its descriptor comes back"
    );
    assert_eq!(
        device.used_index(),
        0,
        "the device never published any of this"
    );
}

// ---------------------------------------------------------------------------
// Interrupt and notification suppression
// ---------------------------------------------------------------------------

#[test]
fn interrupt_suppression_round_trips_through_the_available_flags() {
    let (layout, memory, mut driver, device) = queue_pair(8);
    assert!(
        !driver.interrupts_suppressed(),
        "a fresh queue wants its interrupts"
    );
    assert!(
        device.driver_wants_interrupt(),
        "and the device can see that"
    );

    driver.set_interrupts_suppressed(true);
    assert!(
        driver.interrupts_suppressed(),
        "the driver reads back what it wrote"
    );
    assert_eq!(
        memory.peek_u16(layout.available_flags()),
        AVAIL_F_NO_INTERRUPT,
        "VIRTQ_AVAIL_F_NO_INTERRUPT is bit zero of avail.flags"
    );
    assert!(
        !device.driver_wants_interrupt(),
        "the device sees the suppression"
    );

    driver.set_interrupts_suppressed(false);
    assert!(!driver.interrupts_suppressed(), "and the flag clears again");
    assert!(device.driver_wants_interrupt(), "which the device sees too");
}

#[test]
fn notification_suppression_round_trips_through_the_used_flags() {
    let (layout, memory, driver, mut device) = queue_pair(8);
    assert!(
        driver.device_wants_notification(),
        "a fresh queue wants to be notified"
    );

    device.set_notifications_suppressed(true);
    assert!(
        device.notifications_suppressed(),
        "the device reads back what it wrote"
    );
    assert_eq!(
        memory.peek_u16(layout.used_flags()),
        USED_F_NO_NOTIFY,
        "VIRTQ_USED_F_NO_NOTIFY is bit zero of used.flags"
    );
    assert!(
        !driver.device_wants_notification(),
        "and the driver may skip the notification"
    );

    device.set_notifications_suppressed(false);
    assert!(
        driver.device_wants_notification(),
        "clearing it asks for notifications again"
    );
}

#[test]
fn the_event_index_fields_round_trip() {
    let (_, _, mut driver, mut device) = queue_pair(8);
    driver.set_used_event(0x1234);
    device.set_avail_event(0x5678);

    assert_eq!(
        device.used_event(),
        0x1234,
        "the device reads the driver's used_event"
    );
    assert_eq!(
        driver.avail_event(),
        0x5678,
        "and the driver reads the device's avail_event"
    );
}

#[test]
fn suppression_does_not_disturb_the_rings() {
    let (_, _, mut driver, mut device) = queue_pair(8);
    driver.set_interrupts_suppressed(true);
    device.set_notifications_suppressed(true);
    driver.set_used_event(0xffff);
    device.set_avail_event(0xffff);

    let head = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();
    let completion = round_trip(&mut driver, &mut device, 1);
    assert_eq!(
        completion.head, head,
        "the flags live outside the ring entries and must not alias"
    );
    assert_eq!(
        driver.free_descriptors(),
        8,
        "and the chain still comes back"
    );
}

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------

/// Position of the first [`Event::Barrier`] in `events`.
fn first_barrier(events: &[Event]) -> usize {
    events
        .iter()
        .position(|event| *event == Event::Barrier)
        .expect("a barrier must have been run")
}

/// Position of the last event touching any offset in `range`.
fn last_touch(events: &[Event], range: core::ops::Range<usize>) -> usize {
    events
        .iter()
        .rposition(|event| match *event {
            Event::Read(offset) | Event::Write(offset) => range.contains(&offset),
            Event::Barrier => false,
        })
        .expect("the range must have been touched")
}

/// Position of the first event touching any offset in `range`.
fn first_touch(events: &[Event], range: core::ops::Range<usize>) -> usize {
    events
        .iter()
        .position(|event| match *event {
            Event::Read(offset) | Event::Write(offset) => range.contains(&offset),
            Event::Barrier => false,
        })
        .expect("the range must have been touched")
}

#[test]
fn the_driver_publishes_descriptors_before_the_available_index() {
    let (layout, memory, mut driver, _) = queue_pair(8);

    memory.start_log();
    let _ = driver
        .add_chain(&[Buffer::readable(0x1000, 16), Buffer::writable(0x2000, 32)])
        .unwrap();
    let events = memory.take_log();

    let barrier = first_barrier(&events);
    let descriptors = last_touch(&events, layout.descriptor_table..layout.available_ring);
    let ring_entry = last_touch(
        &events,
        layout.available_entry(0)..layout.available_entry(0) + 2,
    );
    let index = first_touch(&events, layout.available_idx()..layout.available_idx() + 2);

    assert!(
        descriptors < barrier,
        "the descriptors must be written before the barrier, or the device can read a half-built chain"
    );
    assert!(
        ring_entry < barrier,
        "so must the available ring entry that points at them"
    );
    assert!(
        barrier < index,
        "avail.idx is what invites the device to look, so it must land after the barrier"
    );
}

#[test]
fn the_driver_reads_the_used_index_before_the_entry_it_points_at() {
    let (layout, memory, mut driver, mut device) = queue_pair(8);
    let head = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();
    let taken = device.next_chain().unwrap().unwrap();
    device.complete(taken, 1).unwrap();

    memory.start_log();
    let completion = driver.take_used().unwrap().unwrap();
    let events = memory.take_log();

    assert_eq!(
        completion.head, head,
        "the completion is the one the device wrote"
    );
    let barrier = first_barrier(&events);
    let index = first_touch(&events, layout.used_idx()..layout.used_idx() + 2);
    let entry = first_touch(&events, layout.used_entry(0)..layout.used_entry(0) + 8);

    assert!(
        index < barrier,
        "used.idx is read first, since it is what says the entry is ready"
    );
    assert!(
        barrier < entry,
        "the entry is read after the barrier, or the driver can see last lap's completion"
    );
}

#[test]
fn the_device_writes_the_used_entry_before_the_used_index() {
    let (layout, memory, mut driver, mut device) = queue_pair(8);
    let head = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();
    let taken = device.next_chain().unwrap().unwrap();

    memory.start_log();
    device.complete(taken, 512).unwrap();
    let events = memory.take_log();

    assert_eq!(
        taken, head,
        "the device took the chain the driver published"
    );
    let barrier = first_barrier(&events);
    let entry = last_touch(&events, layout.used_entry(0)..layout.used_entry(0) + 8);
    let index = first_touch(&events, layout.used_idx()..layout.used_idx() + 2);

    assert!(
        entry < barrier,
        "the completion is written before the barrier"
    );
    assert!(
        barrier < index,
        "and used.idx only moves after it, or the driver reads a torn entry"
    );
}

#[test]
fn the_device_reads_the_available_index_before_the_entry_it_points_at() {
    let (layout, memory, mut driver, mut device) = queue_pair(8);
    let _ = driver.add_chain(&[Buffer::readable(0x1, 1)]).unwrap();

    memory.start_log();
    let _ = device.next_chain().unwrap().unwrap();
    let events = memory.take_log();

    let barrier = first_barrier(&events);
    let index = first_touch(&events, layout.available_idx()..layout.available_idx() + 2);
    let entry = first_touch(
        &events,
        layout.available_entry(0)..layout.available_entry(0) + 2,
    );

    assert!(index < barrier, "avail.idx is read first");
    assert!(barrier < entry, "and the ring entry only after the barrier");
}

// ---------------------------------------------------------------------------
// Memory access helpers
// ---------------------------------------------------------------------------

#[test]
fn the_wide_accessors_are_little_endian() {
    let mut memory = Shared::new(32);
    memory.write_u64(0, 0x0123_4567_89ab_cdef);
    memory.write_u32(8, 0xdead_beef);
    memory.write_u16(12, 0xf00d);

    assert_eq!(
        memory.read_u64(0),
        0x0123_4567_89ab_cdef,
        "a u64 round trips"
    );
    assert_eq!(memory.read_u32(8), 0xdead_beef, "a u32 round trips");
    assert_eq!(memory.read_u16(12), 0xf00d, "a u16 round trips");

    assert_eq!(
        memory.read_u8(0),
        0xef,
        "virtio 1.x is little-endian on the wire, whatever the host"
    );
    assert_eq!(
        memory.read_u8(7),
        0x01,
        "so the most significant byte is last"
    );
    assert_eq!(memory.read_u8(8), 0xef, "and the same holds for a u32");
}

#[test]
fn a_freed_descriptor_keeps_no_address() {
    let (_, _, mut driver, mut device) = queue_pair(8);
    let head = driver
        .add_chain(&[Buffer::writable(0xdead_beef, 4096)])
        .unwrap();
    let _ = round_trip(&mut driver, &mut device, 4096);

    let descriptor = driver.descriptor(head).unwrap();
    assert_eq!(
        descriptor.address, 0,
        "a stale address in a free descriptor is one bug from a DMA"
    );
    assert_eq!(descriptor.len, 0, "and so is a stale length");
    assert!(!descriptor.has_next(), "a free descriptor is in no chain");
}

#[test]
fn the_queues_format_without_their_memory() {
    let (_, _, driver, device) = queue_pair(8);
    let rendered = std::format!("{driver:?} {device:?}");
    assert!(
        rendered.contains("SplitQueue"),
        "the driver names itself in a debug dump"
    );
    assert!(
        rendered.contains("SplitQueueDevice"),
        "and so does the device"
    );
    assert!(
        rendered.contains("queue_size: 8"),
        "with the queue size, which is what a dump is for"
    );
}
