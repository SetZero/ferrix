//! The btrfs test disk: the `none` fixture image `scripts/gen-btrfs-fixtures.py`
//! made with real `mkfs.btrfs`, unpacked into a raw image every QEMU machine
//! carries as a second `virtio-blk-pci` device, for stage 11's exit: the
//! kernel mounts it through the ring-3 driver and reads its tree back against
//! the fixture's manifest.
//!
//! The fixture is committed packed — only its non-zero 4 KiB blocks, each
//! behind its 8-byte little-endian offset — because a 128 MiB image of mostly
//! zeros has no business in a repository. Unpacking writes each block at its
//! offset into a file of the image's full size, so the file is sparse where
//! the host's filesystem allows and zeros where it does not; either way the
//! device reads what `mkfs.btrfs` wrote. The image is rewritten whenever it is
//! missing, the wrong size, or its first block differs from the fixture's, so
//! a regenerated fixture reaches the next boot without a step of its own.
//!
//! Attached read-only, after the pattern disk, so the pattern disk stays
//! `vda` and this one is `vdb`, in the order devmgr and the boot check name
//! them.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::paths;
use crate::{Error, Result};

/// The packed fixture, as `libs/btrfs`'s tests read it.
const PACKED: &[u8] = include_bytes!("../../libs/btrfs/testdata/none.img.packed");

/// Bytes in a packed block, and in the image's blocks.
const BLOCK: usize = 4096;

/// A packed record: the block's offset, then the block.
const RECORD: usize = 8 + BLOCK;

/// The image's size: what `gen-btrfs-fixtures.py` makes the filesystem on.
pub(crate) const IMAGE_SIZE: u64 = 128 * 1024 * 1024;

/// The image's name under `build/`.
const FILE_NAME: &str = "btrfs-test.img";

/// The image on disk, unpacked if it is not current.
///
/// # Errors
///
/// The fixture being malformed, or the file not writable.
pub(crate) fn ensure() -> Result<PathBuf> {
    let directory = paths::workspace_root().join("build");
    let path = directory.join(FILE_NAME);
    if is_current(&path) {
        return Ok(path);
    }
    std::fs::create_dir_all(&directory)?;
    let partial = directory.join(format!("{FILE_NAME}.{}.partial", std::process::id()));
    write(&partial).map_err(|error| {
        let _ = std::fs::remove_file(&partial);
        Error::new(format!("writing {}: {error}", partial.display()))
    })?;
    std::fs::rename(&partial, &path)
        .map_err(|error| Error::new(format!("renaming to {}: {error}", path.display())))?;
    Ok(path)
}

/// The layout, in one line, for the output of a command that attaches the
/// disk.
pub(crate) fn describe() -> String {
    format!(
        "the none fixture from libs/btrfs/testdata, {} non-zero blocks of {BLOCK} bytes in a \
         {} MiB image made by mkfs.btrfs",
        PACKED.len() / RECORD,
        IMAGE_SIZE / (1024 * 1024),
    )
}

/// The packed records, as `(offset, block)`.
fn records() -> impl Iterator<Item = (u64, &'static [u8])> {
    PACKED.chunks_exact(RECORD).map(|record| {
        let (offset, block) = record.split_at(8);
        let offset = u64::from_le_bytes(offset.try_into().unwrap_or([0; 8]));
        (offset, block)
    })
}

/// Write the image: full size, each packed block at its offset.
fn write(path: &Path) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    file.set_len(IMAGE_SIZE)?;
    for (offset, block) in records() {
        let _ = file.seek(SeekFrom::Start(offset))?;
        file.write_all(block)?;
    }
    file.sync_all()
}

/// Whether the image on disk is the fixture: the right size, and its first
/// packed block where the fixture puts it.
fn is_current(path: &Path) -> bool {
    let check = || -> std::io::Result<bool> {
        let mut file = File::open(path)?;
        if file.metadata()?.len() != IMAGE_SIZE {
            return Ok(false);
        }
        let Some((offset, block)) = records().next() else {
            return Ok(false);
        };
        let _ = file.seek(SeekFrom::Start(offset))?;
        let mut held = vec![0; BLOCK];
        file.read_exact(&mut held)?;
        Ok(held == block)
    };
    check().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixture_is_whole_records_inside_the_image() {
        assert_eq!(PACKED.len() % RECORD, 0, "whole records");
        assert!(records().count() > 0, "some blocks");
        for (offset, block) in records() {
            assert_eq!(offset % BLOCK as u64, 0, "block-aligned offset {offset:#x}");
            assert!(offset + BLOCK as u64 <= IMAGE_SIZE, "inside the image");
            assert_eq!(block.len(), BLOCK);
        }
    }

    #[test]
    fn the_superblock_is_where_btrfs_keeps_it() {
        // btrfs's primary superblock is at 64 KiB and starts with its magic
        // at offset 64: `_BHRfS_M`.
        let super_block = records()
            .find(|(offset, _)| *offset == 0x10000)
            .expect("the fixture has a superblock block");
        assert_eq!(&super_block.1[64..72], b"_BHRfS_M");
    }
}
