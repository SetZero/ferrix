//! The driver against a device that behaves.

use std::vec;
use std::vec::Vec;

use ferrix_virtio::blk::{
    FEATURE_BLK_SIZE, FEATURE_CONFIG_WCE, FEATURE_DISCARD, FEATURE_FLUSH, FEATURE_MQ, FEATURE_RO,
    FEATURE_SEG_MAX, FEATURE_SIZE_MAX, FEATURE_WRITE_ZEROES,
};
use ferrix_virtio::pci::{FEATURE_ACCESS_PLATFORM, FEATURE_VERSION_1, STATUS_FAILED};

use super::fake::{PAGE, Rig, Setup};
use crate::{
    Accepted, Completion, DevicePages, InitError, Op, Rings, Status, SubmitError, Teardown,
};

/// Bytes nobody would mistake for zeros or for another request's.
fn pattern(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

/// A successful completion.
const fn ok(id: u64, bytes: u64) -> Completion {
    Completion {
        id,
        status: Status::Ok,
        bytes,
    }
}

#[test]
fn bring_up_follows_the_status_protocol_and_takes_only_the_features_it_honours() {
    let rig = Setup::new().build();
    let device = rig.device.borrow();
    // Reset, ACKNOWLEDGE, DRIVER, FEATURES_OK, DRIVER_OK.
    assert_eq!(device.status_writes, vec![0, 1, 3, 11, 15]);
    assert_eq!(
        device.accepted,
        FEATURE_VERSION_1
            | FEATURE_ACCESS_PLATFORM
            | FEATURE_SEG_MAX
            | FEATURE_BLK_SIZE
            | FEATURE_FLUSH
    );
    for declined in [
        FEATURE_CONFIG_WCE,
        FEATURE_MQ,
        FEATURE_DISCARD,
        FEATURE_WRITE_ZEROES,
    ] {
        assert_eq!(device.accepted & declined, 0);
    }
    let (size, _, _, _, vector, enabled) = device.queue();
    assert_eq!((size, vector, enabled), (64, 1, true));
    drop(device);

    let info = rig.driver.info();
    assert_eq!(info.queue_size, 64);
    assert_eq!(info.vector, 1);
    assert!(info.flush && !info.read_only);
    assert_eq!(info.limits.max_segments, 62);
    assert_eq!(info.config.capacity, 256);
    assert!(rig.driver.is_idle());
    rig.assert_clean();
}

#[test]
fn writes_read_back_through_scattered_pages() {
    let mut rig = Setup::new().build();
    let bytes = pattern(7, 3 * PAGE);
    rig.data.write(0, &bytes);
    assert_eq!(
        rig.submit(1, Op::Write, 16, 24, 0),
        Ok(Accepted::Queued { chains: 1 })
    );
    assert_eq!(rig.complete_all(), vec![ok(1, 3 * 4096)]);
    assert_eq!(
        &rig.device.borrow().disk[16 * 512..16 * 512 + 3 * PAGE],
        &bytes[..]
    );

    assert_eq!(
        rig.submit(2, Op::Read, 16, 24, 4 * PAGE as u64),
        Ok(Accepted::Queued { chains: 1 })
    );
    assert_eq!(rig.complete_all(), vec![ok(2, 3 * 4096)]);
    assert_eq!(rig.data.read(4 * PAGE, 3 * PAGE), bytes);

    // One descriptor a page, each at that page's device address.
    let device = rig.device.borrow();
    let pages = rig.data.device_pages();
    let addresses: Vec<_> = device.chains[1]
        .iter()
        .map(|d| (d.address, d.len))
        .collect();
    assert_eq!(
        addresses,
        vec![(pages[4], 4096), (pages[5], 4096), (pages[6], 4096)]
    );
    drop(device);
    assert!(rig.driver.is_idle());
    rig.assert_clean();
}

#[test]
fn consecutive_device_addresses_become_one_segment() {
    let mut setup = Setup::new();
    setup.data_scattered = false;
    let mut rig = setup.build();
    rig.data.write(0, &pattern(1, 3 * PAGE));
    let _ = rig.submit(1, Op::Write, 0, 24, 0).unwrap();
    assert_eq!(rig.complete_all(), vec![ok(1, 3 * 4096)]);
    let device = rig.device.borrow();
    assert_eq!(device.chains[0].len(), 1);
    assert_eq!(device.chains[0][0].address, rig.data.device_pages()[0]);
    assert_eq!(device.chains[0][0].len, 3 * 4096);
    drop(device);
    rig.assert_clean();
}

#[test]
fn a_request_starting_mid_page_is_cut_at_the_page_boundary() {
    let mut rig = Setup::new().build();
    let bytes = pattern(3, 4096);
    rig.data.write(1024, &bytes);
    let _ = rig.submit(1, Op::Write, 8, 8, 1024).unwrap();
    assert_eq!(rig.complete_all(), vec![ok(1, 4096)]);
    let device = rig.device.borrow();
    let pages = rig.data.device_pages();
    let segments: Vec<_> = device.chains[0]
        .iter()
        .map(|d| (d.address, d.len))
        .collect();
    assert_eq!(segments, vec![(pages[0] + 1024, 3072), (pages[1], 1024)]);
    assert_eq!(&device.disk[8 * 512..8 * 512 + 4096], &bytes[..]);
    drop(device);
    rig.assert_clean();
}

#[test]
fn seg_max_splits_a_request_into_chains_that_complete_as_one() {
    let mut setup = Setup::new();
    setup.config.seg_max = Some(2);
    let mut rig = setup.build();
    let bytes = pattern(9, 8 * PAGE);
    rig.data.write(0, &bytes);
    assert_eq!(
        rig.submit(5, Op::Write, 64, 64, 0),
        Ok(Accepted::Queued { chains: 4 })
    );
    assert_eq!(rig.complete_all(), vec![ok(5, 8 * 4096)]);
    assert_eq!(
        &rig.device.borrow().disk[64 * 512..64 * 512 + 8 * PAGE],
        &bytes[..]
    );

    rig.data.write(8 * PAGE, &vec![0; 8 * PAGE]);
    assert_eq!(
        rig.submit(6, Op::Read, 64, 64, 8 * PAGE as u64),
        Ok(Accepted::Queued { chains: 4 })
    );
    assert_eq!(rig.complete_all(), vec![ok(6, 8 * 4096)]);
    assert_eq!(rig.data.read(8 * PAGE, 8 * PAGE), bytes);
    assert!(
        rig.device
            .borrow()
            .chains
            .iter()
            .all(|chain| chain.len() <= 2)
    );
    assert!(rig.driver.is_idle());
    rig.assert_clean();
}

#[test]
fn size_max_bounds_every_descriptor() {
    let mut setup = Setup::new();
    setup.offered |= FEATURE_SIZE_MAX;
    setup.config.size_max = Some(1024);
    setup.data_scattered = false;
    let mut rig = setup.build();
    rig.data.write(0, &pattern(4, 2 * PAGE));
    let _ = rig.submit(1, Op::Write, 0, 16, 0).unwrap();
    assert_eq!(rig.complete_all(), vec![ok(1, 2 * 4096)]);
    let device = rig.device.borrow();
    let lengths: Vec<_> = device.chains[0].iter().map(|d| d.len).collect();
    assert_eq!(lengths, vec![1024; 8]);
    drop(device);
    rig.assert_clean();
}

#[test]
fn a_flush_goes_to_the_device() {
    let mut rig = Setup::new().build();
    assert_eq!(
        rig.submit(3, Op::Flush, 99, 99, 99),
        Ok(Accepted::Queued { chains: 1 })
    );
    assert_eq!(rig.complete_all(), vec![ok(3, 0)]);
    assert_eq!(rig.device.borrow().flushes, 1);
    rig.assert_clean();
}

#[test]
fn a_flush_to_a_device_without_a_cache_completes_at_once() {
    let mut setup = Setup::new();
    setup.offered &= !FEATURE_FLUSH;
    let mut rig = setup.build();
    assert_eq!(
        rig.submit(3, Op::Flush, 0, 0, 0),
        Ok(Accepted::Completed(ok(3, 0)))
    );
    assert_eq!(rig.device.borrow().notifications, 0);
    assert!(rig.driver.is_idle());
}

#[test]
fn a_read_only_disk_refuses_writes_and_serves_reads() {
    let mut setup = Setup::new();
    setup.offered |= FEATURE_RO;
    let mut rig = setup.build();
    assert!(rig.driver.info().read_only);
    assert_eq!(
        rig.submit(1, Op::Write, 0, 8, 0),
        Err(SubmitError::ReadOnly)
    );
    assert_eq!(rig.device.borrow().notifications, 0);
    assert!(rig.driver.is_idle());
    let _ = rig.submit(2, Op::Read, 0, 8, 0).unwrap();
    assert_eq!(rig.complete_all(), vec![ok(2, 4096)]);
    rig.assert_clean();
}

#[test]
fn the_queue_fills_and_drains() {
    let mut setup = Setup::new();
    setup.queue_max = 8;
    let mut rig = setup.build();
    assert_eq!(rig.driver.info().queue_size, 8);
    // A one-page read is three descriptors; two fit in eight, a third does not.
    let _ = rig.submit(1, Op::Read, 0, 8, 0).unwrap();
    let _ = rig.submit(2, Op::Read, 8, 8, PAGE as u64).unwrap();
    assert_eq!(
        rig.submit(3, Op::Read, 16, 8, 2 * PAGE as u64),
        Err(SubmitError::QueueFull)
    );
    assert_eq!(rig.driver.free_descriptors(), 2);
    let mut done = rig.complete_all();
    done.sort_by_key(|completion| completion.id);
    assert_eq!(done, vec![ok(1, 4096), ok(2, 4096)]);
    assert_eq!(rig.driver.free_descriptors(), 8);
    let _ = rig.submit(3, Op::Read, 16, 8, 2 * PAGE as u64).unwrap();
    assert_eq!(rig.complete_all(), vec![ok(3, 4096)]);
    assert!(rig.driver.is_idle());
    rig.assert_clean();
}

#[test]
fn a_request_no_empty_queue_could_hold_is_too_large() {
    let mut setup = Setup::new();
    setup.queue_max = 8;
    setup.config.seg_max = Some(1);
    let mut rig = setup.build();
    // Four scattered pages, one segment a chain: twelve descriptors.
    assert_eq!(
        rig.submit(1, Op::Read, 0, 32, 0),
        Err(SubmitError::TooLarge)
    );
    assert!(rig.driver.is_idle());
}

#[test]
fn completions_come_back_in_the_order_the_device_chose() {
    let mut rig = Setup::new().build();
    for (index, id) in [10_u64, 11, 12].into_iter().enumerate() {
        let _ = rig
            .submit(id, Op::Read, index as u64 * 8, 8, (index * PAGE) as u64)
            .unwrap();
    }
    {
        let mut device = rig.device.borrow_mut();
        device.take_available();
        device.complete(2);
        device.complete(0);
        device.complete(0);
    }
    let ids: Vec<_> = rig.drain().iter().map(|completion| completion.id).collect();
    assert_eq!(ids, vec![12, 10, 11]);
    assert!(rig.driver.is_idle());
    rig.assert_clean();
}

#[test]
fn a_short_slice_leaves_the_rest_for_the_next_call() {
    let mut rig = Setup::new().build();
    for id in 0..3_u64 {
        let _ = rig
            .submit(id, Op::Read, id * 8, 8, id * PAGE as u64)
            .unwrap();
    }
    let _ = rig.run_device();
    let mut out = [ok(99, 0); 1];
    for id in 0..3_u64 {
        let drained = rig.driver.on_interrupt(&mut out).unwrap();
        assert_eq!(drained.completions, 1);
        assert_eq!(out[0].id, id);
        assert_eq!(drained.more, id < 2);
    }
    assert!(rig.driver.is_idle());
}

#[test]
fn requests_off_the_disk_the_blocks_or_the_data_are_refused_untracked() {
    let mut setup = Setup::new();
    setup.config.blk_size = Some(4096);
    let mut rig = setup.build();
    let cases = [
        ((0, 0, 0), SubmitError::Empty),
        ((0, 1, 0), SubmitError::NotAligned),
        ((1, 8, 0), SubmitError::NotAligned),
        ((256, 8, 0), SubmitError::OutOfRange),
        ((248, 16, 0), SubmitError::OutOfRange),
        ((0xFFFF_FFFF_FFFF_FFF8, 16, 0), SubmitError::OutOfRange),
        ((0, 16, 15 * PAGE as u64), SubmitError::OutsideData),
        ((0, 8, u64::MAX), SubmitError::OutsideData),
    ];
    for ((sector, count, offset), error) in cases {
        assert_eq!(
            rig.submit(1, Op::Read, sector, count, offset),
            Err(error),
            "sector {sector} count {count} offset {offset}"
        );
    }
    assert_eq!(rig.device.borrow().notifications, 0);
    assert!(rig.driver.is_idle());
    let _ = rig.submit(1, Op::Read, 248, 8, 15 * PAGE as u64).unwrap();
    assert_eq!(rig.complete_all(), vec![ok(1, 4096)]);
    rig.assert_clean();
}

#[test]
fn a_chain_the_device_fails_ends_the_request_at_that_chain() {
    let mut setup = Setup::new();
    setup.config.seg_max = Some(1);
    setup.misbehave.fail_from_sector = Some(16);
    let mut rig = setup.build();
    assert_eq!(
        rig.submit(1, Op::Write, 0, 32, 0),
        Ok(Accepted::Queued { chains: 4 })
    );
    assert_eq!(
        rig.complete_all(),
        vec![Completion {
            id: 1,
            status: Status::IoError,
            bytes: 2 * 4096
        }]
    );
    // An I/O error is an answer, not a broken device.
    assert_eq!(rig.driver.fault(), None);
    let _ = rig.submit(2, Op::Read, 0, 8, 0).unwrap();
    assert_eq!(rig.complete_all(), vec![ok(2, 4096)]);
    rig.assert_clean();
}

#[test]
fn shutdown_resets_before_handing_back_the_memory() {
    let mut rig = Setup::new().build();
    let _ = rig.submit(7, Op::Read, 0, 8, 0).unwrap();
    let Rig { device, driver, .. } = rig;
    match driver.shutdown() {
        Teardown::Released(released) => {
            assert!(matches!(released.rings, Rings::Queue(_)));
            assert_eq!(released.abandoned().collect::<Vec<_>>(), vec![7]);
        }
        Teardown::Wedged(_) => panic!("the device reset"),
    }
    let device = device.borrow();
    assert_eq!(device.status_writes.last(), Some(&0));
    assert_eq!(device.status(), 0);
}

#[test]
fn a_new_capacity_is_taken_when_asked() {
    let mut rig = Setup::new().build();
    assert_eq!(
        rig.submit(1, Op::Read, 300, 8, 0),
        Err(SubmitError::OutOfRange)
    );
    rig.device.borrow_mut().config[..8].copy_from_slice(&512_u64.to_le_bytes());
    assert_eq!(rig.driver.refresh_config(), Ok(512));
    let _ = rig.submit(1, Op::Read, 248, 8, 0).unwrap();
    assert_eq!(rig.driver.info().limits.capacity, 512);
}

#[test]
fn rings_over_scattered_pages_get_a_queue_whose_areas_each_fit_a_page() {
    let mut setup = Setup::new();
    setup.queue_max = 512;
    setup.slots = 512;
    setup.options.max_queue_size = 512;
    setup.ring_pages = 4;
    setup.area_pages = 3;
    let mut rig = setup.build();
    // 512 entries need an eight-kilobyte descriptor table across two pages
    // whose addresses do not follow on; 256 entries fit each area in one.
    assert_eq!(rig.driver.info().queue_size, 256);
    let _ = rig.submit(1, Op::Read, 0, 8, 0).unwrap();
    assert_eq!(rig.complete_all(), vec![ok(1, 4096)]);
    rig.assert_clean();

    setup.ring_scattered = false;
    let mut rig = setup.build();
    assert_eq!(rig.driver.info().queue_size, 512);
    let _ = rig.submit(1, Op::Read, 0, 8, 0).unwrap();
    assert_eq!(rig.complete_all(), vec![ok(1, 4096)]);
    rig.assert_clean();
}

#[test]
fn too_little_memory_for_any_queue_fails_the_device_and_resets_it() {
    for shrink in [0, 1] {
        let mut setup = Setup::new();
        if shrink == 0 {
            setup.area_pages = 0;
        } else {
            setup.slots = 2;
        }
        let (_, device, _, result) = setup.try_build();
        let failure = result.expect_err("no queue fits");
        assert_eq!(failure.error, InitError::NoRoom);
        assert!(matches!(failure.teardown, Teardown::Released(_)));
        let device = device.borrow();
        assert!(device.status_writes.iter().any(|s| s & STATUS_FAILED != 0));
        assert_eq!(device.status_writes.last(), Some(&0));
    }
}
