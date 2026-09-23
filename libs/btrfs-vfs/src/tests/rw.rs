//! Tests for the writable mount, through the [`Inode`] trait the VFS calls.
//!
//! What they check is that the trait's contract holds over a real volume:
//! what a write puts in comes back out of a read, out of a fresh mount after
//! a commit, and out of the stage 11 reader, which knows nothing of this
//! crate; and that a file unlinked while something holds it stays readable
//! until that goes, as POSIX says.

extern crate std;

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::vec;

use ferrix_btrfs::chunk::ChunkMapEntry;
use ferrix_btrfs::volume::{Device, ReadKind, Volume};
use ferrix_sync::SpinParker;
use ferrix_vfs::tmpfs::HeapStorage;
use ferrix_vfs::tmpfs::Storage;
use ferrix_vfs::{Clock, Errno, FileSystem, FileType, Inode, Metadata, NewNode, Timespec};

use alloc::sync::Arc;
use alloc::vec::Vec;
use ferrix_btrfs::BtrfsError;

use super::{BLOCK, IMAGE_SIZE};
use crate::rw::RwBtrfs;

/// An empty volume with `mkfs.btrfs`'s defaults, which the write path starts
/// from.
const BLANK: &[u8] = include_bytes!("../../../btrfs/testdata/blank.img.packed");
/// A volume whose default subvolume is not the top-level tree: not writable.
const SUBVOL: &[u8] = include_bytes!("../../../btrfs/testdata/default-subvol.img.packed");

/// A packed image in memory that can be written to, shared by every clone,
/// as one disk is.
#[derive(Clone, Debug)]
struct Disk(Arc<Mutex<BTreeMap<u64, [u8; BLOCK]>>>);

impl Disk {
    fn new(packed: &[u8]) -> Disk {
        let blocks = packed
            .chunks_exact(8 + BLOCK)
            .map(|record| {
                let offset = u64::from_le_bytes(record[..8].try_into().unwrap());
                (offset, record[8..].try_into().unwrap())
            })
            .collect();
        Disk(Arc::new(Mutex::new(blocks)))
    }
}

impl Device for Disk {
    fn read_at(
        &mut self,
        physical: u64,
        buf: &mut [u8],
        _kind: ReadKind,
    ) -> Result<(), BtrfsError> {
        if physical
            .checked_add(buf.len() as u64)
            .is_none_or(|end| end > IMAGE_SIZE)
        {
            return Err(BtrfsError::DeviceRead { physical });
        }
        let blocks = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut done = 0;
        while done < buf.len() {
            let at = physical + done as u64;
            let base = at - at % BLOCK as u64;
            let within = (at - base) as usize;
            let take = (BLOCK - within).min(buf.len() - done);
            match blocks.get(&base) {
                Some(block) => {
                    buf[done..done + take].copy_from_slice(&block[within..within + take]);
                }
                None => buf[done..done + take].fill(0),
            }
            done += take;
        }
        Ok(())
    }
}

impl ferrix_btrfs_write::WriteDevice for Disk {
    fn write_at(&mut self, physical: u64, data: &[u8]) -> ferrix_btrfs_write::Result<()> {
        if physical
            .checked_add(data.len() as u64)
            .is_none_or(|end| end > IMAGE_SIZE)
        {
            return Err(ferrix_btrfs_write::Error::DeviceWrite { physical });
        }
        let mut blocks = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut done = 0;
        while done < data.len() {
            let at = physical + done as u64;
            let base = at - at % BLOCK as u64;
            let within = (at - base) as usize;
            let take = (BLOCK - within).min(data.len() - done);
            let block = blocks.entry(base).or_insert([0; BLOCK]);
            block[within..within + take].copy_from_slice(&data[done..done + take]);
            done += take;
        }
        Ok(())
    }

    fn flush(&mut self) -> ferrix_btrfs_write::Result<()> {
        Ok(())
    }
}

/// A clock that stands still, so a test's timestamps are its own.
#[derive(Debug)]
struct Fixed;

impl Clock for Fixed {
    fn now(&self) -> Timespec {
        Timespec {
            tv_sec: 1_790_000_000,
            tv_nsec: 0,
        }
    }
}

fn storage() -> Arc<dyn Storage> {
    Arc::new(HeapStorage::new(1 << 30))
}

fn mount(disk: &Disk) -> Arc<RwBtrfs<Disk>> {
    RwBtrfs::mount(
        disk.clone(),
        0x0800_0010,
        storage(),
        Arc::new(Fixed),
        &SpinParker,
    )
    .expect("the blank volume mounts for writing")
}

/// Make a file with `data` in it under `dir`.
fn make_file(dir: &Arc<dyn Inode>, name: &[u8], data: &[u8]) -> Arc<dyn Inode> {
    let file = dir.create(name, NewNode::Regular, 0o644).unwrap();
    if !data.is_empty() {
        assert_eq!(file.write_at(0, data, false).unwrap().0, data.len());
    }
    file
}

/// Read a whole file through the trait.
fn read_all(file: &Arc<dyn Inode>) -> Vec<u8> {
    let size = usize::try_from(file.metadata().size).unwrap();
    let mut out = vec![0u8; size];
    let read = file.read_at(0, &mut out).unwrap();
    out.truncate(read);
    out
}

/// What the stage 11 reader makes of the volume: it knows nothing of the
/// write path, so it is the check that the bytes are really btrfs.
fn read_back_with_the_reader(disk: &Disk, path: &[&[u8]]) -> Option<Metadata> {
    let mut device = disk.clone();
    let mut node = vec![0u8; 65536];
    let volume = Volume::open(&mut device, vec![ChunkMapEntry::EMPTY; 256], &mut node).unwrap();
    let subvolume = volume.default_subvolume();
    let mut ino = subvolume.root_dir();
    for part in path {
        let entry = subvolume
            .lookup(&mut device, ino, part, &mut node)
            .unwrap()?;
        match entry.target {
            ferrix_btrfs::fs::Target::Inode(next) => ino = next,
            ferrix_btrfs::fs::Target::Subvolume(_) => return None,
        }
    }
    let item = subvolume.inode(&mut device, ino, &mut node).unwrap()?;
    crate::metadata(ino, &item, volume.sectorsize()).ok()
}

#[test]
fn a_tree_made_through_the_trait_survives_a_remount() {
    let disk = Disk::new(BLANK);
    let data = vec![7u8; 100_000];
    {
        let fs = mount(&disk);
        let root = fs.root();
        let dir = root.create(b"dir", NewNode::Directory, 0o755).unwrap();
        let file = make_file(&dir, b"file", &data);
        let small = make_file(&root, b"small", b"inline bytes");
        let link = root
            .create(b"link", NewNode::Symlink(b"dir/file"), 0o777)
            .unwrap();
        assert_eq!(link.read_link().unwrap(), b"dir/file");
        assert_eq!(read_all(&file), data);
        assert_eq!(read_all(&small), b"inline bytes");
        file.fsync(false).unwrap();
        fs.sync().unwrap();
    }
    // A fresh mount, from the bytes alone.
    let fs = mount(&disk);
    let dir = fs.root().lookup(b"dir").unwrap();
    let file = dir.lookup(b"file").unwrap();
    assert_eq!(file.metadata().size, data.len() as u64);
    assert_eq!(read_all(&file), data);
    assert_eq!(
        read_all(&fs.root().lookup(b"small").unwrap()),
        b"inline bytes"
    );
    assert_eq!(
        fs.root().lookup(b"link").unwrap().read_link().unwrap(),
        b"dir/file"
    );
    // And the reader agrees about the file.
    let meta = read_back_with_the_reader(&disk, &[b"dir", b"file"]).expect("the reader finds it");
    assert_eq!(meta.size, data.len() as u64);
    assert_eq!(meta.kind, FileType::Regular);
}

#[test]
fn writes_land_where_they_are_asked_to() {
    let disk = Disk::new(BLANK);
    let fs = mount(&disk);
    let root = fs.root();
    let file = make_file(&root, b"file", &vec![1u8; 20_000]);
    // Overwrite the middle, append past the end, and write into a hole.
    let _ = file.write_at(4096, &[2u8; 100], false).unwrap();
    let _ = file.write_at(0, &[3u8; 7], true).unwrap();
    let _ = file.write_at(100_000, &[4u8; 10], false).unwrap();
    let mut expected = vec![1u8; 20_000];
    expected[4096..4196].fill(2);
    expected.extend_from_slice(&[3u8; 7]);
    expected.resize(100_000, 0);
    expected.extend_from_slice(&[4u8; 10]);
    assert_eq!(read_all(&file), expected);
    fs.sync().unwrap();
    let fs = mount(&disk);
    let file = fs.root().lookup(b"file").unwrap();
    assert_eq!(read_all(&file), expected, "after a commit and a remount");
}

#[test]
fn what_is_written_outlives_the_last_reference_to_its_file() {
    // A build closes each object file and opens it again to archive it, and
    // nothing need hold the inode in between: the VFS keeps a bounded number
    // of names. The writes must wait for their writeback rather than go with
    // the inode object, and a commit between two writes must not leave the
    // file as long as it was then. Found by reading, while looking for why
    // `cargo xtask test-selfhost`'s rustc read its object files back short.
    let disk = Disk::new(BLANK);
    let fs = mount(&disk);
    let head = vec![1u8; 10_000];
    let tail = vec![2u8; 30_000];
    let file = make_file(&fs.root(), b"object", &head);
    fs.sync().unwrap();
    let _ = file.write_at(head.len() as u64, &tail, false).unwrap();
    let _ = file.write_at(0, &[3u8; 64], false).unwrap();
    drop(file);
    let mut expected = head;
    expected.extend_from_slice(&tail);
    expected[..64].fill(3);
    let file = fs.root().lookup(b"object").unwrap();
    assert_eq!(file.metadata().size, expected.len() as u64);
    assert_eq!(read_all(&file), expected);
    // One never committed at all, made and let go.
    drop(make_file(&fs.root(), b"fresh", b"not yet on the disk"));
    assert_eq!(
        read_all(&fs.root().lookup(b"fresh").unwrap()),
        b"not yet on the disk"
    );
    // And the writeback they were kept for reaches the disk.
    drop(file);
    fs.sync().unwrap();
    let fs = mount(&disk);
    assert_eq!(read_all(&fs.root().lookup(b"object").unwrap()), expected);
    assert_eq!(
        read_all(&fs.root().lookup(b"fresh").unwrap()),
        b"not yet on the disk"
    );
}

#[test]
fn truncation_cuts_and_extends() {
    let disk = Disk::new(BLANK);
    let fs = mount(&disk);
    let file = make_file(&fs.root(), b"file", &vec![9u8; 30_000]);
    file.set_len(5000).unwrap();
    assert_eq!(read_all(&file), vec![9u8; 5000]);
    file.set_len(9000).unwrap();
    let mut expected = vec![9u8; 5000];
    expected.resize(9000, 0);
    assert_eq!(read_all(&file), expected, "the gap reads as zeros");
    fs.sync().unwrap();
    let fs = mount(&disk);
    assert_eq!(read_all(&fs.root().lookup(b"file").unwrap()), expected);
}

#[test]
fn a_listing_names_every_entry_once() {
    let disk = Disk::new(BLANK);
    let fs = mount(&disk);
    let root = fs.root();
    let dir = root.create(b"dir", NewNode::Directory, 0o755).unwrap();
    for index in 0..50 {
        let name = std::format!("f{index:02}");
        let _ = make_file(&dir, name.as_bytes(), b"x");
    }
    let _ = dir.create(b"sub", NewNode::Directory, 0o755).unwrap();
    let mut names = Vec::new();
    let mut cursor = 0;
    // In pieces, resuming from the cursor, the way `getdents64` asks.
    loop {
        let mut piece = Vec::new();
        dir.read_dir(cursor, &mut |entry| {
            piece.push((entry.name.to_vec(), entry.kind, entry.next));
            piece.len() < 7
        })
        .unwrap();
        let Some(&(_, _, next)) = piece.last() else {
            break;
        };
        cursor = next;
        names.extend(piece.into_iter().map(|(name, kind, _)| (name, kind)));
    }
    assert_eq!(names.len(), 51);
    assert!(names.contains(&(b"f00".to_vec(), FileType::Regular)));
    assert!(names.contains(&(b"sub".to_vec(), FileType::Directory)));
    let mut sorted: Vec<Vec<u8>> = names.iter().map(|(name, _)| name.clone()).collect();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 51, "no entry is listed twice");
}

#[test]
fn renaming_and_unlinking_move_names_about() {
    let disk = Disk::new(BLANK);
    let fs = mount(&disk);
    let root = fs.root();
    let dir = root.create(b"dir", NewNode::Directory, 0o755).unwrap();
    let file = make_file(&root, b"file", b"contents");
    root.link(b"hard", &file).unwrap();
    assert_eq!(file.metadata().nlink, 2);
    root.rename(b"file", &dir, b"moved", false).unwrap();
    assert_eq!(root.lookup(b"file").unwrap_err(), Errno::ENOENT);
    assert_eq!(read_all(&dir.lookup(b"moved").unwrap()), b"contents");
    root.unlink(b"hard").unwrap();
    assert_eq!(dir.lookup(b"moved").unwrap().metadata().nlink, 1);
    // A directory with something in it will not go, and an empty one will.
    assert_eq!(root.rmdir(b"dir").unwrap_err(), Errno::ENOTEMPTY);
    dir.unlink(b"moved").unwrap();
    root.rmdir(b"dir").unwrap();
    assert_eq!(root.lookup(b"dir").unwrap_err(), Errno::ENOENT);
    fs.sync().unwrap();
    let fs = mount(&disk);
    assert_eq!(fs.root().lookup(b"dir").unwrap_err(), Errno::ENOENT);
}

#[test]
fn a_file_unlinked_while_open_stays_readable() {
    let disk = Disk::new(BLANK);
    let fs = mount(&disk);
    let root = fs.root();
    let file = make_file(&root, b"doomed", b"still here");
    root.unlink(b"doomed").unwrap();
    assert_eq!(root.lookup(b"doomed").unwrap_err(), Errno::ENOENT);
    assert_eq!(file.metadata().nlink, 0);
    assert_eq!(read_all(&file), b"still here", "the open file still reads");
    let ino = file.metadata().ino;
    fs.sync().unwrap();
    drop(file);
    // The next operation drains what the drop left, and the inode goes.
    let _ = root.lookup(b"anything");
    fs.sync().unwrap();
    let fs = mount(&disk);
    assert_eq!(fs.root().lookup(b"doomed").unwrap_err(), Errno::ENOENT);
    assert!(ino >= 256);
}

#[test]
fn a_volume_with_a_subvolume_is_not_writable() {
    let disk = Disk::new(SUBVOL);
    let failed = RwBtrfs::mount(disk, 0x0800_0010, storage(), Arc::new(Fixed), &SpinParker);
    assert_eq!(
        failed.err(),
        Some(Errno::EROFS),
        "subvolumes are refused, not damaged"
    );
}

#[test]
fn statfs_says_it_is_btrfs() {
    let disk = Disk::new(BLANK);
    let fs = mount(&disk);
    let stat = fs.statfs();
    assert_eq!(stat.magic, crate::BTRFS_SUPER_MAGIC);
    assert_eq!(stat.block_size, 4096);
    assert!(stat.blocks_free > 0 && stat.blocks_free <= stat.blocks);
}
