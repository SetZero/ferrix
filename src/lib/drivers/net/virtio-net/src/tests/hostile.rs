//! The driver against a device told to misbehave.
//!
//! Every one of these is a device breaking a `MUST` of virtio 1.2 §5.1, and
//! every one must end as an error the caller can act on — the driver failed,
//! the device told so, the memory kept until a reset — rather than as a panic
//! or as bytes handed upwards that nobody checked.

use std::vec;
use std::vec::Vec;

use ferrix_virtio::net::{FEATURE_MAC, HDR_F_NEEDS_CSUM, HDR_GSO_TCPV4, HEADER_LEN, NetError};
use ferrix_virtio::pci::{NO_VECTOR, STATUS_FAILED, TransportError};

use super::fake::{Misbehave, Rig, Setup, TestTeardown, free};
use crate::{
    DeviceError, Event, Frame, InitError, RECEIVE_QUEUE, Released, Slot, SubmitError, Teardown,
};

/// A rig whose device is told to get `misbehave` wrong.
fn misbehaving(misbehave: Misbehave) -> Rig {
    let mut setup = Setup::new();
    setup.misbehave = misbehave;
    setup.build()
}

/// Deliver a frame and take what the driver makes of it.
///
/// The delivery itself is checked by the caller, since a function returning a
/// `Result` may not assert.
fn deliver(rig: &mut Rig) -> Result<Vec<Event>, DeviceError> {
    let mut out = [Event::Sent { id: u64::MAX }; 4];
    let drained = rig.driver.on_interrupt(&mut out)?;
    Ok(out[..drained.events].to_vec())
}

/// Whether the driver told the device it had given up.
fn failed(rig: &Rig) -> bool {
    rig.device.borrow().status() & STATUS_FAILED != 0
}

#[test]
fn a_device_without_a_feature_the_driver_requires_is_refused() {
    let mut setup = Setup::new();
    setup.offered &= !FEATURE_MAC;
    let (_bus, device, _regions, driver) = setup.try_build();
    let Err(failure) = driver else {
        panic!("a device with no address should be refused")
    };
    assert_eq!(
        failure.error,
        InitError::Transport(TransportError::MissingFeatures {
            missing: FEATURE_MAC
        })
    );
    assert_eq!(
        device.borrow().accepted,
        0,
        "nothing was accepted, so nothing was agreed"
    );
    assert!(
        device.borrow().status() & STATUS_FAILED != 0 || device.borrow().status() == 0,
        "the device was told, and then reset"
    );
    assert_nothing_abandoned(failure.teardown);
}

#[test]
fn a_device_that_will_not_keep_the_features_written_is_refused() {
    let mut setup = Setup::new();
    setup.misbehave.refuse_features = true;
    let (_bus, _device, _regions, driver) = setup.try_build();
    let failure = driver.expect_err("refused");
    assert_eq!(
        failure.error,
        InitError::Transport(TransportError::FeaturesRefused)
    );
    assert_nothing_abandoned(failure.teardown);
}

#[test]
fn a_device_that_will_not_give_a_queue_its_vector_is_refused() {
    let mut setup = Setup::new();
    setup.misbehave.drop_vector = true;
    let (_bus, _device, _regions, driver) = setup.try_build();
    let failure = driver.expect_err("refused");
    assert_eq!(
        failure.error,
        InitError::VectorRefused {
            queue: RECEIVE_QUEUE,
            asked: 1,
            kept: NO_VECTOR
        },
        "the first queue is where it is noticed"
    );
    assert_nothing_abandoned(failure.teardown);
}

#[test]
fn a_device_whose_configuration_never_settles_is_refused() {
    let mut setup = Setup::new();
    setup.misbehave.churn_config = true;
    let (_bus, _device, _regions, driver) = setup.try_build();
    let failure = driver.expect_err("refused");
    assert_eq!(failure.error, InitError::ConfigUnstable);
    assert_nothing_abandoned(failure.teardown);
}

#[test]
fn a_completion_shorter_than_the_header_is_a_device_error() {
    let mut rig = misbehaving(Misbehave {
        short_header: true,
        ..Misbehave::default()
    });
    assert!(rig.deliver(&[0xAB; 64]), "a buffer was posted");
    assert_eq!(
        deliver(&mut rig),
        Err(DeviceError::Protocol(NetError::HeaderTruncated {
            have: HEADER_LEN - 1,
            needed: HEADER_LEN
        }))
    );
    assert_broken(&mut rig);
}

#[test]
fn a_completion_longer_than_the_buffer_is_a_device_error() {
    let mut rig = misbehaving(Misbehave {
        extra_written: 5000,
        ..Misbehave::default()
    });
    assert!(rig.deliver(&[0xAB; 64]), "a buffer was posted");
    assert_eq!(
        deliver(&mut rig),
        Err(DeviceError::Protocol(NetError::WrittenTooLong {
            written: HEADER_LEN + 64 + 5000,
            capacity: HEADER_LEN + 1514
        }))
    );
    assert_broken(&mut rig);
}

#[test]
fn a_used_entry_naming_a_descriptor_the_driver_never_gave_is_a_device_error() {
    let mut rig = Setup::new().build();
    // Nine hundred and ninety-nine is not a descriptor of a sixteen-entry
    // queue, so the queue itself refuses it before anything is read.
    rig.device.borrow_mut().forge_used(RECEIVE_QUEUE, 999, 64);
    let mut out = [Event::Sent { id: u64::MAX }; 4];
    let error = rig.driver.on_interrupt(&mut out).expect_err("a fault");
    assert!(
        matches!(
            error,
            DeviceError::Queue(_) | DeviceError::UnknownChain { .. }
        ),
        "{error:?}"
    );
    assert_broken(&mut rig);
}

#[test]
fn a_frame_carrying_an_offload_nobody_negotiated_is_a_device_error() {
    let mut rig = misbehaving(Misbehave {
        header_flags: HDR_F_NEEDS_CSUM,
        ..Misbehave::default()
    });
    assert!(rig.deliver(&[0xAB; 64]), "a buffer was posted");
    assert_eq!(
        deliver(&mut rig),
        Err(DeviceError::Protocol(NetError::UnexpectedOffload {
            flags: HDR_F_NEEDS_CSUM,
            gso_type: 0
        }))
    );
    assert_broken(&mut rig);

    let mut segmented = misbehaving(Misbehave {
        header_gso: HDR_GSO_TCPV4,
        ..Misbehave::default()
    });
    assert!(segmented.deliver(&[0xAB; 64]), "a buffer was posted");
    assert!(matches!(
        deliver(&mut segmented),
        Err(DeviceError::Protocol(NetError::UnexpectedOffload { .. }))
    ));
    assert_broken(&mut segmented);
}

#[test]
fn a_frame_spread_over_buffers_that_were_never_merged_is_a_device_error() {
    let mut rig = misbehaving(Misbehave {
        num_buffers: Some(2),
        ..Misbehave::default()
    });
    assert!(rig.deliver(&[0xAB; 64]), "a buffer was posted");
    assert_eq!(
        deliver(&mut rig),
        Err(DeviceError::Protocol(NetError::MergedBuffers {
            num_buffers: 2
        }))
    );
    assert_broken(&mut rig);
}

#[test]
fn a_device_that_says_it_needs_a_reset_stops_the_driver() {
    let mut rig = Setup::new().build();
    rig.device.borrow_mut().misbehave.needs_reset = true;
    let mut out = [Event::Sent { id: u64::MAX }; 4];
    assert_eq!(
        rig.driver.on_interrupt(&mut out),
        Err(DeviceError::NeedsReset)
    );
    assert_eq!(rig.driver.fault(), Some(DeviceError::NeedsReset));
    assert_eq!(
        rig.driver.submit(&Frame {
            id: 1,
            offset: 0,
            len: 64
        }),
        Err(SubmitError::Broken)
    );
}

#[test]
fn a_shutdown_after_a_reset_gives_every_buffer_back() {
    let mut rig = Setup::new().build();
    // Two frames in flight, and one receive buffer in the caller's hands.
    for id in [11, 12] {
        assert_eq!(
            rig.driver.submit(&Frame {
                id,
                offset: 0,
                len: 64
            }),
            Ok(())
        );
    }
    assert!(rig.deliver(&[7; 100]));
    let events = rig.drain();
    assert_eq!(events.len(), 1, "the frames sent are not done yet");

    let Rig { driver, .. } = rig;
    match driver.shutdown() {
        Teardown::Released(released) => {
            let abandoned: Vec<u64> = released.abandoned().collect();
            assert_eq!(abandoned, vec![11, 12], "what was promised and not sent");
            // Both rings and every region come back, which is the whole point
            // of waiting for the reset.
            assert!(matches!(released.receive_rings, crate::Rings::Queue(_)));
            assert!(matches!(released.transmit_rings, crate::Rings::Queue(_)));
        }
        Teardown::Wedged(_) => panic!("the device reset, so the memory is free"),
    }
}

#[test]
fn a_device_that_will_not_reset_keeps_the_memory_for_good() {
    let mut setup = Setup::new();
    setup.misbehave.never_reset = true;
    setup.options.reset_polls = 4;
    let (_bus, _device, _regions, driver) = setup.try_build();
    // Bring-up itself fails, because the reset that starts the status protocol
    // never finishes.
    let failure = driver.expect_err("refused");
    assert_eq!(
        failure.error,
        InitError::Transport(TransportError::ResetTimedOut)
    );
    assert!(
        matches!(failure.teardown, Teardown::Wedged(_)),
        "memory a device may still write to is never freed"
    );
    free(failure.teardown);
}

/// A failed bring-up promised nobody anything, and its parts come back.
fn assert_nothing_abandoned(teardown: TestTeardown) {
    let released: &Released<_, _, _, _, Vec<Slot>> = match &teardown {
        Teardown::Released(released) => released,
        Teardown::Wedged(released) => released,
    };
    assert_eq!(released.abandoned().count(), 0);
    free(teardown);
}

/// A driver that has faulted accepts nothing more and has told the device.
fn assert_broken(rig: &mut Rig) {
    assert!(rig.driver.fault().is_some(), "the driver records the fault");
    assert!(failed(rig), "the device was told with FAILED");
    assert_eq!(
        rig.driver.submit(&Frame {
            id: 99,
            offset: 0,
            len: 64
        }),
        Err(SubmitError::Broken)
    );
}
