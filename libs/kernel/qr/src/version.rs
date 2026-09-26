// SPDX-License-Identifier: MIT
//
// Ported from Linux, drivers/gpu/drm/drm_panic_qr.rs; see the crate
// documentation for the upstream commit and LICENSE-MIT for the notice.

//! The per-version tables of the QR code specification, for error correction
//! level L only, and the arithmetic over them.
//!
//! The tables are upstream's, value for value. What changed is how they are
//! reached: upstream indexes them with `VPARAM[self.0 - 1]` on every call,
//! which is a panic for a version of 0 or 41. Here a [`Version`] can only be
//! built by looking its row up once, so holding one is the proof that the row
//! exists.

/// Generator polynomials for ECC, only those that are needed for low quality.
///
/// Each coefficient is an exponent of the field's generator α (so the
/// polynomial for 7 codewords is `x^7 + α^87 x^6 + α^229 x^5 + ...`), with the
/// leading `α^0` left out.
const P7: [u8; 7] = [87, 229, 146, 149, 238, 102, 21];
const P10: [u8; 10] = [251, 67, 46, 61, 118, 70, 64, 94, 32, 45];
const P15: [u8; 15] = [
    8, 183, 61, 91, 202, 37, 51, 58, 58, 237, 140, 124, 5, 99, 105,
];
const P18: [u8; 18] = [
    215, 234, 158, 94, 184, 97, 118, 170, 79, 187, 152, 148, 252, 179, 5, 98, 96, 153,
];
const P20: [u8; 20] = [
    17, 60, 79, 50, 61, 163, 26, 187, 202, 180, 221, 225, 83, 239, 156, 164, 212, 212, 188, 190,
];
const P22: [u8; 22] = [
    210, 171, 247, 242, 93, 230, 14, 109, 221, 53, 200, 74, 8, 172, 98, 80, 219, 134, 160, 105,
    165, 231,
];
const P24: [u8; 24] = [
    229, 121, 135, 48, 211, 117, 251, 126, 159, 180, 169, 152, 192, 226, 228, 218, 111, 0, 117,
    232, 87, 96, 227, 21,
];
const P26: [u8; 26] = [
    173, 125, 158, 2, 103, 182, 118, 17, 145, 201, 111, 28, 165, 53, 161, 21, 245, 142, 13, 102,
    48, 227, 153, 145, 218, 70,
];
const P28: [u8; 28] = [
    168, 223, 200, 104, 224, 234, 108, 180, 110, 190, 195, 147, 205, 27, 232, 201, 21, 43, 245, 87,
    42, 195, 212, 119, 242, 37, 9, 123,
];
const P30: [u8; 30] = [
    41, 173, 145, 152, 216, 31, 179, 182, 50, 48, 110, 86, 239, 96, 222, 125, 42, 173, 226, 193,
    224, 130, 156, 37, 251, 216, 238, 40, 192, 180,
];

/// QR Code parameters for Low quality ECC:
/// - Error Correction polynomial.
/// - Number of blocks in group 1.
/// - Number of blocks in group 2.
/// - Block size in group 1.
///
/// (Block size in group 2 is one more than group 1).
#[derive(Debug)]
pub(crate) struct VersionParameter(&'static [u8], u8, u8, u8);

const VPARAM: [VersionParameter; 40] = [
    VersionParameter(&P7, 1, 0, 19),    // V1
    VersionParameter(&P10, 1, 0, 34),   // V2
    VersionParameter(&P15, 1, 0, 55),   // V3
    VersionParameter(&P20, 1, 0, 80),   // V4
    VersionParameter(&P26, 1, 0, 108),  // V5
    VersionParameter(&P18, 2, 0, 68),   // V6
    VersionParameter(&P20, 2, 0, 78),   // V7
    VersionParameter(&P24, 2, 0, 97),   // V8
    VersionParameter(&P30, 2, 0, 116),  // V9
    VersionParameter(&P18, 2, 2, 68),   // V10
    VersionParameter(&P20, 4, 0, 81),   // V11
    VersionParameter(&P24, 2, 2, 92),   // V12
    VersionParameter(&P26, 4, 0, 107),  // V13
    VersionParameter(&P30, 3, 1, 115),  // V14
    VersionParameter(&P22, 5, 1, 87),   // V15
    VersionParameter(&P24, 5, 1, 98),   // V16
    VersionParameter(&P28, 1, 5, 107),  // V17
    VersionParameter(&P30, 5, 1, 120),  // V18
    VersionParameter(&P28, 3, 4, 113),  // V19
    VersionParameter(&P28, 3, 5, 107),  // V20
    VersionParameter(&P28, 4, 4, 116),  // V21
    VersionParameter(&P28, 2, 7, 111),  // V22
    VersionParameter(&P30, 4, 5, 121),  // V23
    VersionParameter(&P30, 6, 4, 117),  // V24
    VersionParameter(&P26, 8, 4, 106),  // V25
    VersionParameter(&P28, 10, 2, 114), // V26
    VersionParameter(&P30, 8, 4, 122),  // V27
    VersionParameter(&P30, 3, 10, 117), // V28
    VersionParameter(&P30, 7, 7, 116),  // V29
    VersionParameter(&P30, 5, 10, 115), // V30
    VersionParameter(&P30, 13, 3, 115), // V31
    VersionParameter(&P30, 17, 0, 115), // V32
    VersionParameter(&P30, 17, 1, 115), // V33
    VersionParameter(&P30, 13, 6, 115), // V34
    VersionParameter(&P30, 12, 7, 121), // V35
    VersionParameter(&P30, 6, 14, 121), // V36
    VersionParameter(&P30, 17, 4, 122), // V37
    VersionParameter(&P30, 4, 18, 122), // V38
    VersionParameter(&P30, 20, 4, 117), // V39
    VersionParameter(&P30, 19, 6, 118), // V40
];

/// The largest number of error correction codewords in one block.
pub(crate) const MAX_EC_SIZE: usize = 30;
/// The largest data block, in group 2 of version 38.
pub(crate) const MAX_BLK_SIZE: usize = 123;

/// Position of the alignment pattern grid.
const ALIGNMENT_PATTERNS: [&[u8]; 40] = [
    &[],
    &[6, 18],
    &[6, 22],
    &[6, 26],
    &[6, 30],
    &[6, 34],
    &[6, 22, 38],
    &[6, 24, 42],
    &[6, 26, 46],
    &[6, 28, 50],
    &[6, 30, 54],
    &[6, 32, 58],
    &[6, 34, 62],
    &[6, 26, 46, 66],
    &[6, 26, 48, 70],
    &[6, 26, 50, 74],
    &[6, 30, 54, 78],
    &[6, 30, 56, 82],
    &[6, 30, 58, 86],
    &[6, 34, 62, 90],
    &[6, 28, 50, 72, 94],
    &[6, 26, 50, 74, 98],
    &[6, 30, 54, 78, 102],
    &[6, 28, 54, 80, 106],
    &[6, 32, 58, 84, 110],
    &[6, 30, 58, 86, 114],
    &[6, 34, 62, 90, 118],
    &[6, 26, 50, 74, 98, 122],
    &[6, 30, 54, 78, 102, 126],
    &[6, 26, 52, 78, 104, 130],
    &[6, 30, 56, 82, 108, 134],
    &[6, 34, 60, 86, 112, 138],
    &[6, 30, 58, 86, 114, 142],
    &[6, 34, 62, 90, 118, 146],
    &[6, 30, 54, 78, 102, 126, 150],
    &[6, 24, 50, 76, 102, 128, 154],
    &[6, 28, 54, 80, 106, 132, 158],
    &[6, 32, 58, 84, 110, 136, 162],
    &[6, 26, 54, 82, 110, 138, 166],
    &[6, 30, 58, 86, 114, 142, 170],
];

/// Version information for format V7-V40.
const VERSION_INFORMATION: [u32; 34] = [
    0b00_0111_1100_1001_0100,
    0b00_1000_0101_1011_1100,
    0b00_1001_1010_1001_1001,
    0b00_1010_0100_1101_0011,
    0b00_1011_1011_1111_0110,
    0b00_1100_0111_0110_0010,
    0b00_1101_1000_0100_0111,
    0b00_1110_0110_0000_1101,
    0b00_1111_1001_0010_1000,
    0b01_0000_1011_0111_1000,
    0b01_0001_0100_0101_1101,
    0b01_0010_1010_0001_0111,
    0b01_0011_0101_0011_0010,
    0b01_0100_1001_1010_0110,
    0b01_0101_0110_1000_0011,
    0b01_0110_1000_1100_1001,
    0b01_0111_0111_1110_1100,
    0b01_1000_1110_1100_0100,
    0b01_1001_0001_1110_0001,
    0b01_1010_1111_1010_1011,
    0b01_1011_0000_1000_1110,
    0b01_1100_1100_0001_1010,
    0b01_1101_0011_0011_1111,
    0b01_1110_1101_0111_0101,
    0b01_1111_0010_0101_0000,
    0b10_0000_1001_1101_0101,
    0b10_0001_0110_1111_0000,
    0b10_0010_1000_1011_1010,
    0b10_0011_0111_1001_1111,
    0b10_0100_1011_0000_1011,
    0b10_0101_0100_0010_1110,
    0b10_0110_1010_0110_0100,
    0b10_0111_0101_0100_0001,
    0b10_1000_1100_0110_1001,
];

/// Format info for low quality ECC.
///
/// Indexed by mask pattern; the encoder only ever uses mask 0.
pub(crate) const FORMAT_INFOS_QR_L: [u16; 8] = [
    0x77c4, 0x72f3, 0x7daa, 0x789d, 0x662f, 0x6318, 0x6c41, 0x6976,
];

/// The highest version the specification defines.
pub(crate) const MAX_VERSION: u8 = 40;

/// Width in modules of a symbol of the highest version.
pub(crate) const MAX_WIDTH: usize = 17 + 4 * MAX_VERSION as usize;

/// Codewords (data and error correction) in a symbol of the highest version:
/// the scratch space the encoder needs.
pub(crate) const MAX_CODEWORDS: usize = match VPARAM.last() {
    Some(p) => {
        let blocks = p.1 as usize + p.2 as usize;
        // Every block holds `p.3` bytes, and the group 2 blocks one more.
        let data = p.3 as usize * blocks + p.2 as usize;
        data + blocks * p.0.len()
    }
    // The table has 40 rows; an empty one would fail every test first.
    None => 0,
};

/// A QR code version between 1 and 40, together with its row of parameters.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Version {
    number: u8,
    param: &'static VersionParameter,
    alignment: &'static [u8],
}

impl Version {
    /// The version numbered `number`, if the specification defines one.
    pub(crate) fn new(number: u8) -> Option<Self> {
        let index = usize::from(number).checked_sub(1)?;
        Some(Self {
            number,
            param: VPARAM.get(index)?,
            alignment: ALIGNMENT_PATTERNS.get(index)?,
        })
    }

    /// Every version, smallest first.
    pub(crate) fn all() -> impl Iterator<Item = Self> {
        (1..=MAX_VERSION).filter_map(Self::new)
    }

    /// The version number, 1 to 40.
    pub(crate) const fn number(self) -> u8 {
        self.number
    }

    /// Width (and height) of the symbol in modules; at most 177, so a `u8`.
    pub(crate) const fn width(self) -> u8 {
        self.number * 4 + 17
    }

    /// Data codewords the symbol holds.
    pub(crate) fn max_data(self) -> usize {
        self.g1_blk_size() * self.g1_blocks() + (self.g1_blk_size() + 1) * self.g2_blocks()
    }

    /// Data and error correction codewords together.
    pub(crate) fn total_codewords(self) -> usize {
        self.max_data() + self.ec_size() * (self.g1_blocks() + self.g2_blocks())
    }

    pub(crate) const fn ec_size(self) -> usize {
        self.param.0.len()
    }

    pub(crate) fn g1_blocks(self) -> usize {
        usize::from(self.param.1)
    }

    pub(crate) fn g2_blocks(self) -> usize {
        usize::from(self.param.2)
    }

    pub(crate) fn g1_blk_size(self) -> usize {
        usize::from(self.param.3)
    }

    pub(crate) const fn alignment_pattern(self) -> &'static [u8] {
        self.alignment
    }

    pub(crate) const fn poly(self) -> &'static [u8] {
        self.param.0
    }

    pub(crate) fn version_info(self) -> u32 {
        usize::from(self.number)
            .checked_sub(7)
            .and_then(|index| VERSION_INFORMATION.get(index))
            .copied()
            .unwrap_or(0)
    }
}
