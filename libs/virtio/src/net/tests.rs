//! Tests for virtio-net's device protocol.
//!
//! The configuration offsets and the header layout are checked against byte
//! positions written out by hand from Linux's `virtio_net.h`, not against this
//! module's own constants, since a constant tested against itself proves
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

/// Every feature the configuration has a field for.
const EVERY_FIELD: u64 =
    FEATURE_MAC | FEATURE_STATUS | FEATURE_MQ | FEATURE_MTU | FEATURE_SPEED_DUPLEX;

/// A configuration with every field set to something distinct.
fn full_config() -> Config {
    Config {
        mac: Some([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]),
        status: Some(STATUS_LINK_UP),
        max_virtqueue_pairs: Some(3),
        mtu: Some(9000),
        speed: Some(10_000),
        duplex: Some(DUPLEX_FULL),
    }
}

#[test]
fn the_configuration_fields_sit_where_the_specification_puts_them() {
    let mut bytes = vec![0_u8; CONFIG_LEN as usize];
    full_config().encode(&mut bytes);
    // `struct virtio_net_config`, packed: six bytes of MAC, then three
    // sixteen-bit fields, then a thirty-two bit speed and a byte of duplex.
    assert_eq!(&bytes[0..6], &[0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    assert_eq!(&bytes[6..8], &1_u16.to_le_bytes());
    assert_eq!(&bytes[8..10], &3_u16.to_le_bytes());
    assert_eq!(&bytes[10..12], &9000_u16.to_le_bytes());
    assert_eq!(&bytes[12..16], &10_000_u32.to_le_bytes());
    assert_eq!(bytes[16], DUPLEX_FULL);
    assert_eq!(CONFIG_LEN, 24);
}

#[test]
fn a_configuration_reads_back_exactly_what_was_encoded() {
    let mut bytes = vec![0_u8; CONFIG_LEN as usize];
    let config = full_config();
    config.encode(&mut bytes);
    assert_eq!(
        Config::read(bytes.as_slice(), EVERY_FIELD),
        Ok(config),
        "every field, with every feature negotiated"
    );
}

#[test]
fn a_field_whose_feature_was_not_negotiated_is_not_read() {
    let mut bytes = vec![0_u8; CONFIG_LEN as usize];
    full_config().encode(&mut bytes);
    let config = Config::read(bytes.as_slice(), FEATURE_MAC).expect("a configuration");
    assert_eq!(config.mac, Some([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]));
    assert_eq!(config.status, None);
    assert_eq!(config.mtu, None);
    assert_eq!(config.speed, None);
    assert_eq!(config.duplex, None);
    assert_eq!(config.max_virtqueue_pairs, None);
}

#[test]
fn a_block_too_short_for_a_negotiated_field_is_an_error_rather_than_a_zero() {
    let bytes = vec![0_u8; 8];
    // Eight bytes hold the MAC and the status, and nothing after them.
    assert!(Config::read(bytes.as_slice(), FEATURE_MAC | FEATURE_STATUS).is_ok());
    for (feature, len) in [
        (FEATURE_MAC, 5),
        (FEATURE_STATUS, 7),
        (FEATURE_MQ, 9),
        (FEATURE_MTU, 11),
        (FEATURE_SPEED_DUPLEX, 16),
    ] {
        let short = vec![0_u8; len];
        assert_eq!(
            Config::read(short.as_slice(), feature),
            Err(NetError::ConfigTruncated { feature }),
            "feature {feature:#x} in {len} bytes"
        );
    }
}

#[test]
fn a_device_without_a_status_field_counts_as_a_link_that_is_up() {
    let mut config = full_config();
    config.status = None;
    assert!(config.link_up(), "no status field means no way to say down");
    config.status = Some(0);
    assert!(!config.link_up());
    config.status = Some(STATUS_LINK_UP | STATUS_ANNOUNCE);
    assert!(config.link_up());
}

#[test]
fn the_header_is_twelve_bytes_whenever_the_modern_interface_is_in_use() {
    // The rule the module documentation argues, each way round.
    assert_eq!(header_len(FEATURE_VERSION_1), 12, "modern, no merging");
    assert_eq!(
        header_len(FEATURE_VERSION_1 | FEATURE_MRG_RXBUF),
        12,
        "modern, merging"
    );
    assert_eq!(header_len(FEATURE_MRG_RXBUF), 12, "legacy, merging");
    assert_eq!(header_len(0), 10, "legacy, no merging");
    assert_eq!(
        header_len(DRIVER_FEATURES),
        12,
        "what this driver ends up with"
    );
}

#[test]
fn a_header_encodes_to_the_field_order_the_specification_gives() {
    let header = Header {
        flags: HDR_F_DATA_VALID,
        gso_type: HDR_GSO_TCPV4,
        hdr_len: 0x1234,
        gso_size: 0x5678,
        csum_start: 0x9ABC,
        csum_offset: 0xDEF0,
        num_buffers: 2,
    };
    assert_eq!(
        header.encode(),
        [
            HDR_F_DATA_VALID,
            HDR_GSO_TCPV4,
            0x34,
            0x12,
            0x78,
            0x56,
            0xBC,
            0x9A,
            0xF0,
            0xDE,
            0x02,
            0x00,
        ]
    );
}

#[test]
fn a_header_parses_back_to_itself_under_both_lengths() {
    let header = Header {
        flags: HDR_F_DATA_VALID,
        gso_type: HDR_GSO_NONE,
        hdr_len: 54,
        gso_size: 1460,
        csum_start: 34,
        csum_offset: 6,
        num_buffers: 1,
    };
    let bytes = header.encode();
    assert_eq!(Header::decode(&bytes, FEATURE_VERSION_1), Ok(header));
    // The legacy header stops before `num_buffers`, which reads as the one
    // buffer a device without merging must use.
    assert_eq!(Header::decode(&bytes[..10], 0), Ok(header));
    assert_eq!(
        Header::decode(&bytes[..10], FEATURE_VERSION_1),
        Err(NetError::HeaderTruncated {
            have: 10,
            needed: 12
        }),
        "ten bytes are not a modern header"
    );
}

#[test]
fn a_received_header_claiming_an_offload_nobody_negotiated_is_refused() {
    let plain = Header::plain();
    assert_eq!(plain.check_received(DRIVER_FEATURES), Ok(()));

    let mut needs_csum = plain;
    needs_csum.flags = HDR_F_NEEDS_CSUM;
    assert_eq!(
        needs_csum.check_received(DRIVER_FEATURES),
        Err(NetError::UnexpectedOffload {
            flags: HDR_F_NEEDS_CSUM,
            gso_type: HDR_GSO_NONE
        })
    );
    assert_eq!(
        needs_csum.check_received(DRIVER_FEATURES | FEATURE_GUEST_CSUM),
        Ok(()),
        "the same header is fine once the feature is negotiated"
    );

    let mut segmented = plain;
    segmented.gso_type = HDR_GSO_TCPV6;
    assert!(segmented.check_received(DRIVER_FEATURES).is_err());
    assert_eq!(
        segmented.check_received(DRIVER_FEATURES | FEATURE_GUEST_TSO6),
        Ok(())
    );

    // `DATA_VALID` is the device saying it checked a checksum, which asks the
    // driver for nothing.
    let mut checked = plain;
    checked.flags = HDR_F_DATA_VALID;
    assert_eq!(checked.check_received(DRIVER_FEATURES), Ok(()));
}

#[test]
fn a_frame_spread_over_buffers_the_driver_did_not_agree_to_merge_is_refused() {
    let mut header = Header::plain();
    header.num_buffers = 2;
    assert_eq!(
        header.check_received(DRIVER_FEATURES),
        Err(NetError::MergedBuffers { num_buffers: 2 })
    );
    assert_eq!(
        header.check_received(DRIVER_FEATURES | FEATURE_MRG_RXBUF),
        Ok(())
    );
    // Zero is a device that never wrote the field, which is one buffer.
    header.num_buffers = 0;
    assert_eq!(header.check_received(DRIVER_FEATURES), Ok(()));
}

#[test]
fn the_limits_follow_the_advised_mtu_and_the_header_length() {
    let mut config = full_config();
    config.mtu = None;
    let limits = Limits::new(&config, DRIVER_FEATURES).expect("limits");
    assert_eq!(limits.mtu, DEFAULT_MTU);
    assert_eq!(limits.frame_capacity, 1514);
    assert_eq!(limits.header_len, 12);
    assert_eq!(limits.buffer_len, 1526);

    config.mtu = Some(9000);
    let jumbo = Limits::new(&config, DRIVER_FEATURES).expect("limits");
    assert_eq!((jumbo.frame_capacity, jumbo.buffer_len), (9014, 9026));

    config.mtu = Some(MIN_MTU - 1);
    assert_eq!(
        Limits::new(&config, DRIVER_FEATURES),
        Err(NetError::BadMtu(MIN_MTU - 1))
    );
}

/// Four pages, the middle two consecutive and the rest scattered.
const PAGES: [u64; 4] = [0x1_0000_0000, 0x1_0000_1000, 0x7_0000_0000, 0x3_0000_0000];

#[test]
fn consecutive_device_addresses_become_one_descriptor() {
    let data = Data {
        pages: &PAGES,
        offset: 0,
        len: 8192,
    };
    let segments = plan(&data, false).expect("a plan");
    assert_eq!(segments.count(), 1);
    assert_eq!(segments.bytes(), 8192);
    assert_eq!(segments.buffers(), &[Buffer::readable(PAGES[0], 8192)]);
}

#[test]
fn a_frame_crossing_pages_that_do_not_follow_on_is_cut_at_the_boundary() {
    let data = Data {
        pages: &PAGES,
        offset: 4096 + 3000,
        len: 2000,
    };
    let segments = plan(&data, true).expect("a plan");
    assert_eq!(
        segments.buffers(),
        &[
            Buffer::writable(PAGES[1] + 3000, 1096),
            Buffer::writable(PAGES[2], 904),
        ]
    );
    assert_eq!(segments.bytes(), 2000);
}

#[test]
fn a_frame_outside_its_region_or_of_no_bytes_is_refused() {
    let empty = Data {
        pages: &PAGES,
        offset: 0,
        len: 0,
    };
    assert_eq!(plan(&empty, false), Err(NetError::EmptyFrame));

    let past = Data {
        pages: &PAGES,
        offset: 4 * 4096 - 10,
        len: 20,
    };
    assert_eq!(plan(&past, false), Err(NetError::OutsideRegion));

    let none = Data {
        pages: &[],
        offset: 0,
        len: 64,
    };
    assert_eq!(plan(&none, false), Err(NetError::OutsideRegion));
}

#[test]
fn a_frame_scattered_over_more_pages_than_a_chain_can_name_is_refused() {
    // Twenty pages, none of them following on from the last.
    let pages: Vec<u64> = (0..20).map(|page| 0x1000_0000 + page * 0x2000).collect();
    let data = Data {
        pages: &pages,
        offset: 0,
        len: 20 * 4096,
    };
    assert_eq!(plan(&data, false), Err(NetError::TooManySegments));
}

/// A queue of eight descriptors over a `Vec`, and the device's end of it.
fn queue(memory: &Mem) -> (SplitQueue<&Mem>, SplitQueueDevice<&Mem>) {
    let layout = Layout::for_size(8).expect("a layout");
    (
        SplitQueue::new(layout, memory),
        SplitQueueDevice::new(layout, memory),
    )
}

#[test]
fn a_transmit_chain_is_the_header_then_the_frame_the_device_reads() {
    let layout = Layout::for_size(8).expect("a layout");
    let memory = Mem(RefCell::new(vec![0_u8; layout.total_size]));
    let (mut driver, device) = queue(&memory);

    let data = Data {
        pages: &PAGES,
        offset: 4096 + 3000,
        len: 2000,
    };
    let segments = plan(&data, false).expect("a plan");
    let chain = publish(&mut driver, 0xAB00, HEADER_LEN, &segments, false).expect("a chain");
    assert_eq!(
        (chain.descriptors, chain.frame_len, chain.total),
        (3, 2000, 2012)
    );

    let mut read = [Descriptor {
        address: 0,
        len: 0,
        flags: 0,
        next: 0,
    }; 8];
    let count = device.read_chain(chain.head, &mut read).expect("a chain");
    assert_eq!(count, 3);
    assert_eq!((read[0].address, read[0].len), (0xAB00, 12));
    assert!(!read[0].is_device_writable(), "the device reads the header");
    assert_eq!((read[1].address, read[1].len), (PAGES[1] + 3000, 1096));
    assert_eq!((read[2].address, read[2].len), (PAGES[2], 904));
    assert!(!read[2].is_device_writable());
}

#[test]
fn a_receive_chain_is_written_by_the_device_throughout() {
    let layout = Layout::for_size(8).expect("a layout");
    let memory = Mem(RefCell::new(vec![0_u8; layout.total_size]));
    let (mut driver, device) = queue(&memory);

    let data = Data {
        pages: &PAGES,
        offset: 0,
        len: 1514,
    };
    let segments = plan(&data, true).expect("a plan");
    let chain = publish(&mut driver, 0x40, HEADER_LEN, &segments, true).expect("a chain");

    let mut read = [Descriptor {
        address: 0,
        len: 0,
        flags: 0,
        next: 0,
    }; 8];
    let count = device.read_chain(chain.head, &mut read).expect("a chain");
    assert_eq!(count, 2);
    assert!(read[0].is_device_writable() && read[1].is_device_writable());
    assert_eq!(chain.total, 1526);
}

#[test]
fn a_completion_gives_the_frame_length_the_device_wrote_past_the_header() {
    let chain = Chain {
        head: 0,
        descriptors: 2,
        frame_len: 1514,
        total: 1526,
    };
    let header = Header::plain().encode();
    assert_eq!(
        parse_receipt(&chain, 12 + 64, &header, DRIVER_FEATURES),
        Ok(Receipt {
            header: Header::plain(),
            len: 64
        })
    );
    // A device that wrote nothing but the header sent a frame of no bytes,
    // which is odd but not impossible; it is the caller's to discard.
    assert_eq!(
        parse_receipt(&chain, 12, &header, DRIVER_FEATURES).map(|receipt| receipt.len),
        Ok(0)
    );
}

#[test]
fn a_completion_longer_than_the_chain_or_shorter_than_the_header_is_refused() {
    let chain = Chain {
        head: 0,
        descriptors: 2,
        frame_len: 1514,
        total: 1526,
    };
    let header = Header::plain().encode();
    assert_eq!(
        parse_receipt(&chain, 1527, &header, DRIVER_FEATURES),
        Err(NetError::WrittenTooLong {
            written: 1527,
            capacity: 1526
        })
    );
    assert_eq!(
        parse_receipt(&chain, 11, &header, DRIVER_FEATURES),
        Err(NetError::HeaderTruncated {
            have: 11,
            needed: 12
        })
    );
    assert_eq!(parse_sent(&chain, 0), Ok(()));
    assert_eq!(parse_sent(&chain, 1526), Ok(()), "a device counting reads");
    assert_eq!(
        parse_sent(&chain, u32::MAX),
        Err(NetError::WrittenTooLong {
            written: u32::MAX,
            capacity: 1526
        })
    );
}
