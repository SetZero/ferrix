# Stage 11 — Block core and btrfs, read ✅

Request queues, merging, the I/O scheduler. Then btrfs stage A: superblock,
chunk tree, root tree, fs trees, extents inline and regular, crc32c, and
zstd/zlib/lzo.

The item parsing is `libs/` code — pure functions over bytes, fuzzed against
images `mkfs.btrfs` produced.

**Exit:** Ferrix mounts an image made by real `mkfs.btrfs`, and reads a file
tree out of it that byte-for-byte matches what the host wrote.

**Done — the read path, host-side, against real images.** Started while
stages 8 to 10 are under way, because everything short of the kernel mount is
logic `cargo test`, Miri and a fuzzer can reach.

* `libs/fs/btrfs` — the read path, allocating nothing and forbidding `unsafe`.
  `volume.rs` mounts: superblock, system chunk array, chunk tree, root tree, and
  the default subvolume's fs tree — the one the root tree's `default` entry
  names, as Linux's `get_default_subvol_objectid` finds it, or the top-level
  tree when there is no entry. It holds one node buffer rather than a path,
  re-descending from the root to reach the next leaf, and checks every node
  against the level, generation and fsid its parent promised. `fs.rs` answers
  what a VFS asks: stat data, lookup by name hash, `readdir` from a resumable
  `DIR_INDEX` cursor, and `read`, which zero-fills and copies extents over the
  top so every kind of hole reads the same way. `compress/` holds zlib, LZO and
  zstd decoders, each written for btrfs's framing of its format.
* **Real images.** `scripts/gen/gen-btrfs-fixtures.py` builds four images with real
  `mkfs.btrfs` — uncompressed, zlib, LZO and zstd, with 4 KiB nodes so the fs
  tree is deeper than a leaf — packed to their non-zero blocks, beside a
  manifest of every path's size and CRC-32C. All four read back exactly.
  (Stage 12 added two more, `blank` and `root`, which the write path starts
  from.) A fifth, small image is made with `mkfs.btrfs -u default:sub`, so its default
  subvolume is not the top-level tree; it mounts the subvolume, and its
  top-level file is not visible. Each decoder is also checked against an
  independent implementation: `miniz_oxide`, `lzokay-native` and `ruzstd`.
* **Data checksums.** Every data sector a read takes from disk is checked
  against the checksum tree before its bytes are used: an uncompressed extent
  in whole sectors, a compressed one on its on-disk bytes before the decoder
  sees them. A sector the tree has no checksum for fails, as on Linux, where
  `btrfs_lookup_bio_sums` expects zeros for a checksum hole and
  `btrfs_data_csum_ok` fails the read. `NODATASUM` files are read unchecked, and
  inline extents are covered by their node's checksum. Checksum items are held
  to `check_csum_item`. A damaged sector reads as an error, never as bytes.
* **`INODE_EXTREF`** records parse, held to `check_inode_extref`, and their key
  hash matches the offsets `mkfs.btrfs` filed real extrefs under. Nothing reads
  back-references yet: lookups and listings use directory entries.
* **A bounded cache of metadata reads.** Every lookup descends a tree from its
  root, and each descent used to read every node on the way from the device
  again. `ferrix-btrfs`'s `Device` now says what each read is for —
  `ReadKind::Metadata` for the superblock and tree nodes, `ReadKind::Data` for
  an extent's bytes — and `libs/fs/btrfs-vfs` keeps metadata reads in a CLOCK cache
  of 1024 entries every handle of a mount shares: a hit hands out a shared
  reference and copies with no lock held, and a miss reads with no lock held. A
  cached node is trusted no more than a read one, since every node is still
  checked against its parent pointer and its checksum. File data is never kept
  there; it belongs in the page cache. A second walk to a file reads no metadata
  from the device, and a cache of two entries still reads every file back.
* `libs/fs/btrfs-vfs` — the mount: stage 8's `FileSystem` and `Inode` over the
  read path, read-only, holding no lock across I/O. Tested through the trait,
  and through `Namespace` at `/mnt` on a tmpfs root. Since stage 12 it holds
  the read-write mount as well, `rw::RwBtrfs`, over `libs/fs/btrfs-write`.
* `libs/fs/block` — the block core's queue: merging, flush and FUA barriers that
  no request crosses, and deadline scheduling, checked against a model by the
  tests and the `block_queue` fuzz target.
* The `btrfs_read` fuzz target starts each run from a real image and applies
  the input as edits, re-checksumming what it edited, so a hostile image that
  checksums correctly reaches the walker and the extent arithmetic.
* **What Linux's tree-checker refuses, refused here too.** A review against
  it found consistency checks the parsers lacked. The parsers now mirror
  `check_leaf` (payloads packed back to back), `check_dir_item` (names, types
  and the name hash), `check_extent_data_item` (extents inside their disk
  extent, none overlapping the last), `btrfs_check_chunk_valid`, and the size
  and root checks of `btrfs_validate_super`. Chunks that overlap are refused,
  and so is a directory name a VFS could not hand to a program — empty, `.`,
  `..`, or containing `/` or NUL. All four images and three more `mkfs.btrfs`
  images from the review still read back.
* **A lock a walk may sleep under.** The namespace's rename lock is held
  across a rename's two path walks, and a walk into btrfs waits for a disk;
  it was a spin lock, so the first such wait would have stalled every CPU
  queued for it. It is now `ferrix_sync::SleepLock`, a lock whose waiters
  sleep on a `Parking` the kernel lends — a `sched::WaitQueue` per lock,
  through `sync::SchedParker` — and which spins on the host. The open file's
  offset is the same kind of lock, held across the read, write or directory
  listing it positions, so two reads racing on one description get
  consecutive bytes, as under Linux's `f_pos_lock`; a stream, `pread` and
  `pwrite` never take it. An uncontended release never touches the wait
  queue, since the offset is taken on every read.

* **The kernel mount.** `mount -t btrfs /dev/vda /mnt` names a block node;
  the kernel takes its number to devfs's registry, wraps the `BlockDevice` it
  finds as the volume's `Device` (whole sectors on one side, byte offsets on
  the other, a bounce buffer only for a read that is not sector-aligned), and
  mounted read-only: a mount without `MS_RDONLY` was `EROFS` until stage 12,
  whose writer it gets now (see stage 12). The mount keeps one inode object per
  inode, found by number, so two names for a file share one page cache; a
  regular file's data lives in the pages the kernel's VMO storage lends it,
  filled from the volume in runs by a `PageSource` that reads with no lock
  held, zero-fills past the end, and on a damaged sector keeps the pages before
  it and answers `EIO` for the bad one alone. `/proc/filesystems` lists
  `btrfs` as the one type needing a device. Tested through the trait and the
  namespace on the host with heap pages; the same code runs over the kernel's.

**Done — the exit, on all three architectures.** `xtask` unpacks the `none`
fixture — the image `scripts/gen/gen-btrfs-fixtures.py` made with real
`mkfs.btrfs` — into a raw disk and attaches it as a second `virtio-blk-pci`
after the pattern disk. The boot check starts a driver for every virtio-blk
function, so `/sbin/blk` serves the fixture as `vdb`; stage 11's check then
mounts it read-only at `/mnt` through the kernel's own mount path, exactly as
`mount -t btrfs -o ro /dev/vdb /mnt` would, and walks the fixture's manifest:
every file read whole through the VFS and the inode's VMO pages, its size and
CRC-32C compared with what the host computed from the bytes it gave
`mkfs.btrfs`; every directory found to be one; the link's target read and
compared the same way. The mount stays, and `test-vfs` reads a file of it
through busybox. The run recorded when it landed, on x86-64, AArch64 and
ARMv7-A at four processors and at two: `vdb mounted read-only at /mnt: 101
files (1270061 bytes), 17 directories and 1 links read back as the host wrote
them`, through the ring-3 driver, the block ring, the registry, the volume
reader with its checksums and the page source. What the check compares is a
checksum per file rather than every byte on the wire, and the checksum is the
manifest's; a byte wrong anywhere in the stack is a CRC that differs.

**Still to do, after the exit.**

* A mapping of a file on the read-only mount. File `mmap` landed on
  2026-09-13, and a file on stage 12's writable mount maps and faults its
  pages in from the disk, but the read-only mount's inodes offer no mapping,
  so `mmap` of a file there is `ENODEV`. What is owed is `Inode::mapping` on
  it and the check that a mapping sees the pages `read` fills.
* Device numbers are passed through as btrfs stores them, not yet checked
  against how Linux reports them.
* A hole written by btrfs itself: `mkfs.btrfs --rootdir` writes a sparse
  file's gap out as data, so no fixture has one. Stage 12's writer makes
  real holes, and its host tests read them back through this reader.
* Entering subvolumes, and the `subvol=`/`subvolid=` mount options: btrfs stage
  C.

---

