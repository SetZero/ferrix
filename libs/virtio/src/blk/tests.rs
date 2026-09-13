//! Tests for virtio-blk's device protocol.
//!
//! The configuration offsets and the header layout are checked against byte
//! positions written out by hand from Linux's `virtio_blk.h`, not against
//! this module's own constants, since a constant tested against itself proves
//! nothing. The chain tests read back what the device would see through
//! [`SplitQueueDevice`], because a descriptor that is right in the driver's
//! bookkeeping and wrong in the table is exactly the bug worth catching.

extern crate std;

use core::cell::RefCell;
use std::vec;
use std::vec::Vec;

use super::*;
use crate::{Descriptor, Layout, SplitQueueDevice};

/// Queue memory over a `Vec`, shared by the driver and device halves.
struct Mem(RefCell<Vec<u8>>);

// SAFETY: a `Vec` sized to the layout the queue is built with, so every offset
// is in range; nothing here dereferences the table as a struct, so its
// alignment is never relied on; driver and device are stepped one after the
// other, so the barrier has nothing to order.
unsafe impl QueueMemory for &Mem {
    fn read_u8(&self, offset: usize) -> u8 {
        self.0.borrow().get(offset).copied().unwrap_or(0)
    }
    fn write_u8(&mut self, offset: usize, value: u8) {
        if let Some(slot) = self.0.borrow_mut().get_mut(offset) {
            *slot = value;
        }
    }
    fn barrier(&self) {}
}

/// A configuration with every field set to something distinct.
fn full_config() -> Config {
    Config {
        capacity: 0x0102_0304_0506_0708,
        size_max: Some(0x1111_2222),
        seg_max: Some(0x3333_4444),
        geometry: Some(Geometry {
            cylinders: 0x5566,
            heads: 0x77,
            sectors: 0x88,
        }),
        blk_size: Some(4096),
        topology: Some(Topology {
            physical_block_exp: 3,
            alignment_offset: 1,
            min_io_size: 0x99AA,
            opt_io_size: 0xBBCC_DDEE,
        }),
        writeback: Some(1),
        num_queues: Some(0x0203),
        discard: Some(RangeLimits {
            max_sectors: 0x1000,
            max_segments: 0x2000,
            sector_alignment: 0x3000,
        }),
        write_zeroes: Some(WriteZeroes {
            max_sectors: 0x4000,
            max_segments: 0x5000,
            may_unmap: 1,
        }),
        secure_erase: Some(RangeLimits {
            max_sectors: 0x6000,
            max_segments: 0x7000,
            sector_alignment: 0x8000,
        }),
        zoned: Some(Zoned {
            zone_sectors: 0x9000,
            max_open_zones: 0xA000,
            max_active_zones: 0xB000,
            max_append_sectors: 0xC000,
            write_granularity: 0xD000,
            model: 2,
        }),
    }
}

/// Every feature that guards a configuration field.
const ALL_FIELDS: u64 = FEATURE_SIZE_MAX
    | FEATURE_SEG_MAX
    | FEATURE_GEOMETRY
    | FEATURE_BLK_SIZE
    | FEATURE_TOPOLOGY
    | FEATURE_CONFIG_WCE
    | FEATURE_MQ
    | FEATURE_DISCARD
    | FEATURE_WRITE_ZEROES
    | FEATURE_SECURE_ERASE
    | FEATURE_ZONED;

#[test]
fn feature_bits_are_linuxs() {
    let bits = [
        (FEATURE_BARRIER, 0),
        (FEATURE_SIZE_MAX, 1),
        (FEATURE_SEG_MAX, 2),
        (FEATURE_GEOMETRY, 4),
        (FEATURE_RO, 5),
        (FEATURE_BLK_SIZE, 6),
        (FEATURE_SCSI, 7),
        (FEATURE_FLUSH, 9),
        (FEATURE_TOPOLOGY, 10),
        (FEATURE_CONFIG_WCE, 11),
        (FEATURE_MQ, 12),
        (FEATURE_DISCARD, 13),
        (FEATURE_WRITE_ZEROES, 14),
        (FEATURE_SECURE_ERASE, 16),
        (FEATURE_ZONED, 17),
    ];
    for (feature, bit) in bits {
        assert_eq!(feature, 1 << bit, "bit {bit}");
    }
}

#[test]
fn the_driver_takes_what_it_honours_and_nothing_that_obliges_it() {
    for taken in [
        FEATURE_SIZE_MAX,
        FEATURE_SEG_MAX,
        FEATURE_RO,
        FEATURE_BLK_SIZE,
        FEATURE_FLUSH,
        FEATURE_VERSION_1,
        FEATURE_ACCESS_PLATFORM,
    ] {
        assert_ne!(DRIVER_FEATURES & taken, 0, "{taken:#x} should be taken");
    }
    for declined in [
        FEATURE_BARRIER,
        FEATURE_SCSI,
        FEATURE_CONFIG_WCE,
        FEATURE_MQ,
        FEATURE_DISCARD,
        FEATURE_WRITE_ZEROES,
        FEATURE_SECURE_ERASE,
        FEATURE_ZONED,
        1 << 28, // VIRTIO_RING_F_INDIRECT_DESC
        1 << 29, // VIRTIO_RING_F_EVENT_IDX
        1 << 34, // VIRTIO_F_RING_PACKED
    ] {
        assert_eq!(
            DRIVER_FEATURES & declined,
            0,
            "{declined:#x} should be declined"
        );
    }
    assert_eq!(REQUIRED_FEATURES, FEATURE_VERSION_1);
}

#[test]
fn the_configuration_fields_sit_where_linux_puts_them() {
    let mut bytes = vec![0_u8; CONFIG_LEN as usize];
    full_config().encode(&mut bytes);
    // Positions from `struct virtio_blk_config`, counted by hand.
    assert_eq!(&bytes[0..8], &0x0102_0304_0506_0708_u64.to_le_bytes());
    assert_eq!(&bytes[8..12], &0x1111_2222_u32.to_le_bytes());
    assert_eq!(&bytes[12..16], &0x3333_4444_u32.to_le_bytes());
    assert_eq!(&bytes[16..20], &[0x66, 0x55, 0x77, 0x88]);
    assert_eq!(&bytes[20..24], &4096_u32.to_le_bytes());
    assert_eq!(&bytes[24..32], &[3, 1, 0xAA, 0x99, 0xEE, 0xDD, 0xCC, 0xBB]);
    assert_eq!(bytes[32], 1);
    assert_eq!(&bytes[34..36], &[0x03, 0x02]);
    assert_eq!(&bytes[36..40], &0x1000_u32.to_le_bytes());
    assert_eq!(&bytes[44..48], &0x3000_u32.to_le_bytes());
    assert_eq!(&bytes[48..52], &0x4000_u32.to_le_bytes());
    assert_eq!(bytes[56], 1);
    assert_eq!(&bytes[60..64], &0x6000_u32.to_le_bytes());
    assert_eq!(&bytes[68..72], &0x8000_u32.to_le_bytes());
    assert_eq!(&bytes[72..76], &0x9000_u32.to_le_bytes());
    assert_eq!(&bytes[88..92], &0xD000_u32.to_le_bytes());
    assert_eq!(bytes[92], 2);
}

#[test]
fn a_configuration_reads_back_what_was_encoded() {
    let mut bytes = vec![0_u8; CONFIG_LEN as usize];
    full_config().encode(&mut bytes);
    let config = Config::read(bytes.as_slice(), ALL_FIELDS).unwrap();
    assert_eq!(config, full_config());
}

#[test]
fn fields_whose_features_were_not_negotiated_are_absent() {
    let mut bytes = vec![0_u8; CONFIG_LEN as usize];
    full_config().encode(&mut bytes);
    let config = Config::read(bytes.as_slice(), FEATURE_BLK_SIZE).unwrap();
    assert_eq!(config.capacity, 0x0102_0304_0506_0708);
    assert_eq!(config.blk_size, Some(4096));
    assert_eq!(config.size_max, None);
    assert_eq!(config.seg_max, None);
    assert_eq!(config.geometry, None);
    assert_eq!(config.zoned, None);
}

#[test]
fn a_short_block_is_fine_until_a_negotiated_field_lies_past_it() {
    let mut bytes = vec![0_u8; 24];
    full_config().encode(&mut bytes);
    let config = Config::read(bytes.as_slice(), FEATURE_BLK_SIZE | FEATURE_SEG_MAX).unwrap();
    assert_eq!(config.blk_size, Some(4096));

    assert_eq!(
        Config::read(bytes.as_slice(), FEATURE_TOPOLOGY),
        Err(BlkError::ConfigTruncated {
            feature: FEATURE_TOPOLOGY
        })
    );
    assert_eq!(
        Config::read(&bytes[..23], FEATURE_BLK_SIZE),
        Err(BlkError::ConfigTruncated {
            feature: FEATURE_BLK_SIZE
        })
    );
    assert_eq!(
        Config::read(&bytes[..7], 0),
        Err(BlkError::ConfigTruncated { feature: 0 })
    );
}

#[test]
fn every_truncation_of_the_block_is_an_answer_not_a_panic() {
    let mut bytes = vec![0_u8; CONFIG_LEN as usize];
    full_config().encode(&mut bytes);
    for len in 0..=bytes.len() {
        let _ = Config::read(&bytes[..len], ALL_FIELDS);
    }
    let mut short = [0_u8; 3];
    full_config().encode(&mut short);
    assert_eq!(short, [8, 7, 6]);
}

#[test]
fn limits_take_the_block_size_linux_would() {
    let mut config = full_config();
    for good in [512, 1024, 2048, 4096] {
        config.blk_size = Some(good);
        assert_eq!(Limits::new(&config).unwrap().block_size, good);
    }
    for bad in [0, 1, 256, 511, 513, 1000, 8192] {
        config.blk_size = Some(bad);
        assert_eq!(Limits::new(&config), Err(BlkError::BadBlockSize(bad)));
    }
    config.blk_size = None;
    assert_eq!(Limits::new(&config).unwrap().block_size, SECTOR_SIZE);
}

#[test]
fn limits_read_absent_or_zero_segment_bounds_as_linux_does() {
    let mut config = full_config();
    config.seg_max = None;
    config.size_max = None;
    let limits = Limits::new(&config).unwrap();
    assert_eq!(limits.max_segments, 1);
    assert_eq!(limits.max_segment_size, u32::MAX);

    config.seg_max = Some(0);
    config.size_max = Some(0);
    let limits = Limits::new(&config).unwrap();
    assert_eq!(limits.max_segments, 1);
    assert_eq!(limits.max_segment_size, u32::MAX);

    config.seg_max = Some(100_000);
    config.size_max = Some(4096);
    let limits = Limits::new(&config).unwrap();
    assert_eq!(limits.max_segments, MAX_DATA_SEGMENTS as u32);
    assert_eq!(limits.max_segment_size, 4096);
}

#[test]
fn the_header_is_type_reserved_sector_little_endian() {
    let header = Header {
        kind: RequestType::Out,
        sector: 0x1122_3344_5566_7788,
    };
    assert_eq!(
        header.encode(),
        [
            1, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11
        ]
    );
    assert_eq!(Header::decode(header.encode()), Ok(header));
}

#[test]
fn request_types_are_linuxs_and_round_trip() {
    let codes = [
        (RequestType::In, 0),
        (RequestType::Out, 1),
        (RequestType::Flush, 4),
        (RequestType::GetId, 8),
        (RequestType::Discard, 11),
        (RequestType::WriteZeroes, 13),
        (RequestType::SecureErase, 14),
    ];
    for (kind, code) in codes {
        assert_eq!(kind.code(), code);
        assert_eq!(RequestType::from_code(code), Some(kind));
    }
    let mut bytes = [0_u8; 16];
    bytes[0] = 2; // VIRTIO_BLK_T_SCSI_CMD, legacy
    assert_eq!(Header::decode(bytes), Err(BlkError::UnknownRequestType(2)));
    assert!(RequestType::In.device_writes_data());
    assert!(RequestType::GetId.device_writes_data());
    assert!(!RequestType::Out.device_writes_data());
}

#[test]
fn only_the_three_defined_statuses_are_statuses() {
    assert_eq!(Status::from_byte(0), Ok(Status::Ok));
    assert_eq!(Status::from_byte(1), Ok(Status::IoError));
    assert_eq!(Status::from_byte(2), Ok(Status::Unsupported));
    for byte in 3..=u8::MAX {
        assert_eq!(Status::from_byte(byte), Err(BlkError::BadStatus(byte)));
    }
    for status in [Status::Ok, Status::IoError, Status::Unsupported] {
        assert_eq!(Status::from_byte(status.byte()), Ok(status));
    }
}

/// A region of `count` pages at device addresses that never follow on.
fn scattered(count: usize) -> Vec<u64> {
    (0..count as u64)
        .map(|page| 0x9000_0000 - page * 3 * PAGE_SIZE)
        .collect()
}

/// A region of `count` pages at consecutive device addresses.
fn consecutive(count: usize) -> Vec<u64> {
    (0..count as u64)
        .map(|page| 0x4000_0000 + page * PAGE_SIZE)
        .collect()
}

#[test]
fn a_page_aligned_request_over_scattered_pages_takes_one_descriptor_a_page() {
    let pages = scattered(4);
    let data = Data {
        pages: &pages,
        offset: 0,
        len: 3 * PAGE_SIZE,
    };
    let segments = plan(&data, true, 512, 126, u32::MAX).unwrap();
    assert_eq!(segments.bytes(), 3 * PAGE_SIZE);
    let buffers: Vec<_> = segments
        .buffers()
        .iter()
        .map(|b| (b.address, b.len))
        .collect();
    assert_eq!(
        buffers,
        vec![(pages[0], 4096), (pages[1], 4096), (pages[2], 4096)]
    );
    assert!(segments.buffers().iter().all(|b| b.device_writable));
}

#[test]
fn consecutive_device_addresses_merge_into_one_descriptor() {
    let pages = consecutive(4);
    let data = Data {
        pages: &pages,
        offset: 1024,
        len: 3 * PAGE_SIZE,
    };
    let segments = plan(&data, false, 512, 126, u32::MAX).unwrap();
    assert_eq!(segments.count(), 1);
    assert_eq!(segments.buffers()[0].address, pages[0] + 1024);
    assert_eq!(segments.buffers()[0].len, 3 * 4096);
    assert!(!segments.buffers()[0].device_writable);
}

#[test]
fn a_merge_stops_at_the_first_page_that_does_not_follow() {
    let mut pages = consecutive(4);
    pages[2] = 0x7000_0000;
    let data = Data {
        pages: &pages,
        offset: 0,
        len: 4 * PAGE_SIZE,
    };
    let segments = plan(&data, true, 512, 126, u32::MAX).unwrap();
    let buffers: Vec<_> = segments
        .buffers()
        .iter()
        .map(|b| (b.address, b.len))
        .collect();
    // Page 3 follows page 2's old address, not its new one.
    assert_eq!(
        buffers,
        vec![(pages[0], 8192), (pages[2], 4096), (pages[3], 4096)]
    );
}

#[test]
fn size_max_cuts_a_run_into_pieces() {
    let pages = consecutive(2);
    let data = Data {
        pages: &pages,
        offset: 0,
        len: 2 * PAGE_SIZE,
    };
    let segments = plan(&data, true, 512, 126, 3000).unwrap();
    let lengths: Vec<_> = segments.buffers().iter().map(|b| b.len).collect();
    assert_eq!(lengths, vec![3000, 3000, 2192]);
    assert_eq!(segments.buffers()[1].address, pages[0] + 3000);
}

#[test]
fn seg_max_takes_the_longest_whole_block_prefix() {
    let pages = scattered(8);
    // Starting mid-page, so page boundaries are not block boundaries.
    let data = Data {
        pages: &pages,
        offset: 100,
        len: 5 * 4096,
    };
    let segments = plan(&data, true, 4096, 2, u32::MAX).unwrap();
    // Two descriptors reach 3996 + 4096 bytes; one whole 4096 block of that.
    assert_eq!(segments.bytes(), 4096);
    let lengths: Vec<_> = segments.buffers().iter().map(|b| b.len).collect();
    assert_eq!(lengths, vec![3996, 100]);

    let rest = Data {
        offset: 100 + 4096,
        len: 4 * 4096,
        ..data
    };
    let segments = plan(&rest, true, 4096, 2, u32::MAX).unwrap();
    assert_eq!(segments.bytes(), 4096);
}

#[test]
fn a_block_that_cannot_fit_the_segments_allowed_is_unsplittable() {
    let pages = scattered(2);
    let data = Data {
        pages: &pages,
        offset: 4000,
        len: 512,
    };
    assert_eq!(
        plan(&data, true, 512, 1, u32::MAX),
        Err(BlkError::Unsplittable)
    );
    assert!(plan(&data, true, 512, 2, u32::MAX).is_ok());
}

#[test]
fn data_past_the_region_or_in_part_blocks_is_refused() {
    let pages = consecutive(2);
    let past = Data {
        pages: &pages,
        offset: 4096,
        len: 8192,
    };
    assert_eq!(
        plan(&past, true, 512, 126, u32::MAX),
        Err(BlkError::OutsideRegion)
    );
    let wrap = Data {
        pages: &pages,
        offset: u64::MAX,
        len: 512,
    };
    assert_eq!(
        plan(&wrap, true, 512, 126, u32::MAX),
        Err(BlkError::OutsideRegion)
    );
    let ragged = Data {
        pages: &pages,
        offset: 0,
        len: 700,
    };
    assert_eq!(
        plan(&ragged, true, 512, 126, u32::MAX),
        Err(BlkError::NotWholeUnits)
    );
}

#[test]
fn a_page_at_the_top_of_the_address_space_overflows_rather_than_wraps() {
    let pages = [u64::MAX - 100, 0];
    let data = Data {
        pages: &pages,
        offset: 0,
        len: 512,
    };
    assert_eq!(
        plan(&data, true, 512, 126, u32::MAX),
        Err(BlkError::AddressOverflow)
    );
    let top = [u64::MAX - PAGE_SIZE + 1];
    let data = Data {
        pages: &top,
        offset: 0,
        len: 4096,
    };
    assert_eq!(
        plan(&data, true, 512, 126, u32::MAX),
        Err(BlkError::AddressOverflow)
    );
}

#[test]
fn no_data_plans_to_no_segments() {
    let data = Data {
        pages: &[],
        offset: 0,
        len: 0,
    };
    let segments = plan(&data, false, 512, 126, u32::MAX).unwrap();
    assert_eq!(segments.count(), 0);
    assert_eq!(segments.bytes(), 0);
    assert_eq!(segments, Segments::none());
}

#[test]
fn the_most_segments_a_chain_takes_is_capped() {
    let pages = scattered(200);
    let data = Data {
        pages: &pages,
        offset: 0,
        len: 200 * PAGE_SIZE,
    };
    let segments = plan(&data, true, 512, usize::MAX, u32::MAX).unwrap();
    assert_eq!(segments.count(), MAX_DATA_SEGMENTS);
    assert_eq!(segments.bytes(), MAX_DATA_SEGMENTS as u64 * PAGE_SIZE);
}

/// An empty descriptor, for filling an output slice.
const BLANK: Descriptor = Descriptor {
    address: 0,
    len: 0,
    flags: 0,
    next: 0,
};

#[test]
fn a_read_chain_is_header_then_writable_data_then_status() {
    let layout = Layout::for_size(8).unwrap();
    let memory = Mem(RefCell::new(vec![0; layout.total_size]));
    let mut queue = SplitQueue::new(layout, &memory);
    let mut device = SplitQueueDevice::new(layout, &memory);

    let pages = scattered(2);
    let data = Data {
        pages: &pages,
        offset: 0,
        len: 2 * PAGE_SIZE,
    };
    let segments = plan(&data, true, 512, 126, u32::MAX).unwrap();
    let chain = publish(&mut queue, 0x1000, &segments, 0x2010).unwrap();
    assert_eq!(chain.descriptors, 4);
    assert_eq!(chain.data_len, 8192);
    assert_eq!(chain.writable, 8193);

    let head = device.next_chain().unwrap().unwrap();
    assert_eq!(head, chain.head);
    let mut read = [BLANK; 8];
    let count = device.read_chain(head, &mut read).unwrap();
    assert_eq!(count, 4);
    assert_eq!((read[0].address, read[0].len), (0x1000, 16));
    assert!(!read[0].is_device_writable());
    assert_eq!((read[1].address, read[1].len), (pages[0], 4096));
    assert_eq!((read[2].address, read[2].len), (pages[1], 4096));
    assert!(read[1].is_device_writable() && read[2].is_device_writable());
    assert_eq!((read[3].address, read[3].len), (0x2010, 1));
    assert!(read[3].is_device_writable());
}

#[test]
fn a_write_chain_has_readable_data_and_only_the_status_writable() {
    let layout = Layout::for_size(8).unwrap();
    let memory = Mem(RefCell::new(vec![0; layout.total_size]));
    let mut queue = SplitQueue::new(layout, &memory);
    let pages = consecutive(1);
    let data = Data {
        pages: &pages,
        offset: 0,
        len: 512,
    };
    let segments = plan(&data, false, 512, 126, u32::MAX).unwrap();
    let chain = publish(&mut queue, 0x1000, &segments, 0x2000).unwrap();
    assert_eq!(chain.writable, 1);
    assert_eq!(chain.data_len, 512);

    let flush = publish(&mut queue, 0x1010, &Segments::none(), 0x2001);
    assert_eq!(flush.unwrap().descriptors, 2);
    assert_eq!(queue.free_descriptors(), 3);
}

#[test]
fn a_chain_the_queue_cannot_hold_is_refused_without_side_effects() {
    let layout = Layout::for_size(4).unwrap();
    let memory = Mem(RefCell::new(vec![0; layout.total_size]));
    let mut queue = SplitQueue::new(layout, &memory);
    let pages = scattered(4);
    let data = Data {
        pages: &pages,
        offset: 0,
        len: 3 * PAGE_SIZE,
    };
    let segments = plan(&data, true, 512, 126, u32::MAX).unwrap();
    assert_eq!(
        publish(&mut queue, 0x1000, &segments, 0x2000),
        Err(BlkError::Queue(QueueError::ChainTooLong))
    );
    assert_eq!(queue.free_descriptors(), 4);
    assert_eq!(
        publish(&mut queue, u64::MAX - 3, &Segments::none(), 0x2000),
        Err(BlkError::AddressOverflow)
    );
    assert_eq!(
        publish(&mut queue, 0x1000, &Segments::none(), u64::MAX),
        Err(BlkError::AddressOverflow)
    );
    assert_eq!(queue.free_descriptors(), 4);
}

#[test]
fn a_completion_is_bounded_by_the_chain_and_its_status_defined() {
    let chain = Chain {
        head: 0,
        descriptors: 3,
        data_len: 512,
        writable: 513,
    };
    assert_eq!(parse_completion(&chain, 513, 0), Ok(Status::Ok));
    assert_eq!(parse_completion(&chain, 0, 1), Ok(Status::IoError));
    assert_eq!(
        parse_completion(&chain, 514, 0),
        Err(BlkError::WrittenTooLong {
            written: 514,
            writable: 513
        })
    );
    assert_eq!(
        parse_completion(&chain, 1, 0xFF),
        Err(BlkError::BadStatus(0xFF))
    );
}

#[test]
fn errors_describe_themselves() {
    use std::string::ToString;
    let errors = [
        BlkError::ConfigTruncated { feature: 64 },
        BlkError::BadBlockSize(3),
        BlkError::UnknownRequestType(99),
        BlkError::BadStatus(9),
        BlkError::WrittenTooLong {
            written: 2,
            writable: 1,
        },
        BlkError::OutsideRegion,
        BlkError::AddressOverflow,
        BlkError::NotWholeUnits,
        BlkError::Unsplittable,
        BlkError::Queue(QueueError::OutOfDescriptors),
    ];
    for error in errors {
        assert!(!error.to_string().is_empty());
    }
}
