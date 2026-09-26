//! Tests for the encoder.
//!
//! Every symbol is checked from two independent directions, because either
//! alone has a blind spot:
//!
//! - **`rqrr` reads it back** from a rendered greyscale image, through finder
//!   detection, format and version decoding, unmasking, error correction and
//!   segment parsing — what a phone does. But error correction repairs a few
//!   misplaced modules without saying so, so a decode alone can pass a symbol
//!   that is wrong.
//! - **`qrcode` builds the same symbol** from the same bits, version and mask,
//!   and the two matrices are compared module for module. That catches a
//!   single wrong module, but on its own would only prove agreement with
//!   another encoder (one upstream names as an inspiration).
//!
//! Capacities come from the specification's table (ISO/IEC 18004, byte mode
//! at level L), not from this crate's tables, so that a wrong row in
//! `VPARAM` fails here rather than being agreed with.

extern crate std;

use std::format;
use std::string::String;
use std::vec;
use std::vec::Vec;

use super::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Byte-mode capacity at level L of each version, from the specification.
const BYTE_CAPACITY_L: [usize; 40] = [
    17, 32, 53, 78, 106, 134, 154, 192, 230, 271, 321, 367, 425, 458, 520, 586, 644, 718, 792, 858,
    929, 1003, 1091, 1171, 1273, 1367, 1465, 1528, 1628, 1732, 1840, 1952, 2068, 2188, 2303, 2431,
    2563, 2699, 2809, 2953,
];

/// Deterministic, non-repeating bytes covering every value.
fn pseudo_random(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 24) as u8
        })
        .collect()
}

/// A payload of `len` printable ASCII bytes, like a panic report.
fn text(len: usize) -> Vec<u8> {
    b"FERRIX-PANIC stage 3 self-check failed: the timer never fired\n  at kernel/src/main.rs:153\n"
        .iter()
        .copied()
        .cycle()
        .take(len)
        .collect()
}

/// The owned result of an encode, so a test can hold several at once.
struct Encoded {
    version: u8,
    width: usize,
    dark: Vec<bool>,
}

impl Encoded {
    fn is_dark(&self, x: usize, y: usize) -> bool {
        x < self.width && y < self.width && self.dark[y * self.width + x]
    }
}

/// Encode into freshly dirtied minimum-size buffers.
fn encode(url: Option<&[u8]>, data: &[u8]) -> Result<Encoded, Error> {
    let mut modules = vec![0xa5; MIN_MODULES_LEN];
    let mut tmp = vec![0x5a; MIN_TMP_LEN];
    let symbol = generate(url, data, &mut modules, &mut tmp)?;
    let width = symbol.width();
    let dark = (0..width * width)
        .map(|i| symbol.is_dark(i % width, i / width))
        .collect();
    Ok(Encoded {
        version: symbol.version(),
        width,
        dark,
    })
}

/// The fewest decimal digits that hold `len` bytes, derived from the
/// definition rather than copied from the encoder's table.
fn digits_for(len: usize) -> usize {
    (0..)
        .find(|&d| 10u128.pow(d) >= 1u128 << (8 * len))
        .unwrap() as usize
}

/// Upstream's byte-to-digit packing, written independently.
fn to_digits(data: &[u8]) -> Vec<u8> {
    let mut out = String::new();
    for chunk in data.chunks(7) {
        let mut buf = [0u8; 8];
        buf[..chunk.len()].copy_from_slice(chunk);
        let width = digits_for(chunk.len());
        out.push_str(&format!("{:0width$}", u64::from_le_bytes(buf)));
    }
    out.into_bytes()
}

/// What a web page does with the digits: the inverse of [`to_digits`].
fn from_digits(digits: &[u8]) -> Vec<u8> {
    let full = digits_for(7);
    let mut out = Vec::new();
    for chunk in digits.chunks(full) {
        let len = (0..=7).find(|&n| digits_for(n) == chunk.len()).unwrap();
        let value: u64 = std::str::from_utf8(chunk).unwrap().parse().unwrap();
        out.extend_from_slice(&value.to_le_bytes()[..len]);
    }
    out
}

// ---------------------------------------------------------------------------
// The two independent checks
// ---------------------------------------------------------------------------

/// Render the symbol with a quiet zone and read it with `rqrr`.
fn decode(symbol: &Encoded) -> (rqrr::MetaData, Vec<u8>) {
    const QUIET: usize = 4;
    const SCALE: usize = 3;
    let size = (symbol.width + 2 * QUIET) * SCALE;
    let mut image = rqrr::PreparedImage::prepare_from_greyscale(size, size, |x, y| {
        // Below the quiet zone the subtraction wraps to a huge coordinate,
        // which exercises `is_dark`'s out-of-range answer in `encode`'s copy.
        let (mx, my) = (
            (x / SCALE).wrapping_sub(QUIET),
            (y / SCALE).wrapping_sub(QUIET),
        );
        if symbol.is_dark(mx, my) { 0 } else { 255 }
    });
    let grids = image.detect_grids();
    assert_eq!(grids.len(), 1, "expected exactly one symbol in the image");
    let mut out = Vec::new();
    let meta = grids[0]
        .decode_to(&mut out)
        .expect("rqrr failed to decode the symbol");
    (meta, out)
}

/// The symbol `qrcode` draws for the same segments in the same version, at
/// level L with mask pattern 0.
fn reference(version: u8, url: Option<&[u8]>, data: &[u8]) -> Vec<bool> {
    use qrcode::bits::Bits;
    use qrcode::canvas::{Canvas, MaskPattern};
    use qrcode::{Color, EcLevel, Version as QrVersion};

    let qr_version = QrVersion::Normal(i16::from(version));
    let mut bits = Bits::new(qr_version);
    match url {
        None => bits.push_byte_data(data).unwrap(),
        Some(url) => {
            bits.push_byte_data(url).unwrap();
            bits.push_numeric_data(&to_digits(data)).unwrap();
        }
    }
    bits.push_terminator(EcLevel::L).unwrap();
    let (data_codewords, ec_codewords) =
        qrcode::ec::construct_codewords(&bits.into_bytes(), qr_version, EcLevel::L).unwrap();
    let mut canvas = Canvas::new(qr_version, EcLevel::L);
    canvas.draw_all_functional_patterns();
    canvas.draw_data(&data_codewords, &ec_codewords);
    canvas.apply_mask(MaskPattern::Checkerboard);
    canvas
        .into_colors()
        .into_iter()
        .map(|c| c == Color::Dark)
        .collect()
}

/// Assert the symbol matches `qrcode`'s module for module, naming the first
/// module that differs.
fn assert_matches_reference(symbol: &Encoded, url: Option<&[u8]>, data: &[u8]) {
    let expected = reference(symbol.version, url, data);
    assert_eq!(expected.len(), symbol.dark.len(), "matrix sizes differ");
    if let Some(i) = (0..expected.len()).find(|&i| expected[i] != symbol.dark[i]) {
        panic!(
            "version {} ({} bytes of data): module ({}, {}) is {} but the reference has it {}",
            symbol.version,
            data.len(),
            i % symbol.width,
            i / symbol.width,
            if symbol.dark[i] { "dark" } else { "light" },
            if expected[i] { "dark" } else { "light" },
        );
    }
}

/// Encode plain data, and check the symbol both ways.
fn round_trip(data: &[u8]) -> u8 {
    let symbol = encode(None, data).unwrap();
    assert_matches_reference(&symbol, None, data);
    let (meta, decoded) = decode(&symbol);
    assert_eq!(
        meta.version.0,
        usize::from(symbol.version),
        "rqrr read another version"
    );
    assert_eq!(
        meta.ecc_level, 1,
        "rqrr numbers the levels M=0, L=1, H=2, Q=3"
    );
    assert_eq!(meta.mask, 0, "the encoder only uses mask 0");
    assert_eq!(decoded, data, "rqrr read back other bytes");
    symbol.version
}

// ---------------------------------------------------------------------------
// Buffer sizes and the version/width relation
// ---------------------------------------------------------------------------

#[test]
fn minimum_buffers_are_upstreams() {
    assert_eq!(MIN_MODULES_LEN, 4071);
    assert_eq!(MIN_TMP_LEN, 3706);
    assert_eq!(MAX_VERSION, 40);
}

#[test]
fn width_is_17_plus_4_per_version() {
    for (index, &capacity) in BYTE_CAPACITY_L.iter().enumerate() {
        let version = index + 1;
        let symbol = encode(None, &text(capacity)).unwrap();
        assert_eq!(usize::from(symbol.version), version);
        assert_eq!(symbol.width, 17 + 4 * version);
    }
}

// ---------------------------------------------------------------------------
// Plain data: the mode the panic handler uses
// ---------------------------------------------------------------------------

#[test]
fn round_trips_small_payloads() {
    for len in [0, 1, 2, 3, 7, 16] {
        assert_eq!(
            round_trip(&text(len)),
            1,
            "{len} bytes should fit version 1"
        );
    }
}

/// Every version at exactly its capacity, and one byte past it: the version
/// is the smallest that holds the payload, and both symbols read back.
#[test]
fn round_trips_every_version_boundary() {
    for (index, &capacity) in BYTE_CAPACITY_L.iter().enumerate() {
        let version = (index + 1) as u8;
        let data = pseudo_random(capacity + 1, capacity as u32);
        assert_eq!(round_trip(&data[..capacity]), version, "{capacity} bytes");
        if version < MAX_VERSION {
            assert_eq!(round_trip(&data), version + 1, "{} bytes", capacity + 1);
        }
    }
}

#[test]
fn round_trips_the_largest_payload() {
    let data = text(BYTE_CAPACITY_L[39]);
    assert_eq!(round_trip(&data), 40);
}

#[test]
fn round_trips_every_byte_value() {
    let data: Vec<u8> = (0..=255).collect();
    let _ = round_trip(&data);
    let data: Vec<u8> = (0..=255).rev().cycle().take(1000).collect();
    let _ = round_trip(&data);
}

#[test]
fn rejects_a_payload_past_version_40() {
    let data = text(BYTE_CAPACITY_L[39] + 1);
    assert!(matches!(encode(None, &data), Err(Error::TooLarge)));
}

/// Buffers left over from a larger symbol must not bleed into a smaller one.
#[test]
fn reused_buffers_give_the_same_symbol() {
    let mut modules = vec![0u8; MIN_MODULES_LEN];
    let mut tmp = vec![0u8; MIN_TMP_LEN];
    let _ = generate(None, &pseudo_random(2953, 7), &mut modules, &mut tmp).unwrap();

    let data = text(40);
    let symbol = generate(None, &data, &mut modules, &mut tmp).unwrap();
    let fresh = encode(None, &data).unwrap();
    assert_eq!(symbol.width(), fresh.width);
    for y in 0..fresh.width {
        for x in 0..fresh.width {
            assert_eq!(
                symbol.is_dark(x, y),
                fresh.is_dark(x, y),
                "module ({x}, {y})"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// url + numeric data
// ---------------------------------------------------------------------------

const URL: &[u8] = b"https://panic.example.org/?v=1&z=";

fn round_trip_url(url: &[u8], data: &[u8]) -> u8 {
    let symbol = encode(Some(url), data).unwrap();
    assert_matches_reference(&symbol, Some(url), data);
    let (meta, decoded) = decode(&symbol);
    assert_eq!(meta.version.0, usize::from(symbol.version));
    let (prefix, digits) = decoded.split_at(url.len());
    assert_eq!(prefix, url, "the url does not lead the payload");
    assert!(
        digits.iter().all(u8::is_ascii_digit),
        "the data is not all digits"
    );
    assert_eq!(
        digits,
        to_digits(data),
        "the digits are not upstream's packing"
    );
    assert_eq!(
        from_digits(digits),
        data,
        "the digits do not unpack to the data"
    );
    symbol.version
}

#[test]
fn round_trips_url_and_data() {
    for len in [0, 1, 2, 6, 7, 8, 13, 14, 15, 100, 999] {
        let _ = round_trip_url(URL, &pseudo_random(len, len as u32 + 1));
    }
}

#[test]
fn an_empty_url_still_packs_data_as_digits() {
    let _ = round_trip_url(b"", &pseudo_random(50, 3));
}

// ---------------------------------------------------------------------------
// max_data_size
// ---------------------------------------------------------------------------

#[test]
fn max_data_size_without_url_fits_its_version() {
    for version in 1..=MAX_VERSION {
        let len = max_data_size(version, 0);
        let capacity = BYTE_CAPACITY_L[usize::from(version) - 1];
        assert!(
            len <= capacity,
            "version {version}: {len} > capacity {capacity}"
        );
        // Upstream reserves 3 bytes where versions 1-9 need 1.5 and 10-40 need
        // 2.5, so it is never more than a byte or two short.
        assert!(
            capacity - len <= 2,
            "version {version}: {len} wastes too much"
        );
        let symbol = encode(None, &text(len)).unwrap();
        assert_eq!(symbol.version, version);
    }
}

#[test]
fn max_data_size_with_url_fits_its_version() {
    for version in 1..=MAX_VERSION {
        for url_len in [1, 10, URL.len(), 100] {
            let len = max_data_size(version, url_len);
            if len == 0 {
                continue;
            }
            let url = text(url_len);
            // The encoded size depends only on the lengths, so the worst-case
            // value (all 0xff) is as good as any.
            let symbol = encode(Some(&url), &vec![0xff; len]).unwrap();
            assert!(
                symbol.version <= version,
                "version {version}, url {url_len}: {len} bytes needed version {}",
                symbol.version
            );
        }
    }
    let _ = round_trip_url(URL, &pseudo_random(max_data_size(40, URL.len()), 9));
}

#[test]
fn max_data_size_edges() {
    assert_eq!(max_data_size(0, 0), 0);
    assert_eq!(max_data_size(41, 0), 0);
    assert_eq!(max_data_size(u8::MAX, 10), 0);
    // Version 1 holds 19 data codewords: a url of 14 or more leaves nothing.
    assert_eq!(max_data_size(1, 14), 0);
    assert_eq!(max_data_size(1, usize::MAX), 0);
    assert!(max_data_size(1, 13) == 0 || max_data_size(1, 12) > 0);
    assert_eq!(max_data_size(40, 0), 2953);
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[test]
fn rejects_small_buffers_before_looking_at_the_data() {
    let mut modules = vec![0u8; MIN_MODULES_LEN];
    let mut tmp = vec![0u8; MIN_TMP_LEN];
    let mut short_modules = vec![0u8; MIN_MODULES_LEN - 1];
    let mut short_tmp = vec![0u8; MIN_TMP_LEN - 1];
    let data = text(1);

    let result = generate(None, &data, &mut short_modules, &mut tmp);
    assert_eq!(result.unwrap_err(), Error::ModulesTooSmall);
    let result = generate(None, &data, &mut modules, &mut short_tmp);
    assert_eq!(result.unwrap_err(), Error::TmpTooSmall);
    let result = generate(Some(URL), &data, &mut [], &mut []);
    assert_eq!(result.unwrap_err(), Error::ModulesTooSmall);
}

#[test]
fn larger_buffers_are_fine() {
    let mut modules = vec![0u8; MIN_MODULES_LEN * 2];
    let mut tmp = vec![0u8; MIN_TMP_LEN + 1];
    let symbol = generate(None, b"hello", &mut modules, &mut tmp).unwrap();
    assert_eq!(symbol.version(), 1);
}

#[test]
fn rejects_url_and_data_past_version_40() {
    let url = text(2954);
    assert!(matches!(encode(Some(&url), &[]), Err(Error::TooLarge)));
    let data = vec![0u8; 3000];
    assert!(matches!(encode(Some(URL), &data), Err(Error::TooLarge)));
}

#[test]
fn errors_display_a_sentence() {
    for error in [Error::TooLarge, Error::ModulesTooSmall, Error::TmpTooSmall] {
        let message = format!("{error}");
        assert!(message.len() > 20, "{error:?} displays as {message:?}");
    }
    assert!(format!("{}", Error::ModulesTooSmall).contains("4071"));
    assert!(format!("{}", Error::TmpTooSmall).contains("3706"));
}

// ---------------------------------------------------------------------------
// Structure, read straight off the specification
// ---------------------------------------------------------------------------

/// The finder patterns, their separators, the timing patterns, the dark
/// module and both copies of the format information, for a version with and
/// without version information.
#[test]
fn function_patterns_are_where_the_specification_puts_them() {
    for len in [10, 300] {
        let symbol = encode(None, &text(len)).unwrap();
        let w = symbol.width;
        for (cx, cy) in [(0, 0), (w - 7, 0), (0, w - 7)] {
            for dy in 0..7 {
                for dx in 0..7 {
                    let ring = dx.max(dy).max(6 - dx).max(6 - dy);
                    // Dark on the outer ring and the 3x3 centre, light between.
                    let dark = ring == 6 || ring <= 4;
                    assert_eq!(
                        symbol.is_dark(cx + dx, cy + dy),
                        dark,
                        "finder at ({cx}, {cy})"
                    );
                }
            }
        }
        for i in 0..8 {
            assert!(!symbol.is_dark(7, i) && !symbol.is_dark(i, 7), "separator");
        }
        for i in 8..w - 8 {
            assert_eq!(symbol.is_dark(i, 6), i % 2 == 0, "horizontal timing at {i}");
            assert_eq!(symbol.is_dark(6, i), i % 2 == 0, "vertical timing at {i}");
        }
        assert!(symbol.is_dark(8, w - 8), "the dark module");

        // Format information for level L, mask 0, most significant bit first.
        let format: u16 = 0b111_0111_1100_0100;
        let bit = |i: usize| format >> (14 - i) & 1 == 1;
        let around_top_left = [
            (0, 8),
            (1, 8),
            (2, 8),
            (3, 8),
            (4, 8),
            (5, 8),
            (7, 8),
            (8, 8),
        ]
        .into_iter()
        .chain([(8, 7), (8, 5), (8, 4), (8, 3), (8, 2), (8, 1), (8, 0)]);
        let split = (0..7)
            .map(|i| (8, w - 1 - i))
            .chain((0..8).map(|i| (w - 8 + i, 8)));
        for (i, ((ax, ay), (bx, by))) in around_top_left.zip(split).enumerate() {
            assert_eq!(
                symbol.is_dark(ax, ay),
                bit(i),
                "format bit {i} near the top left"
            );
            assert_eq!(
                symbol.is_dark(bx, by),
                bit(i),
                "format bit {i} in the split copy"
            );
        }
    }
}

#[test]
fn version_information_appears_from_version_7() {
    let symbol = encode(None, &text(BYTE_CAPACITY_L[6])).unwrap();
    assert_eq!(symbol.version, 7);
    let w = symbol.width;
    // Version 7's 18-bit version information, per the specification's table.
    let info: u32 = 0b00_0111_1100_1001_0100;
    for i in 0..18 {
        let (row, column) = (i / 3, w - 11 + i % 3);
        let dark = info >> i & 1 == 1;
        assert_eq!(symbol.is_dark(column, row), dark, "top right copy, bit {i}");
        assert_eq!(
            symbol.is_dark(row, column),
            dark,
            "bottom left copy, bit {i}"
        );
    }
}

#[test]
fn outside_the_symbol_is_light() {
    let mut modules = vec![0u8; MIN_MODULES_LEN];
    let mut tmp = vec![0u8; MIN_TMP_LEN];
    let symbol = generate(None, b"x", &mut modules, &mut tmp).unwrap();
    let w = symbol.width();
    assert!(symbol.is_dark(0, 0));
    for (x, y) in [(w, 0), (0, w), (w, w), (usize::MAX, 0), (0, usize::MAX)] {
        assert!(!symbol.is_dark(x, y), "({x}, {y})");
    }
}
