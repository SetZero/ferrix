//! The device description, region checks and the descriptor arithmetic.

use crate::geometry::{
    Device, DeviceError, DeviceFlags, descriptors_for, max_sectors, pages_spanned,
    region_in_data_vmo,
};

#[test]
fn a_device_description_is_checked() {
    let new = |block, max, flags| Device::new(block, 100, max, DeviceFlags(flags), 4096);
    assert!(new(512, 1, 0).is_ok(), "the smallest block");
    assert!(new(65536, 1, 7).is_ok(), "the largest block, every flag");
    for block in [0, 511, 768, 1 << 17] {
        assert_eq!(
            new(block, 1, 0),
            Err(DeviceError::BlockSize),
            "block {block}"
        );
    }
    assert_eq!(new(512, 0, 0), Err(DeviceError::NoSectors), "no sectors");
    assert_eq!(new(512, 1, 8), Err(DeviceError::UnknownFlags), "flag 8");
    let device = new(65536, 1, 0).expect("valid");
    assert_eq!(
        device.payload_len(u32::MAX),
        u64::from(u32::MAX) * 65536,
        "no overflow"
    );
}

#[test]
fn a_region_must_end_inside_the_data_vmo_without_overflowing() {
    assert!(region_in_data_vmo(0, 10, 10), "exactly fills");
    assert!(!region_in_data_vmo(1, 10, 10), "one past");
    assert!(region_in_data_vmo(10, 0, 10), "empty at the end");
    assert!(
        !region_in_data_vmo(u64::MAX, 1, u64::MAX),
        "overflowing end"
    );
    assert!(
        !region_in_data_vmo(2, u64::MAX - 1, u64::MAX),
        "overflowing length"
    );
}

#[test]
fn pages_spanned_counts_every_page_a_region_touches() {
    let cases = [
        ((0, 0), Some(0)),
        ((0, 1), Some(1)),
        ((0, 4096), Some(1)),
        ((1, 4096), Some(2)),
        ((4095, 2), Some(2)),
        ((4096, 4097), Some(2)),
        ((100, 3 * 4096), Some(4)),
        ((u64::MAX, 2), None),
    ];
    for ((offset, len), pages) in cases {
        assert_eq!(pages_spanned(offset, len, 4096), pages, "{offset} + {len}");
    }
    assert_eq!(pages_spanned(0, 1, 0), None, "page size zero");
    assert_eq!(
        pages_spanned(0, 1, 3000),
        None,
        "page size not a power of two"
    );
    assert_eq!(
        descriptors_for(4095, 2, 4096),
        Some(4),
        "header, two pages, status"
    );
    assert_eq!(
        descriptors_for(0, 0, 4096),
        Some(2),
        "a flush: header and status"
    );
}

#[test]
fn max_sectors_fits_every_placement_and_one_more_does_not() {
    let page = 16_u32;
    let queues = if cfg!(miri) { 0..=5_u16 } else { 0..=12 };
    for queue_size in queues {
        for seg_max in 0..=8_u32 {
            for block in [1_u32, 2, 4, 16, 32, 64] {
                let most = max_sectors(seg_max, queue_size, page, block);
                let fits = |sectors| fits(sectors, block, page, queue_size, seg_max);
                let case = std::format!("queue {queue_size} seg_max {seg_max} block {block}");
                assert!(
                    most == 0 || fits(u64::from(most)),
                    "{case}: {most} does not fit"
                );
                assert!(!fits(u64::from(most) + 1), "{case}: {most} + 1 fits too");
            }
        }
    }
}

/// Whether `sectors` sectors fit the queue and `seg_max` wherever the region
/// starts within a page.
fn fits(sectors: u64, block: u32, page: u32, queue_size: u16, seg_max: u32) -> bool {
    (0..u64::from(page)).all(|offset| {
        let len = sectors * u64::from(block);
        let descriptors = descriptors_for(offset, len, u64::from(page)).expect("small numbers");
        descriptors <= u64::from(queue_size) && descriptors - 2 <= u64::from(seg_max)
    })
}

#[test]
fn max_sectors_for_real_devices() {
    // 254 data descriptors in a 256-entry queue, each at most a page of which
    // the first may be one byte: 253 pages and a byte, 2024 sectors of 512.
    assert_eq!(max_sectors(u32::MAX, 256, 4096, 512), 2024, "queue-bound");
    assert_eq!(max_sectors(126, 256, 4096, 512), 1000, "seg_max-bound");
    assert_eq!(max_sectors(u32::MAX, 256, 4096, 4096), 253, "4 KiB sectors");
    assert_eq!(max_sectors(u32::MAX, 2, 4096, 512), 0, "no room for data");
    assert_eq!(
        max_sectors(u32::MAX, 256, 4095, 512),
        0,
        "page size not a power of two"
    );
    assert_eq!(max_sectors(u32::MAX, 256, 4096, 0), 0, "block size zero");
}
