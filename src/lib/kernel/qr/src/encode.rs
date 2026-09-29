// SPDX-License-Identifier: MIT
//
// Ported from Linux, drivers/gpu/drm/drm_panic_qr.rs; see the crate
// documentation for the upstream commit and LICENSE-MIT for the notice.

//! From payload to codewords: segment encoding, padding, and Reed-Solomon
//! error correction, ending in the interleaved byte order the symbol is drawn
//! in.

use crate::Error;
use crate::version::{MAX_BLK_SIZE, MAX_EC_SIZE, Version};

// 4 bits segment header.
const MODE_STOP: u16 = 0;
const MODE_NUMERIC: u16 = 1;
const MODE_BINARY: u16 = 4;
/// Padding bytes.
const PADDING: [u8; 2] = [236, 17];

/// Number of bits to encode `n` decimal characters in numeric mode, for
/// `n` of 0 to 3.
///
/// Upstream's `NUM_CHARS_BITS` table, as a match so that no caller indexes.
const fn num_chars_bits(n: u32) -> usize {
    match n {
        0 => 0,
        1 => 4,
        2 => 7,
        _ => 10,
    }
}

/// Number of decimal digits required to encode `n` bytes of binary data, for
/// `n` of 0 to 7. eg: you need 15 decimal digits to fit 6 bytes of binary data.
///
/// Upstream's `BYTES_TO_DIGITS` table, as a match.
const fn bytes_to_digits(n: usize) -> u32 {
    match n {
        0 => 0,
        1 => 3,
        2 => 5,
        3 => 8,
        4 => 10,
        5 => 13,
        6 => 15,
        _ => 17,
    }
}

/// One segment of the payload.
#[derive(Debug)]
pub(crate) enum Segment<'a> {
    /// Bytes packed seven at a time into 17 decimal digits (see the crate
    /// documentation), then encoded in numeric mode.
    Numeric(&'a [u8]),
    /// Bytes in byte mode.
    Binary(&'a [u8]),
}

impl Segment<'_> {
    const fn header(&self) -> (u16, usize) {
        match self {
            Segment::Binary(_) => (MODE_BINARY, 4),
            Segment::Numeric(_) => (MODE_NUMERIC, 4),
        }
    }

    /// Returns the size of the length field in bits, depending on QR Version.
    const fn length_bits_count(&self, version: Version) -> usize {
        match self {
            Segment::Binary(_) => match version.number() {
                1..=9 => 8,
                _ => 16,
            },
            Segment::Numeric(_) => match version.number() {
                1..=9 => 10,
                10..=26 => 12,
                _ => 14,
            },
        }
    }

    /// Number of characters in the segment.
    fn character_count(&self) -> usize {
        match self {
            Segment::Binary(data) => data.len(),
            Segment::Numeric(data) => {
                // 17 decimal numbers per 7 bytes + remainder. Saturating so
                // that a slice too long for any version on a 32-bit target
                // still fails the fit test rather than wrapping into it.
                (data.len() / 7)
                    .saturating_mul(17)
                    .saturating_add(bytes_to_digits(data.len() % 7) as usize)
            }
        }
    }

    /// The length field, or `None` if the count does not fit in it.
    ///
    /// Upstream casts the count to `u16` unchecked. That is sound only because
    /// every version's capacity happens to keep the count below the field's
    /// limit; this makes it a check instead of an observation.
    fn length_field(&self, version: Version) -> Option<(u16, usize)> {
        let bits = self.length_bits_count(version);
        let count = u16::try_from(self.character_count()).ok()?;
        (u32::from(count) < 1u32 << bits).then_some((count, bits))
    }

    /// Bits the segment takes: header, length field and data.
    fn total_size_bits(&self, version: Version) -> usize {
        let data_size = match self {
            Segment::Binary(data) => data.len().saturating_mul(8),
            Segment::Numeric(_) => {
                let digits = self.character_count();
                (digits / 3)
                    .saturating_mul(10)
                    .saturating_add(num_chars_bits((digits % 3) as u32))
            }
        };
        // header + length + data.
        data_size
            .saturating_add(4)
            .saturating_add(self.length_bits_count(version))
    }

    const fn iter(&self) -> SegmentIterator<'_> {
        SegmentIterator {
            segment: self,
            offset: 0,
            decfifo: DecFifo { value: 0, len: 0 },
        }
    }
}

/// Returns the smallest QR version than can hold these segments.
fn version_for(segments: &[&Segment<'_>]) -> Option<Version> {
    Version::all().find(|&v| {
        let bits = segments
            .iter()
            .fold(0usize, |sum, s| sum.saturating_add(s.total_size_bits(v)));
        v.max_data().saturating_mul(8) >= bits
            && segments.iter().all(|s| s.length_field(v).is_some())
    })
}

/// A decimal digit FIFO, holding at most 19 digits.
///
/// Upstream keeps the digits in a `[u8; 19]` and shifts it on every push. The
/// same FIFO fits in one `u64`: digits are only pushed while fewer than 3 are
/// held, so the value is below `100 * 10^17 = 10^19 < 2^64`, and popping the
/// oldest three is a division by a power of ten. That removes every index.
///
/// Upstream also carries a hand-written `div10` for 32-bit Arm, because the
/// kernel does not link the helper `u64` division compiles to there. A Rust
/// freestanding target links `compiler_builtins`, which provides it.
#[derive(Debug)]
struct DecFifo {
    value: u64,
    len: u32,
}

impl DecFifo {
    /// Append `digits` decimal digits, `chunk` being below `10^digits`.
    ///
    /// Only called with `len < 3` and `digits <= 17`, so the multiplication
    /// stays below `10^19`.
    fn push(&mut self, chunk: u64, digits: u32) {
        self.value = self.value * 10u64.pow(digits) + chunk;
        self.len += digits;
    }

    /// Pop 3 decimal digits from the FIFO.
    fn pop3(&mut self) -> Option<(u16, usize)> {
        if self.len == 0 {
            return None;
        }
        let poplen = self.len.min(3);
        self.len -= poplen;
        let divisor = 10u64.pow(self.len);
        let out = self.value / divisor;
        self.value %= divisor;
        Some((u16::try_from(out).ok()?, num_chars_bits(poplen)))
    }
}

#[derive(Debug)]
struct SegmentIterator<'a> {
    segment: &'a Segment<'a>,
    offset: usize,
    decfifo: DecFifo,
}

impl Iterator for SegmentIterator<'_> {
    type Item = (u16, usize);

    fn next(&mut self) -> Option<Self::Item> {
        match self.segment {
            Segment::Binary(data) => {
                let byte = u16::from(*data.get(self.offset)?);
                self.offset += 1;
                Some((byte, 8))
            }
            Segment::Numeric(data) => {
                let rest = data.get(self.offset..).unwrap_or_default();
                if self.decfifo.len < 3 && !rest.is_empty() {
                    // If there are less than 3 decimal digits in the fifo,
                    // take the next 7 bytes of input, and push them to the fifo.
                    let mut buf = [0u8; 8];
                    let len = rest.len().min(7);
                    for (dst, &src) in buf.iter_mut().zip(rest.iter().take(len)) {
                        *dst = src;
                    }
                    let chunk = u64::from_le_bytes(buf);
                    self.decfifo.push(chunk, bytes_to_digits(len));
                    self.offset += len;
                }
                self.decfifo.pop3()
            }
        }
    }
}

/// Appends bits most significant first, dropping any past the buffer's end.
///
/// Upstream writes up to three bytes at computed offsets. Bit at a time is
/// slower and has no offsets to get wrong. The only bits that can fall off
/// the end are the stop code's, which the specification lets a full symbol
/// truncate: [`version_for`] has already proven that everything else fits.
#[derive(Debug)]
struct BitWriter<'a> {
    buf: &'a mut [u8],
    offset: usize,
}

impl BitWriter<'_> {
    fn push(&mut self, (number, len_bits): (u16, usize)) {
        for bit in (0..len_bits.min(16)).rev() {
            if (number >> bit) & 1 != 0
                && let Some(byte) = self.buf.get_mut(self.offset / 8)
            {
                *byte |= 0x80 >> (self.offset % 8);
            }
            self.offset += 1;
        }
    }
}

/// Multiply in GF(256) with the QR code's field polynomial,
/// `x^8 + x^4 + x^3 + x^2 + 1`.
///
/// Upstream multiplies through `EXP_TABLE`/`LOG_TABLE` lookups indexed by
/// computed values. Shift-and-add needs no table and no index, and costs about
/// two million byte operations for the largest symbol, on a path that runs
/// once.
const fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut product = 0;
    while b != 0 {
        if b & 1 != 0 {
            product ^= a;
        }
        let carry = a & 0x80 != 0;
        a <<= 1;
        if carry {
            a ^= 0x1d;
        }
        b >>= 1;
    }
    product
}

/// `α^exponent`, where α = 2 generates the field.
const fn gf_exp(exponent: u8) -> u8 {
    let mut value = 1;
    let mut i = 0;
    while i < exponent {
        value = gf_mul(value, 2);
        i += 1;
    }
    value
}

/// Data to be put in the QR code, with correct segment encoding, padding, and
/// Error Code Correction.
#[derive(Debug)]
pub(crate) struct EncodedMsg<'a> {
    /// Data codewords followed by error correction codewords, block by block.
    data: &'a [u8],
    version: Version,
}

impl<'a> EncodedMsg<'a> {
    /// Encode `segments` into the front of `tmp`, in the smallest version
    /// that holds them.
    pub(crate) fn new(segments: &[&Segment<'_>], tmp: &'a mut [u8]) -> Result<Self, Error> {
        let version = version_for(segments).ok_or(Error::TooLarge)?;
        let buf = tmp
            .get_mut(..version.total_codewords())
            .ok_or(Error::TmpTooSmall)?;
        // clear the output.
        buf.fill(0);

        let (data, ec) = buf
            .split_at_mut_checked(version.max_data())
            .ok_or(Error::TmpTooSmall)?;
        add_segments(data, segments, version);
        compute_error_code(data, ec, version);

        Ok(EncodedMsg { data: buf, version })
    }

    pub(crate) const fn version(&self) -> Version {
        self.version
    }

    pub(crate) fn iter(&self) -> EncodedMsgIterator<'_> {
        EncodedMsgIterator {
            em: self,
            offset: 0,
        }
    }
}

fn add_segments(data: &mut [u8], segments: &[&Segment<'_>], version: Version) {
    let mut writer = BitWriter {
        buf: data,
        offset: 0,
    };

    for s in segments {
        writer.push(s.header());
        // `version_for` chose a version whose length fields hold every count.
        if let Some(field) = s.length_field(version) {
            writer.push(field);
        }
        for bits in s.iter() {
            writer.push(bits);
        }
    }
    writer.push((MODE_STOP, 4));

    let pad_offset = writer.offset.div_ceil(8);
    if let Some(pad) = data.get_mut(pad_offset..) {
        for (byte, &padding) in pad.iter_mut().zip(PADDING.iter().cycle()) {
            *byte = padding;
        }
    }
}

/// Fill `ec` with each block's error correction codewords, in block order.
fn compute_error_code(data: &[u8], ec: &mut [u8], version: Version) {
    let ec_size = version.ec_size();
    let mut generator = [0u8; MAX_EC_SIZE];
    for (value, &exponent) in generator.iter_mut().zip(version.poly()) {
        *value = gf_exp(exponent);
    }
    let generator = generator.get(..ec_size).unwrap_or_default();

    let g1_blk_size = version.g1_blk_size();
    let (g1, g2) = data
        .split_at_checked(version.g1_blocks() * g1_blk_size)
        .unwrap_or((data, &[]));
    let blocks = g1
        .chunks_exact(g1_blk_size)
        .chain(g2.chunks_exact(g1_blk_size + 1));
    for (block, ecc) in blocks.zip(ec.chunks_exact_mut(ec_size)) {
        error_code_for_block(block, generator, ecc);
    }
}

/// The remainder of `block * x^ec_size` divided by the generator polynomial.
fn error_code_for_block(block: &[u8], generator: &[u8], ecc: &mut [u8]) {
    let mut tmp = [0u8; MAX_BLK_SIZE + MAX_EC_SIZE];
    for (dst, &src) in tmp.iter_mut().zip(block) {
        *dst = src;
    }
    for i in 0..block.len() {
        let Some((&mut lead_coeff, rest)) = tmp.get_mut(i..).and_then(<[u8]>::split_first_mut)
        else {
            break;
        };
        if lead_coeff == 0 {
            continue;
        }
        for (u, &g) in rest.iter_mut().zip(generator) {
            *u ^= gf_mul(g, lead_coeff);
        }
    }
    let remainder = tmp.get(block.len()..).unwrap_or_default();
    for (dst, &src) in ecc.iter_mut().zip(remainder) {
        *dst = src;
    }
}

/// Iterator, to retrieve the data in the interleaved order needed by QR code.
#[derive(Debug)]
pub(crate) struct EncodedMsgIterator<'a> {
    em: &'a EncodedMsg<'a>,
    offset: usize,
}

impl Iterator for EncodedMsgIterator<'_> {
    type Item = u8;

    /// Send the bytes in interleaved mode, first byte of first block of group1,
    /// then first byte of second block of group1, ...
    fn next(&mut self) -> Option<Self::Item> {
        let version = self.em.version;
        let (g1_blocks, g2_blocks) = (version.g1_blocks(), version.g2_blocks());
        let (g1_blk_size, ec_size) = (version.g1_blk_size(), version.ec_size());
        let g2_blk_size = g1_blk_size + 1;
        let blocks = g1_blocks + g2_blocks;
        let g1_end = g1_blocks * g1_blk_size;
        let g2_end = g1_end + g2_blocks * g2_blk_size;
        let ec_end = g2_end + ec_size * blocks;

        if self.offset >= ec_end {
            return None;
        }

        let offset = if self.offset < g1_blk_size * blocks {
            // group1 and group2 interleaved
            let blk = self.offset.checked_rem(blocks)?;
            let blk_off = self.offset.checked_div(blocks)?;
            if blk < g1_blocks {
                blk * g1_blk_size + blk_off
            } else {
                g1_end + g2_blk_size * (blk - g1_blocks) + blk_off
            }
        } else if self.offset < g2_end {
            // last byte of group2 blocks
            let blk2 = self.offset - blocks * g1_blk_size;
            g1_end + blk2 * g2_blk_size + g2_blk_size - 1
        } else {
            // EC blocks
            let ec_offset = self.offset - g2_end;
            let blk = ec_offset.checked_rem(blocks)?;
            let blk_off = ec_offset.checked_div(blocks)?;
            g2_end + blk * ec_size + blk_off
        };
        self.offset += 1;
        self.em.data.get(offset).copied()
    }
}
