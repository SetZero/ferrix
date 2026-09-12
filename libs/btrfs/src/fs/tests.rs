//! Tests for reading files and directories out of real `mkfs.btrfs` images.
//!
//! `testdata/manifest.txt` lists every path the generator put in the images,
//! with its size and CRC-32C. Each test resolves every path from the root
//! directory one component at a time, the way a VFS walk would, and reads it
//! back: a file in awkwardly sized pieces, so reads start and end in the middle
//! of extents, and a directory as a listing compared against the manifest.

extern crate std;

use core::ops::ControlFlow;
use std::boxed::Box;
use std::collections::{BTreeMap, BTreeSet};
use std::vec;
use std::vec::Vec;

use super::*;
use crate::chunk::ChunkMapEntry;
use crate::crc32c::crc32c;
use crate::items::{FT_DIR, FT_REG_FILE, FT_SYMLINK};
use crate::volume::tests::{IMAGES, PackedDevice};

/// One manifest line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Expected {
    kind: &'static str,
    size: u64,
    crc: u32,
}

fn manifest() -> BTreeMap<Vec<u8>, Expected> {
    include_str!("../../testdata/manifest.txt")
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

/// Everything a read needs, allocated once per test.
struct Fixture {
    device: PackedDevice,
    chunks: [ChunkMapEntry; 16],
    node: Vec<u8>,
    compressed: Vec<u8>,
    plain: Vec<u8>,
    zstd: Box<compress::zstd::Workspace>,
}

impl Fixture {
    fn new(packed: &[u8]) -> Fixture {
        Fixture {
            device: PackedDevice::new(packed),
            chunks: [ChunkMapEntry::EMPTY; 16],
            node: vec![0; 65536],
            compressed: vec![0; MAX_UNCOMPRESSED],
            plain: vec![0; MAX_UNCOMPRESSED],
            zstd: Box::default(),
        }
    }
}

/// Resolve `path` from the root directory, one component at a time.
fn resolve(
    sub: &Subvolume<'_, &mut [ChunkMapEntry; 16]>,
    device: &mut PackedDevice,
    path: &[u8],
    node: &mut [u8],
) -> Entry {
    let mut entry = Entry {
        target: Target::Inode(sub.root_dir()),
        kind: FT_DIR,
    };
    for component in path.split(|&b| b == b'/') {
        let Target::Inode(dir) = entry.target else {
            panic!("the fixture has no subvolumes to cross");
        };
        entry = sub
            .lookup(device, dir, component, node)
            .unwrap()
            .unwrap_or_else(|| {
                panic!(
                    "{:?} is missing",
                    std::string::String::from_utf8_lossy(path)
                )
            });
    }
    entry
}

/// Read all of `ino` in `chunk`-byte pieces.
fn read_all(
    sub: &Subvolume<'_, &mut [ChunkMapEntry; 16]>,
    device: &mut PackedDevice,
    ino: u64,
    chunk: usize,
    node: &mut [u8],
    buffers: &mut ReadBuffers<impl ExtentBuffers>,
) -> Vec<u8> {
    let mut contents = Vec::new();
    let mut piece = vec![0xAAu8; chunk];
    loop {
        let offset = contents.len() as u64;
        let n = sub
            .read(device, ino, offset, &mut piece, node, buffers)
            .unwrap();
        if n == 0 {
            return contents;
        }
        contents.extend_from_slice(&piece[..n]);
    }
}

/// The names the manifest puts directly inside directory `dir`.
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

/// Resolve and read back every manifest path from one image.
fn check_image(name: &str) {
    let (_, packed) = IMAGES.iter().find(|(n, _)| *n == name).unwrap();
    let f = &mut Fixture::new(packed);
    let volume = Volume::open(&mut f.device, &mut f.chunks, &mut f.node).unwrap();
    let sub = volume.default_subvolume();
    let mut buffers =
        ReadBuffers::new((&mut f.compressed[..], &mut f.plain[..], &mut *f.zstd)).unwrap();
    let manifest = manifest();

    check_listing(
        &sub,
        &mut f.device,
        sub.root_dir(),
        b"",
        &manifest,
        &mut f.node,
        name,
    );
    for (path, expected) in &manifest {
        let entry = resolve(&sub, &mut f.device, path, &mut f.node);
        let Target::Inode(ino) = entry.target else {
            panic!("{name}: no subvolumes in the fixture");
        };
        let shown = std::string::String::from_utf8_lossy(path);
        match expected.kind {
            "dir" => {
                assert_eq!(entry.kind, FT_DIR, "{name}: {shown} is a directory");
                check_listing(&sub, &mut f.device, ino, path, &manifest, &mut f.node, name);
            }
            "file" | "link" => {
                let kind = if expected.kind == "file" {
                    FT_REG_FILE
                } else {
                    FT_SYMLINK
                };
                assert_eq!(entry.kind, kind, "{name}: {shown} has the right type");
                // 3001 bytes: prime-ish, so reads straddle every extent boundary.
                let contents = read_all(&sub, &mut f.device, ino, 3001, &mut f.node, &mut buffers);
                assert_eq!(contents.len() as u64, expected.size, "{name}: {shown} size");
                assert_eq!(crc32c(&contents), expected.crc, "{name}: {shown} contents");
            }
            other => panic!("unknown manifest type {other}"),
        }
    }
}

/// List `ino` and compare the names with the manifest's children of `path`.
fn check_listing(
    sub: &Subvolume<'_, &mut [ChunkMapEntry; 16]>,
    device: &mut PackedDevice,
    ino: u64,
    path: &[u8],
    manifest: &BTreeMap<Vec<u8>, Expected>,
    node: &mut [u8],
    name: &str,
) {
    let mut listed = BTreeSet::new();
    let mut last_index = 1;
    sub.read_dir(device, ino, 2, node, |entry| {
        assert!(entry.index > last_index, "{name}: indexes ascend");
        last_index = entry.index;
        assert!(
            listed.insert(entry.name.to_vec()),
            "{name}: no name listed twice"
        );
        ControlFlow::Continue(())
    })
    .unwrap();
    assert_eq!(
        listed,
        children(manifest, path),
        "{name}: listing of {path:?}"
    );
}

#[test]
fn the_uncompressed_image_reads_back_exactly() {
    check_image("none");
}

#[test]
#[ignore = "the zlib decoder is still a stub; un-ignore when it lands"]
fn the_zlib_image_reads_back_exactly() {
    check_image("zlib");
}

#[test]
#[ignore = "the lzo decoder is still a stub; un-ignore when it lands"]
fn the_lzo_image_reads_back_exactly() {
    check_image("lzo");
}

#[test]
#[ignore = "the zstd decoder is still a stub; un-ignore when it lands"]
fn the_zstd_image_reads_back_exactly() {
    check_image("zstd");
}

#[test]
fn a_missing_name_is_a_miss_not_an_error() {
    let f = &mut Fixture::new(IMAGES[0].1);
    let volume = Volume::open(&mut f.device, &mut f.chunks, &mut f.node).unwrap();
    let sub = volume.default_subvolume();
    let found = sub.lookup(&mut f.device, sub.root_dir(), b"no-such-file", &mut f.node);
    assert_eq!(found, Ok(None), "an absent name is None");
}

#[test]
fn a_read_past_the_end_returns_nothing_and_a_listing_can_resume() {
    let f = &mut Fixture::new(IMAGES[0].1);
    let volume = Volume::open(&mut f.device, &mut f.chunks, &mut f.node).unwrap();
    let sub = volume.default_subvolume();
    let mut buffers =
        ReadBuffers::new((&mut f.compressed[..], &mut f.plain[..], &mut *f.zstd)).unwrap();
    let entry = resolve(&sub, &mut f.device, b"big.txt", &mut f.node);
    let Target::Inode(ino) = entry.target else {
        panic!("big.txt is a file");
    };
    let mut out = [0xAAu8; 16];
    let n = sub
        .read(
            &mut f.device,
            ino,
            1 << 40,
            &mut out,
            &mut f.node,
            &mut buffers,
        )
        .unwrap();
    assert_eq!(n, 0, "nothing lies past the end of the file");

    let mut first = Vec::new();
    sub.read_dir(&mut f.device, sub.root_dir(), 2, &mut f.node, |entry| {
        first.push(entry.index);
        ControlFlow::Break(())
    })
    .unwrap();
    let mut rest = Vec::new();
    sub.read_dir(
        &mut f.device,
        sub.root_dir(),
        first[0] + 1,
        &mut f.node,
        |entry| {
            rest.push(entry.index);
            ControlFlow::Continue(())
        },
    )
    .unwrap();
    assert!(
        !rest.contains(&first[0]),
        "resuming after an entry skips it"
    );
    assert!(rest.iter().all(|&i| i > first[0]), "and continues after it");
}

#[test]
fn a_hole_reads_as_zeroes() {
    let f = &mut Fixture::new(IMAGES[0].1);
    let volume = Volume::open(&mut f.device, &mut f.chunks, &mut f.node).unwrap();
    let sub = volume.default_subvolume();
    let mut buffers =
        ReadBuffers::new((&mut f.compressed[..], &mut f.plain[..], &mut *f.zstd)).unwrap();
    let entry = resolve(&sub, &mut f.device, b"sparse.bin", &mut f.node);
    let Target::Inode(ino) = entry.target else {
        panic!("sparse.bin is a file");
    };
    // The generator writes 8 KiB, seeks to 1 MiB and writes 4 KiB more.
    let mut middle = vec![0xAAu8; 512 * 1024];
    let n = sub
        .read(
            &mut f.device,
            ino,
            256 * 1024,
            &mut middle,
            &mut f.node,
            &mut buffers,
        )
        .unwrap();
    assert_eq!(n, middle.len(), "the hole is inside the file");
    assert!(middle.iter().all(|&b| b == 0), "and reads as zeroes");
}

#[test]
fn read_buffers_smaller_than_an_extent_are_refused() {
    let mut small = vec![0u8; 4096];
    let mut plain = vec![0u8; MAX_UNCOMPRESSED];
    let mut zstd = Box::<compress::zstd::Workspace>::default();
    assert!(
        matches!(
            ReadBuffers::new((&mut small[..], &mut plain[..], &mut *zstd)),
            Err(BtrfsError::Truncated { .. })
        ),
        "a 4 KiB buffer cannot hold a compressed extent"
    );
}
