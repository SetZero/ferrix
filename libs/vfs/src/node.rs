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
/// * No operation may sleep while holding a lock the VFS can see, because
///   there is none: the VFS never calls into an inode with a dentry lock held.
///   A filesystem's own locks are its own business, and a filesystem that does
///   I/O must not hold a spin lock across it.
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

    /// Change a regular file's length.
    fn set_len(&self, len: u64) -> Result<()> {
        let _ = len;
        Err(Errno::EINVAL)
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
}
