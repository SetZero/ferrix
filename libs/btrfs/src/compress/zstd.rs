//! Zstandard (RFC 8878) frames. Stub: replaced by the real decoder.

use crate::BtrfsError;
use crate::items::COMPRESS_ZSTD;

/// Tables and the literal buffer a decode needs, allocated once by the caller.
#[derive(Debug, Default)]
pub struct Workspace {}

/// Expand zstd frames into `output`, returning the bytes written.
pub fn decompress(
    input: &[u8],
    output: &mut [u8],
    workspace: &mut Workspace,
) -> Result<usize, BtrfsError> {
    let _ = (input, output, workspace);
    Err(BtrfsError::BadCompressedData {
        compression: COMPRESS_ZSTD,
    })
}
