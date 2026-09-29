//! The test disk: a raw image every QEMU machine carries as a
//! `virtio-blk-pci` device, for stage 10's ring-3 block driver to read.
//!
//! Generated here rather than by `dd` or `qemu-img` for the reason `fat.rs`
//! writes FAT32: no host tool, and the same bytes on every machine. It lives at
//! `build/test-disk.img`, shared by every architecture because nothing in it
//! depends on one, and is rewritten whenever it is missing, the wrong size, or
//! carries a different [`VERSION`] — so changing the layout below means
//! bumping that number.
//!
//! # Layout
//!
//! [`SECTORS`] sectors of [`SECTOR_SIZE`] bytes: 64 MiB. Every sector `n`
//! (counting from 0) is its own label, so a driver that read the wrong sector,
//! read half of one, or read nothing can tell which:
//!
//! | Offset    | Bytes | Contents                                          |
//! |-----------|-------|---------------------------------------------------|
//! | 0         | 8     | `n`, little-endian `u64`                          |
//! | 8         | 9     | [`MAGIC`], `b"FERRIXBLK"`                          |
//! | 17        | 1     | [`VERSION`], currently 1                          |
//! | 18 to 511 | 494   | for byte offset `i`: `(n * K >> (8 * (i % 8))) as u8 ^ i as u8` |
//!
//! where `K` is [`FILL_MULTIPLIER`], `0x9e37_79b9_7f4a_7c15`, the
//! multiplication wrapping at 64 bits. [`sector`] computes it, and a driver
//! test can carry the same few lines. The fill makes two sectors whose
//! numbers differ in any bit differ in most of their bytes, not only in the
//! first eight.
//!
//! No sector is ever zero, so there is nothing for a filesystem to leave as a
//! hole: the file takes its whole 64 MiB on every platform.
//!
//! # Why no firmware boots from it
//!
//! Sector 0 is the sector number zero, the magic, and a fill that for `n = 0`
//! is the offset itself — so bytes 510 and 511 are `0xfe 0xff`, not the MBR's
//! `0x55 0xaa`, byte 0 is no jump instruction a FAT boot sector starts with,
//! and neither sector 1 nor the last sector starts with a GPT header's
//! `EFI PART`. Firmware that looks for a partition table or a filesystem finds
//! neither, and moves on to the next disk.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::paths;
use crate::{Error, Result};

/// Bytes in a sector: virtio-blk's unit, whatever the device's block size.
pub(crate) const SECTOR_SIZE: usize = 512;

/// Sectors on the disk: 64 MiB of them.
pub(crate) const SECTORS: u64 = 64 * 1024 * 1024 / SECTOR_SIZE as u64;

/// What every sector carries at offset 8.
pub(crate) const MAGIC: &[u8; 9] = b"FERRIXBLK";

/// The layout's version, at offset 17 of every sector. Bump it whenever the
/// layout changes, and every tree regenerates its image.
pub(crate) const VERSION: u8 = 1;

/// Where the fill starts: after the sector number, the magic and the version.
pub(crate) const FILL_OFFSET: usize = 8 + MAGIC.len() + 1;

/// The fill's multiplier: 2^64 divided by the golden ratio, which spreads a
/// change in any bit of the sector number across all eight bytes.
pub(crate) const FILL_MULTIPLIER: u64 = 0x9e37_79b9_7f4a_7c15;

/// The image's name under `build/`.
const FILE_NAME: &str = "test-disk.img";

/// Sectors written per `write` call: one MiB at a time.
const CHUNK_SECTORS: u64 = 2048;

/// The layout, in one line, for the output of a command that attaches the disk.
pub(crate) fn describe() -> String {
    format!(
        "{SECTORS} sectors of {SECTOR_SIZE} bytes; sector n holds n as a little-endian u64 at 0, \
         b\"{}\" at 8, version {VERSION} at 17, and at each offset i from {FILL_OFFSET} to \
         {last}: (n * {FILL_MULTIPLIER:#x} >> (8 * (i % 8))) as u8 ^ i as u8",
        String::from_utf8_lossy(MAGIC),
        last = SECTOR_SIZE - 1,
    )
}

/// The contents of sector `number`.
pub(crate) fn sector(number: u64) -> [u8; SECTOR_SIZE] {
    let seed = number.wrapping_mul(FILL_MULTIPLIER);
    let mut bytes = [0; SECTOR_SIZE];
    for (offset, byte) in bytes.iter_mut().enumerate() {
        *byte = (seed >> (8 * (offset % 8))) as u8 ^ offset as u8;
    }
    let header = number
        .to_le_bytes()
        .into_iter()
        .chain(*MAGIC)
        .chain([VERSION]);
    for (byte, value) in bytes.iter_mut().zip(header) {
        *byte = value;
    }
    bytes
}

/// The test disk, written first if it is missing or stale.
pub(crate) fn ensure() -> Result<PathBuf> {
    let directory = paths::workspace_root().join("build");
    let path = directory.join(FILE_NAME);
    if is_current(&path) {
        return Ok(path);
    }
    std::fs::create_dir_all(&directory)?;
    // Written beside the image and renamed over it, so a QEMU started by a
    // concurrent `test-boot` for another architecture opens either the old
    // file or the whole new one, never a half-written one.
    let partial = directory.join(format!("{FILE_NAME}.{}.partial", std::process::id()));
    write(&partial).map_err(|error| {
        let _ = std::fs::remove_file(&partial);
        Error::new(format!("writing {}: {error}", partial.display()))
    })?;
    std::fs::rename(&partial, &path)
        .map_err(|error| Error::new(format!("renaming to {}: {error}", path.display())))?;
    Ok(path)
}

/// Write the whole image to `path`.
fn write(path: &Path) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    let mut chunk = Vec::with_capacity(CHUNK_SECTORS as usize * SECTOR_SIZE);
    let mut number = 0;
    while number < SECTORS {
        chunk.clear();
        let end = SECTORS.min(number + CHUNK_SECTORS);
        for n in number..end {
            chunk.extend_from_slice(&sector(n));
        }
        file.write_all(&chunk)?;
        number = end;
    }
    file.sync_all()
}

/// True if `path` is an image of this layout: the right size, and the first
/// and last sectors this version writes. The layout is a function of
/// [`VERSION`], so two sectors are enough to tell an old one from a new one.
fn is_current(path: &Path) -> bool {
    let check = || -> std::io::Result<bool> {
        let mut file = File::open(path)?;
        if file.metadata()?.len() != SECTORS * SECTOR_SIZE as u64 {
            return Ok(false);
        }
        let mut held = [0; SECTOR_SIZE];
        file.read_exact(&mut held)?;
        if held != sector(0) {
            return Ok(false);
        }
        let _ = file.seek(SeekFrom::End(-(SECTOR_SIZE as i64)))?;
        file.read_exact(&mut held)?;
        Ok(held == sector(SECTORS - 1))
    };
    check().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sector_names_itself_and_the_layout() {
        for number in [0, 1, 255, 256, 0x1_0000, SECTORS - 1] {
            let bytes = sector(number);
            assert_eq!(bytes[..8], number.to_le_bytes(), "sector {number}");
            assert_eq!(&bytes[8..17], MAGIC, "sector {number}");
            assert_eq!(bytes[17], VERSION, "sector {number}");
        }
    }

    #[test]
    fn the_fill_is_the_documented_rule() {
        let number = 0x1234_5678_u64;
        let bytes = sector(number);
        let seed = number.wrapping_mul(FILL_MULTIPLIER);
        for (offset, byte) in bytes.iter().enumerate().skip(FILL_OFFSET) {
            let want = (seed >> (8 * (offset % 8))) as u8 ^ offset as u8;
            assert_eq!(*byte, want, "offset {offset}");
        }
        // For sector zero the rule leaves only the offset.
        assert_eq!(sector(0)[510..], [0xfe, 0xff]);
    }

    #[test]
    fn neighbouring_sectors_differ_beyond_their_numbers() {
        for (a, b) in [(0, 1), (1, 257), (7, 7 + (1 << 32))] {
            let (a, b) = (sector(a), sector(b));
            let differing = a[FILL_OFFSET..]
                .iter()
                .zip(&b[FILL_OFFSET..])
                .filter(|(x, y)| x != y)
                .count();
            assert!(
                differing > (SECTOR_SIZE - FILL_OFFSET) / 2,
                "only {differing} fill bytes differ"
            );
        }
    }

    #[test]
    fn nothing_on_it_looks_bootable() {
        let first = sector(0);
        assert_ne!(first[510..], [0x55, 0xaa], "an MBR signature");
        assert!(
            ![0xeb, 0xe9].contains(&first[0]),
            "a FAT boot sector's jump"
        );
        for number in [1, SECTORS - 1] {
            assert_ne!(&sector(number)[..8], b"EFI PART", "a GPT header");
        }
    }

    #[test]
    fn the_description_carries_the_constants() {
        let line = describe();
        for part in [
            "131072 sectors of 512 bytes",
            "FERRIXBLK",
            "0x9e3779b97f4a7c15",
        ] {
            assert!(line.contains(part), "`{part}` missing from `{line}`");
        }
    }

    #[test]
    fn a_stale_or_short_image_is_not_current() {
        let directory =
            std::env::temp_dir().join(format!("ferrix-test-disk-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("disk.img");
        assert!(!is_current(&path), "a missing image");
        std::fs::write(&path, sector(0)).unwrap();
        assert!(!is_current(&path), "a one-sector image");
        write(&path).unwrap();
        assert!(is_current(&path), "a freshly written image");
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        // Sector zero's number is already eight zero bytes.
        file.write_all(&[0xff; 8]).unwrap();
        drop(file);
        assert!(!is_current(&path), "an image whose first sector changed");
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
