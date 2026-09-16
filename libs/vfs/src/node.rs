//! What a filesystem implements.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::fmt;

use ferrix_linux_abi::errno::Errno;
pub use ferrix_linux_abi::types::Timespec;
use ferrix_linux_abi::types::{
    DT_BLK, DT_CHR, DT_DIR, DT_FIFO, DT_LNK, DT_REG, DT_SOCK, S_IFBLK, S_IFCHR, S_IFDIR, S_IFIFO,
    S_IFLNK, S_IFMT, S_IFREG, S_IFSOCK,
};

use crate::Result;

/// The kind of object an inode is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// A regular file.
    Regular,
    /// A directory.
    Directory,
    /// A symbolic link.
    Symlink,
    /// A character device.
    CharDevice,
    /// A block device.
    BlockDevice,
    /// A named pipe.
    Fifo,
    /// A socket's name in the filesystem.
    Socket,
}

impl FileType {
    /// The `S_IF*` bits `stat` reports for this kind.
    #[must_use]
    pub const fn mode_bits(self) -> u32 {
        match self {
            FileType::Regular => S_IFREG,
            FileType::Directory => S_IFDIR,
            FileType::Symlink => S_IFLNK,
            FileType::CharDevice => S_IFCHR,
            FileType::BlockDevice => S_IFBLK,
            FileType::Fifo => S_IFIFO,
            FileType::Socket => S_IFSOCK,
        }
    }

    /// The kind a mode word names, or `None` for bits no kind has.
    #[must_use]
    pub const fn from_mode(mode: u32) -> Option<FileType> {
        match mode & S_IFMT {
            S_IFREG => Some(FileType::Regular),
            S_IFDIR => Some(FileType::Directory),
            S_IFLNK => Some(FileType::Symlink),
            S_IFCHR => Some(FileType::CharDevice),
            S_IFBLK => Some(FileType::BlockDevice),
            S_IFIFO => Some(FileType::Fifo),
            S_IFSOCK => Some(FileType::Socket),
            _ => None,
        }
    }

    /// The `DT_*` value `getdents64` reports for this kind.
    #[must_use]
    pub const fn dirent_type(self) -> u8 {
        match self {
            FileType::Regular => DT_REG,
            FileType::Directory => DT_DIR,
            FileType::Symlink => DT_LNK,
            FileType::CharDevice => DT_CHR,
            FileType::BlockDevice => DT_BLK,
            FileType::Fifo => DT_FIFO,
            FileType::Socket => DT_SOCK,
        }
    }
}

/// What `stat` reports about an inode, less the device, which is the mount's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    /// Inode number, unique within the filesystem.
    pub ino: u64,
    /// What kind of object it is.
    pub kind: FileType,
    /// Permission and set-id bits, `0o7777` at most.
    pub permissions: u32,
    /// Number of names it has; for a directory, two plus its subdirectories.
    pub nlink: u32,
    /// Owning user.
    pub uid: u32,
    /// Owning group.
    pub gid: u32,
    /// Size in bytes; for a symbolic link, the length of its target.
    pub size: u64,
    /// The device a device node stands for, as Linux's `(major << 8) | minor`
    /// encoding widened to 64 bits. Zero for everything else.
    pub rdev: u64,
    /// Storage actually committed, in 512-byte blocks.
    pub blocks: u64,
    /// The block size a program should use for I/O.
    pub block_size: u32,
    /// Last access.
    pub atime: Timespec,
    /// Last modification of the contents.
    pub mtime: Timespec,
    /// Last change to the metadata or the contents.
    pub ctime: Timespec,
}

impl Metadata {
    /// The whole mode word: kind and permissions.
    #[must_use]
    pub const fn mode(&self) -> u32 {
        self.kind.mode_bits() | self.permissions
    }
}

/// What [`Inode::create`] is asked to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewNode<'a> {
    /// An empty regular file.
    Regular,
    /// An empty directory.
    Directory,
    /// A symbolic link to the given target.
    Symlink(&'a [u8]),
    /// A character or block device node.
    Device {
        /// [`FileType::CharDevice`] or [`FileType::BlockDevice`].
        kind: FileType,
        /// The device it stands for.
        rdev: u64,
    },
    /// A named pipe.
    Fifo,
    /// A socket's name.
    Socket,
}

impl NewNode<'_> {
    /// The kind of inode this will be.
    #[must_use]
    pub const fn kind(&self) -> FileType {
        match self {
            NewNode::Regular => FileType::Regular,
            NewNode::Directory => FileType::Directory,
            NewNode::Symlink(_) => FileType::Symlink,
            NewNode::Device { kind, .. } => *kind,
            NewNode::Fifo => FileType::Fifo,
            NewNode::Socket => FileType::Socket,
        }
    }
}

/// A change to an inode's metadata. `None` leaves a field as it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SetAttributes {
    /// New permission bits.
    pub permissions: Option<u32>,
    /// New owner.
    pub uid: Option<u32>,
    /// New group.
    pub gid: Option<u32>,
    /// New access time.
    pub atime: Option<Timespec>,
    /// New modification time.
    pub mtime: Option<Timespec>,
}

/// One entry a directory reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry<'a> {
    /// The inode number the name refers to.
    pub ino: u64,
    /// What kind of object that is.
    pub kind: FileType,
    /// The name, without any terminator.
    pub name: &'a [u8],
    /// The cursor that resumes reading *after* this entry.
    pub next: u64,
}

/// The cursor at which a filesystem's own entries begin.
///
/// Cursors 0 and 1 are `.` and `..`, which the VFS reports itself because it
/// is the one that knows what a directory's parent is — across a mount point,
/// a filesystem's root directory has a parent the filesystem has never heard
/// of. A filesystem's [`Inode::read_dir`] is never asked for a cursor below
/// this.
pub const FIRST_CURSOR: u64 = 2;

/// What an open object is ready for, as `poll` and `select` report it.
///
/// Four booleans rather than `POLL*` bits, because the bits are the system
/// call layer's encoding and this crate does not encode anything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one independent poll(2) condition, and a bit set would be \
              the encoding this type exists to leave to the system call layer"
)]
pub struct Readiness {
    /// A read would not block: `POLLIN`.
    pub readable: bool,
    /// A write would not block: `POLLOUT`.
    pub writable: bool,
    /// The other end is gone: `POLLHUP`.
    pub hangup: bool,
    /// Writing would fail: `POLLERR`.
    pub error: bool,
}

impl Readiness {
    /// Ready for both, which is what Linux reports for a regular file or a
    /// directory: neither ever makes a reader or a writer wait.
    pub const ALWAYS: Readiness = Readiness {
        readable: true,
        writable: true,
        hangup: false,
        error: false,
    };
}

/// What `statfs` reports about a mounted filesystem.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatFs {
    /// The filesystem's magic number, `f_type`: `TMPFS_MAGIC` and the like.
    /// Programs test it to find out what they are running on.
    pub magic: u64,
    /// The block size the counts below are in.
    pub block_size: u64,
    /// Blocks in total.
    pub blocks: u64,
    /// Blocks free.
    pub blocks_free: u64,
    /// Blocks free to an unprivileged user.
    pub blocks_available: u64,
    /// Inodes in total.
    pub files: u64,
    /// Inodes free.
    pub files_free: u64,
    /// The longest name a directory entry may have.
    pub name_max: u64,
}

/// Where time comes from, for the timestamps a filesystem keeps.
pub trait Clock: Send + Sync + fmt::Debug {
    /// The time now.
    fn now(&self) -> Timespec;
}

/// A mounted filesystem instance.
pub trait FileSystem: Send + Sync + fmt::Debug {
    /// Its root directory.
    fn root(&self) -> Arc<dyn Inode>;
    /// The name `/proc/mounts` would give its type: `tmpfs`, `proc`.
    fn name(&self) -> &'static str;
    /// The device number `stat` reports for every inode in it.
    fn device(&self) -> u64;

    /// What `statfs` reports.
    ///
    /// The default knows nothing: no magic number, no counts, and the name
    /// limit every filesystem here shares. A filesystem a program might test
    /// for — tmpfs, procfs — says what it is.
    fn statfs(&self) -> StatFs {
        StatFs {
            block_size: 4096,
            name_max: crate::path::NAME_MAX as u64,
            ..StatFs::default()
        }
    }
}

/// One file, directory, link or device, as a filesystem implements it.
///
/// Every operation has a default that refuses the way Linux refuses it for an
/// object of the wrong kind, so a filesystem implements only what its objects
/// are. A directory that does not override [`Inode::read_at`] is refused with
/// `EINVAL` from here, but the VFS turns a read of a directory into `EISDIR`
/// before asking; the defaults are the answer for a kind the VFS cannot see.
///
/// # The contract
///
/// * An operation may block. The VFS holds none of an open file description's
///   locks and no dentry lock across a call; the one VFS lock held across
///   calls is the namespace's rename lock, during `rename`, as the crate
///   documentation says. A filesystem's own locks are its own business, and a
///   filesystem that does I/O must not hold a spin lock across it.
/// * A name passed in is already validated: not empty, not `.` or `..`, no
///   `/`, no NUL, no longer than [`crate::path::NAME_MAX`].
/// * Errors are the ones Linux would return for the same operation.
pub trait Inode: Send + Sync + fmt::Debug {
    /// What `stat` reports.
    fn metadata(&self) -> Metadata;

    /// This inode as a value that can be downcast, so a filesystem can
    /// recognise its own inodes when `link` or `rename` hands it one.
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;

    /// Change the metadata.
    fn set_attributes(&self, change: &SetAttributes) -> Result<()> {
        let _ = change;
        Err(Errno::EPERM)
    }

    /// Read up to `buf.len()` bytes at `offset`, returning how many were read.
    /// Zero is end of file.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let _ = (offset, buf);
        Err(Errno::EINVAL)
    }

    /// Write `data` at `offset`, or at the end if `append`, returning the
    /// count written and the offset just past it.
    fn write_at(&self, offset: u64, data: &[u8], append: bool) -> Result<(usize, u64)> {
        let _ = (offset, data, append);
        Err(Errno::EINVAL)
    }

    /// Read from an object with no position, one whose [`Inode::is_stream`]
    /// is true.
    ///
    /// Separate from [`Inode::read_at`] because a stream is the kind of object
    /// that can make a reader wait, and whether it may is the open file's
    /// `O_NONBLOCK` -- which lives on the description, not the inode, and can
    /// change under `fcntl` between two reads. `nonblock` is that flag as it
    /// stands for this call. An object that would wait answers `EAGAIN`
    /// instead when it is set.
    ///
    /// The default reads at offset zero and ignores the flag, which is right
    /// for a stream that never asks a caller to wait on another program.
    fn read_stream(&self, buf: &mut [u8], nonblock: bool) -> Result<usize> {
        let _ = nonblock;
        self.read_at(0, buf)
    }

    /// Take back `bytes`, the start of what the last [`Inode::read_stream`]
    /// gave, which could not be delivered to the reader. The default drops
    /// them: most streams cannot put data back.
    fn unread_stream(&self, bytes: &[u8]) {
        let _ = bytes;
    }

    /// Write to an object with no position; see [`Inode::read_stream`].
    ///
    /// A write that would wait with `nonblock` set answers the count it
    /// managed, or `EAGAIN` if that was nothing.
    fn write_stream(&self, data: &[u8], nonblock: bool) -> Result<usize> {
        let _ = nonblock;
        self.write_at(0, data, false).map(|(count, _)| count)
    }

    /// Change a regular file's length.
    fn set_len(&self, len: u64) -> Result<()> {
        let _ = len;
        Err(Errno::EINVAL)
    }

    /// Lengthen a regular file to `len` if it is shorter, and leave it alone
    /// if it is not: what `fallocate` asks.
    ///
    /// Its own operation rather than a look at the size and a [`Inode::set_len`],
    /// because between the two another writer can extend the file, and the
    /// `set_len` would then cut off what it wrote. The default does exactly
    /// that look and is only as good as a filesystem nobody writes to
    /// concurrently; a filesystem with a lock over its length decides under it.
    fn grow_to(&self, len: u64) -> Result<()> {
        if self.metadata().size >= len {
            return Ok(());
        }
        self.set_len(len)
    }

    /// What the object is ready for.
    ///
    /// The default is [`Readiness::ALWAYS`], which is right for everything that
    /// never makes a caller wait. An object that can — a pipe, a terminal —
    /// reports its state, and wakes whoever waits on it when that changes.
    fn poll(&self) -> Readiness {
        Readiness::ALWAYS
    }

    /// Whether this object has no position: a terminal, a pipe. Offsets passed
    /// to it are ignored and `lseek` on it is `ESPIPE`.
    fn is_stream(&self) -> bool {
        false
    }

    /// Called when the inode is opened. `Some` replaces the object that reads
    /// and writes go to for the life of that open file, while `stat` keeps
    /// reporting this inode.
    ///
    /// This is how a generated file — `/proc/self/maps` — is read
    /// consistently: it renders once, at open, and a program reading it in
    /// small pieces sees one snapshot rather than a different file each time.
    fn open(&self) -> Result<Option<Arc<dyn Inode>>> {
        Ok(None)
    }

    /// Whether the VFS may remember what [`Inode::lookup`] answers in this
    /// directory.
    ///
    /// True for every filesystem whose names change only through the VFS,
    /// which is what lets a cached hit or miss stand until the VFS itself
    /// changes the name. A directory whose names come and go behind its back
    /// answers false — `/proc`, where a process exiting removes a name and a
    /// process starting adds one, and `/proc/<pid>/fd`, where every `open`
    /// does — and every walk through it asks the filesystem afresh. Linux
    /// answers the same question per dentry with `d_revalidate`; one answer
    /// per directory is enough for the directories that exist.
    ///
    /// The cost is that nothing can be mounted inside such a directory: a
    /// mount point is a remembered dentry, and none is remembered here.
    fn caches_lookups(&self) -> bool {
        true
    }

    /// The object called `name` in this directory, or `ENOENT`.
    fn lookup(&self, name: &[u8]) -> Result<Arc<dyn Inode>> {
        let _ = name;
        Err(Errno::ENOTDIR)
    }

    /// Make a new object called `name` in this directory; `EEXIST` if the name
    /// is taken.
    fn create(&self, name: &[u8], node: NewNode<'_>, permissions: u32) -> Result<Arc<dyn Inode>> {
        let _ = (name, node, permissions);
        Err(Errno::ENOTDIR)
    }

    /// Give `target` a further name in this directory.
    fn link(&self, name: &[u8], target: &Arc<dyn Inode>) -> Result<()> {
        let _ = (name, target);
        Err(Errno::ENOTDIR)
    }

    /// Remove the name `name`, which must not be a directory.
    fn unlink(&self, name: &[u8]) -> Result<()> {
        let _ = name;
        Err(Errno::ENOTDIR)
    }

    /// Remove the empty directory called `name`.
    fn rmdir(&self, name: &[u8]) -> Result<()> {
        let _ = name;
        Err(Errno::ENOTDIR)
    }

    /// Move `old` in this directory to `new` in `new_parent`, replacing what
    /// is there unless `replace` is false.
    fn rename(
        &self,
        old: &[u8],
        new_parent: &Arc<dyn Inode>,
        new: &[u8],
        replace: bool,
    ) -> Result<()> {
        let _ = (old, new_parent, new, replace);
        Err(Errno::ENOTDIR)
    }

    /// Report entries from `cursor` onwards, until `emit` returns `false` or
    /// there are none left. An entry `emit` refused has not been consumed.
    fn read_dir(&self, cursor: u64, emit: &mut dyn FnMut(DirEntry<'_>) -> bool) -> Result<()> {
        let _ = (cursor, emit);
        Err(Errno::ENOTDIR)
    }

    /// A symbolic link's target.
    fn read_link(&self) -> Result<Vec<u8>> {
        Err(Errno::EINVAL)
    }

    /// The object a memory mapping of this file maps: in the kernel, the
    /// file's VMO, whose pages `read_at` copies out of too.
    ///
    /// `None`, the default, for an object that cannot be mapped, which `mmap`
    /// answers with `ENODEV`. A filesystem whose file contents live in a
    /// [`crate::tmpfs::Pages`] store answers that store's
    /// [`crate::tmpfs::Pages::object`], making it first if a mapping comes
    /// before any read.
    ///
    /// A store filled from a page source must not be offered here until a
    /// fault can fill it. A private mapping's first write copies the page it
    /// finds, and a page the source has not filled yet would be copied as
    /// zeros over the file's data; the kernel's private file fault documents
    /// the hook it needs. tmpfs's stores have no source, so they are safe.
    fn mapping(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }

    /// The seals on this file, as `fcntl(F_GET_SEALS)` reports them.
    ///
    /// `EINVAL`, the default, for a filesystem that cannot seal, which is
    /// Linux's answer for any file not on shmem.
    fn seals(&self) -> Result<u32> {
        Err(Errno::EINVAL)
    }

    /// Add `seals`, as `fcntl(F_ADD_SEALS)` does.
    ///
    /// A write seal is refused with `EBUSY` while a shared mapping may write
    /// the file. `writably_mapped` answers that, and is asked only after the
    /// seal has been stored: a mapping counts itself before it reads the seals,
    /// so one of the two always sees the other. `EPERM` once `F_SEAL_SEAL` is
    /// set; `EINVAL` for a filesystem that cannot seal.
    fn add_seals(&self, seals: u32, writably_mapped: &dyn Fn() -> bool) -> Result<()> {
        let _ = (seals, writably_mapped);
        Err(Errno::EINVAL)
    }
}
