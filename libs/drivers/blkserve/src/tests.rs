//! Host tests for the serve loop: the ring's own kernel side on one end, a
//! fake device on the other, and the loop between them stepped by hand.

extern crate std;

use std::cell::RefCell;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use ferrix_blkring::bell::{BELL_COMPLETE, Wait};
use ferrix_blkring::geometry::{Device, DeviceFlags};
use ferrix_blkring::kernel::{KernelSide, Slot};
use ferrix_blkring::layout::{RingLayout, Status as RingStatus, Submission, header};
use ferrix_blkring::{DriverSide, RingMemory};
use ferrix_virtio_blk::{
    Accepted, Completion, DeviceError, Drained, Op, Request, Status, SubmitError,
};

use super::{Disk, Fault, Serve};

const ENTRIES: u32 = 8;
const RING_BYTES: u64 = 4096;
const BLOCK: u32 = 4096;
const CAPACITY_BLOCKS: u64 = 1024;
const MAX_SECTORS: u32 = 8;
const DATA_BYTES: u64 = 64 * 1024;

/// One ring VMO, seen from both sides.
#[derive(Clone)]
struct Mem(Rc<RefCell<Vec<u8>>>);

impl Mem {
    fn new() -> Mem {
        Mem(Rc::new(RefCell::new(vec![0; RING_BYTES as usize])))
    }
}

impl RingMemory for Mem {
    fn read_u8(&self, offset: usize) -> u8 {
        self.0.borrow()[offset]
    }

    fn write_u8(&mut self, offset: usize, value: u8) {
        self.0.borrow_mut()[offset] = value;
    }

    fn barrier(&self) {}
}

/// A device that queues up to `capacity` requests and completes them when
/// the test says, or refuses writes, or breaks.
struct FakeDisk {
    capacity: usize,
    in_flight: Vec<Request>,
    done: Vec<Completion>,
    read_only: bool,
    broken: Option<DeviceError>,
}

impl FakeDisk {
    fn new(capacity: usize) -> FakeDisk {
        FakeDisk {
            capacity,
            in_flight: Vec::new(),
            done: Vec::new(),
            read_only: false,
            broken: None,
        }
    }

    fn finish(&mut self, id: u64, status: Status, bytes: u64) {
        let at = self
            .in_flight
            .iter()
            .position(|request| request.id == id)
            .expect("the request is in flight");
        let _ = self.in_flight.remove(at);
        self.done.push(Completion { id, status, bytes });
    }
}

impl Disk for FakeDisk {
    fn submit(&mut self, request: &Request) -> Result<Accepted, SubmitError> {
        if let Some(error) = self.broken {
            return Err(SubmitError::Device(error));
        }
        if self.read_only && request.op == Op::Write {
            return Err(SubmitError::ReadOnly);
        }
        if self.in_flight.len() >= self.capacity {
            return Err(SubmitError::QueueFull);
        }
        self.in_flight.push(*request);
        Ok(Accepted::Queued { chains: 1 })
    }

    fn drain(&mut self, out: &mut [Completion]) -> Result<Drained, DeviceError> {
        if let Some(error) = self.broken {
            return Err(error);
        }
        let take = self.done.len().min(out.len());
        for (slot, completion) in out.iter_mut().zip(self.done.drain(..take)) {
            *slot = completion;
        }
        Ok(Drained {
            completions: take,
            config_changed: false,
            more: !self.done.is_empty(),
        })
    }
}

fn device() -> Device {
    Device::new(
        BLOCK,
        CAPACITY_BLOCKS,
        MAX_SECTORS,
        DeviceFlags::FLUSH,
        DATA_BYTES,
    )
    .unwrap()
}

/// The loop over a fresh ring, and the kernel side attached to the same
/// memory, with `storage` for its table.
fn pair(
    disk: FakeDisk,
    storage: &mut [Slot],
) -> (Serve<Mem, FakeDisk, 8>, KernelSide<'_, Mem>, Mem) {
    let memory = Mem::new();
    let layout = RingLayout::standard(ENTRIES).unwrap();
    let driver = DriverSide::new(memory.clone(), RING_BYTES, layout, device()).unwrap();
    let kernel = KernelSide::attach(memory.clone(), RING_BYTES, device(), storage).unwrap();
    (Serve::new(driver, disk), kernel, memory)
}

fn none() -> [Completion; 4] {
    [Completion {
        id: 0,
        status: Status::Ok,
        bytes: 0,
    }; 4]
}

#[test]
fn submissions_reach_the_device_with_sectors_scaled_and_come_back_completed() {
    let mut storage = [Slot::EMPTY; 8];
    let (mut serve, mut kernel, _) = pair(FakeDisk::new(4), &mut storage);
    kernel.submit(Submission::read(7, 3, 2, 0)).unwrap();
    kernel.submit(Submission::write(8, 10, 1, 8192)).unwrap();
    kernel.submit(Submission::flush(9)).unwrap();
    let _ = kernel.publish();

    assert_eq!(serve.on_bell(), Ok(None), "nothing completed yet");
    let disk = &serve.disk;
    assert_eq!(disk.in_flight.len(), 3);
    assert_eq!(
        disk.in_flight[0],
        Request {
            id: 7,
            op: Op::Read,
            sector: 3 * 8,
            count: 2 * 8,
            data_offset: 0,
        },
        "a 4 KiB block is eight virtio sectors"
    );
    assert_eq!(disk.in_flight[1].op, Op::Write);
    assert_eq!(disk.in_flight[2].op, Op::Flush);

    serve.disk.finish(8, Status::Ok, 4096);
    serve.disk.finish(7, Status::Ok, 8192);
    serve.disk.finish(9, Status::Ok, 0);
    let mut out = none();
    assert_eq!(
        serve.on_interrupt(&mut out),
        Ok(None),
        "the kernel asked for no bell"
    );

    let mut seen = Vec::new();
    while let Some(done) = kernel.poll().unwrap() {
        seen.push((done.submission.id, done.status, done.bytes_done));
    }
    assert_eq!(
        seen,
        vec![
            (8, RingStatus::Ok, 4096),
            (7, RingStatus::Ok, 8192),
            (9, RingStatus::Ok, 0)
        ],
        "completions in the order the device finished them"
    );
    assert_eq!(serve.ring().held(), 0);
}

#[test]
fn a_full_device_queue_holds_submissions_in_order_and_sleeps_on_the_interrupt() {
    let mut storage = [Slot::EMPTY; 8];
    let (mut serve, mut kernel, _) = pair(FakeDisk::new(1), &mut storage);
    for id in 1..=4 {
        kernel.submit(Submission::read(id, id, 1, 0)).unwrap();
    }
    let _ = kernel.publish();

    assert_eq!(serve.on_bell(), Ok(None));
    assert_eq!(serve.disk.in_flight.len(), 1, "one fits the queue");
    assert!(serve.throttled(), "the rest wait");
    assert_eq!(
        serve.before_sleep(),
        Ok(Wait::Sleep),
        "sleep on the interrupt, whatever the ring holds"
    );

    let mut out = none();
    for expected in 1..=4_u64 {
        assert_eq!(serve.disk.in_flight[0].id, expected, "first in, first out");
        serve.disk.finish(expected, Status::Ok, 4096);
        let _ = serve.on_interrupt(&mut out).unwrap();
    }
    assert!(!serve.throttled());
    assert_eq!(serve.before_sleep(), Ok(Wait::Sleep));
    let mut ids = Vec::new();
    while let Some(done) = kernel.poll().unwrap() {
        ids.push(done.submission.id);
    }
    assert_eq!(ids, vec![1, 2, 3, 4]);
}

#[test]
fn a_write_the_device_refuses_as_read_only_is_completed_read_only() {
    let mut storage = [Slot::EMPTY; 8];
    let mut disk = FakeDisk::new(4);
    disk.read_only = true;
    let (mut serve, mut kernel, _) = pair(disk, &mut storage);
    kernel.submit(Submission::write(5, 0, 1, 0)).unwrap();
    let _ = kernel.publish();
    assert_eq!(serve.on_bell(), Ok(None));
    let done = kernel.poll().unwrap().expect("completed at once");
    assert_eq!((done.submission.id, done.status), (5, RingStatus::ReadOnly));
    assert!(serve.disk.in_flight.is_empty());
}

#[test]
fn the_kernel_gets_a_bell_when_it_asked_for_one() {
    let mut storage = [Slot::EMPTY; 8];
    let (mut serve, mut kernel, _) = pair(FakeDisk::new(4), &mut storage);
    kernel.submit(Submission::read(1, 0, 1, 0)).unwrap();
    let _ = kernel.publish();
    assert_eq!(serve.on_bell(), Ok(None));
    assert_eq!(
        kernel.prepare_to_sleep(),
        Ok(Wait::Sleep),
        "the kernel goes to sleep"
    );
    serve.disk.finish(1, Status::Ok, 4096);
    let mut out = none();
    let bell = serve
        .on_interrupt(&mut out)
        .unwrap()
        .expect("a sleeping kernel is rung");
    assert_eq!(bell.key(), BELL_COMPLETE);
    assert_eq!(bell.tail(), 1);
}

#[test]
fn a_corrupt_ring_stops_the_loop_and_stays_stopped() {
    let mut storage = [Slot::EMPTY; 8];
    let (mut serve, _kernel, mut memory) = pair(FakeDisk::new(4), &mut storage);
    // The kernel's tail jumps past the ring's capacity: corruption.
    memory.write_u32(header::SUB_TAIL, ENTRIES + 1);
    let fault = serve.on_bell().unwrap_err();
    assert!(matches!(fault, Fault::Ring(_)), "{fault}");
    assert_eq!(serve.fault(), Some(fault));
    assert_eq!(
        serve.before_sleep(),
        Err(fault),
        "every call after reports it"
    );
}

#[test]
fn a_broken_device_stops_the_loop() {
    let mut storage = [Slot::EMPTY; 8];
    let (mut serve, mut kernel, _) = pair(FakeDisk::new(4), &mut storage);
    kernel.submit(Submission::read(1, 0, 1, 0)).unwrap();
    let _ = kernel.publish();
    serve.disk.broken = Some(DeviceError::NeedsReset);
    assert_eq!(serve.on_bell(), Err(Fault::Device(DeviceError::NeedsReset)));
    let mut out = none();
    assert_eq!(
        serve.on_interrupt(&mut out),
        Err(Fault::Device(DeviceError::NeedsReset))
    );
}
