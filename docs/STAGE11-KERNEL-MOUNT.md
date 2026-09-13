# Stage 11: the kernel btrfs mount — design

Draft 5, ferrix-61 (stage 11). Approved by ferrix-32; ferrix-32's notes and
ferrix-e6's review folded in below. For ferrix-32 (product owner) and ferrix-e6
(stage 8, VFS and file-backed `mmap`). Written before any kernel code, as
asked. It builds on the page-cache interface recorded in `docs/BACKLOG.md`
(41045d6) and on the block ring spec (`docs/BLOCK-RING.md` once it lands).

## 1. What has to work

Stage 11's exit: a real btrfs image on a virtio-blk disk, served by the ring-3
driver, mounted read-only through the VFS, every file reading back exactly —
and, once stage 8's file `mmap` is on `main`, a mapping of a file seeing the
same pages `read` fills. The volume-side logic is already on `main`
(`libs/btrfs`, `libs/btrfs-vfs`, `libs/block`, 4e14ba4). What is missing is
everything between the ring and `Btrfs::mount`, plus three correctness items
the product owner made part of "done".

## 2. The stack, bottom to top

```
virtio-blk device
  └─ ring-3 driver (stage 10, native, on ferrix-rt)
       └─ block ring: submission/completion rings + pinned data VMO   (BLOCK-RING.md)
            └─ kernel ring glue, kernel/src/block/ring.rs             (new, §3)
                 └─ libs/block::Queue: merge, barriers, deadlines     (on main)
                      └─ kernel BlockDevice: sleeping sector reads    (new, §3)
                           └─ btrfs Device impl + metadata node cache (new, §4)
                                └─ libs/btrfs-vfs: Btrfs, Node         (on main; §5 changes)
                                     └─ PageSource → the inode's VMO   (stage 8's interface)
                                          └─ VFS: read(2), mmap, mount(2)
```

## 3. The kernel block device — ferrix-d9's, reviewed by stage 11

The ring's kernel side is stage 10's (ferrix-d9, BACKLOG row "Ring-3 virtio-blk
reading sectors"), designed in d9's `blkring-kernel-side-design.md`: one kernel
task per ring, `BLOCK_RING_CREATE(device)`, `libs/block`'s `Queue` and waiters,
fixed data regions of `max_sectors × block_size`, and driver death answering
`EIO` to everything outstanding. Its disk type implements stage 8's
`fs::block::BlockDevice` (`read(sector, buf)`, `sectors`, `sector_size`,
`read_only`, `EIO` once gone; the sleep in `read` not interruptible by signals,
as on Linux) and keeps the `BlockRegistration` that
`devfs::register_block(name, major, index × 16, disk)` returns for the ring's
lifetime; dropping it unpublishes the node and the `/proc/partitions` row, and
`BlockRefused::NameInUse` goes back to the driver as refusal 8. The major is
254, the first block major Linux's `__register_blkdev` hands out dynamically.
Stage 11 reviews that design; it does not write the disk type.

## 4. The btrfs `Device`, and the metadata node cache

* **`Device` over `BlockDevice`.** `read_at(physical, buf)` rounds out to
  sectors, bounces only a partial first or last sector, and reports any
  failure as `BtrfsError::DeviceRead`. The handle is an `Arc`, so `Clone` is
  cheap, as `BlockHandle` requires.
* **Node cache: in `libs/btrfs-vfs`, in front of the device (as landed).**
  The product owner chose a device-level cache over threading a hook through
  `libs/btrfs`'s read path. `ferrix-btrfs`'s `Device::read_at` takes a
  `ReadKind` — `Metadata` for the superblock and tree nodes, `Data` for
  extents — and `libs/btrfs-vfs` keeps only metadata reads, so the node cache
  and the page cache never hold the same bytes:
  * **Bounded:** 1024 reads, evicted by CLOCK, shared by every handle of a
    mount as `Arc<[u8]>`.
  * **Trusted no more than a read:** on a read-only volume the bytes at a
    physical address never change, so a hit is exactly what the device would
    return, and `libs/btrfs` still checks every node's level, generation, fsid
    and checksum.
  * **No lock across I/O:** a hit takes a reference under the lock and copies
    with it released; a miss reads with no lock held and inserts only if still
    absent.
  * **Stage 12** must invalidate by physical range before writes exist.
  * **Fuzzing:** the alternating cached and uncached harness belongs in
    `libs/btrfs-vfs`, and is still to write.

## 5. `libs/btrfs-vfs` changes

1. **One `Node` per inode.** Today `lookup` builds a fresh `Node` every time
   (`lib.rs:367`), so two names for one file would get two page caches.
   `Shared` keeps `SpinLock<BTreeMap<u64, Weak<Node>>>`; a lookup returns the
   live `Node` if there is one. `Node`'s `Drop` takes that lock and removes its
   entry if the entry is still dead, so the map does not grow with every inode
   ever looked up. A file mapping holds the inode, not only its VMO (stage 8's
   side), so a lookup while a mapping exists finds the same `Node` and cache.
2. **The page cache.** Built on ferrix-e6's `libs/vfs` half, verified on
   develop c979d03 as 6b14a53 and e747aa5, not yet landed: `PageSource`,
   `Pages::object`, `Storage::allocate_with` and `HeapPages::with_source`, all
   in `ferrix_vfs::tmpfs` for now. A fill returning `Ok(0)` or more pages than
   asked is `EIO`, and a failed fill inserts nothing. A regular file's `Node`
   creates its `Pages` lazily,
   through `Storage::allocate_with(source)`, set-if-absent under the `Node`'s
   lock so two first users cannot make two caches (`allocate_with` does not
   sleep). Creation is one helper, called both from `read_at` and from the
   provided `Inode` method stage 8's file-`mmap` landing adds to reach a file's
   pages, because a mapping can come before any read. The source holds `Arc<Shared>`,
   the inode number and `i_size`, and implements
   `PageSource::fill_range(first, pages)`:
   * up to 32 pages per call (128 KiB, one compressed extent's worth), read in
     one `Subvolume::read` into a pooled scratch buffer, then split into the
     pages;
   * zeros past `i_size`;
   * a data checksum failure is never a zeroed page: a failure partway through
     a run returns `Ok(n)` for the leading pages that verified, so they are
     cached, and the next call, starting at the bad page, returns `Err(EIO)`;
   * called with no lock held, so it may sleep in `BlockDevice::read`.

   `read_at` becomes: clamp to `i_size`, then `pages.read`. mmap gets the VMO
   through `Pages::object()`; the fault path is stage 8's.

   The read path copies twice, from the ring's data VMO into scratch and from
   scratch into the pages. That is the first correct baseline. A later
   zero-copy read pins the page-cache pages themselves as the ring's buffers
   (ARCHITECTURE §3), and needs nothing here unpicked; it is a P2 backlog row.
3. **`mount(2)`.** `filesystem_named` learns `b"btrfs"`. The source path must
   resolve to a block device node. The mount does not open it — opening a
   block node is `ENXIO` until this stage — but takes its `rdev` and looks it up
   in stage 8's block-device registry, `devfs::block_device(rdev)`, which
   ferrix-e6 is building now so it lands before K1. The node is created on HELLO under the name HELLO
   carries (§9 (b)), and the mount is read-only: a read-write
   request, or `MS_REMOUNT` to read-write, is `EROFS`, so nothing pretends
   stage 12 exists. Mount options (`subvol=`, `subvolid=`) come later; an
   unknown option is `EINVAL`.

## 6. Correctness items that gate "done"

* **Default subvolume** — done in `stage11-followups`: the root tree's
  `default` entry names the subvolume, as Linux's
  `get_default_subvol_objectid` finds it.
* **Data checksums** — written in `stage11-csum`, awaiting its build and gate. For each on-disk sector of a regular or compressed
  extent, look up the `EXTENT_CSUM` item in the checksum tree and compare
  CRC-32C, the only algorithm the mount accepts. Compressed extents are
  verified on their on-disk bytes, before decompression. Inline extents are
  already covered by the node checksum; holes and preallocated ranges have
  none. Inodes flagged `NODATASUM` skip verification. **A sector that should
  have a checksum and has none is an `EIO`**, confirmed against Linux's source
  (torvalds/linux `master`, fetched 2026-09-13): `btrfs_lookup_bio_sums` in
  `fs/btrfs/file-item.c` fills that sector's expected checksum with zeros and
  warns "csum hole found for disk bytenr range", and `btrfs_data_csum_ok` in
  `fs/btrfs/inode.c` then compares the computed checksum against those zeros,
  reports a data checksum error and fails the read. (Only the data-relocation
  tree treats a hole as `NODATASUM`, and this reader never reads it.) The code
  comment cites both functions. A volume whose checksum algorithm is not
  CRC-32C is already refused at mount, with a message naming the algorithm.
* **`INODE_EXTREF`**, parsed after the mount works; it only matters to
  reverse lookups, not to reading.

## 7. The exit test in QEMU

xtask attaches a second, read-only virtio-blk disk carrying a real
`mkfs.btrfs` image, the fixtures' `none` image unpacked, beside the pattern
disk. `test-vfs` mounts it at `/mnt` and checks every manifest entry's size
and CRC-32C. Two more checks join it: a copy of the image with one data sector
flipped must return `EIO` for that file and correct data for the rest, and a
mapping of a file must equal what `read` returns. The `mmap` check is part of
stage 11's exit, because the shared-page claim is about btrfs files; if the
mount is ready before stage 8's `mmap`, stage 11 is called done on the read
checks, the section says so, and the `mmap` check joins when `mmap` lands.

## 8. Order of work

| Step | What | Waits on |
|---|---|---|
| L1 | One `Node` per inode; node cache; data checksums; `INODE_EXTREF` | nothing — next |
| L2 | `PageSource` for btrfs files; `read_at` through the cache | ferrix-e6's `libs/vfs` `PageSource` (with the `OpenFile::read` lock fix) |
| K1 | The ring's kernel side and its disk (ferrix-d9's) | `ferrix-blkring` on develop (landing A); stage 8's block registry |
| K2 | The btrfs `Device` adapter over `Arc<dyn BlockDevice>`, and `mount -t btrfs` through `devfs::block_device(rdev)` | K1; ferrix-e6's block-device registry; ferrix-e6's `Namespace::rename` fix, which today holds its spin lock across both path walks and so across a btrfs `lookup` that can sleep; ferrix-e6's kernel VMO fill path (`VmoStorage::allocate_with` is `ENODEV` until it lands) |
| X | The exit test with a btrfs disk | K2; ferrix-e6's file `mmap` for the `mmap` check |

## 9. Open questions

* **(a) settled by ferrix-e6:** `HeapStorage::allocate_with(source)` comes in
  the `libs/vfs` `PageSource` landing, so `libs/btrfs-vfs` tests its source on
  the host and under Miri with no test storage of its own.
* **(b) settled by ferrix-32:** the kernel creates the node in devfs when it
  accepts HELLO, with the name HELLO carries. devmgr chooses it in Linux's
  virtio scheme (`vda`, `vdb`), and the node gets Linux's virtio-blk major and
  minor, so `mount -t btrfs /dev/vda /mnt` reads as on Linux and
  `/proc/partitions` lists it. devmgr owns the policy: in stage 11 it starts
  block drivers in PCI-address order and names them `vda`, `vdb`, … in that
  order, so names are stable per machine (BLOCK-RING.md §6.1). HELLO also carries the device's PCI `location`
  and its 20-byte virtio `serial`, kept on the block device for a later
  `/dev/disk/by-path` and `by-id`, and a HELLO naming a `location` another
  accepted driver already serves is refused.
* **(c) settled by ferrix-32:** yes, see §7.
