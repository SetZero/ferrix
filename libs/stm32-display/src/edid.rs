//! The parts of a monitor's EDID the driver acts on: whether the block is
//! whole, the preferred timing, the monitor's name, and — from the CEA-861
//! extension — whether the sink speaks HDMI rather than only DVI.

use crate::mode::Mode;

/// Bytes of one EDID block.
pub const BLOCK_BYTES: usize = 128;

/// The eight bytes every base block starts with.
pub const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

/// A CEA-861 extension block's tag.
pub const CEA_EXTENSION: u8 = 0x02;

/// The IEEE OUI of HDMI Licensing, which an HDMI sink's vendor-specific data
/// block carries (00-0C-03, least significant byte first).
pub const HDMI_OUI: [u8; 3] = [0x03, 0x0C, 0x00];

/// Whether `block` sums to zero, as every EDID block must.
#[must_use]
pub fn checksum_ok(block: &[u8; BLOCK_BYTES]) -> bool {
    block.iter().fold(0_u8, |sum, byte| sum.wrapping_add(*byte)) == 0
}

/// Whether `block` is a base block: the header and the checksum.
#[must_use]
pub fn is_base(block: &[u8; BLOCK_BYTES]) -> bool {
    block.starts_with(&HEADER) && checksum_ok(block)
}

/// How many extension blocks follow the base block.
#[must_use]
pub fn extensions(block: &[u8; BLOCK_BYTES]) -> u8 {
    block.get(126).copied().unwrap_or(0)
}

/// The four 18-byte descriptors of a base block.
fn descriptors(block: &[u8; BLOCK_BYTES]) -> impl Iterator<Item = [u8; 18]> + '_ {
    (0..4).filter_map(move |index| {
        let start = 54 + index * 18;
        block.get(start..start + 18)?.try_into().ok()
    })
}

/// The preferred timing: the first descriptor, when it is a timing this
/// module can read.
#[must_use]
pub fn preferred(block: &[u8; BLOCK_BYTES]) -> Option<Mode> {
    Mode::from_detailed_timing(&descriptors(block).next()?)
}

/// The monitor's name from its name descriptor (tag 0xFC), up to thirteen
/// bytes, ended at the first newline, as ASCII.
#[must_use]
pub fn name(block: &[u8; BLOCK_BYTES]) -> Option<([u8; 13], usize)> {
    let descriptor = descriptors(block).find(|d| d.starts_with(&[0, 0, 0, 0xFC]))?;
    let text: [u8; 13] = descriptor.get(5..18)?.try_into().ok()?;
    let len = text
        .iter()
        .position(|&byte| byte == b'\n' || byte == 0)
        .unwrap_or(text.len());
    Some((text, len))
}

/// Whether a CEA-861 extension block says the sink is HDMI: a vendor-specific
/// data block (tag 3) carrying HDMI Licensing's OUI.
#[must_use]
pub fn is_hdmi(extension: &[u8; BLOCK_BYTES]) -> bool {
    let [tag, _, end, ..] = *extension;
    if tag != CEA_EXTENSION || !checksum_ok(extension) {
        return false;
    }
    // Data blocks run from byte 4 to the offset of the first descriptor.
    let end = usize::from(end).min(BLOCK_BYTES);
    let mut at = 4;
    while at < end {
        let Some(&header) = extension.get(at) else {
            return false;
        };
        let (kind, len) = (header >> 5, usize::from(header & 0x1F));
        if kind == 3 && len >= 3 && extension.get(at + 1..at + 4) == Some(&HDMI_OUI[..]) {
            return true;
        }
        at += 1 + len;
    }
    false
}
