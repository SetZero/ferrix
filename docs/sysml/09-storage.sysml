/*
 * Ferrix — SysML v2 model, part 9: storage.
 *
 * docs/ARCHITECTURE.md §8. Block core, VFS, the small in-kernel filesystems
 * and btrfs in three stages. What exists today is marked on each part.
 */
package FerrixStorage {
    private import ScalarValues::*;
    private import FerrixLifecycle::*;
    private import FerrixMemory::Vmo;

    // -----------------------------------------------------------------------
    // Block core, stage 11
    // -----------------------------------------------------------------------

    #planned part def BlockCore {
        @stage { number = 11; }
        doc /* Request queues, merging, an I/O scheduler with per-cgroup
             * bandwidth, and the ring protocol to userspace block drivers. A
             * read into a page-cache page fills the VMO the cache already
             * holds. The queue itself (merging, flush and FUA barriers,
             * deadline scheduling) is written ahead in libs/block. */
        part queues : RequestQueue[0..*];
        part ioScheduler : IoScheduler;
        action submit;
        action flush;
        action fua;
    }
    part def RequestQueue;
    part def IoScheduler;

    #implemented part def NetCore {
        doc /* The networking stage (docs/ROADMAP.md, Networking), which is
             * not on the path to rustc and was taken after stage 11: AF_INET
             * and AF_INET6 sockets over libs/net, AF_NETLINK route sockets
             * and the ifreq ioctls for configuring an interface, /proc/net
             * for the programs that read it, the net ring, and virtio-net as
             * its first driver, in ring 3, with AF_UNIX names under it all.
             * Its exit criterion is met: cargo xtask test-net configures
             * eth0, resolves a name and fetches a file byte for byte on
             * every architecture. */
    }

    // -----------------------------------------------------------------------
    // VFS, stage 8
    // -----------------------------------------------------------------------

    #planned part def Inode {
        @stage { number = 8; }
        ref part pages : Vmo {
            doc /* The page cache is the inode's VMO; a mapped file and a read
                 * file are the same pages. */
        }
    }

    #planned part def Dentry {
        @stage { number = 8; }
        attribute name : String;
        ref part inode : Inode[0..1] {
            doc /* Absent for a negative entry, which is what makes a failed
                 * lookup cheap the second time. */
        }
    }

    #planned part def Mount {
        @stage { number = 8; }
        ref part root : Dentry;
        ref part filesystem : Filesystem;
    }

    #planned part def Vfs {
        @stage { number = 8; }
        doc /* rustc opens tens of thousands of files during a build; this is a
             * performance requirement. Inode cache, dentry cache with negative
             * entries, mount table per mount namespace, fd tables with their
             * sharing rules. */
        part inodes : Inode[0..*];
        part dentries : Dentry[0..*];
        part mounts : Mount[0..*];
        action openat;
        action getdents64;
        action statx;
        action renameat2;
        action pread64;
    }

    #planned part def PageCache {
        @stage { number = 8; }
        doc /* Unified with VMOs: not a separate cache but the set of file
             * VMOs, subject to reclaim's LRU. */
        ref part vmos : Vmo[0..*];
    }

    // -----------------------------------------------------------------------
    // Filesystems
    // -----------------------------------------------------------------------

    abstract #planned part def Filesystem {
        action mount;
        action lookup;
        action read;
        action write;
    }

    #planned part def Tmpfs :> Filesystem { @stage { number = 8; } }
    #planned part def Devfs :> Filesystem { @stage { number = 8; } }
    #planned part def Procfs :> Filesystem {
        @stage { number = 8; }
        doc /* self/maps, self/exe, self/fd backed by the real VM and fd
             * table; cpuinfo, meminfo. */
    }
    #planned part def Sysfs :> Filesystem;
    #planned part def Cgroupfs :> Filesystem { @stage { number = 13; } }

    #planned part def InitramfsUnpack {
        @stage { number = 8; }
        doc /* libs/cpio: the "newc" reader. Borrows, copies nothing,
             * allocates nothing, rejects unsafe paths. Unpacked into tmpfs. */
    }

    #writtenAhead part def BtrfsParsing {
        @stage { number = 11; }
        doc /* libs/btrfs: superblock, sys chunk array and chunk map
             * (logical to physical), B-tree nodes and leaves, item payloads
             * (inode, inode ref, dir, extent data), crc32c. Pure functions over
             * bytes, forbid(unsafe_code); no cache, no transactions. Fuzzed
             * from images mkfs.btrfs produced. */
    }

    #planned part def Btrfs :> Filesystem {
        @stage { number = 11; }
        doc /* The real filesystem, read and write, single device, no RAID
             * 5/6. The largest single piece of work in the project. */
        part parsing : BtrfsParsing;
        part read : BtrfsRead;
        part write : BtrfsWrite;
        part subvolumes : BtrfsSubvolumes;
    }

    #writtenAhead part def BtrfsRead {
        @stage { number = 11; }
        doc /* Stage A: superblock, chunk tree, root tree, fs trees, extent
             * data inline and regular, directory and inode items, crc32c
             * verification, zstd/zlib/lzo. Enough to mount what mkfs.btrfs
             * produced and read a sysroot out of it.
             *
             * Written ahead in libs/btrfs and libs/btrfs-vfs, host-tested
             * against four real images; not yet mounted by the kernel, which
             * needs a block device. Data checksums are not verified yet. */
    }

    #inProgress part def BtrfsWrite {
        @stage { number = 12; }
        doc /* Stage B: copy-on-write allocation through the extent tree,
             * delayed refs, transaction commit against both superblock copies
             * with the right flush/FUA ordering, the free-space tree, the log
             * tree and its replay.
             *
             * allocateCow and commitTransaction are in libs/btrfs-write,
             * host-tested and clean under host btrfs check; replayLog and the
             * kernel mount are still to do. */
        action allocateCow;
        action commitTransaction;
        action replayLog;
    }

    #planned part def BtrfsSubvolumes {
        doc /* Stage C: subvolumes and snapshots, then the rest. */
    }

    #planned part def Filesystems {
        part tmpfs : Tmpfs;
        part devfs : Devfs;
        part procfs : Procfs;
        part sysfs : Sysfs;
        part cgroupfs : Cgroupfs;
        part btrfs : Btrfs;
        part initramfs : InitramfsUnpack;
    }
}
