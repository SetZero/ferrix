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
