// SPDX-License-Identifier: MIT
//
// Ported from Linux, drivers/gpu/drm/drm_panic_qr.rs, whose original author is
// Jocelyn Falempe <jfalempe@redhat.com>. Upstream carries the SPDX line above
// and no copyright line; the notice and licence text are in LICENSE-MIT.

//! A QR code encoder with no heap, for drawing a panic report on a
//! framebuffer.
//!
//! # Provenance
//!
//! This is a port of the Linux kernel's `drivers/gpu/drm/drm_panic_qr.rs`,
//! which is MIT-licensed (see `LICENSE-MIT`), as of upstream commit
//! `7dfabaa0c489ce65883c26ba5cdb15169f168d7a` ("drm/panic: use
//! `core::ffi::CStr` method names", authored 2025-08-13, committed
//! 2025-09-16) — the most recent change to the file on `torvalds/linux`
//! `master` when it was fetched on 2026-09-12. Its original author is Jocelyn
//! Falempe.
//!
//! Upstream's own description, which holds for the port:
//!
//! > It is called from a panic handler, so it shouldn't allocate memory and
//! > does all the work on the stack or on the provided buffers. For
//! > simplification, it only supports low error correction, and applies the
//! > first mask (checkerboard). It will draw the smallest QR code that can
//! > contain the string passed as parameter. To get the most compact QR code,
//! > the start of the URL is encoded as binary, and the compressed kmsg is
//! > encoded as numeric.
//! >
//! > The binary data must be a valid URL parameter, so the easiest way is to
//! > use base64 encoding. But this wastes 25% of data space, so the whole
//! > stack trace won't fit in the QR code. So instead it encodes every 7 bytes
//! > of input into 17 decimal digits, and then uses the efficient numeric
//! > encoding, that encode 3 decimal digits into 10bits. This makes 168bits of
//! > compressed data into 51 decimal digits, into 170bits in the QR code, so
//! > wasting only 1.17%. And the numbers are valid URL parameter, so the
//! > website can do the reverse, to get the binary data. This is the same
//! > algorithm used by Fido v2.2 QR-initiated authentication specification.
//! >
//! > Inspired by these 3 projects, all under MIT license:
//! >
//! > * <https://github.com/kennytm/qrcode-rust>
//! > * <https://github.com/erwanvivien/fast_qr>
//! > * <https://github.com/bjguillot/qr>
//!
//! # What changed in the port
//!
//! The algorithm, the tables and the symbols it draws are upstream's; the
//! tests compare the output module for module against an independent
//! encoder. What changed is the shape around it:
//!
//! - **No kernel bindings.** `kernel::prelude`, `CStr`, `c_char`, `#[export]`
//!   and the two `unsafe extern "C"` entry points are gone. They are replaced
//!   by [`generate`] and [`max_data_size`], which take slices, so the crate
//!   carries `#![forbid(unsafe_code)]`.
//! - **Separate input and output.** Upstream reads the payload out of the
//!   buffer it then draws the symbol into. [`generate`] borrows the payload
//!   and writes to its own buffer, which the borrow checker requires and which
//!   leaves the caller's message intact.
//! - **Errors say why.** Upstream returns a width of 0 for every failure;
//!   [`Error`] distinguishes a payload too large from a buffer too small.
//! - **No reachable panic.** The workspace denies indexing, so: a version
//!   carries its own table row rather than indexing `VPARAM[v - 1]` on every
//!   call; the `BYTES_TO_DIGITS` and `NUM_CHARS_BITS` tables are `match`es;
//!   the decimal FIFO is one `u64` rather than a shifted `[u8; 19]`; GF(256)
//!   multiplication is shift-and-add rather than `EXP_TABLE`/`LOG_TABLE`
//!   lookups; bits are appended one at a time rather than into up to three
//!   computed byte offsets; the segment length field is checked rather than
//!   cast; and length arithmetic that depends on the caller's slice lengths
//!   saturates. Not one indexing exemption was needed.
//! - **No `div10`.** Upstream divides a `u64` by 10 by hand on 32-bit Arm,
//!   because the kernel does not link the helper that division compiles to
//!   there. A freestanding Rust target links `compiler_builtins`, which
//!   provides it.
//! - **One file became four**, split along the stages: `version` (tables),
//!   `encode` (segments, padding and error correction) and `image`
//!   (drawing), with the API here.
//!
//! # Use
//!
//! ```
//! use ferrix_qr::{Error, MIN_MODULES_LEN, MIN_TMP_LEN, generate, max_data_size};
//!
//! let mut modules = [0u8; MIN_MODULES_LEN];
//! let mut tmp = [0u8; MIN_TMP_LEN];
//!
//! // Truncate the report to what a version 20 symbol holds, so it stays
//! // legible on the screen it is drawn on.
//! let report: &[u8] = b"panic: the timer never fired";
//! let report = &report[..report.len().min(max_data_size(20, 0))];
//!
//! let symbol = generate(None, report, &mut modules, &mut tmp)?;
//! assert!(symbol.version() <= 20);
//! for y in 0..symbol.width() {
//!     for x in 0..symbol.width() {
//!         let _dark = symbol.is_dark(x, y);
//!     }
//! }
//! # Ok::<(), Error>(())
//! ```

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

use crate::encode::{EncodedMsg, Segment};
use crate::image::QrImage;
use crate::version::{MAX_CODEWORDS, MAX_WIDTH, Version};

mod encode;
mod image;
mod version;

/// The highest QR code version, whose symbol is 177 modules wide.
pub const MAX_VERSION: u8 = version::MAX_VERSION;

/// Smallest `modules` buffer [`generate`] accepts: 4071 bytes.
///
/// The symbol is stored one bit per module, each row starting on a byte
/// boundary, so a symbol `w` modules wide takes `w * ceil(w / 8)` bytes. For
/// version 40 that is `177 * 23`. Upstream requires the same.
pub const MIN_MODULES_LEN: usize = MAX_WIDTH * MAX_WIDTH.div_ceil(8);

/// Smallest `tmp` buffer [`generate`] accepts: 3706 bytes.
///
/// The encoder writes every codeword of the symbol there before drawing it:
/// for version 40 at level L, 2956 data codewords in 25 blocks, and 30 error
/// correction codewords per block. Upstream requires the same.
pub const MIN_TMP_LEN: usize = MAX_CODEWORDS;

/// Why a payload could not be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The payload does not fit in a version 40 symbol at error correction
    /// level L. [`max_data_size`] says how much does.
    TooLarge,
    /// The `modules` buffer is shorter than [`MIN_MODULES_LEN`].
    ModulesTooSmall,
    /// The `tmp` buffer is shorter than [`MIN_TMP_LEN`].
    TmpTooSmall,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("payload does not fit in a version 40 QR code"),
            Self::ModulesTooSmall => write!(
                f,
                "module buffer is shorter than the {MIN_MODULES_LEN} bytes a version 40 QR code needs"
            ),
            Self::TmpTooSmall => write!(
                f,
                "scratch buffer is shorter than the {MIN_TMP_LEN} bytes a version 40 QR code needs"
            ),
        }
    }
}

impl core::error::Error for Error {}

/// An encoded QR code symbol, borrowed from the buffer it was drawn into.
///
/// The symbol has no quiet zone: a reader needs four light modules around it,
/// which the caller draws.
#[derive(Debug, Clone, Copy)]
pub struct Symbol<'a> {
    /// One bit per module, most significant first, each row starting on a
    /// byte boundary. A set bit is a light module, as upstream draws it.
    modules: &'a [u8],
    width: u8,
    version: u8,
}

impl Symbol<'_> {
    /// Width and height in modules: `17 + 4 * version`.
    pub fn width(&self) -> usize {
        usize::from(self.width)
    }

    /// The QR code version, 1 to 40: the smallest that holds the payload.
    pub const fn version(&self) -> u8 {
        self.version
    }

    /// Whether the module at column `x`, row `y` is dark. The origin is the
    /// top left corner.
    ///
    /// Coordinates outside the symbol are light, which is what the quiet zone
    /// around it must be, so a caller may draw the quiet zone by asking.
    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        let width = self.width();
        if x >= width || y >= width {
            return false;
        }
        // Both coordinates are below 177, so nothing here can overflow.
        let stride = width.div_ceil(8);
        self.modules
            .get(y * stride + x / 8)
            .is_some_and(|byte| byte & (0x80 >> (x % 8)) == 0)
    }
}

/// Encode `data` (optionally after a `url` prefix, as upstream does) into a
/// QR symbol.
///
/// - With `url` of `None`, `data` is encoded as one byte-mode segment, so a
///   reader returns exactly `data`. This is the mode for plain text.
/// - With `url` of `Some`, the url is a byte-mode segment and `data` follows
///   as a numeric segment, every 7 bytes little-endian becoming 17 decimal
///   digits (and a final `n < 7` bytes the fewest digits that hold them: 0, 3,
///   5, 8, 10, 13 or 15). A reader returns the url followed by the digits, so
///   the url can end in a query parameter that a web page turns back into
///   bytes. `Some(b"")` is not `None`: it still encodes `data` as digits.
///
/// `modules` receives the symbol and must hold at least [`MIN_MODULES_LEN`]
/// bytes; `tmp` is scratch and must hold at least [`MIN_TMP_LEN`]. Both
/// minimums are for the largest symbol regardless of the payload, so that a
/// buffer that works for a short message cannot fail for a long one. Neither
/// buffer needs to be cleared first.
///
/// Returns the smallest symbol that holds the payload, at error correction
/// level L with mask pattern 0, or an error if the data does not fit or a
/// buffer is too small.
pub fn generate<'a>(
    url: Option<&[u8]>,
    data: &[u8],
    modules: &'a mut [u8],
    tmp: &mut [u8],
) -> Result<Symbol<'a>, Error> {
    if modules.len() < MIN_MODULES_LEN {
        return Err(Error::ModulesTooSmall);
    }
    if tmp.len() < MIN_TMP_LEN {
        return Err(Error::TmpTooSmall);
    }
    let em = match url {
        None => EncodedMsg::new(&[&Segment::Binary(data)], tmp)?,
        Some(url) => EncodedMsg::new(&[&Segment::Binary(url), &Segment::Numeric(data)], tmp)?,
    };
    let version = em.version();
    let (modules, width) = QrImage::new(version, em.iter(), modules).into_parts();
    Ok(Symbol {
        modules,
        width,
        version: version.number(),
    })
}

/// Largest `data` length that fits with the given url length, for callers
/// sizing a payload.
///
/// Returns the largest `data.len()` for which [`generate`] produces a symbol
/// of at most `version`, or 0 if `version` is not between 1 and 40 or the url
/// leaves no room.
///
/// - If `url_len` > 0, remove the 2 segments header/length and also count the
///   conversion to numeric segments.
/// - If `url_len` = 0, only removes 3 bytes for 1 binary segment. This is the
///   answer for `url` of `None`.
///
/// Both answers are upstream's and are slightly conservative: the numeric
/// case assumes 39 bytes of payload per 40 of capacity, where the encoding
/// needs about 40.5 per 40.
pub fn max_data_size(version: u8, url_len: usize) -> usize {
    let Some(version) = Version::new(version) else {
        return 0;
    };
    let max_data = version.max_data();

    if url_len > 0 {
        // Binary segment (URL) 4 + 16 bits, numeric segment (kmsg) 4 + 12 bits => 5 bytes.
        match max_data
            .checked_sub(url_len)
            .and_then(|room| room.checked_sub(5))
        {
            Some(max) if max > 0 => max * 39 / 40,
            _ => 0,
        }
    } else {
        // Remove 3 bytes for the binary segment (header 4 bits, length 16 bits, stop 4bits).
        max_data.saturating_sub(3)
    }
}

#[cfg(test)]
mod tests;
