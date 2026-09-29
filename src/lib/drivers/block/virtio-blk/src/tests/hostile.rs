//! The driver against a device told to misbehave.
//!
//! Every case ends the same way or it is a bug: an error, no panic, no access
//! outside memory the driver was given, `FAILED` told to the device once the
//! driver stops trusting it, and — after the reset — every accepted request
//! that did not complete reported as abandoned.

use core::cell::RefCell;
use core::mem::ManuallyDrop;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use ferrix_virtio::QueueError;
use ferrix_virtio::blk::{BlkError, FEATURE_BLK_SIZE};
use ferrix_virtio::pci::{FEATURE_VERSION_1, NO_VECTOR, STATUS_FAILED, TransportError};

use super::fake::{Device, PAGE, Rig, Setup};
use crate::{Completion, DeviceError, InitError, Op, Rings, Status, SubmitError, Teardown};

/// Bring a driver up against `setup` and require it to fail; return the
/// error, the device, and whether the memory came back.
fn refused(setup: &Setup) -> (InitError, Rc<RefCell<Device>>, bool, bool) {
    let (_, device, _, result) = setup.try_build();
    let failure = result.expect_err("the device is refused");
    let (released, queue_built) = match failure.teardown {
        Teardown::Released(released) => (true, matches!(released.rings, Rings::Queue(_))),
        Teardown::Wedged(released) => {
            // Test memory, freed so Miri does not report the deliberate leak.
            let released = ManuallyDrop::into_inner(released);
            (false, matches!(released.rings, Rings::Queue(_)))
        }
    };
    (failure.error, device, released, queue_built)
}

/// Require the device was told `FAILED` and then reset.
fn failed_and_reset(device: &Rc<RefCell<Device>>) {
    let device = device.borrow();
    let failed = device
        .status_writes
        .iter()
        .position(|status| status & STATUS_FAILED != 0)
        .expect("FAILED was set");
    let reset = device
        .status_writes
        .iter()
        .rposition(|status| *status == 0)
        .expect("the device was reset");
    assert!(
        failed < reset,
        "FAILED before the reset: {:?}",
        device.status_writes
    );
}

#[test]
fn a_device_that_refuses_the_features_is_failed_and_reset() {
    let mut setup = Setup::new();
    setup.misbehave.refuse_features = true;
    let (error, device, released, queue_built) = refused(&setup);
    assert_eq!(error, InitError::Transport(TransportError::FeaturesRefused));
    assert!(released && !queue_built);
    failed_and_reset(&device);
}

#[test]
fn a_device_without_version_1_is_refused() {
    let mut setup = Setup::new();
    setup.offered &= !FEATURE_VERSION_1;
    let (error, device, released, _) = refused(&setup);
    assert_eq!(
        error,
        InitError::Transport(TransportError::MissingFeatures {
            missing: FEATURE_VERSION_1
        })
    );
    assert!(released);
    failed_and_reset(&device);
}

#[test]
fn a_device_that_never_resets_keeps_its_memory_for_good() {
    let mut setup = Setup::new();
    setup.misbehave.never_reset = true;
    setup.options.reset_polls = 16;
    let (error, _, released, _) = refused(&setup);
    assert_eq!(error, InitError::Transport(TransportError::ResetTimedOut));
    assert!(
        !released,
        "memory a live device may write must not be released"
    );
}

#[test]
fn a_device_that_will_not_reset_at_shutdown_is_wedged() {
    let mut setup = Setup::new();
    setup.options.reset_polls = 16;
    let mut rig = setup.build();
    let _ = rig.submit(4, Op::Read, 0, 8, 0).unwrap();
    rig.device.borrow_mut().misbehave.never_reset = true;
    match rig.driver.shutdown() {
        Teardown::Wedged(released) => {
            assert_eq!(released.abandoned().collect::<Vec<_>>(), vec![4]);
            let _ = ManuallyDrop::into_inner(released);
        }
        Teardown::Released(_) => panic!("the device did not reset"),
    }
}

#[test]
fn a_configuration_that_keeps_changing_is_refused() {
    let mut setup = Setup::new();
    setup.misbehave.churn_config = true;
    let (error, device, released, _) = refused(&setup);
    assert_eq!(error, InitError::ConfigUnstable);
    assert!(released);
    failed_and_reset(&device);
}

#[test]
fn a_block_size_linux_would_refuse_is_refused() {
    let mut setup = Setup::new();
    setup.config.blk_size = Some(1000);
    let (error, _, released, _) = refused(&setup);
    assert_eq!(error, InitError::Config(BlkError::BadBlockSize(1000)));
    assert!(released);
}

#[test]
fn a_configuration_block_too_short_for_its_features_is_refused() {
    let mut setup = Setup::new();
    setup.config_len = 16;
    let (error, _, _, _) = refused(&setup);
    assert_eq!(
        error,
        InitError::Config(BlkError::ConfigTruncated {
            feature: FEATURE_BLK_SIZE
        })
    );
}

#[test]
fn a_device_without_a_request_queue_is_refused() {
    let mut setup = Setup::new();
    setup.queue_max = 0;
    let (error, device, _, _) = refused(&setup);
    assert_eq!(
        error,
        InitError::Transport(TransportError::NoSuchQueue { index: 0 })
    );
    failed_and_reset(&device);
}

#[test]
fn a_device_that_drops_the_queue_vector_is_refused_after_the_queue_is_built() {
    let mut setup = Setup::new();
    setup.misbehave.drop_vector = true;
    let (error, device, released, queue_built) = refused(&setup);
    assert_eq!(
        error,
        InitError::VectorRefused {
            asked: 1,
            kept: NO_VECTOR
        }
    );
    assert!(released && queue_built);
    failed_and_reset(&device);
}

/// Require the driver has given up with `fault`, refuses everything after,
/// told the device, and abandons exactly `abandoned` at shutdown.
fn broken(mut rig: Rig, fault: DeviceError, abandoned: &[u64]) {
    assert_eq!(rig.driver.fault(), Some(fault));
    assert_eq!(
        rig.submit(1000, Op::Read, 0, 8, 0),
        Err(SubmitError::Broken)
    );
    let mut out = [Completion {
        id: 0,
        status: Status::Ok,
        bytes: 0,
    }; 4];
    assert_eq!(rig.driver.on_interrupt(&mut out), Err(fault));
    assert_ne!(rig.device.borrow().status() & STATUS_FAILED, 0);
    assert_eq!(rig.bus.faults.get(), 0);

    let Rig { device, driver, .. } = rig;
    match driver.shutdown() {
        Teardown::Released(released) => {
            let mut ids: Vec<_> = released.abandoned().collect();
            ids.sort_unstable();
            assert_eq!(ids, abandoned);
        }
        Teardown::Wedged(_) => panic!("the device reset"),
    }
    assert_eq!(device.borrow().status(), 0);
}

/// Submit one one-page read, let the device serve it, and take the fault.
fn served_with(setup: &Setup) -> (Rig, Result<crate::Drained, DeviceError>) {
    let mut rig = setup.build();
    let _ = rig.submit(1, Op::Read, 0, 8, 0).unwrap();
    let _ = rig.run_device();
    let mut out = [Completion {
        id: 0,
        status: Status::Ok,
        bytes: 0,
    }; 4];
    let result = rig.driver.on_interrupt(&mut out);
    (rig, result)
}

#[test]
fn a_device_claiming_more_written_than_the_chain_holds_is_broken() {
    let mut setup = Setup::new();
    setup.misbehave.extra_written = 1;
    let (rig, result) = served_with(&setup);
    let fault = DeviceError::Protocol(BlkError::WrittenTooLong {
        written: 4098,
        writable: 4097,
    });
    assert_eq!(result, Err(fault));
    broken(rig, fault, &[1]);
}

#[test]
fn a_status_byte_virtio_does_not_define_is_broken() {
    let mut setup = Setup::new();
    setup.misbehave.status = Some(3);
    let (rig, result) = served_with(&setup);
    let fault = DeviceError::Protocol(BlkError::BadStatus(3));
    assert_eq!(result, Err(fault));
    broken(rig, fault, &[1]);
}

#[test]
fn a_completion_without_a_status_byte_is_broken() {
    let mut setup = Setup::new();
    setup.misbehave.skip_status = true;
    let (rig, result) = served_with(&setup);
    let fault = DeviceError::Protocol(BlkError::BadStatus(0xFF));
    assert_eq!(result, Err(fault));
    broken(rig, fault, &[1]);
}

/// Submit one two-page read, then have the device forge the used ring.
fn forged(forge: impl FnOnce(&mut Device)) -> (Rig, Result<crate::Drained, DeviceError>) {
    let mut rig = Setup::new().build();
    let _ = rig.submit(1, Op::Read, 0, 16, 0).unwrap();
    {
        let mut device = rig.device.borrow_mut();
        device.take_available();
        forge(&mut device);
    }
    let mut out = [Completion {
        id: 0,
        status: Status::Ok,
        bytes: 0,
    }; 4];
    let result = rig.driver.on_interrupt(&mut out);
    (rig, result)
}

#[test]
fn a_used_entry_naming_a_descriptor_out_of_range_is_broken() {
    let (rig, result) = forged(|device| device.forge_used(60_000, 1));
    let fault = DeviceError::Queue(QueueError::DescriptorOutOfRange);
    assert_eq!(result, Err(fault));
    broken(rig, fault, &[1]);
}

#[test]
fn a_used_entry_naming_a_free_descriptor_is_broken() {
    let (rig, result) = forged(|device| device.forge_used(40, 1));
    let fault = DeviceError::Queue(QueueError::NotAChainHead);
    assert_eq!(result, Err(fault));
    broken(rig, fault, &[1]);
}

#[test]
fn a_used_entry_naming_the_inside_of_a_chain_is_broken() {
    let (rig, result) = forged(|device| device.forge_used(1, 1));
    let fault = DeviceError::Queue(QueueError::NotAChainHead);
    assert_eq!(result, Err(fault));
    broken(rig, fault, &[1]);
}

#[test]
fn a_used_index_that_jumps_is_broken() {
    let (rig, result) = forged(|device| device.jump_used_index(100));
    let fault = DeviceError::Queue(QueueError::UsedIndexJumped);
    assert_eq!(result, Err(fault));
    broken(rig, fault, &[1]);
}

#[test]
fn a_device_that_needs_a_reset_is_broken() {
    let mut rig = Setup::new().build();
    let _ = rig.submit(2, Op::Read, 0, 8, 0).unwrap();
    rig.device.borrow_mut().misbehave.needs_reset = true;
    let mut out = [Completion {
        id: 0,
        status: Status::Ok,
        bytes: 0,
    }; 1];
    assert_eq!(
        rig.driver.on_interrupt(&mut out),
        Err(DeviceError::NeedsReset)
    );
    rig.device.borrow_mut().misbehave.needs_reset = false;
    broken(rig, DeviceError::NeedsReset, &[2]);
}

#[test]
fn a_device_that_corrupts_the_descriptor_links_breaks_a_submission_that_is_still_answered() {
    let mut rig = Setup::new().build();
    rig.device.borrow_mut().scribble_links();
    let fault = DeviceError::Queue(QueueError::DescriptorOutOfRange);
    assert_eq!(
        rig.submit(9, Op::Read, 0, 16, 0),
        Err(SubmitError::Device(fault))
    );
    broken(rig, fault, &[9]);
}

#[test]
fn completions_taken_before_a_fault_are_delivered_first() {
    let mut rig = Setup::new().build();
    let _ = rig.submit(1, Op::Read, 0, 8, 0).unwrap();
    let _ = rig.submit(2, Op::Read, 8, 8, PAGE as u64).unwrap();
    {
        let mut device = rig.device.borrow_mut();
        device.take_available();
        device.complete(0);
        device.misbehave.status = Some(9);
        device.complete(0);
    }
    let mut out = [Completion {
        id: 0,
        status: Status::Ok,
        bytes: 0,
    }; 4];
    let drained = rig.driver.on_interrupt(&mut out).unwrap();
    assert_eq!(drained.completions, 1);
    assert!(!drained.more);
    assert_eq!(
        out[0],
        Completion {
            id: 1,
            status: Status::Ok,
            bytes: 4096
        }
    );
    broken(rig, DeviceError::Protocol(BlkError::BadStatus(9)), &[2]);
}
