//! zlib (RFC 1950) around DEFLATE (RFC 1951). Stub: replaced by the real decoder.

use crate::BtrfsError;
use crate::items::COMPRESS_ZLIB;

/// Expand a zlib stream into `output`, returning the bytes written.
pub fn decompress(input: &[u8], output: &mut [u8]) -> Result<usize, BtrfsError> {
    let _ = (input, output);
    Err(BtrfsError::BadCompressedData {
        compression: COMPRESS_ZLIB,
    })
}
