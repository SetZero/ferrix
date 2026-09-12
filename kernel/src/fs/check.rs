//! Stage 8's self-checks for the root the kernel built.
//!
//! Two properties the host tests of `libs/vfs` cannot establish, because both
//! are about this machine rather than the logic. That the archive the loader
//! handed over is the one the build wrote, unpacked intact through the direct
//! map — hard link and symbolic link included. And that tmpfs over VMO pages
//! stores what it is given, gives zeros where nothing was written, and gives
//! every frame back once the file is gone.

use alloc::vec;
use alloc::vec::Vec;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_vfs::{Errno, FileType, Namespace, OpenFlags, RenameMode};

use crate::fs::{self, Report as Built};
use crate::mm;

/// The marker `xtask/src/initramfs.rs` writes, byte for byte.
const MARKER: &[u8] = b"unpacked by the kernel from a cpio archive the loader handed it\n";

/// Where the marker is unpacked.
const MARKER_PATH: &[u8] = b"/etc/ferrix/initramfs";

/// What the checks measured, for the boot log.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Report {
    /// Whether the archive's marker, link and symbolic link were checked.
    pub(crate) initramfs_verified: bool,
    /// Pages a file under `/tmp` committed while the check wrote it.
    pub(crate) pages: u64,
    /// Frames the tmpfs check cost once the file was gone. Zero, or the page
    /// store is leaking.
    pub(crate) leaked: i64,
}

/// Run them. `Err` names the first thing that was not true.
pub(crate) fn run(built: &Built) -> Result<Report, &'static str> {
    let initramfs_verified = match built.unpacked {
        Some(_) => {
            check_the_archive_unpacked_intact()?;
            true
        }
        None => false,
    };
    check_tmp_is_its_own_mount()?;

    // Twice, measured on the second, for the reason `syscall::check` gives:
    // the heap keeps the last page of a size class it has used, and a single
    // run cannot tell that apart from a leak.
    let _warm = check_tmpfs_stores_pages()?;
    let before = mm::free_frames();
    let pages = check_tmpfs_stores_pages()?;
    let leaked = i64::try_from(before).unwrap_or(i64::MAX)
        - i64::try_from(mm::free_frames()).unwrap_or(i64::MAX);

    Ok(Report {
        initramfs_verified,
        pages,
        leaked,
    })
}

/// Read a whole file through the namespace.
fn read_all(ns: &Namespace, path: &[u8]) -> Result<Vec<u8>, &'static str> {
    let ctx = ns.context();
    let read = OpenFlags {
        read: true,
        ..OpenFlags::default()
    };
    let file = ns
        .open(&ctx, None, path, &read, 0)
        .map_err(|_| "a file the check needs would not open")?;
    let mut contents = Vec::new();
    let mut chunk = [0_u8; 97];
    loop {
        let count = file
            .read(&mut chunk)
            .map_err(|_| "a file the check opened would not read")?;
        if count == 0 {
            return Ok(contents);
        }
        contents.extend_from_slice(chunk.get(..count).ok_or("a read overran its buffer")?);
    }
}

/// The marker reads back exactly, and both of its other names reach it.
fn check_the_archive_unpacked_intact() -> Result<(), &'static str> {
    let ns = fs::namespace();
    let ctx = ns.context();
    if read_all(ns, MARKER_PATH)? != MARKER {
        return Err("the initramfs marker does not hold what the build wrote");
    }

    let marker = ns
        .resolve(&ctx, None, MARKER_PATH, false)
        .and_then(|at| ns.stat(&at))
        .map_err(|_| "the initramfs marker would not stat")?;
    let link = ns
        .resolve(&ctx, None, b"/etc/ferrix/initramfs.link", false)
        .and_then(|at| ns.stat(&at))
        .map_err(|_| "the initramfs hard link is missing")?;
    if link.metadata.ino != marker.metadata.ino || marker.metadata.nlink != 2 {
        return Err("the initramfs hard link is a copy, not a second name");
    }

    let symlink = b"/etc/ferrix/initramfs.symlink";
    let kind = ns
        .resolve(&ctx, None, symlink, false)
        .and_then(|at| ns.stat(&at))
        .map_err(|_| "the initramfs symbolic link is missing")?
        .metadata
        .kind;
    if kind != FileType::Symlink || read_all(ns, symlink)? != MARKER {
        return Err("the initramfs symbolic link does not lead to the marker");
    }

    // The whole-file read that loading a program goes through: through the
    // link, as `execve` of `/bin/sh` will be, and refusing what is not a file.
    let whole = fs::read_file(&ctx, None, symlink);
    if whole.as_deref() != Ok(MARKER) {
        return Err("reading a whole file through a symbolic link did not give the marker");
    }
    if fs::read_file(&ctx, None, b"/etc/ferrix") != Err(Errno::EISDIR) {
        return Err("reading a directory as a whole file was not refused");
    }
    Ok(())
}

/// `/tmp` is a separate filesystem from the root, sticky and world-writable.
fn check_tmp_is_its_own_mount() -> Result<(), &'static str> {
    let ns = fs::namespace();
    let ctx = ns.context();
    let root = ns.stat(&ctx.root).map_err(|_| "the root would not stat")?;
    let tmp = ns
        .resolve(&ctx, None, b"/tmp", true)
        .and_then(|at| ns.stat(&at))
        .map_err(|_| "/tmp is missing")?;
    if tmp.dev == root.dev {
        return Err("/tmp is not a filesystem of its own");
    }
    if tmp.metadata.permissions != 0o1777 {
        return Err("/tmp is not sticky and world-writable");
    }
    Ok(())
}

/// A byte of the pattern the tmpfs check writes, by its offset.
fn pattern(at: usize) -> u8 {
    (at.wrapping_mul(31) ^ (at >> 12)) as u8
}

/// Write across pages, read back, truncate into a page, grow again, rename,
/// and remove. Returns the pages the file had committed.
fn check_tmpfs_stores_pages() -> Result<u64, &'static str> {
    let ns = fs::namespace();
    let ctx = ns.context();
    let page = usize::try_from(PAGE_SIZE).map_err(|_| "the page size does not fit")?;
    let len = page * 3 + 123;
    let data: Vec<u8> = (0..len).map(pattern).collect();

    let create = OpenFlags {
        read: true,
        write: true,
        create: true,
        exclusive: true,
        ..OpenFlags::default()
    };
    let file = ns
        .open(&ctx, None, b"/tmp/stage8-check", &create, 0o600)
        .map_err(|_| "a file could not be created under /tmp")?;
    for piece in data.chunks(1000) {
        if file.write(piece) != Ok(piece.len()) {
            return Err("a write to a tmpfs file came back short");
        }
    }
    let stat = ns
        .stat(file.location())
        .map_err(|_| "the file would not stat")?;
    let pages = stat.metadata.blocks * 512 / PAGE_SIZE;
    if stat.metadata.size != len as u64 || pages != 4 {
        return Err("the file's size or committed pages are not what was written");
    }
    if read_all(ns, b"/tmp/stage8-check")? != data {
        return Err("a tmpfs file read back different bytes");
    }

    // Into the second page, then out past where the data used to end: the
    // old bytes must not reappear.
    let keep = page + 10;
    file.set_len(keep as u64)
        .and_then(|()| file.set_len(len as u64))
        .map_err(|_| "a tmpfs file would not truncate and grow")?;
    let mut back = vec![0_u8; len];
    if file.read_at(0, &mut back) != Ok(len) {
        return Err("a regrown tmpfs file read back short");
    }
    let head_intact = back.get(..keep) == data.get(..keep);
    let tail_zero = back
        .get(keep..)
        .is_some_and(|tail| tail.iter().all(|&b| b == 0));
    if !head_intact || !tail_zero {
        return Err("truncating a tmpfs file did not discard what it cut off");
    }

    ns.rename(
        &ctx,
        (None, b"/tmp/stage8-check"),
        (None, b"/tmp/stage8-check.moved"),
        RenameMode::NoReplace,
    )
    .map_err(|_| "a tmpfs file would not rename")?;
    if read_all(ns, b"/tmp/stage8-check.moved")?.len() != len {
        return Err("a renamed tmpfs file lost its contents");
    }
    ns.unlink(&ctx, None, b"/tmp/stage8-check.moved")
        .map_err(|_| "a tmpfs file would not unlink")?;
    drop(file);
    Ok(pages)
}
