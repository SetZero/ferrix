//! The initramfs: a cpio "newc" archive the loader hands the kernel.
//!
//! Written here rather than by `cpio` or `bsdcpio` for the reason `fat.rs`
//! writes FAT32: one fewer host tool, and an archive that is the same bytes on
//! every machine, because every timestamp, inode number and owner below is a
//! constant rather than something read off the build host's filesystem.
//!
//! # What is in it
//!
//! The directories a Linux userland expects to find — `/bin`, `/dev`, `/etc`,
//! `/proc`, `/tmp` — and a few files the kernel's stage 8 self-check reads back
//! through the VFS: a marker with known contents, a hard link to it and a
//! symbolic link to it. Each is a shape the unpacker has to get right, placed
//! where a check that the unpack happened can find it.

use std::path::Path;

use crate::{Error, Result};

/// Every timestamp in the archive: 2026-01-01 00:00:00 UTC, the same instant
/// `fat.rs` stamps the boot image with.
pub(crate) const FIXED_MTIME: u32 = 1_767_225_600;

/// Where the marker is unpacked, relative to the root.
pub(crate) const MARKER_PATH: &str = "etc/ferrix/initramfs";

/// The marker's contents. The kernel's self-check compares them byte for byte,
/// and `kernel/src/fs/check.rs` carries the same string.
pub(crate) const MARKER: &[u8] =
    b"unpacked by the kernel from a cpio archive the loader handed it\n";

/// The magic of a plain newc header.
const MAGIC: &[u8] = b"070701";

/// `c_mode` type bits.
const S_IFDIR: u32 = 0o040_000;
const S_IFREG: u32 = 0o100_000;
const S_IFLNK: u32 = 0o120_000;

/// A newc archive being written.
struct Newc {
    bytes: Vec<u8>,
    next_ino: u32,
}

impl Newc {
    fn new() -> Newc {
        Newc {
            bytes: Vec::new(),
            next_ino: 1,
        }
    }

    fn pad(&mut self) {
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
    }

    fn entry(&mut self, name: &str, mode: u32, ino: u32, nlink: u32, data: &[u8]) -> Result<()> {
        let size = u32::try_from(data.len())
            .map_err(|_| Error::new(format!("{name} is too large for a newc entry")))?;
        let name_size = u32::try_from(name.len() + 1)
            .map_err(|_| Error::new(format!("{name} is too long for a newc entry")))?;
        // ino, mode, uid, gid, nlink, mtime, filesize, devmajor, devminor,
        // rdevmajor, rdevminor, namesize, check.
        let fields = [
            ino,
            mode,
            0,
            0,
            nlink,
            FIXED_MTIME,
            size,
            0,
            0,
            0,
            0,
            name_size,
            0,
        ];
        self.bytes.extend_from_slice(MAGIC);
        for field in fields {
            self.bytes
                .extend_from_slice(format!("{field:08X}").as_bytes());
        }
        self.bytes.extend_from_slice(name.as_bytes());
        self.bytes.push(0);
        self.pad();
        self.bytes.extend_from_slice(data);
        self.pad();
        Ok(())
    }

    fn ino(&mut self) -> u32 {
        let ino = self.next_ino;
        self.next_ino += 1;
        ino
    }

    fn directory(&mut self, name: &str, permissions: u32) -> Result<()> {
        let ino = self.ino();
        self.entry(name, S_IFDIR | permissions, ino, 2, &[])
    }

    fn file(&mut self, name: &str, permissions: u32, data: &[u8]) -> Result<()> {
        let ino = self.ino();
        self.entry(name, S_IFREG | permissions, ino, 1, data)
    }

    fn symlink(&mut self, name: &str, target: &str) -> Result<()> {
        let ino = self.ino();
        self.entry(name, S_IFLNK | 0o777, ino, 1, target.as_bytes())
    }

    /// One file with several names. newc repeats the inode number on each and
    /// carries the data on the last, which is what GNU cpio writes and what an
    /// unpacker has to cope with.
    fn hard_linked(&mut self, names: &[&str], permissions: u32, data: &[u8]) -> Result<()> {
        let ino = self.ino();
        let nlink = u32::try_from(names.len()).map_err(|_| Error::new("too many hard links"))?;
        for (at, name) in names.iter().enumerate() {
            let body = if at + 1 == names.len() { data } else { &[] };
            self.entry(name, S_IFREG | permissions, ino, nlink, body)?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<u8>> {
        self.entry("TRAILER!!!", 0, 0, 1, &[])?;
        Ok(self.bytes)
    }
}

/// Where a program given with `--init` goes, relative to the root.
pub(crate) const PROGRAM_PATH: &str = "bin/busybox";

/// The applets `cargo xtask test-vfs` starts, each a symbolic link beside the
/// program. The kernel starts a program by `argv[0]` and needs none of them;
/// they are there so that the tree looks like the one `PATH=/bin` promises,
/// and so that the names resolve once a shell can look them up.
pub(crate) const APPLETS: &[&str] = &["cat", "ln", "ls", "mkdir", "mv", "rm", "rmdir", "sh"];

/// The archive every image carries, with `program` at `/bin/busybox` when one
/// is given.
pub(crate) fn build(program: Option<&Path>) -> Result<Vec<u8>> {
    let program = program
        .map(|path| {
            std::fs::read(path)
                .map_err(|error| Error::new(format!("reading {}: {error}", path.display())))
        })
        .transpose()?;
    build_with(program.as_deref())
}

/// [`build`], with the program's bytes rather than its path.
fn build_with(program: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut archive = Newc::new();
    archive.directory(".", 0o755)?;
    for (name, permissions) in [
        ("bin", 0o755),
        ("dev", 0o755),
        ("etc", 0o755),
        ("etc/ferrix", 0o755),
        ("proc", 0o555),
        ("tmp", 0o1777),
    ] {
        archive.directory(name, permissions)?;
    }
    archive.file("etc/hostname", 0o644, b"ferrix\n")?;
    let link = format!("{MARKER_PATH}.link");
    archive.hard_linked(&[MARKER_PATH, &link], 0o644, MARKER)?;
    archive.symlink(&format!("{MARKER_PATH}.symlink"), "initramfs")?;
    if let Some(program) = program {
        archive.file(PROGRAM_PATH, 0o755, program)?;
        for applet in APPLETS {
            archive.symlink(&format!("bin/{applet}"), "busybox")?;
        }
    }
    archive.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_archive_is_the_same_bytes_every_time() {
        assert_eq!(build(None).unwrap(), build(None).unwrap());
        let program: &[u8] = b"\x7fELF not really";
        assert_eq!(
            build_with(Some(program)).unwrap(),
            build_with(Some(program)).unwrap()
        );
    }

    #[test]
    fn a_program_goes_in_bin_with_its_applets_beside_it() {
        let program: &[u8] = b"\x7fELF not really";
        let bytes = build_with(Some(program)).unwrap();
        let archive = ferrix_cpio::Archive::new(&bytes);
        assert_eq!(archive.find(PROGRAM_PATH).unwrap().unwrap().data, program);
        for applet in APPLETS {
            let link = archive.find(&format!("bin/{applet}")).unwrap().unwrap();
            assert_eq!(link.symlink_target(), Some("busybox"), "bin/{applet}");
        }
        assert!(
            ferrix_cpio::Archive::new(&build(None).unwrap())
                .find(PROGRAM_PATH)
                .unwrap()
                .is_none(),
            "without --init there is no program to find"
        );
    }

    #[test]
    fn the_archive_reads_back_with_the_kernels_own_reader() {
        let bytes = build(None).unwrap();
        let archive = ferrix_cpio::Archive::new(&bytes);
        let names: Vec<&str> = archive.entries().map(|entry| entry.unwrap().name).collect();
        assert_eq!(names.first(), Some(&"."));
        assert!(names.contains(&"tmp"));

        let marker = archive.find(MARKER_PATH).unwrap().unwrap();
        assert!(marker.data.is_empty(), "the data belongs on the last link");
        assert_eq!(marker.nlink, 2);
        let link = archive
            .find(&format!("{MARKER_PATH}.link"))
            .unwrap()
            .unwrap();
        assert_eq!(link.data, MARKER);
        assert_eq!(link.ino, marker.ino);

        let symlink = archive
            .find(&format!("{MARKER_PATH}.symlink"))
            .unwrap()
            .unwrap();
        assert_eq!(symlink.symlink_target(), Some("initramfs"));
        assert!(names.iter().all(|name| ferrix_cpio::is_safe_path(name)));
    }
}
