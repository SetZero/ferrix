//! Random submissions and completions against a model of the disk.
//!
//! Each scenario draws a device — block size, `seg_max`, `size_max`, queue
//! size, whether it flushes, whether the data pages' device addresses follow
//! on — and then a few thousand steps, each one of: submit a random read,
//! write or flush; let the device take what is published and serve a random
//! subset in a random order; take completions into a random-sized slice.
//!
//! Held throughout: every completion names a request in flight, and each
//! request completes exactly once; a read returns what the model says the
//! disk holds; a queue refuses a request as full only while something is in
//! flight; and whenever nothing is, the driver is idle — no request, chain or
//! header slot held and every descriptor free. At the end the device's disk
//! equals the model's, and the device saw no protocol error and no access
//! outside the pages it was given.
//!
//! In-flight requests never share sectors or a data window, so the model is
//! exact whatever order the device serves them in.

use std::collections::BTreeMap;
use std::vec;
use std::vec::Vec;

use ferrix_virtio::blk::{FEATURE_FLUSH, FEATURE_SIZE_MAX};

use super::fake::{PAGE, Rig, Setup};
use crate::{Accepted, Completion, Op, Status, SubmitError};

/// xorshift64*, seeded; no dependency, and the same sequence everywhere.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 { 0 } else { self.next() % bound }
    }

    fn pick<T: Copy>(&mut self, choices: &[T]) -> T {
        choices[self.below(choices.len() as u64) as usize]
    }
}

/// Pages in each request's data window.
const WINDOW_PAGES: usize = 3;
/// Data windows, and so the most requests in flight.
const WINDOWS: usize = 6;
/// The disk, in sectors.
const CAPACITY: u64 = 256;

/// Seeds and steps: fewer under Miri, which interprets every byte copy.
#[cfg(not(miri))]
const SCENARIOS: (u64, usize) = (32, 1500);
#[cfg(miri)]
const SCENARIOS: (u64, usize) = (2, 40);

/// The most blocks one request reads or writes.
#[cfg(not(miri))]
const MOST_BLOCKS: u64 = 24;
#[cfg(miri)]
const MOST_BLOCKS: u64 = 2;

/// A request in flight.
#[derive(Clone, Copy, Debug)]
struct Flight {
    op: Op,
    sector: u64,
    bytes: u64,
    window: usize,
    offset_in_window: usize,
}

impl Flight {
    fn overlaps(&self, sector: u64, bytes: u64) -> bool {
        self.op != Op::Flush
            && sector * 512 < self.sector * 512 + self.bytes
            && self.sector * 512 < sector * 512 + bytes
    }
}

/// One scenario's state.
struct Scenario {
    rng: Rng,
    rig: Rig,
    block: u64,
    disk: Vec<u8>,
    flights: BTreeMap<u64, Flight>,
    next_id: u64,
    completed: u64,
}

impl Scenario {
    fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let mut setup = Setup::new();
        setup.config.capacity = CAPACITY;
        let block = rng.pick(&[512_u64, 4096]);
        setup.config.blk_size = Some(block as u32);
        setup.config.seg_max = Some(rng.pick(&[1, 2, 3, 8, 62]));
        if rng.below(2) == 0 {
            setup.offered |= FEATURE_SIZE_MAX;
            setup.config.size_max = Some(rng.pick(&[4096, 6000, 8192]));
        }
        if rng.below(3) == 0 {
            setup.offered &= !FEATURE_FLUSH;
        }
        setup.queue_max = rng.pick(&[8, 16, 64]);
        setup.data_pages = WINDOWS * WINDOW_PAGES;
        setup.data_scattered = rng.below(3) != 0;
        Scenario {
            rng,
            rig: setup.build(),
            block,
            disk: vec![0; (CAPACITY * 512) as usize],
            flights: BTreeMap::new(),
            next_id: 1,
            completed: 0,
        }
    }

    /// A data window no request in flight is using.
    fn free_window(&mut self) -> Option<usize> {
        let start = self.rng.below(WINDOWS as u64) as usize;
        (0..WINDOWS)
            .map(|step| (start + step) % WINDOWS)
            .find(|window| self.flights.values().all(|flight| flight.window != *window))
    }

    fn submit(&mut self) {
        let Some(window) = self.free_window() else {
            return;
        };
        let id = self.next_id;
        self.next_id += 1;
        let op = match self.rng.below(10) {
            0..=5 => Op::Read,
            6..=8 => Op::Write,
            _ => Op::Flush,
        };
        let offset_in_window = (self.rng.below(8) * 512) as usize;
        let room = (WINDOW_PAGES * PAGE - offset_in_window) as u64 / self.block;
        let blocks = 1 + self.rng.below(room.min(MOST_BLOCKS));
        let bytes = if op == Op::Flush {
            0
        } else {
            blocks * self.block
        };
        let units = (CAPACITY * 512 - bytes) / self.block;
        let sector = self.rng.below(units + 1) * self.block / 512;
        if op != Op::Flush && self.flights.values().any(|f| f.overlaps(sector, bytes)) {
            return;
        }
        let data_offset = window * WINDOW_PAGES * PAGE + offset_in_window;
        let written: Vec<u8> = (0..bytes).map(|_| self.rng.next() as u8).collect();
        if op == Op::Write {
            self.rig.data.write(data_offset, &written);
        }

        let result = self
            .rig
            .submit(id, op, sector, (bytes / 512) as u32, data_offset as u64);
        match result {
            Ok(Accepted::Queued { .. }) => {
                if op == Op::Write {
                    let at = (sector * 512) as usize;
                    self.disk[at..at + written.len()].copy_from_slice(&written);
                }
                let _ = self.flights.insert(
                    id,
                    Flight {
                        op,
                        sector,
                        bytes,
                        window,
                        offset_in_window,
                    },
                );
            }
            Ok(Accepted::Completed(completion)) => {
                assert_eq!(op, Op::Flush);
                assert_eq!(
                    completion,
                    Completion {
                        id,
                        status: Status::Ok,
                        bytes: 0
                    }
                );
                self.completed += 1;
            }
            Err(SubmitError::QueueFull) => {
                assert!(!self.flights.is_empty(), "an empty queue said it was full");
            }
            Err(SubmitError::TooLarge | SubmitError::Unsplittable) => {}
            Err(error) => panic!("a valid request was refused: {error:?}"),
        }
    }

    fn serve(&mut self) {
        let mut device = self.rig.device.borrow_mut();
        device.take_available();
        let serve = self.rng.below(device.taken() as u64 + 1);
        for _ in 0..serve {
            let index = self.rng.below(device.taken() as u64) as usize;
            device.complete(index);
        }
    }

    fn take(&mut self) {
        let len = 1 + self.rng.below(4) as usize;
        let mut out = vec![
            Completion {
                id: 0,
                status: Status::Ok,
                bytes: 0,
            };
            len
        ];
        let drained = self.rig.driver.on_interrupt(&mut out).expect("no fault");
        for completion in &out[..drained.completions] {
            let flight = self
                .flights
                .remove(&completion.id)
                .expect("a completion for a request in flight, and only once");
            assert_eq!(completion.status, Status::Ok);
            assert_eq!(completion.bytes, flight.bytes);
            if flight.op == Op::Read {
                let offset = flight.window * WINDOW_PAGES * PAGE + flight.offset_in_window;
                let at = (flight.sector * 512) as usize;
                assert_eq!(
                    self.rig.data.read(offset, flight.bytes as usize),
                    &self.disk[at..at + flight.bytes as usize],
                    "read {} returned what the disk holds",
                    completion.id
                );
            }
            self.completed += 1;
        }
    }

    fn check_idle(&self) {
        if self.flights.is_empty() {
            assert!(self.rig.driver.is_idle(), "nothing in flight leaks nothing");
            assert_eq!(self.rig.driver.requests_in_flight(), 0);
        }
    }

    fn run(mut self, steps: usize) {
        for _ in 0..steps {
            match self.rng.below(10) {
                0..=4 => self.submit(),
                5..=7 => self.serve(),
                _ => self.take(),
            }
            self.check_idle();
        }
        for _ in 0..4 * WINDOWS {
            if self.flights.is_empty() {
                break;
            }
            let _ = self.rig.run_device();
            self.take();
        }
        assert!(self.flights.is_empty(), "every request completes");
        assert!(self.rig.driver.is_idle());
        assert_eq!(
            self.rig.driver.free_descriptors(),
            self.rig.driver.info().queue_size
        );
        assert!(
            self.rig.device.borrow().disk == self.disk,
            "the disk matches the model"
        );
        assert!(self.completed > 0);
        self.rig.assert_clean();
    }
}

#[test]
fn random_submissions_and_completions_match_a_model_of_the_disk() {
    let (seeds, steps) = SCENARIOS;
    for seed in 0..seeds {
        Scenario::new(seed).run(steps);
    }
}
