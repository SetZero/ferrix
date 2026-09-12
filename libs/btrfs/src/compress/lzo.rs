//! LZO1X, in btrfs's segmented framing. Stub: replaced by the real decoder.

use crate::BtrfsError;
use crate::items::COMPRESS_LZO;

/// Expand a btrfs LZO extent into `output`, returning the bytes written.
pub fn decompress(input: &[u8], output: &mut [u8], sectorsize: u32) -> Result<usize, BtrfsError> {
    let _ = (input, output, sectorsize);
    Err(BtrfsError::BadCompressedData {
        compression: COMPRESS_LZO,
    })
}
