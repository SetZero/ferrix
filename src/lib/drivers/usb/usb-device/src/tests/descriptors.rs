//! The descriptors, field by field.

use std::vec::Vec;

use crate::TransferKind;
use crate::acm::{
    ADB_IN, ADB_OUT, CONFIGURATION_BYTES, DATA_IN, DATA_OUT, DEVICE, FULL_SPEED_CONFIGURATION,
    FULL_SPEED_ENDPOINTS, HIGH_SPEED_CONFIGURATION, HIGH_SPEED_ENDPOINTS, NOTIFY_IN, QUALIFIER,
};

/// A configuration split at each descriptor's `bLength`.
fn split(configuration: &[u8]) -> Vec<&[u8]> {
    let mut parts = Vec::new();
    let mut rest = configuration;
    while !rest.is_empty() {
        let length = usize::from(rest[0]);
        assert!(length >= 2, "a descriptor is at least two bytes");
        let (part, tail) = rest.split_at(length);
        parts.push(part);
        rest = tail;
    }
    parts
}

fn word(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

#[test]
fn device_descriptor() {
    let d = DEVICE;
    assert_eq!(d.len(), 18, "bLength");
    assert_eq!(d[0], 18, "bLength");
    assert_eq!(d[1], 1, "bDescriptorType DEVICE");
    assert_eq!(word(&d, 2), 0x0200, "bcdUSB 2.00, so no BOS is asked for");
    assert_eq!(
        (d[4], d[5], d[6]),
        (0xEF, 0x02, 0x01),
        "class, subclass, protocol: interface association"
    );
    assert_eq!(d[7], 64, "bMaxPacketSize0");
    assert_eq!(word(&d, 8), 0x1209, "idVendor: pid.codes");
    assert_eq!(word(&d, 10), 0x0001, "idProduct: the test PID");
    assert_eq!(word(&d, 12), 0x0100, "bcdDevice");
    assert_eq!((d[14], d[15], d[16]), (1, 2, 3), "string indices");
    assert_eq!(d[17], 1, "bNumConfigurations");
}

#[test]
fn qualifier() {
    let q = QUALIFIER;
    assert_eq!((q[0], q[1]), (10, 6), "bLength, DEVICE_QUALIFIER");
    assert_eq!(word(&q, 2), 0x0200, "bcdUSB");
    assert_eq!(
        (q[4], q[5], q[6]),
        (DEVICE[4], DEVICE[5], DEVICE[6]),
        "class"
    );
    assert_eq!(q[7], 64, "bMaxPacketSize0 at the other speed");
    assert_eq!((q[8], q[9]), (1, 0), "one configuration, reserved zero");
}

fn check_configuration(configuration: &[u8], bulk: u16, interval: u8) {
    assert_eq!(configuration.len(), CONFIGURATION_BYTES, "the whole");
    let parts = split(configuration);
    let types: Vec<(u8, u8)> = parts.iter().map(|p| (p[0], p[1])).collect();
    assert_eq!(
        types,
        [
            (9, 2),
            (8, 0x0B),
            (9, 4),
            (5, 0x24),
            (5, 0x24),
            (4, 0x24),
            (5, 0x24),
            (7, 5),
            (9, 4),
            (7, 5),
            (7, 5),
            (9, 4),
            (7, 5),
            (7, 5),
        ],
        "the descriptors in order"
    );

    let c = parts[0];
    assert_eq!(usize::from(word(c, 2)), CONFIGURATION_BYTES, "wTotalLength");
    assert_eq!(c[4], 3, "bNumInterfaces: the serial port's two and adb's");
    assert_eq!(c[5], 1, "bConfigurationValue");
    assert_eq!(c[6], 0, "iConfiguration");
    assert_eq!(c[7], 0xC0, "bmAttributes: reserved bit 7, self-powered");
    assert_eq!(c[8], 1, "bMaxPower: 2 mA");

    let iad = parts[1];
    assert_eq!(&iad[2..], [0, 2, 2, 2, 1, 0], "interfaces 0-1, CDC ACM");

    let communication = parts[2];
    assert_eq!(
        &communication[2..],
        [0, 0, 1, 2, 2, 1, 0],
        "interface 0, one endpoint, class 2 subclass 2 protocol 1"
    );
    assert_eq!(&parts[3][2..], [0x00, 0x10, 0x01], "header, CDC 1.10");
    assert_eq!(&parts[4][2..], [0x01, 0x00, 1], "call management, data 1");
    assert_eq!(&parts[5][2..], [0x02, 0x02], "ACM, line requests");
    assert_eq!(&parts[6][2..], [0x06, 0, 1], "union: 0 controls 1");

    let notify = parts[7];
    assert_eq!(notify[2], NOTIFY_IN, "bEndpointAddress");
    assert_eq!(notify[3], 3, "interrupt");
    assert_eq!(word(notify, 4), 16, "wMaxPacketSize");
    assert_eq!(notify[6], interval, "bInterval");

    let data = parts[8];
    assert_eq!(
        &data[2..],
        [1, 0, 2, 0x0A, 0, 0, 0],
        "interface 1, data class"
    );
    let adb = parts[11];
    assert_eq!(
        &adb[2..],
        [2, 0, 2, 0xFF, 0x42, 0x01, 0],
        "interface 2, adb's class, subclass and protocol, as AOSP's"
    );
    for (part, address) in [
        (parts[9], DATA_OUT),
        (parts[10], DATA_IN),
        (parts[12], ADB_OUT),
        (parts[13], ADB_IN),
    ] {
        assert_eq!(part[2], address, "bEndpointAddress");
        assert_eq!(part[3], 2, "bulk");
        assert_eq!(word(part, 4), bulk, "wMaxPacketSize");
        assert_eq!(part[6], 0, "bInterval");
    }
}

#[test]
fn high_speed_configuration() {
    check_configuration(&HIGH_SPEED_CONFIGURATION, 512, 9);
}

#[test]
fn full_speed_configuration() {
    check_configuration(&FULL_SPEED_CONFIGURATION, 64, 32);
}

#[test]
fn endpoint_lists_match_the_descriptors() {
    for (list, configuration) in [
        (HIGH_SPEED_ENDPOINTS, HIGH_SPEED_CONFIGURATION),
        (FULL_SPEED_ENDPOINTS, FULL_SPEED_CONFIGURATION),
    ] {
        let described: Vec<(u8, u8, u16, u8)> = split(&configuration)
            .into_iter()
            .filter(|part| part[1] == 5)
            .map(|part| (part[2], part[3], word(part, 4), part[6]))
            .collect();
        let listed: Vec<(u8, u8, u16, u8)> = list
            .iter()
            .map(|endpoint| {
                let kind = match endpoint.kind {
                    TransferKind::Bulk => 2,
                    TransferKind::Interrupt => 3,
                };
                (
                    endpoint.address,
                    kind,
                    endpoint.max_packet,
                    endpoint.interval,
                )
            })
            .collect();
        assert_eq!(described, listed, "the list is the descriptors'");
    }
}
