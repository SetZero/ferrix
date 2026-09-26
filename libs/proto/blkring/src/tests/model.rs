//! Honest sides exchanging random traffic, against a model: every id completes
//! exactly once, with what the driver said, and no wake-up is lost.

use std::collections::{BTreeMap, BTreeSet};
use std::vec::Vec;

use super::support::{BLOCK, Mem, Rng, device, with_pair};
use crate::bell::Wait;
use crate::driver::{Consumed, DriverSide};
use crate::kernel::{Completed, KernelSide, SubmitError};
use crate::layout::{Op, Status, Submission};

const SEEDS: u64 = if cfg!(miri) { 3 } else { 300 };
const STEPS: usize = if cfg!(miri) { 150 } else { 2000 };

#[derive(Default)]
struct Model {
    next_id: u64,
    outstanding: BTreeMap<u64, Submission>,
    held: Vec<Submission>,
    sent: BTreeMap<u64, (Status, u64)>,
    completed: BTreeSet<u64>,
    unpublished_submissions: usize,
    unpublished_completions: usize,
}

type Kernel<'a, 'b> = KernelSide<'a, Mem<'b>>;

#[test]
fn honest_sides_complete_every_id_exactly_once() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let entries = 2_u32 << rng.below(4);
        with_pair(entries, |kernel, driver, _shared| {
            let mut model = Model {
                next_id: rng.u64() >> 4,
                ..Model::default()
            };
            for _ in 0..STEPS {
                step(&mut rng, kernel, driver, &mut model);
            }
            settle(kernel, driver, &mut model, entries);
            assert!(
                model.completed.len() > STEPS / 50,
                "seed {seed}: only {} ids completed, so the exchange is not exercised",
                model.completed.len()
            );
            assert!(
                model.outstanding.is_empty(),
                "seed {seed}: ids never completed"
            );
            assert_eq!(
                kernel.outstanding(),
                0,
                "seed {seed}: the kernel still holds ids"
            );
        });
    }
}

fn step(
    rng: &mut Rng,
    kernel: &mut Kernel<'_, '_>,
    driver: &mut DriverSide<Mem<'_>>,
    model: &mut Model,
) {
    match rng.below(10) {
        0 | 1 => submit(rng, kernel, model),
        2 => {
            let asked = driver.wants_bell();
            let bell = kernel.publish();
            let expect_bell = asked && model.unpublished_submissions > 0;
            assert_eq!(bell.is_some(), expect_bell, "submit bell");
            model.unpublished_submissions = 0;
        }
        3 => consume(driver, model),
        4 | 5 => complete(rng, driver, model),
        6 => {
            let asked = kernel.wants_bell();
            let bell = driver.publish();
            let expect_bell = asked && model.unpublished_completions > 0;
            assert_eq!(bell.is_some(), expect_bell, "complete bell");
            model.unpublished_completions = 0;
        }
        7 | 8 => {
            if let Some(completed) = kernel.poll().expect("honest driver") {
                record(model, &completed, kernel);
            }
        }
        _ => handshake(rng, kernel, driver),
    }
}

fn handshake(rng: &mut Rng, kernel: &mut Kernel<'_, '_>, driver: &mut DriverSide<Mem<'_>>) {
    match rng.below(4) {
        0 => {
            let wait = kernel.prepare_to_sleep().expect("honest driver");
            assert_eq!(
                kernel.wants_bell(),
                wait == Wait::Sleep,
                "asks only when sleeping"
            );
        }
        1 => kernel.woke(),
        2 => {
            let wait = driver.prepare_to_sleep().expect("honest kernel");
            assert_eq!(
                driver.wants_bell(),
                wait == Wait::Sleep,
                "asks only when sleeping"
            );
        }
        _ => driver.woke(),
    }
}

fn submit(rng: &mut Rng, kernel: &mut Kernel<'_, '_>, model: &mut Model) {
    let device = device();
    let id = model.next_id;
    model.next_id = model.next_id.wrapping_add(1 + rng.below(3));
    let submission = match rng.below(5) {
        0 => Submission::flush(id),
        choice => {
            let count = 1 + rng.below(u64::from(device.max_sectors())) as u32;
            let sector = rng.below(device.capacity() - u64::from(count));
            let len = device.payload_len(count);
            let offset = rng.below(device.data_vmo_size() - len);
            if choice == 1 {
                Submission::read(id, sector, count, offset)
            } else if rng.chance(4) {
                Submission::write(id, sector, count, offset).with_fua()
            } else {
                Submission::write(id, sector, count, offset)
            }
        }
    };
    match kernel.submit(submission) {
        Ok(()) => {
            assert!(
                model.outstanding.insert(id, submission).is_none(),
                "id {id} reused"
            );
            model.unpublished_submissions += 1;
        }
        Err(SubmitError::Full) => {
            assert_eq!(
                kernel.outstanding(),
                kernel.capacity(),
                "full only at capacity"
            );
        }
        Err(other) => panic!("an honest submission was refused: {other:?}"),
    }
}

fn consume(driver: &mut DriverSide<Mem<'_>>, model: &mut Model) {
    match driver.consume().expect("honest kernel") {
        Some(Consumed::Request(submission)) => {
            assert_eq!(
                model.outstanding.get(&submission.id),
                Some(&submission),
                "the driver sees what the kernel sent"
            );
            assert!(
                !model.held.iter().any(|held| held.id == submission.id),
                "consumed twice"
            );
            model.held.push(submission);
        }
        Some(refused @ Consumed::Refused { .. }) => panic!("an honest submission: {refused:?}"),
        None => {}
    }
}

fn complete(rng: &mut Rng, driver: &mut DriverSide<Mem<'_>>, model: &mut Model) {
    if model.held.is_empty() {
        return;
    }
    let index = rng.below(model.held.len() as u64) as usize;
    let submission = model.held.swap_remove(index);
    let len = u64::from(submission.count) * BLOCK;
    let status = [
        Status::Ok,
        Status::Ok,
        Status::IoError,
        Status::Unsupported,
        Status::ReadOnly,
    ][rng.below(5) as usize];
    let bytes = match status {
        Status::Ok if rng.chance(4) => rng.below(len + 1),
        Status::Ok => len,
        _ => 0,
    };
    driver
        .complete(submission.id, status, bytes)
        .expect("room for what is held");
    let _ = model.sent.insert(submission.id, (status, bytes));
    model.unpublished_completions += 1;
}

fn record(model: &mut Model, completed: &Completed, kernel: &Kernel<'_, '_>) {
    let id = completed.submission.id;
    let submission = model
        .outstanding
        .remove(&id)
        .expect("completed an id not outstanding");
    assert_eq!(completed.submission, submission, "the kernel's own copy");
    assert!(model.completed.insert(id), "id {id} completed twice");
    let (status, bytes) = model.sent.remove(&id).expect("the driver sent it");
    let len = kernel.device().payload_len(submission.count);
    let seen = if status == Status::Ok && submission.op != Op::Read && bytes < len {
        Status::IoError
    } else {
        status
    };
    assert_eq!(
        (completed.status, completed.bytes_done),
        (seen, bytes),
        "id {id}"
    );
}

/// Publish, drain and complete until nothing is outstanding.
fn settle(
    kernel: &mut Kernel<'_, '_>,
    driver: &mut DriverSide<Mem<'_>>,
    model: &mut Model,
    entries: u32,
) {
    for _ in 0..4 * entries + 8 {
        let _ = kernel.publish();
        model.unpublished_submissions = 0;
        for _ in 0..entries {
            consume(driver, model);
        }
        for submission in core::mem::take(&mut model.held) {
            let len = u64::from(submission.count) * BLOCK;
            driver
                .complete(submission.id, Status::Ok, len)
                .expect("room");
            let _ = model.sent.insert(submission.id, (Status::Ok, len));
        }
        let _ = driver.publish();
        model.unpublished_completions = 0;
        while let Some(completed) = kernel.poll().expect("honest driver") {
            record(model, &completed, kernel);
        }
        if model.outstanding.is_empty() {
            return;
        }
    }
}
