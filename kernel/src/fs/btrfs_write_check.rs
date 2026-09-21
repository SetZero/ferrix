//! Stage 12's exit, the guest's half: a blank btrfs volume `mkfs.btrfs` made,
//! written through the kernel's own mount path, and read back after a remount.
//!
//! `xtask` attaches a fresh copy of the `blank` fixture as the third
//! virtio-blk disk, so by the time this runs it is `vdc` in the registry. The
//! check mounts it *writable* — `mount -t btrfs /dev/vdc /mnt-rw`, with no
//! `MS_RDONLY` — and builds a tree through the VFS: nested directories, files
//! at every size that matters (empty, inline, a sector, many extents, and one
//! larger than the data chunk `mkfs.btrfs` made, so the writer must allocate
//! a chunk), a file with a hole, a symlink, a hard link, an overwrite in the
//! middle of a file, a truncation, a rename and an unlink.
//!
//! Then it syncs, **unmounts, and mounts again**, and only then reads
//! everything back. That is the point of the check: after the unmount nothing
//! of the tree is in memory, so every byte compared comes off the disk,
//! through the driver, the block ring, the volume reader and its checksums.
//! What the guest cannot judge — whether the trees it wrote are the trees
//! btrfs would have written — is `btrfs check` on the host, which `xtask
//! test-btrfs` runs over the same image after the boot.

use alloc::vec;
use alloc::vec::Vec;

use ferrix_btrfs::crc32c::{crc32c, crc32c_update};
use ferrix_vfs::initramfs::makedev;
use ferrix_vfs::{Errno, FileType, NewNode, RenameMode};

use crate::block_ring::VIRTIO_BLK_MAJOR;
use crate::fs;

/// Where the writable disk is mounted.
const MOUNT_POINT: &[u8] = b"/mnt-rw";

/// The writable disk: the third virtio-blk function, `vdc`.
const DISK_INDEX: u32 = 2;

/// The big file's size: more than the 8 MiB data chunk `mkfs.btrfs` makes, so
/// writing it must allocate a chunk, with everything that means — a chunk
/// item, device extents, the device item and the block group.
const BIG: usize = 10 * 1024 * 1024;

/// How much is written at a time, which is what a program's `write` would be.
const CHUNK: usize = 64 * 1024;

/// What the check found.
#[derive(Debug)]
pub(crate) struct Report {
    pub(crate) files: u32,
    pub(crate) directories: u32,
    /// Bytes of file data written and read back.
    pub(crate) bytes: u64,
    /// Why nothing was checked: no third disk on this machine.
    pub(crate) skipped: Option<&'static str>,
}

/// One file the check makes, and what it must hold afterwards.
struct Expected {
    path: &'static [u8],
    len: usize,
    crc: u32,
}

/// The bytes file `seed` holds at `offset`: different in every file and along
/// each one, so a read that lands in the wrong place is a CRC that differs.
fn byte(seed: u64, offset: usize) -> u8 {
    let mixed = (offset as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ seed.wrapping_mul(31);
    (mixed >> 24) as u8
}

/// `len` bytes of file `seed` from `offset`.
fn pattern(seed: u64, offset: usize, len: usize) -> Vec<u8> {
    (offset..offset.saturating_add(len))
        .map(|at| byte(seed, at))
        .collect()
}

/// Write `data` at `offset` in the file at `path`, as a program would.
fn write_at(path: &[u8], offset: u64, data: &[u8]) -> Result<(), &'static str> {
    let ns = fs::namespace();
    let ctx = ns.context();
    let at = ns
        .resolve(&ctx, None, path, true)
        .map_err(|_| "a file written to is gone")?;
    let inode = at.inode().map_err(|_| "a file written to has no inode")?;
    let (written, _) = inode
        .write_at(offset, data, false)
        .map_err(|_| "a file could not be written")?;
    if written == data.len() {
        Ok(())
    } else {
        Err("a write was short")
    }
}

/// Write `data` into a new file at `path`, in pieces, as a program would.
fn write_file(path: &[u8], data: &[u8]) -> Result<(), &'static str> {
    make(path)?;
    let mut at = 0u64;
    for piece in data.chunks(CHUNK) {
        write_at(path, at, piece)?;
        at = at.saturating_add(piece.len() as u64);
    }
    Ok(())
}

/// Make an empty file at `path`.
fn make(path: &[u8]) -> Result<(), &'static str> {
    let ns = fs::namespace();
    let ctx = ns.context();
    ns.mknod(&ctx, None, path, NewNode::Regular, 0o644)
        .map_err(|_| "a file could not be created")
}

/// Write `len` bytes of file `seed` at `path`, a piece at a time, and answer
/// the CRC-32C of what was written.
///
/// Nothing holds the whole file: a kernel with a few megabytes of heap
/// cannot, and a check that needed to would be testing the heap.
fn write_pattern(path: &[u8], seed: u64, len: usize) -> Result<u32, &'static str> {
    make(path)?;
    let mut register = !0u32;
    let mut at = 0usize;
    while at < len {
        let piece = pattern(seed, at, CHUNK.min(len - at));
        write_at(path, at as u64, &piece)?;
        register = crc32c_update(register, &piece);
        at = at.saturating_add(piece.len());
    }
    Ok(!register)
}

/// Read the file at `path` back a piece at a time; answer its length and its
/// CRC-32C.
fn read_pattern(path: &[u8]) -> Result<(usize, u32), &'static str> {
    let ns = fs::namespace();
    let ctx = ns.context();
    let at = ns
        .resolve(&ctx, None, path, true)
        .map_err(|_| "a file written is gone")?;
    let inode = at.inode().map_err(|_| "a file written has no inode")?;
    let mut buffer = vec![0u8; CHUNK];
    let mut register = !0u32;
    let mut offset = 0u64;
    loop {
        let read = inode
            .read_at(offset, &mut buffer)
            .map_err(|_| "a file written could not be read back")?;
        if read == 0 {
            break;
        }
        register = crc32c_update(register, buffer.get(..read).unwrap_or_default());
        offset = offset.saturating_add(read as u64);
    }
    let len = usize::try_from(offset).map_err(|_| "a file is longer than this machine counts")?;
    Ok((len, !register))
}

/// Build the tree, and say what every file must hold afterwards.
fn build() -> Result<Vec<Expected>, &'static str> {
    let ns = fs::namespace();
    let ctx = ns.context();
    let mut expected = Vec::new();
    for dir in [b"/mnt-rw/a".as_slice(), b"/mnt-rw/a/b".as_slice()] {
        ns.mkdir(&ctx, None, dir, 0o755)
            .map_err(|_| "a directory could not be made")?;
    }
    // Empty, inline, one sector, several extents, and past the data chunk.
    let sizes: [(&'static [u8], usize); 5] = [
        (b"/mnt-rw/empty", 0),
        (b"/mnt-rw/a/inline", 700),
        (b"/mnt-rw/a/sector", 4096),
        (b"/mnt-rw/a/b/many", 300_000),
        (b"/mnt-rw/big", BIG),
    ];
    for (seed, (path, len)) in sizes.iter().enumerate() {
        let crc = write_pattern(path, seed as u64, *len)?;
        expected.push(Expected {
            path,
            len: *len,
            crc,
        });
    }
    // An overwrite in the middle of the many-extent file: the writer cuts the
    // extent it lands in and keeps what is on either side.
    let patch = pattern(99, 0, 8192);
    write_at(b"/mnt-rw/a/b/many", 12_288, &patch)?;
    let mut many = pattern(3, 0, 300_000);
    many.get_mut(12_288..20_480)
        .ok_or("the overwrite is outside the file")?
        .copy_from_slice(&patch);
    if let Some(entry) = expected.iter_mut().find(|e| e.path == b"/mnt-rw/a/b/many") {
        entry.crc = crc32c(&many);
    }
    // A file with a hole: written past its end, so the gap reads as zeros.
    let tail = pattern(7, 0, 1000);
    write_file(b"/mnt-rw/sparse", &pattern(7, 0, 100))?;
    write_at(b"/mnt-rw/sparse", 200_000, &tail)?;
    let mut sparse = pattern(7, 0, 100);
    sparse.resize(200_000, 0);
    sparse.extend_from_slice(&tail);
    expected.push(Expected {
        path: b"/mnt-rw/sparse",
        len: sparse.len(),
        crc: crc32c(&sparse),
    });
    // A truncation, a link, a rename and an unlink.
    let cut = pattern(11, 0, 50_000);
    write_file(b"/mnt-rw/cut", &cut)?;
    let cut_at = ns
        .resolve(&ctx, None, b"/mnt-rw/cut", true)
        .map_err(|_| "the truncated file is gone")?;
    ns.truncate(&cut_at, 5000)
        .map_err(|_| "the truncation failed")?;
    expected.push(Expected {
        path: b"/mnt-rw/cut",
        len: 5000,
        crc: crc32c(cut.get(..5000).unwrap_or_default()),
    });
    ns.symlink(&ctx, None, b"/mnt-rw/link", b"a/b/many")
        .map_err(|_| "the symlink could not be made")?;
    ns.link(
        &ctx,
        (None, b"/mnt-rw/a/inline"),
        false,
        (None, b"/mnt-rw/hard"),
    )
    .map_err(|_| "the hard link could not be made")?;
    let inline = expected
        .iter()
        .find(|e| e.path == b"/mnt-rw/a/inline")
        .ok_or("the inline file is missing")?;
    expected.push(Expected {
        path: b"/mnt-rw/hard",
        len: inline.len,
        crc: inline.crc,
    });
    let doomed = pattern(13, 0, 9000);
    write_file(b"/mnt-rw/doomed", &doomed)?;
    ns.unlink(&ctx, None, b"/mnt-rw/doomed")
        .map_err(|_| "the unlink failed")?;
    ns.rename(
        &ctx,
        (None, b"/mnt-rw/big"),
        (None, b"/mnt-rw/a/moved"),
        RenameMode::Replace,
    )
    .map_err(|_| "the rename failed")?;
    if let Some(entry) = expected.iter_mut().find(|e| e.path == b"/mnt-rw/big") {
        entry.path = b"/mnt-rw/a/moved";
    }
    Ok(expected)
}

/// Read everything back and compare, after the remount.
fn verify(expected: &[Expected], report: &mut Report) -> Result<(), &'static str> {
    let ns = fs::namespace();
    let ctx = ns.context();
    for entry in expected {
        let (len, crc) = read_pattern(entry.path)?;
        if len != entry.len {
            return Err("a file read back with the wrong size");
        }
        if crc != entry.crc {
            return Err("a file read back with the wrong contents");
        }
        report.files += 1;
        report.bytes = report.bytes.saturating_add(len as u64);
    }
    // What was removed is gone, and what moved is only where it moved to.
    for missing in [b"/mnt-rw/doomed".as_slice(), b"/mnt-rw/big".as_slice()] {
        if fs::read_file(&ctx, None, missing).is_ok() {
            return Err("a file that was removed is still there");
        }
    }
    let target = ns
        .read_link(&ctx, None, b"/mnt-rw/link")
        .map_err(|_| "the symlink could not be read")?;
    if target != b"a/b/many" {
        return Err("the symlink points somewhere else");
    }
    for dir in [b"/mnt-rw/a".as_slice(), b"/mnt-rw/a/b".as_slice()] {
        let at = ns
            .resolve(&ctx, None, dir, true)
            .map_err(|_| "a directory written is gone")?;
        if at.inode().map(|inode| inode.metadata().kind) != Ok(FileType::Directory) {
            return Err("a directory written is not one");
        }
        report.directories += 1;
    }
    Ok(())
}

/// Mount the blank disk writable, build a tree on it, and read it back after
/// a remount.
///
/// # Errors
///
/// What did not match, as a sentence.
pub(crate) fn run() -> Result<Report, &'static str> {
    let rdev = makedev(VIRTIO_BLK_MAJOR, DISK_INDEX * 16);
    let mut report = Report {
        files: 0,
        directories: 0,
        bytes: 0,
        skipped: None,
    };
    if fs::devfs::block_device(rdev).is_none() {
        report.skipped = Some("no third disk is served");
        return Ok(report);
    }
    let ns = fs::namespace();
    let ctx = ns.context();
    match ns.mkdir(&ctx, None, MOUNT_POINT, 0o755) {
        Ok(()) | Err(Errno::EEXIST) => {}
        Err(_) => return Err("the mount point could not be made"),
    }
    let at = ns
        .resolve(&ctx, None, MOUNT_POINT, true)
        .map_err(|_| "the mount point could not be resolved")?;
    let volume =
        fs::btrfs::mount_rw(rdev).map_err(|_| "the blank disk would not mount writable")?;
    let _ = ns
        .mount(volume, &at)
        .map_err(|_| "the volume could not be mounted at /mnt-rw")?;

    let expected = build()?;
    // Everything written must be on the disk before the unmount, and the
    // unmount must leave nothing of it in memory.
    let place = ns
        .resolve(&ctx, None, MOUNT_POINT, true)
        .map_err(|_| "the mount point could not be resolved again")?;
    place
        .mount
        .filesystem()
        .sync()
        .map_err(|_| "the volume could not be synced")?;
    ns.unmount(&place)
        .map_err(|_| "the volume could not be unmounted")?;
    let again =
        fs::btrfs::mount_rw(rdev).map_err(|_| "the volume would not mount a second time")?;
    let _ = ns
        .mount(again, &at)
        .map_err(|_| "the volume could not be mounted again")?;
    verify(&expected, &mut report)?;
    Ok(report)
}
