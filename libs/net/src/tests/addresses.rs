//! Addresses, prefixes and the text they are written as.

use alloc::string::ToString;

use crate::addr::{IpAddress, IpCidr, Ipv4, Ipv6};

#[test]
fn an_ipv4_address_is_written_as_four_numbers() {
    assert_eq!(Ipv4::new([10, 0, 2, 15]).to_string(), "10.0.2.15");
    assert_eq!(Ipv4::LOOPBACK.to_string(), "127.0.0.1");
    assert_eq!(Ipv4::BROADCAST.to_string(), "255.255.255.255");
}

#[test]
fn an_ipv4_address_survives_the_trip_through_a_number() {
    let address = Ipv4::new([192, 168, 1, 1]);
    assert_eq!(address.to_bits(), 0xC0A8_0101);
    assert_eq!(Ipv4::from_bits(address.to_bits()), address);
}

#[test]
fn the_ipv4_kinds_are_told_apart() {
    assert!(Ipv4::UNSPECIFIED.is_unspecified());
    assert!(Ipv4::new([127, 9, 9, 9]).is_loopback());
    assert!(Ipv4::new([224, 0, 0, 1]).is_multicast());
    assert!(Ipv4::new([169, 254, 3, 4]).is_link_local());
    assert!(!Ipv4::new([10, 0, 0, 1]).is_multicast());
}

#[test]
fn an_ipv6_address_collapses_its_longest_run_of_zeroes_once() {
    assert_eq!(Ipv6::LOOPBACK.to_string(), "::1");
    assert_eq!(Ipv6::UNSPECIFIED.to_string(), "::");
    let address = Ipv6::new([0x20, 0x01, 0x0D, 0xB8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    assert_eq!(address.to_string(), "2001:db8::1");
}

#[test]
fn a_mapped_ipv4_address_is_written_as_one() {
    let mapped = Ipv6::from_v4_mapped(Ipv4::new([10, 0, 2, 15]));
    assert!(mapped.is_v4_mapped());
    assert_eq!(mapped.v4_mapped(), Some(Ipv4::new([10, 0, 2, 15])));
    assert_eq!(mapped.to_string(), "::ffff:10.0.2.15");
}

#[test]
fn the_solicited_node_group_takes_the_last_three_bytes() {
    let address = Ipv6::new([
        0xFE, 0x80, 0, 0, 0, 0, 0, 0, 0x02, 0x11, 0x22, 0xFF, 0xFE, 0x33, 0x44, 0x55,
    ]);
    let group = address.solicited_node();
    assert_eq!(
        group.octets(),
        [
            0xFF, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xFF, 0x33, 0x44, 0x55
        ]
    );
    assert!(group.is_multicast());
}

#[test]
fn a_multicast_address_maps_to_the_hardware_address_its_rfc_gives() {
    assert_eq!(
        Ipv4::new([224, 0, 0, 251]).multicast_mac(),
        [0x01, 0x00, 0x5E, 0x00, 0x00, 0xFB]
    );
    assert_eq!(
        Ipv6::ALL_NODES.multicast_mac(),
        [0x33, 0x33, 0x00, 0x00, 0x00, 0x01]
    );
}

#[test]
fn a_prefix_holds_the_addresses_inside_it_and_no_others() {
    let network = IpCidr::new(IpAddress::V4(Ipv4::new([10, 0, 0, 0])), 24);
    assert!(network.contains(IpAddress::V4(Ipv4::new([10, 0, 0, 1]))));
    assert!(network.contains(IpAddress::V4(Ipv4::new([10, 0, 0, 255]))));
    assert!(!network.contains(IpAddress::V4(Ipv4::new([10, 0, 1, 1]))));
    // An address of the other family is never in an IPv4 prefix, however the
    // bits compare.
    assert!(!network.contains(IpAddress::V6(Ipv6::LOOPBACK)));
}

#[test]
fn a_prefix_of_zero_holds_everything_of_its_family() {
    let default = IpCidr::new(IpAddress::V4(Ipv4::UNSPECIFIED), 0);
    assert!(default.contains(IpAddress::V4(Ipv4::new([1, 1, 1, 1]))));
    assert!(default.contains(IpAddress::V4(Ipv4::new([255, 255, 255, 255]))));
    assert!(!default.contains(IpAddress::V6(Ipv6::LOOPBACK)));
}

#[test]
fn an_ipv4_prefix_knows_its_broadcast() {
    let network = IpCidr::new(IpAddress::V4(Ipv4::new([10, 0, 0, 1])), 24);
    assert_eq!(network.broadcast(), Some(Ipv4::new([10, 0, 0, 255])));
    let host = IpCidr::new(IpAddress::V4(Ipv4::new([10, 0, 0, 1])), 32);
    assert_eq!(host.broadcast(), Some(Ipv4::new([10, 0, 0, 1])));
    let six = IpCidr::new(IpAddress::V6(Ipv6::LOOPBACK), 128);
    assert_eq!(six.broadcast(), None);
}
