//! Shutdown (§6.3): outstanding requests fail at once, and the data VMO is
//! held until devmgr confirms the device reset, whatever ended the ring.

use std::vec::Vec;

use super::support::{Mem, Rng, read, with_pair};
use crate::bell::Wait;
use crate::driver::DriverSide;
use crate::kernel::{DataVmo, EndReason, Ending, KernelSide, Phase, RingInUse, SubmitError};
use crate::layout::{Status, header};
use crate::{Corruption, Submission};

#[test]
fn an_orderly_stop_fails_what_is_left_at_once_and_holds_the_data_vmo() {
    with_pair(4, |kernel, driver, _shared| {
        for id in 1..=3 {
            kernel.submit(read(id)).expect("room");
        }
        let _ = kernel.publish();
        assert_eq!(kernel.data_vmo(), DataVmo::InUse, "running");
        kernel.stop();
        assert_eq!(kernel.phase(), Phase::Stopping, "STOP sent");
        assert_eq!(
            kernel.submit(read(9)),
            Err(SubmitError::NotRunning),
            "no new work"
        );
        assert_eq!(
            kernel.data_vmo(),
            DataVmo::InUse,
            "the driver is still finishing"
        );
        assert!(matches!(driver.consume(), Ok(Some(_))), "1");
        assert!(matches!(driver.consume(), Ok(Some(_))), "2");
        driver.complete(1, Status::Ok, 512).expect("held");
        driver.complete(2, Status::IoError, 0).expect("held");
        let _ = driver.publish();
        assert!(
            matches!(kernel.poll(), Ok(Some(_))),
            "1 completes while stopping"
        );
        assert!(
            matches!(kernel.poll(), Ok(Some(_))),
            "2 completes while stopping"
        );

        let failed: Vec<u64> = kernel.end(Ending::Stopped).map(|s| s.id).collect();
        assert_eq!(failed, [3], "what the driver never took fails at once");
        assert_eq!(kernel.phase(), Phase::Ended(EndReason::Stopped), "ended");
        assert_eq!(
            kernel.data_vmo(),
            DataVmo::HeldUntilReset,
            "STOPPED is the driver's word, not devmgr's"
        );
        assert_ended_quietly(kernel);
        assert_eq!(kernel.confirm_reset(), Ok(()), "devmgr confirms");
        assert_eq!(kernel.data_vmo(), DataVmo::Releasable, "released only now");
        assert_eq!(
            kernel.phase(),
            Phase::ResetConfirmed(EndReason::Stopped),
            "confirmed"
        );
    });
}

fn assert_ended_quietly(kernel: &mut KernelSide<'_, Mem<'_>>) {
    assert_eq!(kernel.outstanding(), 0, "nothing outstanding");
    assert_eq!(kernel.poll(), Ok(None), "an ended ring has nothing to poll");
    assert_eq!(
        kernel.prepare_to_sleep(),
        Ok(Wait::Sleep),
        "nor anything pending"
    );
    assert_eq!(kernel.publish(), None, "nor anything to publish");
    assert_eq!(
        kernel.submit(read(10)),
        Err(SubmitError::NotRunning),
        "nor room"
    );
    assert_eq!(kernel.data_vmo(), DataVmo::HeldUntilReset, "still held");
}

#[test]
fn a_dead_driver_fails_everything_at_once_and_leaves_the_data_vmo_held() {
    with_pair(4, |kernel, driver, _shared| {
        for id in 1..=4 {
            kernel.submit(read(id)).expect("room");
        }
        let _ = kernel.publish();
        assert!(matches!(driver.consume(), Ok(Some(_))), "1");
        assert!(matches!(driver.consume(), Ok(Some(_))), "2");
        let mut failed: Vec<u64> = kernel.end(Ending::DriverDied).map(|s| s.id).collect();
        failed.sort_unstable();
        assert_eq!(failed, [1, 2, 3, 4], "held or not, everything fails now");
        assert_eq!(
            kernel.end(Ending::DriverDied).count(),
            0,
            "each exactly once"
        );
        assert_eq!(kernel.phase(), Phase::Ended(EndReason::DriverDied), "dead");
        // A completion the dying driver managed to write changes nothing.
        driver.complete(1, Status::Ok, 512).expect("held");
        let _ = driver.publish();
        assert_ended_quietly(kernel);
        assert_eq!(kernel.confirm_reset(), Ok(()), "devmgr reset the device");
        assert_eq!(kernel.data_vmo(), DataVmo::Releasable, "released only now");
    });
}

#[test]
fn corruption_ends_the_ring_and_holds_the_data_vmo_until_the_reset() {
    with_pair(4, |kernel, _driver, shared| {
        kernel.submit(read(1)).expect("room");
        kernel.submit(read(2)).expect("room");
        let _ = kernel.publish();
        shared.poke_u32(header::COMP_TAIL, 99);
        assert_eq!(kernel.poll(), Err(Corruption::TailOverrun), "corrupt");
        let reason = EndReason::Corrupt(Corruption::TailOverrun);
        assert_eq!(
            kernel.phase(),
            Phase::Ended(reason),
            "corruption ends the ring"
        );
        assert_eq!(
            kernel.data_vmo(),
            DataVmo::HeldUntilReset,
            "held from the moment it is seen"
        );
        let mut failed: Vec<u64> = kernel.end(Ending::DriverDied).map(|s| s.id).collect();
        failed.sort_unstable();
        assert_eq!(failed, [1, 2], "everything outstanding fails");
        assert_eq!(
            kernel.phase(),
            Phase::Ended(reason),
            "the first reason stands"
        );
        assert_eq!(
            kernel.poll(),
            Err(Corruption::TailOverrun),
            "still reported"
        );
        assert_eq!(kernel.confirm_reset(), Ok(()), "devmgr confirms");
        assert_eq!(kernel.data_vmo(), DataVmo::Releasable, "released only now");
        assert_eq!(
            kernel.poll(),
            Err(Corruption::TailOverrun),
            "still reported"
        );
    });
}

#[test]
fn a_live_ring_cannot_be_released() {
    with_pair(4, |kernel, _driver, _shared| {
        assert_eq!(kernel.confirm_reset(), Err(RingInUse), "running");
        assert_eq!(kernel.data_vmo(), DataVmo::InUse, "running");
        kernel.stop();
        assert_eq!(kernel.confirm_reset(), Err(RingInUse), "stopping");
        assert_eq!(kernel.data_vmo(), DataVmo::InUse, "stopping");
    });
}

#[test]
fn nothing_but_confirm_reset_makes_the_data_vmo_releasable() {
    let seeds = if cfg!(miri) { 3 } else { 300 };
    for seed in 0..seeds {
        with_pair(4, |kernel, driver, shared| {
            let mut rng = Rng::new(seed);
            let mut confirmed = false;
            let mut ended = false;
            for _ in 0..200 {
                confirmed |= wander(&mut rng, kernel, driver, shared);
                let vmo = kernel.data_vmo();
                assert!(
                    confirmed || vmo != DataVmo::Releasable,
                    "seed {seed}: releasable before devmgr confirmed the reset"
                );
                assert!(
                    !(ended && vmo == DataVmo::InUse),
                    "seed {seed}: an ended ring came back"
                );
                ended |= vmo != DataVmo::InUse;
            }
        });
    }
}

/// One random operation of either side, or a scribble. Whether it was a
/// successful reset confirmation.
fn wander(
    rng: &mut Rng,
    kernel: &mut KernelSide<'_, Mem<'_>>,
    driver: &mut DriverSide<Mem<'_>>,
    shared: &super::support::Shared,
) -> bool {
    let id = rng.below(8) + 1;
    match rng.below(12) {
        0 => {
            let _ = kernel.submit(read(id));
        }
        1 => {
            let _ = kernel.publish();
        }
        2 => {
            let _ = kernel.poll();
        }
        3 => {
            let _ = kernel.prepare_to_sleep();
        }
        4 => kernel.woke(),
        5 => kernel.stop(),
        6 => {
            let ending = if rng.chance(2) {
                Ending::Stopped
            } else {
                Ending::DriverDied
            };
            let _ = kernel.end(ending).count();
        }
        7 => shared.poke_u32(header::COMP_TAIL, rng.below(8) as u32),
        8 => {
            let _ = driver.consume();
        }
        9 => {
            let _ = driver.complete(id, Status::Ok, 512);
            let _ = driver.publish();
        }
        10 => return rng.chance(4) && kernel.confirm_reset().is_ok(),
        _ => {
            let _ = kernel.submit(Submission::flush(id));
        }
    }
    false
}
