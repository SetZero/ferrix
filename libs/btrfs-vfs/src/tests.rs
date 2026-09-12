//! Tests for the mount, through the [`Inode`] trait the VFS calls.
//!
//! The images are the real `mkfs.btrfs` ones `ferrix-btrfs` tests with, and the
//! expectations are its manifest: every path resolves one component at a time,
//! every file reads back with the right size and CRC-32C, every link's target
//! does, and every directory lists exactly its children — in pieces, resuming
//! from the cursor each piece ended at, the way `getdents64` asks.

extern crate std;

use std::collections::{BTreeMap, BTreeSet};
use std::string::String;
use std::thread;

use ferrix_btrfs::crc32c;

use super::*;

const BLOCK: usize = 4096;
const IMAGE_SIZE: u64 = 128 * 1024 * 1024;

const IMAGES: [(&str, &[u8]); 4] = [
    (
        "none",
        include_bytes!("../../btrfs/testdata/none.img.packed"),
    ),
    (
        "zlib",
        include_bytes!("../../btrfs/testdata/zlib.img.packed"),
    ),
    ("lzo", include_bytes!("../../btrfs/testdata/lzo.img.packed")),
    (
        "zstd",
        include_bytes!("../../btrfs/testdata/zstd.img.packed"),
    ),
];

/// A packed image as a device handle: shared blocks, zeroes elsewhere.
#[derive(Clone)]
struct Image(Arc<BTreeMap<u64, [u8; BLOCK]>>);

impl Image {
    fn new(packed: &[u8]) -> Image {
        let blocks = packed
            .chunks_exact(8 + BLOCK)
            .map(|record| {
                let offset = u64::from_le_bytes(record[..8].try_into().unwrap());
                (offset, record[8..].try_into().unwrap())
            })
            .collect();
        Image(Arc::new(blocks))
    }
}

impl Device for Image {
    fn read_at(&mut self, physical: u64, buf: &mut [u8]) -> core::result::Result<(), BtrfsError> {
        if physical
            .checked_add(buf.len() as u64)
            .is_none_or(|end| end > IMAGE_SIZE)
        {
            return Err(BtrfsError::DeviceRead { physical });
        }
        let mut done = 0;
        while done < buf.len() {
            let at = physical + done as u64;
            let base = at - at % BLOCK as u64;
            let within = (at - base) as usize;
            let len = (BLOCK - within).min(buf.len() - done);
            let dest = &mut buf[done..done + len];
            match self.0.get(&base) {
                Some(block) => dest.copy_from_slice(&block[within..within + len]),
                None => dest.fill(0),
            }
            done += len;
        }
        Ok(())
    }
}

struct Expected {
    kind: &'static str,
    size: u64,
    crc: u32,
}

fn manifest() -> BTreeMap<Vec<u8>, Expected> {
    include_str!("../../btrfs/testdata/manifest.txt")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            let fields: Vec<&'static str> = line.split(' ').collect();
            let path = fields[1]
                .as_bytes()
                .chunks(2)
                .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            let expected = Expected {
                kind: fields[0],
                size: fields[2].parse().unwrap(),
                crc: u32::from_str_radix(fields[3], 16).unwrap(),
            };
            (path, expected)
        })
        .collect()
}

fn mount(name: &str) -> Arc<Btrfs<Image>> {
    let (_, packed) = IMAGES.iter().find(|(n, _)| *n == name).unwrap();
    Btrfs::mount(Image::new(packed), 42).unwrap()
}

fn resolve(fs: &Btrfs<Image>, path: &[u8]) -> Arc<dyn Inode> {
    path.split(|&b| b == b'/').fold(fs.root(), |dir, name| {
        dir.lookup(name)
            .unwrap_or_else(|e| panic!("{} missing: {e:?}", String::from_utf8_lossy(path)))
    })
}

/// Read all of a file in 3001-byte pieces, so reads straddle extent edges.
fn read_all(file: &Arc<dyn Inode>) -> Vec<u8> {
    let mut contents = Vec::new();
    let mut piece = vec![0xAAu8; 3001];
    loop {
        let n = file.read_at(contents.len() as u64, &mut piece).unwrap();
        if n == 0 {
            return contents;
        }
        contents.extend_from_slice(&piece[..n]);
    }
}

/// List a directory five entries at a time, resuming from each piece's cursor.
fn list(dir: &Arc<dyn Inode>) -> Vec<Vec<u8>> {
    let mut names = Vec::new();
    let mut cursor = FIRST_CURSOR;
    loop {
        let mut taken = 0;
        let mut resume = None;
        dir.read_dir(cursor, &mut |entry| {
            if taken == 5 {
                return false;
            }
            names.push(entry.name.to_vec());
            resume = Some(entry.next);
            taken += 1;
            true
        })
        .unwrap();
        match resume {
            Some(next) => cursor = next,
            None => return names,
        }
    }
}

fn children(manifest: &BTreeMap<Vec<u8>, Expected>, dir: &[u8]) -> BTreeSet<Vec<u8>> {
    manifest
        .keys()
        .filter_map(|path| {
            let rest = if dir.is_empty() {
                path.as_slice()
            } else {
                path.strip_prefix(dir)?.strip_prefix(b"/")?
            };
            (!rest.contains(&b'/')).then(|| rest.to_vec())
        })
        .collect()
}

fn check_listing(dir: &Arc<dyn Inode>, path: &[u8], manifest: &BTreeMap<Vec<u8>, Expected>) {
    let names = list(dir);
    let unique: BTreeSet<Vec<u8>> = names.iter().cloned().collect();
    assert_eq!(
        unique.len(),
        names.len(),
        "no name is listed twice in {path:?}"
    );
    assert_eq!(unique, children(manifest, path), "the listing of {path:?}");
}

fn check_image(name: &str) {
    let fs = mount(name);
    assert_eq!(fs.name(), "btrfs", "the type /proc/mounts shows");
    let manifest = manifest();
    check_listing(&fs.root(), b"", &manifest);
    for (path, expected) in &manifest {
        let inode = resolve(&fs, path);
        let meta = inode.metadata();
        let shown = String::from_utf8_lossy(path);
        match expected.kind {
            "dir" => {
                assert_eq!(meta.kind, FileType::Directory, "{name}: {shown}");
                check_listing(&inode, path, &manifest);
            }
            "file" => {
                assert_eq!(meta.kind, FileType::Regular, "{name}: {shown}");
                assert_eq!(meta.size, expected.size, "{name}: {shown} stat size");
                let contents = read_all(&inode);
                assert_eq!(contents.len() as u64, expected.size, "{name}: {shown} size");
                assert_eq!(crc32c(&contents), expected.crc, "{name}: {shown} contents");
            }
            "link" => {
                assert_eq!(meta.kind, FileType::Symlink, "{name}: {shown}");
                let target = inode.read_link().unwrap();
                assert_eq!(
                    target.len() as u64,
                    expected.size,
                    "{name}: {shown} target size"
                );
                assert_eq!(crc32c(&target), expected.crc, "{name}: {shown} target");
            }
            other => panic!("unknown manifest type {other}"),
        }
    }
}

#[test]
fn the_uncompressed_image_mounts_and_reads_back() {
    check_image("none");
}

#[test]
fn the_zlib_image_mounts_and_reads_back() {
    check_image("zlib");
}

#[test]
fn the_lzo_image_mounts_and_reads_back() {
    check_image("lzo");
}

#[test]
fn the_zstd_image_mounts_and_reads_back() {
    check_image("zstd");
}

#[test]
fn every_change_is_refused_as_read_only() {
    let fs = mount("none");
    let root = fs.root();
    let file = resolve(&fs, b"big.txt");
    assert_eq!(
        root.create(b"new", NewNode::Regular, 0o644).err(),
        Some(Errno::EROFS),
        "create"
    );
    assert_eq!(root.unlink(b"big.txt"), Err(Errno::EROFS), "unlink");
    assert_eq!(root.rmdir(b"empty-dir"), Err(Errno::EROFS), "rmdir");
    assert_eq!(root.link(b"again", &file), Err(Errno::EROFS), "link");
    assert_eq!(
        root.rename(b"big.txt", &root, b"moved", true),
        Err(Errno::EROFS),
        "rename"
    );
    assert_eq!(file.write_at(0, b"x", false), Err(Errno::EROFS), "write");
    assert_eq!(file.set_len(0), Err(Errno::EROFS), "truncate");
    assert_eq!(
        file.set_attributes(&SetAttributes::default()),
        Err(Errno::EROFS),
        "chmod and friends"
    );
}

#[test]
fn the_wrong_kind_of_object_is_refused_as_linux_would() {
    let fs = mount("none");
    let file = resolve(&fs, b"big.txt");
    assert_eq!(
        fs.root().lookup(b"no-such-name").err(),
        Some(Errno::ENOENT),
        "a miss"
    );
    assert_eq!(
        file.lookup(b"x").err(),
        Some(Errno::ENOTDIR),
        "lookup in a file"
    );
    assert_eq!(
        file.read_dir(FIRST_CURSOR, &mut |_| true),
        Err(Errno::ENOTDIR),
        "list a file"
    );
    assert_eq!(
        fs.root().read_at(0, &mut [0; 8]),
        Err(Errno::EINVAL),
        "read a directory"
    );
    assert_eq!(file.read_link(), Err(Errno::EINVAL), "readlink on a file");
}

#[test]
fn a_device_without_btrfs_is_refused_with_einval() {
    let blank = Image(Arc::new(BTreeMap::new()));
    assert_eq!(
        Btrfs::mount(blank, 1).err(),
        Some(Errno::EINVAL),
        "no superblock magic"
    );
}

#[test]
fn inodes_report_the_mount_device_and_their_own_numbers() {
    let fs = mount("none");
    assert_eq!(fs.device(), 42, "the device number given at mount");
    let root = fs.root().metadata();
    assert_eq!(root.ino, 256, "btrfs's top-level directory");
    let a = resolve(&fs, b"big.txt").metadata().ino;
    let b = resolve(&fs, b"random.bin").metadata().ino;
    assert_ne!(a, b, "two files are two inodes");
}

#[test]
fn concurrent_readers_of_one_compressed_file_each_get_its_bytes() {
    let fs = mount("zstd");
    let expected = read_all(&resolve(&fs, b"big.txt"));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let fs = Arc::clone(&fs);
            thread::spawn(move || read_all(&resolve(&fs, b"big.txt")))
        })
        .collect();
    for reader in readers {
        assert!(
            reader.join().unwrap() == expected,
            "every reader sees the same file"
        );
    }
}
