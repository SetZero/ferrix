//! Tests for the volume reader, against images real `mkfs.btrfs` wrote.
//!
//! The rest of the crate's tests build their structures by hand, which pins
//! every field to an offset. These pin the reader to btrfs itself: the images
//! in `testdata/` come from `scripts/gen-btrfs-fixtures.py`, and nothing in
//! them was produced by this crate.

extern crate std;

use core::ops::ControlFlow;
use std::collections::BTreeMap;
use std::vec;
use std::vec::Vec;

use super::*;
use crate::chunk::ChunkMapEntry;
use crate::items::INODE_ITEM_KEY;

const BLOCK: usize = 4096;
const IMAGE_SIZE: u64 = 128 * 1024 * 1024;

/// The four images, by the compression `mkfs.btrfs` was asked for.
pub(crate) const IMAGES: [(&str, &[u8]); 4] = [
    ("none", include_bytes!("../../testdata/none.img.packed")),
    ("zlib", include_bytes!("../../testdata/zlib.img.packed")),
    ("lzo", include_bytes!("../../testdata/lzo.img.packed")),
    ("zstd", include_bytes!("../../testdata/zstd.img.packed")),
];

/// A device over a packed image: the non-zero blocks, and zeros elsewhere.
pub(crate) struct PackedDevice {
    blocks: BTreeMap<u64, [u8; BLOCK]>,
    /// Physical offsets to fail reads at, to test error propagation.
    pub(crate) fail_at: Option<u64>,
}

impl PackedDevice {
    pub(crate) fn new(packed: &[u8]) -> PackedDevice {
        let mut blocks = BTreeMap::new();
        for record in packed.chunks_exact(8 + BLOCK) {
            let offset = u64::from_le_bytes(record[..8].try_into().unwrap());
            let previous = blocks.insert(offset, record[8..].try_into().unwrap());
            assert!(previous.is_none(), "the generator packs each block once");
        }
        PackedDevice {
            blocks,
            fail_at: None,
        }
    }

    /// Flip one bit of the image at `physical`.
    pub(crate) fn corrupt(&mut self, physical: u64) {
        let base = physical - physical % BLOCK as u64;
        let block = self.blocks.entry(base).or_insert([0; BLOCK]);
        block[(physical - base) as usize] ^= 0x10;
    }
}

impl Device for PackedDevice {
    fn read_at(&mut self, physical: u64, buf: &mut [u8]) -> Result<(), BtrfsError> {
        let end = physical.checked_add(buf.len() as u64);
        if end.is_none_or(|end| end > IMAGE_SIZE)
            || self
                .fail_at
                .is_some_and(|bad| (physical..end.unwrap()).contains(&bad))
        {
            return Err(BtrfsError::DeviceRead { physical });
        }
        for (i, byte) in buf.iter_mut().enumerate() {
            let at = physical + i as u64;
            let base = at - at % BLOCK as u64;
            *byte = self
                .blocks
                .get(&base)
                .map_or(0, |block| block[(at - base) as usize]);
        }
        Ok(())
    }
}

/// Open `packed` with generous storage, handing both to `test`.
fn with_volume(
    packed: &[u8],
    test: impl FnOnce(&mut PackedDevice, &Volume<&mut [ChunkMapEntry; 16]>, &mut [u8]),
) {
    let mut device = PackedDevice::new(packed);
    let mut chunks = [ChunkMapEntry::EMPTY; 16];
    let mut node = vec![0u8; 65536];
    let volume = Volume::open(&mut device, &mut chunks, &mut node).unwrap();
    test(&mut device, &volume, &mut node);
}

/// Every key in `root`, in the order the walk visits them.
fn all_keys(
    device: &mut PackedDevice,
    volume: &Volume<&mut [ChunkMapEntry; 16]>,
    root: TreeRoot,
    node: &mut [u8],
) -> Vec<BtrfsKey> {
    let mut keys = Vec::new();
    let done = volume
        .walk(device, root, BtrfsKey::MIN, node, |_, item| {
            keys.push(item.key);
            Ok(ControlFlow::<()>::Continue(()))
        })
        .unwrap();
    assert_eq!(done, None, "a walk that never breaks runs off the end");
    keys
}

/// How many paths the manifest lists.
fn manifest_entries() -> usize {
    include_str!("../../testdata/manifest.txt")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .count()
}

#[test]
fn every_image_mounts_and_finds_its_fs_tree() {
    for (name, packed) in IMAGES {
        with_volume(packed, |_, volume, _| {
            assert_eq!(
                volume.nodesize(),
                4096,
                "{name}: the generator asks for 4 KiB nodes"
            );
            assert_eq!(volume.sectorsize(), 4096, "{name}: sector size");
            assert_eq!(
                volume.root_dir(),
                256,
                "{name}: the top-level subvolume's root directory"
            );
            assert_eq!(
                volume.fs_tree().level,
                1,
                "{name}: the generator checks the fs tree is more than a leaf"
            );
            assert!(
                volume.chunks().len() >= 3,
                "{name}: system, metadata and data chunks"
            );
        });
    }
}

#[test]
fn a_walk_visits_every_key_once_in_order() {
    for (name, packed) in IMAGES {
        with_volume(packed, |device, volume, node| {
            let keys = all_keys(device, volume, volume.fs_tree(), node);
            assert!(
                keys.windows(2).all(|pair| pair[0] < pair[1]),
                "{name}: keys must strictly ascend across leaf boundaries"
            );
            let inodes = keys
                .iter()
                .filter(|key| key.item_type == INODE_ITEM_KEY)
                .count();
            // No hard links in the fixture, so every manifest path is its own
            // inode; the root directory is the one inode no path names.
            assert_eq!(
                inodes,
                manifest_entries() + 1,
                "{name}: one inode per path, plus the root directory"
            );
        });
    }
}

#[test]
fn a_walk_stops_when_the_visitor_breaks() {
    with_volume(IMAGES[0].1, |device, volume, node| {
        let mut seen = 0;
        let found = volume
            .walk(device, volume.fs_tree(), BtrfsKey::MIN, node, |_, item| {
                seen += 1;
                Ok(if seen == 5 {
                    ControlFlow::Break(item.key)
                } else {
                    ControlFlow::Continue(())
                })
            })
            .unwrap();
        assert!(found.is_some(), "the break value comes back");
        assert_eq!(seen, 5, "nothing is visited after the break");
    });
}

#[test]
fn the_last_key_at_or_before_is_found_across_leaf_boundaries() {
    for (name, packed) in IMAGES {
        with_volume(packed, |device, volume, node| {
            let root = volume.fs_tree();
            let keys = all_keys(device, volume, root, node);
            for pair in keys.windows(2) {
                let (before, after) = (pair[0], pair[1]);
                assert_eq!(
                    volume
                        .last_at_or_before(device, root, &after, node)
                        .unwrap(),
                    Some(after),
                    "{name}: an exact key finds itself"
                );
                if let Some(just_below) = predecessor(&after).filter(|k| *k != before) {
                    assert_eq!(
                        volume
                            .last_at_or_before(device, root, &just_below, node)
                            .unwrap(),
                        Some(before),
                        "{name}: a key in the gap finds the item before the gap"
                    );
                }
            }
            let first = keys[0];
            if let Some(below_all) = predecessor(&first) {
                assert_eq!(
                    volume
                        .last_at_or_before(device, root, &below_all, node)
                        .unwrap(),
                    None,
                    "{name}: nothing sorts before the first key"
                );
            }
        });
    }
}

#[test]
fn a_node_buffer_smaller_than_a_node_is_refused() {
    let mut device = PackedDevice::new(IMAGES[0].1);
    let mut chunks = [ChunkMapEntry::EMPTY; 16];
    let mut node = vec![0u8; 1024];
    assert!(
        matches!(
            Volume::open(&mut device, &mut chunks, &mut node),
            Err(BtrfsError::Truncated { .. })
        ),
        "a 1 KiB buffer cannot hold a 4 KiB node"
    );
}

#[test]
fn a_corrupt_superblock_is_refused() {
    let mut device = PackedDevice::new(IMAGES[0].1);
    device.corrupt(PRIMARY_OFFSET + 0x100);
    let mut chunks = [ChunkMapEntry::EMPTY; 16];
    let mut node = vec![0u8; 65536];
    assert!(
        matches!(
            Volume::open(&mut device, &mut chunks, &mut node),
            Err(BtrfsError::BadChecksum { .. })
        ),
        "a flipped bit in the superblock fails its checksum"
    );
}

#[test]
fn a_corrupt_tree_node_is_refused_not_misread() {
    with_volume(IMAGES[0].1, |device, volume, node| {
        let root = volume.fs_tree();
        let (_, physical) = volume.chunks().map(root.bytenr).unwrap();
        device.corrupt(physical + 200);
        assert!(
            matches!(
                volume.seek(device, root, &BtrfsKey::MIN, node),
                Err(BtrfsError::BadChecksum { .. })
            ),
            "the fs tree's top node no longer checks out"
        );
    });
}

#[test]
fn a_device_error_propagates() {
    with_volume(IMAGES[0].1, |device, volume, node| {
        let root = volume.fs_tree();
        let (_, physical) = volume.chunks().map(root.bytenr).unwrap();
        device.fail_at = Some(physical);
        assert_eq!(
            volume.seek(device, root, &BtrfsKey::MIN, node).unwrap_err(),
            BtrfsError::DeviceRead { physical },
            "a failed read is reported as such, not as corruption"
        );
    });
}

#[test]
fn a_pointer_to_the_wrong_generation_is_refused() {
    with_volume(IMAGES[0].1, |device, volume, node| {
        let mut stale = volume.fs_tree();
        stale.generation += 1;
        assert_eq!(
            volume
                .seek(device, stale, &BtrfsKey::MIN, node)
                .unwrap_err(),
            BtrfsError::BadTree {
                logical: stale.bytenr
            },
            "a node from another transaction is not the one the pointer meant"
        );
    });
}

#[test]
fn a_tree_deeper_than_btrfs_allows_is_refused() {
    with_volume(IMAGES[0].1, |device, volume, node| {
        let mut deep = volume.fs_tree();
        deep.level = MAX_LEVEL + 1;
        assert!(
            matches!(
                volume.seek(device, deep, &BtrfsKey::MIN, node),
                Err(BtrfsError::BadTree { .. })
            ),
            "the level is checked before anything is read"
        );
    });
}

#[test]
fn predecessor_steps_through_every_field() {
    assert_eq!(
        predecessor(&BtrfsKey::new(5, 1, 7)),
        Some(BtrfsKey::new(5, 1, 6)),
        "offset first"
    );
    assert_eq!(
        predecessor(&BtrfsKey::new(5, 1, 0)),
        Some(BtrfsKey::new(5, 0, u64::MAX)),
        "then type"
    );
    assert_eq!(
        predecessor(&BtrfsKey::new(5, 0, 0)),
        Some(BtrfsKey::new(4, u8::MAX, u64::MAX)),
        "then object id"
    );
    assert_eq!(
        predecessor(&BtrfsKey::MIN),
        None,
        "nothing is below the minimum"
    );
}
