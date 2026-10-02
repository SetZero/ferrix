//! `drm.edid_firmware=`'s grammar entry by entry, the checks a file has to
//! pass, what a monitor calls itself, and the property calls' answers.

extern crate std;

use std::vec::Vec;

use ferrix_linux_abi::drm::{self, Field, GetBlob, GetProperty};

use crate::edid::*;

/// A display descriptor: `00 00 00 <tag> 00` and thirteen bytes of text,
/// ended by a newline and padded with spaces.
fn text_descriptor(tag: u8, text: &str) -> [u8; 18] {
    let mut block = [0x20u8; 18];
    block[..5].copy_from_slice(&[0, 0, 0, tag, 0]);
    let bytes = text.as_bytes();
    block[5..5 + bytes.len()].copy_from_slice(bytes);
    if bytes.len() < 13 {
        block[5 + bytes.len()] = 0x0A;
    }
    block
}

fn seal(block: &mut [u8]) {
    let sum = block[..127]
        .iter()
        .fold(0u8, |sum, &byte| sum.wrapping_add(byte));
    block[127] = 0u8.wrapping_sub(sum);
}

/// A base block that says it is a Lenovo R27qe Gen2, serial UTP03KBB, with
/// `extensions` blocks after it.
fn base(extensions: u8) -> Vec<u8> {
    let mut block = std::vec![0u8; 128];
    block[..8].copy_from_slice(&HEADER);
    // L = 12, E = 5, N = 14, five bits each.
    block[8..10].copy_from_slice(&((12u16 << 10) | (5 << 5) | 14).to_be_bytes());
    block[10..12].copy_from_slice(&0x66F2u16.to_le_bytes());
    block[18] = 1;
    block[19] = 4;
    // A detailed timing first, which is not a display descriptor.
    block[54..72].fill(0x11);
    block[72..90].copy_from_slice(&text_descriptor(0xFC, "R27qe Gen2"));
    block[90..108].copy_from_slice(&text_descriptor(0xFF, "UTP03KBB"));
    block[108..126].copy_from_slice(&text_descriptor(0xFE, "ignored"));
    block[126] = extensions;
    seal(&mut block);
    block
}

/// An extension block with `tag`, its checksum right.
fn extension(tag: u8, fill: u8) -> Vec<u8> {
    let mut block = std::vec![fill; 128];
    block[0] = tag;
    seal(&mut block);
    block
}

#[test]
fn an_entry_without_a_connector_is_every_connectors() {
    assert_eq!(
        firmware_for("edid/r27qe.bin", "Virtual-1"),
        Some("edid/r27qe.bin")
    );
    assert_eq!(
        firmware_for("edid/r27qe.bin", "HDMI-A-1"),
        Some("edid/r27qe.bin")
    );
}

#[test]
fn a_named_entry_is_that_connectors_and_the_first_match_wins() {
    let setting = "Virtual-2:edid/two.bin,Virtual-1:edid/one.bin,Virtual-1:edid/later.bin";
    assert_eq!(firmware_for(setting, "Virtual-1"), Some("edid/one.bin"));
    assert_eq!(firmware_for(setting, "Virtual-2"), Some("edid/two.bin"));
    // Named for others and no fallback: nothing.
    assert_eq!(firmware_for(setting, "HDMI-A-1"), None);
}

#[test]
fn the_last_unnamed_entry_is_the_fallback_and_a_named_one_beats_it() {
    let setting = "edid/first.bin,DP-1:edid/dp.bin,edid/last.bin";
    assert_eq!(firmware_for(setting, "Virtual-1"), Some("edid/last.bin"));
    assert_eq!(firmware_for(setting, "DP-1"), Some("edid/dp.bin"));
    // Empty entries -- two commas -- are skipped, not a fallback of "".
    assert_eq!(
        firmware_for("edid/a.bin,,", "Virtual-1"),
        Some("edid/a.bin")
    );
    assert_eq!(firmware_for(",", "Virtual-1"), None);
    assert_eq!(firmware_for("", "Virtual-1"), None);
}

#[test]
fn a_connector_matches_by_the_start_of_its_name_as_strncmp_does() {
    // Linux compares the entry's length only, so `DP-1:` is `DP-10`'s too,
    // and a longer entry is not a shorter connector's.
    assert_eq!(firmware_for("DP-1:edid/a.bin", "DP-10"), Some("edid/a.bin"));
    assert_eq!(firmware_for("DP-10:edid/a.bin", "DP-1"), None);
}

#[test]
fn a_trailing_newline_on_the_name_is_cut() {
    assert_eq!(
        firmware_for("Virtual-1:edid/a.bin\n", "Virtual-1"),
        Some("edid/a.bin")
    );
    assert_eq!(
        firmware_for("edid/b.bin\n", "Virtual-1"),
        Some("edid/b.bin")
    );
}

#[test]
fn a_name_that_could_reach_another_file_is_refused() {
    assert_eq!(
        confined("edid/LEN-R27qe-Gen2.bin"),
        Ok("edid/LEN-R27qe-Gen2.bin")
    );
    assert_eq!(confined("r27qe.bin"), Ok("r27qe.bin"));
    // `..` inside a name is only a name.
    assert_eq!(confined("edid/a..b.bin"), Ok("edid/a..b.bin"));
    assert_eq!(confined("../etc/shadow"), Err(BadName::Parent));
    assert_eq!(confined("edid/../../etc/shadow"), Err(BadName::Parent));
    assert_eq!(confined("edid/.."), Err(BadName::Parent));
    assert_eq!(confined("/etc/shadow"), Err(BadName::Absolute));
    assert_eq!(confined(""), Err(BadName::Empty));
    assert_eq!(confined("edid/a\0.bin"), Err(BadName::Nul));
    // What the grammar hands on is checked as it stands: a connector's
    // entry naming `/etc/shadow` is refused, not read.
    let chosen = firmware_for("Virtual-1:/etc/shadow", "Virtual-1").unwrap();
    assert_eq!(confined(chosen), Err(BadName::Absolute));
    let chosen = firmware_for("../etc/shadow", "Virtual-1").unwrap();
    assert_eq!(confined(chosen), Err(BadName::Parent));
}

#[test]
fn a_whole_edid_is_kept_as_it_is() {
    let mut bytes = base(2);
    bytes.extend(extension(0x02, 0x05));
    bytes.extend(extension(0x70, 0x09));
    let before = bytes.clone();
    let checked = check(&mut bytes).unwrap();
    assert_eq!(
        checked,
        Checked {
            len: 384,
            extensions: 2,
            dropped: 0,
            repaired: false
        }
    );
    assert_eq!(bytes, before);
}

#[test]
fn a_file_of_the_wrong_size_is_refused() {
    // One extension counted, none there.
    let mut short = base(1);
    assert_eq!(
        check(&mut short),
        Err(Unusable::Size {
            expected: 256,
            found: 128
        })
    );
    // Too short to be a base block at all.
    let mut tiny = std::vec![0u8; 100];
    assert_eq!(
        check(&mut tiny),
        Err(Unusable::Size {
            expected: 0,
            found: 100
        })
    );
    assert_eq!(
        check(&mut []),
        Err(Unusable::Size {
            expected: 0,
            found: 0
        })
    );
}

#[test]
fn a_bad_base_block_is_refused_and_a_nearly_right_header_is_put_right() {
    let mut zero = std::vec![0u8; 128];
    assert_eq!(check(&mut zero), Err(Unusable::Zero));

    let mut corrupt = base(0);
    corrupt[1..4].fill(0);
    assert_eq!(check(&mut corrupt), Err(Unusable::Header));

    let mut sum = base(0);
    sum[127] = sum[127].wrapping_add(1);
    assert_eq!(check(&mut sum), Err(Unusable::Checksum));

    let mut version = base(0);
    version[18] = 2;
    seal(&mut version);
    assert_eq!(check(&mut version), Err(Unusable::Version(2)));

    // Two header bytes wrong is repaired; the checksum is of the repaired
    // block, as Linux checks it again after the repair.
    let right = base(0);
    let mut repairable = right.clone();
    repairable[1] = 0;
    repairable[2] = 0;
    let checked = check(&mut repairable).unwrap();
    assert!(checked.repaired);
    assert_eq!(repairable, right);
}

#[test]
fn a_bad_extension_is_dropped_and_the_base_block_made_to_agree() {
    let mut bytes = base(3);
    let mut broken = extension(0x70, 0x01);
    broken[5] ^= 0xFF;
    let kept = extension(0x70, 0x02);
    bytes.extend(broken);
    bytes.extend(&kept);
    // A CTA-861 block with a wrong checksum is kept, as Linux keeps it.
    let mut cta = extension(0x02, 0x03);
    cta[127] ^= 1;
    bytes.extend(&cta);
    let checked = check(&mut bytes).unwrap();
    assert_eq!(checked.len, 384);
    assert_eq!((checked.extensions, checked.dropped), (2, 1));
    assert_eq!(bytes[126], 2);
    assert_eq!(&bytes[128..256], &kept[..]);
    assert_eq!(&bytes[256..384], &cta[..]);
    // And the base block still sums to zero, so what is handed out is an
    // EDID a reader accepts.
    assert_eq!(
        bytes[..128]
            .iter()
            .fold(0u8, |sum, &byte| sum.wrapping_add(byte)),
        0
    );
}

#[test]
fn the_monitor_says_what_it_is() {
    let found = identity(&base(0)).unwrap();
    assert_eq!(found.manufacturer(), "LEN");
    assert_eq!(found.product, 0x66F2);
    assert_eq!(found.serial_number, 0);
    assert_eq!(found.name.unwrap().as_str(), "R27qe Gen2");
    assert_eq!(found.serial.unwrap().as_str(), "UTP03KBB");

    // Thirteen characters with no newline, and spaces round the text.
    let mut block = base(0);
    block[72..90].copy_from_slice(&text_descriptor(0xFC, "  A 13 chars "));
    let found = identity(&block).unwrap();
    assert_eq!(found.name.unwrap().as_str(), "A 13 chars");

    // Not an EDID, and a manufacturer that is not three letters.
    assert_eq!(identity(&[0u8; 128]), None);
    let mut nameless = base(0);
    nameless[8..10].fill(0);
    assert_eq!(identity(&nameless), None);
    assert_eq!(identity(&base(0)[..100]), None);
}

#[test]
fn the_property_is_an_immutable_blob_with_no_values() {
    let mut property = GetProperty {
        prop_id: 66,
        count_values: 4,
        count_enum_blobs: 4,
        ..GetProperty::ZERO
    };
    describe_property(&mut property);
    assert_eq!(&property.name[..5], b"EDID\0");
    assert_eq!(
        property.flags,
        drm::MODE_PROP_BLOB | drm::MODE_PROP_IMMUTABLE
    );
    assert_eq!((property.count_values, property.count_enum_blobs), (0, 0));
    assert_eq!(property.prop_id, 66);
}

#[test]
fn a_blob_is_copied_only_into_room_of_exactly_its_size() {
    // The first call, to learn the length.
    let mut asked = GetBlob {
        blob_id: 67,
        ..GetBlob::ZERO
    };
    assert!(!answer_blob(&mut asked, 384));
    assert_eq!(asked.length, 384);
    // The second, with room for it.
    assert!(answer_blob(&mut asked, 384));
    assert_eq!(asked.length, 384);
    // Room for more, or less, is not copied into: `==`, not `>=`.
    let mut more = GetBlob {
        blob_id: 67,
        length: 512,
        ..GetBlob::ZERO
    };
    assert!(!answer_blob(&mut more, 384));
    assert_eq!(more.length, 384);
}

// -- The preferred timing's refresh ---------------------------------------------

/// The EDID QEMU 10.2's `qemu_edid_generate` makes for a scanout of
/// `width` by `height` at `millihertz`, as far as the refresh goes: its
/// made-up blanking, its clock rounded down to 10 kHz, the timing in the
/// base block's first descriptor, or -- for a clock or size a descriptor
/// cannot hold -- in a `DisplayID` extension after a CTA one, with the base
/// block's descriptors left to display descriptors.
fn qemu_edid(width: u32, height: u32, millihertz: u32) -> Vec<u8> {
    let xblank = width * 35 / 100;
    let yblank = height * 35 / 1000;
    let clock =
        u64::from(millihertz) * u64::from(width + xblank) * u64::from(height + yblank) / 10_000_000;
    let large = width >= 4096 || height >= 4096 || clock >= 65536;
    let mut edid = base(if large { 2 } else { 1 });
    edid[54..126].fill(0);
    for at in [54, 72, 90, 108] {
        edid[at + 3] = 0x10;
    }
    let mut cta = extension(0x02, 0);
    if large {
        let mut did = std::vec![0u8; 128];
        did[..8].copy_from_slice(&[0x70, 0x13, 23, 0x03, 0, 0x03, 0x00, 0x14]);
        did[8..11].copy_from_slice(&(clock as u32).to_le_bytes()[..3]);
        did[11] = 0x88;
        for (at, value) in [
            (12, width - 1),
            (14, xblank - 1),
            (16, width * 25 / 100 - 1),
            (18, width * 3 / 100 - 1),
            (20, height - 1),
            (22, yblank - 1),
            (24, height * 5 / 1000 - 1),
            (26, height * 5 / 1000 - 1),
        ] {
            did[at..at + 2].copy_from_slice(&(value as u16).to_le_bytes());
        }
        seal(&mut did);
        cta.extend_from_slice(&did);
    } else {
        let d = &mut edid[54..72];
        d[..2].copy_from_slice(&(clock as u16).to_le_bytes());
        d[2] = width as u8;
        d[3] = xblank as u8;
        d[4] = (((width & 0xF00) >> 4) | ((xblank & 0xF00) >> 8)) as u8;
        d[5] = height as u8;
        d[6] = yblank as u8;
        d[7] = (((height & 0xF00) >> 4) | ((yblank & 0xF00) >> 8)) as u8;
        d[17] = 0x18;
    }
    seal(&mut edid);
    edid.extend_from_slice(&cta);
    edid
}

#[test]
fn the_preferred_timing_says_the_refresh_qemu_made_it_at() {
    // What a VNC display gets, which reports no refresh: QEMU's 75 Hz,
    // 74.998 once its clock is rounded down to 10 kHz.
    assert_eq!(
        preferred_refresh_mhz(&qemu_edid(1920, 1080, 75_000)),
        74_998
    );
    // A GTK window on a 120 Hz monitor, and on a 60 Hz one.
    assert_eq!(
        preferred_refresh_mhz(&qemu_edid(1920, 1080, 120_000)),
        119_999
    );
    assert_eq!(preferred_refresh_mhz(&qemu_edid(1280, 800, 60_000)), 59_995);
    // A large window at 144 Hz, whose clock a descriptor cannot hold: the
    // timing is in the `DisplayID` block, after the CTA one.
    let large = qemu_edid(2560, 1440, 144_000);
    assert_eq!(large.len(), 3 * 128);
    assert_eq!(preferred_refresh_mhz(&large), 144_000);
    // And 4K at 60, whose width a descriptor's 12 bits cannot hold.
    assert_eq!(
        preferred_refresh_mhz(&qemu_edid(4096, 2160, 60_000)),
        60_000
    );
}

#[test]
fn an_edid_that_is_not_one_has_no_refresh() {
    let good = qemu_edid(1920, 1080, 75_000);
    // Too short for a base block, and empty.
    assert_eq!(preferred_refresh_mhz(&good[..127]), 0);
    assert_eq!(preferred_refresh_mhz(&[]), 0);
    // All zero, which is what a device that wrote nothing leaves.
    assert_eq!(preferred_refresh_mhz(&[0; 1024]), 0);
    // A header byte, the checksum, the version wrong.
    for at in [3, 127, 18] {
        let mut bent = good.clone();
        bent[at] ^= 0x40;
        assert_eq!(preferred_refresh_mhz(&bent), 0, "byte {at}");
    }
    // An interlaced timing.
    let mut interlaced = good.clone();
    interlaced[54 + 17] |= 0x80;
    seal(&mut interlaced[..128]);
    assert_eq!(preferred_refresh_mhz(&interlaced), 0);
    // A timing with no pixels a line.
    let mut empty = good.clone();
    empty[56] = 0;
    empty[58] &= 0x0F;
    seal(&mut empty[..128]);
    assert_eq!(preferred_refresh_mhz(&empty), 0);
    // A clock that would make it faster than any display.
    let mut fast = good;
    fast[54..56].copy_from_slice(&0xFFFFu16.to_le_bytes());
    fast[56] = 1;
    fast[57] = 1;
    fast[58] = 0;
    fast[59] = 1;
    fast[60] = 1;
    fast[61] = 0;
    seal(&mut fast[..128]);
    assert_eq!(preferred_refresh_mhz(&fast), 0);
}

#[test]
fn a_displayid_block_is_read_within_its_bounds() {
    let good = qemu_edid(2560, 1440, 144_000);
    // The extension the count names is not there.
    assert_eq!(preferred_refresh_mhz(&good[..256]), 0);
    // A `DisplayID` block with a wrong checksum.
    let mut bent = good.clone();
    bent[256 + 20] ^= 1;
    assert_eq!(preferred_refresh_mhz(&bent), 0);
    // A section that claims more than the block holds, and a data block
    // running past its section: neither is read past.
    for (at, value) in [(2, 0xFF), (7, 0x7F)] {
        let mut long = good.clone();
        long[256 + at] = value;
        seal(&mut long[256..]);
        assert_eq!(preferred_refresh_mhz(&long), 0, "byte {at}");
    }
    // A data block that is not type I timings is stepped over.
    let mut other = good.clone();
    other[256 + 5] = 0x7E;
    seal(&mut other[256..]);
    assert_eq!(preferred_refresh_mhz(&other), 0);
    // A `DisplayID` block where the base block has a timing is not read.
    let mut both = qemu_edid(1920, 1080, 75_000);
    both[126] = 2;
    seal(&mut both[..128]);
    both.extend_from_slice(&good[256..]);
    assert_eq!(preferred_refresh_mhz(&both), 74_998);
}
