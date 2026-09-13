//! Tests for reading files and directories out of real `mkfs.btrfs` images.
//!
//! `testdata/manifest.txt` lists every path the generator put in the images,
//! with its size and CRC-32C. Each test resolves every path from the root
//! directory one component at a time, the way a VFS walk would, and reads it
//! back: a file in awkwardly sized pieces, so reads start and end in the middle
//! of extents, and a directory as a listing compared against the manifest.

extern crate std;

use core::ops::ControlFlow;
use std::collections::{BTreeMap, BTreeSet};
use std::vec;
use std::vec::Vec;

use super::*;
use crate::chunk::ChunkMapEntry;
use crate::crc32c::crc32c;
use crate::items::{FT_DIR, FT_REG_FILE, FT_SYMLINK};
use crate::tree::{HEADER_SIZE, ITEM_SIZE};
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
    zstd: Vec<u8>,
}

impl Fixture {
    fn new(packed: &[u8]) -> Fixture {
        Fixture {
            device: PackedDevice::new(packed),
            chunks: [ChunkMapEntry::EMPTY; 16],
            node: vec![0; 65536],
            compressed: vec![0; MAX_UNCOMPRESSED],
            plain: vec![0; MAX_UNCOMPRESSED],
            zstd: vec![0; compress::zstd::Workspace::SIZE],
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
        ReadBuffers::new((&mut f.compressed[..], &mut f.plain[..], &mut f.zstd[..])).unwrap();
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
#[cfg_attr(
    miri,
    ignore = "reads every file of a real image; plain cargo test covers it, Miri runs the small image tests"
)]
fn the_uncompressed_image_reads_back_exactly() {
    check_image("none");
}

#[test]
#[cfg_attr(
    miri,
    ignore = "reads every file of a real image; plain cargo test covers it, Miri runs the small image tests"
)]
fn the_zlib_image_reads_back_exactly() {
    check_image("zlib");
}

#[test]
#[cfg_attr(
    miri,
    ignore = "reads every file of a real image; plain cargo test covers it, Miri runs the small image tests"
)]
fn the_lzo_image_reads_back_exactly() {
    check_image("lzo");
}

#[test]
#[cfg_attr(
    miri,
    ignore = "reads every file of a real image; plain cargo test covers it, Miri runs the small image tests"
)]
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
        ReadBuffers::new((&mut f.compressed[..], &mut f.plain[..], &mut f.zstd[..])).unwrap();
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
        ReadBuffers::new((&mut f.compressed[..], &mut f.plain[..], &mut f.zstd[..])).unwrap();
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

/// One directory entry payload naming inode 257.
fn dir_entry(name: &[u8], kind: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&257u64.to_le_bytes()); // location objectid
    bytes.push(INODE_ITEM_KEY);
    bytes.extend_from_slice(&0u64.to_le_bytes()); // location offset
    bytes.extend_from_slice(&7u64.to_le_bytes()); // transid
    bytes.extend_from_slice(&0u16.to_le_bytes()); // data_len
    bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
    bytes.push(kind);
    bytes.extend_from_slice(name);
    bytes
}

#[test]
fn a_lookup_refuses_an_entry_filed_under_another_names_hash() {
    let key_for = |name: &[u8]| BtrfsKey::new(256, DIR_ITEM_KEY, name_hash(name));
    let one = dir_entry(b"one", FT_REG_FILE);
    assert_eq!(
        entry_named(&one, &key_for(b"one"), b"one"),
        Ok(Some(Entry {
            target: Target::Inode(257),
            kind: FT_REG_FILE
        })),
        "an entry under its own name's hash is found"
    );
    assert_eq!(
        entry_named(&one, &key_for(b"two"), b"one"),
        Err(BtrfsError::BadItem {
            item_type: DIR_ITEM_KEY
        }),
        "an entry under another name's hash is damage, not a miss"
    );

    // Names that collide share an item, and every entry in it must hash to
    // the key — including one after the entry the lookup was for.
    let mut shared = dir_entry(b"one", FT_REG_FILE);
    shared.extend_from_slice(&dir_entry(b"stray", FT_REG_FILE));
    assert_eq!(
        entry_named(&shared, &key_for(b"one"), b"one"),
        Err(BtrfsError::BadItem {
            item_type: DIR_ITEM_KEY
        }),
        "the whole item is checked, not only up to the match"
    );
}

#[test]
fn names_no_directory_can_hold_are_refused() {
    let longest = [b'x'; 255];
    for good in [
        &b"a"[..],
        b"...",
        b".hidden",
        b"trailing.",
        &longest,
        &[0x80, 0xFE, 0xFF],
    ] {
        assert_eq!(
            check_name(good, DIR_INDEX_KEY),
            Ok(()),
            "{good:?} is a name"
        );
    }
    for bad in [&b""[..], b".", b"..", b"/", b"a/b", b"nul\0byte"] {
        assert_eq!(
            check_name(bad, DIR_INDEX_KEY),
            Err(BtrfsError::BadItem {
                item_type: DIR_INDEX_KEY
            }),
            "{bad:?} is not a name"
        );
        // A lookup refuses it too, even filed under its own hash.
        let key = BtrfsKey::new(256, DIR_ITEM_KEY, name_hash(bad));
        assert_eq!(
            entry_named(&dir_entry(bad, FT_REG_FILE), &key, b"other"),
            Err(BtrfsError::BadItem {
                item_type: DIR_ITEM_KEY
            }),
            "{bad:?} is refused by lookup"
        );
    }
}

/// The first item of `key`'s type at or after `key` in the fs tree: its key,
/// the physical offset of its node, and that of its payload.
fn find_item(
    volume: &Volume<&mut [ChunkMapEntry; 16]>,
    device: &mut PackedDevice,
    key: BtrfsKey,
    node: &mut [u8],
) -> (BtrfsKey, u64, u64) {
    let mut seek = key;
    loop {
        let leaf = volume.seek(device, volume.fs_tree(), &seek, node).unwrap();
        if let Some(item) = leaf.items().next() {
            assert_eq!(item.key.item_type, key.item_type, "the fixture has one");
            let (_, physical) = volume.chunks().map(leaf.node.header().bytenr).unwrap();
            let payload = physical + HEADER_SIZE as u64 + u64::from(item.offset);
            return (item.key, physical, payload);
        }
        seek = leaf.next.expect("the fixture has one");
    }
}

#[test]
fn a_listing_refuses_a_name_with_a_slash_in_it() {
    // A hostile image, checksummed: the first entry of the root directory's
    // index now has a '/' for its first byte. Before, `read_dir` handed that
    // name on, and the VFS would have listed it to `getdents64`.
    let f = &mut Fixture::new(IMAGES[0].1);
    let volume = Volume::open(&mut f.device, &mut f.chunks, &mut f.node).unwrap();
    let root = volume.root_dir();
    let start = BtrfsKey::new(root, DIR_INDEX_KEY, 0);
    let (_, node_at, payload) = find_item(&volume, &mut f.device, start, &mut f.node);
    f.device.write(payload + 30, b"/");
    f.device.reseal(node_at);

    let sub = volume.default_subvolume();
    let result = sub.read_dir(&mut f.device, root, 2, &mut f.node, |_| {
        ControlFlow::Continue(())
    });
    assert_eq!(
        result,
        Err(BtrfsError::BadItem {
            item_type: DIR_INDEX_KEY
        })
    );
}

#[test]
fn a_lookup_refuses_a_name_that_no_longer_matches_its_hash() {
    let f = &mut Fixture::new(IMAGES[0].1);
    let volume = Volume::open(&mut f.device, &mut f.chunks, &mut f.node).unwrap();
    let root = volume.root_dir();
    let start = BtrfsKey::new(root, DIR_ITEM_KEY, 0);
    let (key, node_at, payload) = find_item(&volume, &mut f.device, start, &mut f.node);

    // Read the name back out of the image, then change its first byte.
    let mut header = [0u8; 30];
    f.device.read_at(payload, &mut header).unwrap();
    let mut name = vec![0u8; usize::from(u16::from_le_bytes([header[27], header[28]]))];
    f.device.read_at(payload + 30, &mut name).unwrap();
    assert_eq!(name_hash(&name), key.offset, "the image is well formed");
    f.device.write(payload + 30, &[name[0] ^ 0x01]);
    f.device.reseal(node_at);

    let sub = volume.default_subvolume();
    assert_eq!(
        sub.lookup(&mut f.device, root, &name, &mut f.node),
        Err(BtrfsError::BadItem {
            item_type: DIR_ITEM_KEY
        })
    );
}

#[test]
fn a_read_refuses_an_extent_that_starts_inside_the_one_before() {
    // `sparse.bin` is 8 KiB written at 0, then 4 KiB at 1 MiB. Refile the
    // extent item after the first at 4 KiB, inside the first one, and the two
    // claim the same range of the file.
    let f = &mut Fixture::new(IMAGES[0].1);
    let volume = Volume::open(&mut f.device, &mut f.chunks, &mut f.node).unwrap();
    let sub = volume.default_subvolume();
    let mut buffers =
        ReadBuffers::new((&mut f.compressed[..], &mut f.plain[..], &mut f.zstd[..])).unwrap();
    let Target::Inode(ino) = resolve(&sub, &mut f.device, b"sparse.bin", &mut f.node).target else {
        panic!("sparse.bin is a file");
    };
    let mut whole = vec![0u8; 2 << 20];
    let intact = sub
        .read(&mut f.device, ino, 0, &mut whole, &mut f.node, &mut buffers)
        .expect("the image is well formed");
    assert!(intact > 1 << 20, "the whole sparse file reads back first");

    let after_first = BtrfsKey::new(ino, EXTENT_DATA_KEY, 1);
    let (key, node_at, _) = find_item(&volume, &mut f.device, after_first, &mut f.node);
    assert!(
        key.offset >= 8192,
        "the second extent starts after the first"
    );
    let leaf = volume
        .seek(&mut f.device, volume.fs_tree(), &key, &mut f.node)
        .unwrap();
    let slot = leaf.node.search(&key).unwrap();
    let descriptor = node_at + (HEADER_SIZE + slot as usize * ITEM_SIZE) as u64;
    f.device.write(descriptor + 9, &4096u64.to_le_bytes());
    f.device.reseal(node_at);

    assert_eq!(
        sub.read(&mut f.device, ino, 0, &mut whole, &mut f.node, &mut buffers),
        Err(BtrfsError::BadItem {
            item_type: EXTENT_DATA_KEY
        })
    );
}

#[test]
fn read_buffers_smaller_than_an_extent_are_refused() {
    let mut small = vec![0u8; 4096];
    let mut plain = vec![0u8; MAX_UNCOMPRESSED];
    let mut zstd = vec![0u8; compress::zstd::Workspace::SIZE];
    assert!(
        matches!(
            ReadBuffers::new((&mut small[..], &mut plain[..], &mut zstd[..])),
            Err(BtrfsError::Truncated { .. })
        ),
        "a 4 KiB buffer cannot hold a compressed extent"
    );
}
