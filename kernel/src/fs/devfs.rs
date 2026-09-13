//! devfs: the device nodes every program expects to find in `/dev`.
//!
//! A fixed table, not a directory nodes can be created in, and after it the
//! disks drivers have registered. What is in `/dev` is the kernel's statement
//! of which devices exist: the three memory devices a C library and a shell
//! reach for, the two random devices, the console under both of its names —
//! and, as block drivers come and go, their disks. Here the node *is* the
//! device, which is why this is a filesystem of its own rather than a tmpfs
//! with nodes unpacked into it.
//!
//! # Nodes elsewhere
//!
//! A character device node made by `mknod` in a tmpfs, or unpacked from the
//! initramfs, names a device by number. [`attach_device`] opens it as the
//! device this table gives that number, exactly as the devfs node would open:
//! `/tmp/null` made as `c 1 3` is `/dev/null` to every read and write, and its
//! own inode to `stat`. A number the table does not have is `ENXIO` on open,
//! which is Linux's answer for a character number no driver registered; so is
//! every block device node, including the ones this filesystem lists.
//!
//! # Numbers
//!
//! Linux's, from `Documentation/admin-guide/devices.txt`: major 1 is the
//! memory devices — `null` 3, `zero` 5, `full` 7, `random` 8, `urandom` 9 —
//! and major 5 the alternate TTY devices, `tty` 0 and `console` 1. Programs
//! compare them: `ttyname` walks `/dev` matching `st_rdev`, and a careful
//! daemon checks that what it was handed as `/dev/null` is 1:3 before writing
//! to it. A node with the right name and the wrong number is a wrong answer.
//!
//! # Block devices
//!
//! Stage 11 mounts a btrfs volume from a disk a ring-3 driver serves, and
//! `mount(2)` names that disk by its node here. The node does not open —
//! `ENXIO`, as Linux answers for a block number with no driver behind it — so
//! a mount resolves the node's `st_rdev` with [`block_device`] instead, and
//! reads through the [`BlockDevice`] that returns. The kernel's driver glue
//! calls [`register_block`] when a driver says hello, with the name devmgr
//! chose and numbers derived from it; this registry numbers nothing itself.
//! Block numbers are a namespace of their own: a disk at 1:3 is not
//! `/dev/null`.
//!
//! The registration is a value, and dropping it takes the node, the name and
//! the number away again. A mount that took the device's `Arc` keeps the
//! device, whose reads fail with `EIO` once it has gone away.
//!
//! Three properties a program can see, and how they are kept:
//!
//! - **Listings stay stable across a registration.** The first
//!   [`DEVICES`]`.len()` cursors after the VFS's dot entries are the static
//!   nodes by index, and no registration moves them. The cursors past those
//!   name disks by their registration serial — a number every registration
//!   takes one higher than the last — not by where the disk is in the list.
//!   Disks are listed in serial order, so a listing resumed at a cursor lists
//!   exactly the disks with a serial at least that high that are registered
//!   then: a registration or a drop between two `getdents64` calls neither
//!   repeats nor skips a static node, nor any disk registered throughout.
//! - **Inode numbers are never reused.** A disk's is 2³² plus its serial, far
//!   above the static nodes' index-plus-two. A serial is never handed out
//!   twice, so a name looked up, or opened with `O_PATH`, before its disk went
//!   away can never share a number with a disk registered after — `find` and
//!   `du` take two names with one number to be one file.
//! - **A name registered after a miss is found.** Names here now come and go
//!   behind the VFS's back, so the directory answers
//!   [`Inode::caches_lookups`] with `false`, as procfs does, and every walk
//!   asks the table. Invalidating instead would need the registry to reach
//!   every devtmpfs mount's dentries, which `libs/vfs` offers no way to do; and
//!   what not caching costs here is small: a scan of seven names and a short
//!   locked list, and no mount point inside `/dev`, where nothing can be
//!   created to mount on anyway.
//!
//! # Why it says it is devtmpfs
//!
//! `/proc/mounts` and `/proc/filesystems` name a filesystem by the word
//! `mount -t` takes, and programs decide by that word. An init script greps
//! `/proc/filesystems` for `devtmpfs` before it mounts `/dev`, and a service
//! manager looks in `/proc/mounts` for a `devtmpfs` on `/dev` before deciding
//! whether to mount one. `mount -t devtmpfs` is how a program asks for this
//! filesystem, so the type it reads back is the type it asked for, and the
//! boot's own `/dev` reads the same as one a script mounted. Linux has had no
//! type called `devfs` since 2.6.18, so that name would be a word no program
//! looks for and no `mount` takes.
//!
//! The difference from Linux's is that nothing can be created in it: Linux's
//! `devtmpfs` is a tmpfs the kernel populates, and `mknod` in it works. Here
//! the table is the filesystem, so a program that tries is refused, which it
//! can see, rather than given a node no device answers.
//!
//! # Streams
//!
//! Every node reports [`Inode::is_stream`], so the VFS passes no offsets and
//! `lseek` is `ESPIPE`. Linux lets a program seek `/dev/null` to a position
//! that means nothing; refusing is the answer every character device here can
//! give alike, including the console, which cannot seek on Linux either.

pub(crate) mod check;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

use ferrix_vfs::initramfs::makedev;
use ferrix_vfs::{
    DirEntry, Errno, FIRST_CURSOR, FileSystem, FileType, Inode, Metadata, OpenFile, Result,
    Timespec,
};

use crate::fs;
use crate::fs::block::BlockDevice;
use crate::fs::console::console_inode;
use crate::sync::SpinLock;
use crate::syscall::time;

/// What a node does with reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    /// Reads are at end of file; writes are swallowed.
    Null,
    /// Reads give zeros; writes are swallowed.
    Zero,
    /// Reads give zeros; writes find the device full.
    Full,
    /// Reads give the generator's bytes; writes are accepted and discarded,
    /// where Linux would mix them into a pool this kernel does not have.
    Random,
    /// A node that opens the console: `/dev/tty`, which has a number of its
    /// own and so cannot be the console's inode.
    Console,
    /// The console's own inode: `/dev/console`. Never a devfs node — the name
    /// resolves to [`console_inode`] itself.
    ConsoleItself,
}

/// One node.
#[derive(Debug)]
struct Device {
    /// Its name in `/dev`.
    name: &'static [u8],
    /// Its device number's major half.
    major: u32,
    /// And minor half.
    minor: u32,
    /// Its permission bits.
    permissions: u32,
    /// What it does.
    behaviour: Behaviour,
}

/// Every character node in `/dev`, in the order a listing reports it.
static DEVICES: [Device; 7] = [
    Device {
        name: b"null",
        major: 1,
        minor: 3,
        permissions: 0o666,
        behaviour: Behaviour::Null,
    },
    Device {
        name: b"zero",
        major: 1,
        minor: 5,
        permissions: 0o666,
        behaviour: Behaviour::Zero,
    },
    Device {
        name: b"full",
        major: 1,
        minor: 7,
        permissions: 0o666,
        behaviour: Behaviour::Full,
    },
    Device {
        name: b"random",
        major: 1,
        minor: 8,
        permissions: 0o666,
        behaviour: Behaviour::Random,
    },
    Device {
        name: b"urandom",
        major: 1,
        minor: 9,
        permissions: 0o666,
        behaviour: Behaviour::Random,
    },
    Device {
        name: b"tty",
        major: 5,
        minor: 0,
        permissions: 0o666,
        behaviour: Behaviour::Console,
    },
    Device {
        name: b"console",
        major: 5,
        minor: 1,
        permissions: 0o600,
        behaviour: Behaviour::ConsoleItself,
    },
];

/// The root directory's inode number. A device's is its index plus two.
const ROOT_INO: u64 = 1;

/// The longest name a disk may be registered under.
const BLOCK_NAME_MAX: usize = 32;

/// Every block node's permission bits: read and write for root and the disk
/// group, as a Linux `/dev` gives its disks.
const BLOCK_PERMISSIONS: u32 = 0o660;

/// A disk's inode number is this plus its registration serial.
const BLOCK_INO_BASE: u64 = 1 << 32;

/// The directory cursor that lists the disks from serial 0: the one after
/// the last static node's.
const BLOCK_CURSOR_BASE: u64 = FIRST_CURSOR + DEVICES.len() as u64;

/// A devfs instance.
#[derive(Debug)]
pub(crate) struct Devfs {
    /// `st_dev` for everything in it.
    device: u64,
    /// The directory.
    root: Arc<Node>,
}

impl Devfs {
    /// A devfs, stamped with the time it was made.
    pub(crate) fn new() -> Devfs {
        Devfs {
            device: fs::anonymous_device(),
            root: Arc::new(Node {
                place: Place::Root,
                made: fs::clock().now(),
            }),
        }
    }
}

impl FileSystem for Devfs {
    fn root(&self) -> Arc<dyn Inode> {
        Arc::clone(&self.root) as Arc<dyn Inode>
    }

    fn name(&self) -> &'static str {
        "devtmpfs"
    }

    fn device(&self) -> u64 {
        self.device
    }
}

// -- The block registry ---------------------------------------------------------

/// One registered disk.
#[derive(Debug, Clone)]
struct Disk {
    /// Which registration it is: one higher than the one before, never reused.
    serial: u64,
    /// Its name in `/dev`, the first `name_len` bytes of this.
    name: [u8; BLOCK_NAME_MAX],
    /// How long the name is.
    name_len: usize,
    /// Its device number's major half.
    major: u32,
    /// And minor half.
    minor: u32,
    /// The disk.
    device: Arc<dyn BlockDevice>,
}

impl Disk {
    /// Its name in `/dev`.
    fn name(&self) -> &[u8] {
        self.name.get(..self.name_len).unwrap_or_default()
    }

    /// What a node for it reports: its serial and number, none of which asks
    /// the device, since a block node's `stat` carries no size.
    fn node(&self) -> BlockNode {
        BlockNode {
            serial: self.serial,
            major: self.major,
            minor: self.minor,
        }
    }
}

/// The disks, in registration order, and the serial the next one takes.
#[derive(Debug)]
struct Registry {
    /// The serial the next registration is given.
    next_serial: u64,
    /// Every registered disk, serials ascending.
    disks: Vec<Disk>,
}

/// Every disk `/dev` lists, for every devtmpfs mounted anywhere.
static BLOCKS: SpinLock<Registry> = SpinLock::new(Registry {
    next_serial: 0,
    disks: Vec::new(),
});

/// Why [`register_block`] refused, in the order it checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockRefused {
    /// The name is not 1 to 32 bytes of lowercase letters and digits.
    InvalidName,
    /// A node in `/dev` already has the name: a disk, or a character device.
    NameInUse,
    /// A registered disk already has the number.
    NumberInUse,
}

/// A disk's place in `/dev`, held for as long as the node should exist.
///
/// Not `Clone`, so exactly one drop unpublishes.
#[derive(Debug)]
#[must_use = "dropping the registration takes the node out of /dev at once"]
pub(crate) struct BlockRegistration {
    /// The number registered, major half.
    major: u32,
    /// And minor half.
    minor: u32,
    /// Which registration this is, so that the drop removes this one and no
    /// other.
    serial: u64,
}

impl BlockRegistration {
    /// The device number the disk was registered with.
    pub(crate) fn rdev(&self) -> u64 {
        makedev(self.major, self.minor)
    }
}

impl Drop for BlockRegistration {
    /// Unpublish the node and free the name and the number. The registry's
    /// reference to the device is dropped after the lock is released, since
    /// it may be the last one.
    fn drop(&mut self) {
        let removed = {
            let mut registry = BLOCKS.lock();
            registry
                .disks
                .iter()
                .position(|disk| disk.serial == self.serial)
                .map(|at| registry.disks.remove(at))
        };
        drop(removed);
    }
}

/// Publish a block device node until the returned registration is dropped.
/// `major`/`minor` are derived by the caller from the name; mode is 0660.
///
/// `major` and `minor` are derived by the caller (the kernel's driver glue)
/// from the name devmgr chose; this registry does not number disks.
///
/// # Errors
///
/// In this order: [`BlockRefused::InvalidName`] for a name that is not 1 to
/// 32 bytes of lowercase letters and digits, [`BlockRefused::NameInUse`] for
/// a name any node in `/dev` has, and [`BlockRefused::NumberInUse`] for a
/// number another registered disk has.
pub(crate) fn register_block(
    name: &[u8],
    major: u32,
    minor: u32,
    device: Arc<dyn BlockDevice>,
) -> core::result::Result<BlockRegistration, BlockRefused> {
    let valid = !name.is_empty()
        && name
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let mut stored = [0_u8; BLOCK_NAME_MAX];
    match stored.get_mut(..name.len()) {
        Some(slot) if valid => slot.copy_from_slice(name),
        _ => return Err(BlockRefused::InvalidName),
    }
    if DEVICES.iter().any(|static_node| static_node.name == name) {
        return Err(BlockRefused::NameInUse);
    }
    let mut registry = BLOCKS.lock();
    if registry.disks.iter().any(|disk| disk.name() == name) {
        return Err(BlockRefused::NameInUse);
    }
    if registry
        .disks
        .iter()
        .any(|disk| disk.major == major && disk.minor == minor)
    {
        return Err(BlockRefused::NumberInUse);
    }
    // Saturating, not wrapping: 2^64 registrations is out of reach, and
    // a serial is never handed out below one already given.
    let serial = registry.next_serial;
    registry.next_serial = serial.saturating_add(1);
    registry.disks.push(Disk {
        serial,
        name: stored,
        name_len: name.len(),
        major,
        minor,
        device,
    });
    Ok(BlockRegistration {
        major,
        minor,
        serial,
    })
}

/// The registered disk whose number is `rdev`, for a mount to read through.
///
/// The `Arc` is cloned out, so the registry's lock is not held across any
/// read, and it keeps the device for as long as it is held, registered or not.
pub(crate) fn block_device(rdev: u64) -> Option<Arc<dyn BlockDevice>> {
    BLOCKS
        .lock()
        .disks
        .iter()
        .find(|disk| makedev(disk.major, disk.minor) == rdev)
        .map(|disk| Arc::clone(&disk.device))
}

/// Visit every registered disk, in registration order, with its name,
/// numbers and device. The disks are copied out first, so `visit` runs with
/// no lock held and may ask the device anything.
pub(crate) fn for_each_block(mut visit: impl FnMut(&[u8], u32, u32, &dyn BlockDevice)) {
    for disk in disks_from(0) {
        visit(disk.name(), disk.major, disk.minor, disk.device.as_ref());
    }
}

/// The registered disks with a serial of at least `serial`, copied out.
fn disks_from(serial: u64) -> Vec<Disk> {
    BLOCKS
        .lock()
        .disks
        .iter()
        .filter(|disk| disk.serial >= serial)
        .cloned()
        .collect()
}

/// The registered disk called `name`, copied out.
fn disk_named(name: &[u8]) -> Option<Disk> {
    BLOCKS
        .lock()
        .disks
        .iter()
        .find(|disk| disk.name() == name)
        .cloned()
}

// -- Nodes ----------------------------------------------------------------------

/// What a block node knows of its disk, taken when the name was looked up:
/// enough to answer `stat` after the disk has gone, and nothing that keeps
/// the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockNode {
    /// The registration's serial.
    serial: u64,
    /// The number's major half.
    major: u32,
    /// And minor half.
    minor: u32,
}

/// Which object a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// `/dev` itself.
    Root,
    /// `DEVICES[index]`.
    Device(usize),
    /// A registered disk.
    Block(BlockNode),
}

/// A devfs inode.
#[derive(Debug)]
struct Node {
    /// Which object it is.
    place: Place,
    /// Every timestamp: nothing here is ever modified.
    made: Timespec,
}

impl Node {
    /// The character device this node is, or `None` for the directory and a
    /// disk.
    fn device(&self) -> Option<&'static Device> {
        match self.place {
            Place::Device(index) => DEVICES.get(index),
            Place::Root | Place::Block(_) => None,
        }
    }
}

/// A device's inode number.
fn ino_of(index: usize) -> u64 {
    (index as u64).saturating_add(ROOT_INO + 1)
}

/// A disk's inode number.
fn block_ino(serial: u64) -> u64 {
    BLOCK_INO_BASE.saturating_add(serial)
}

impl Inode for Node {
    fn metadata(&self) -> Metadata {
        let directory = Metadata {
            ino: ROOT_INO,
            kind: FileType::Directory,
            permissions: 0o755,
            nlink: 2,
            uid: 0,
            gid: 0,
            size: 0,
            rdev: 0,
            blocks: 0,
            block_size: 4096,
            atime: self.made,
            mtime: self.made,
            ctime: self.made,
        };
        match (self.place, self.device()) {
            (Place::Device(index), Some(device)) => Metadata {
                ino: ino_of(index),
                kind: FileType::CharDevice,
                permissions: device.permissions,
                nlink: 1,
                rdev: makedev(device.major, device.minor),
                ..directory
            },
            // Size and blocks zero, as Linux reports a block special file: the
            // disk's size is asked of the disk (`/proc/partitions`, and
            // `BLKGETSIZE64` once a block node opens), not of its node.
            (Place::Block(disk), _) => Metadata {
                ino: block_ino(disk.serial),
                kind: FileType::BlockDevice,
                permissions: BLOCK_PERMISSIONS,
                nlink: 1,
                rdev: makedev(disk.major, disk.minor),
                ..directory
            },
            _ => directory,
        }
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn is_stream(&self) -> bool {
        self.place != Place::Root
    }

    /// Disks are registered and dropped without the VFS being told, so a miss
    /// must not be remembered; the module's documentation says why this
    /// rather than invalidating.
    fn caches_lookups(&self) -> bool {
        false
    }

    /// The console's own inode takes the reads and writes, so that whatever
    /// the console layer keys on — its object — is what an open of either
    /// name reaches, while `stat` still reports this node's number. A disk's
    /// node does not open: a mount reaches the disk by number.
    fn open(&self) -> Result<Option<Arc<dyn Inode>>> {
        if let Place::Block(_) = self.place {
            return Err(Errno::ENXIO);
        }
        Ok(self
            .device()
            .filter(|device| device.behaviour == Behaviour::Console)
            .map(|_| console_inode()))
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if let Place::Block(_) = self.place {
            return Err(Errno::ENXIO);
        }
        let device = self.device().ok_or(Errno::EISDIR)?;
        match device.behaviour {
            Behaviour::Null => Ok(0),
            Behaviour::Zero | Behaviour::Full => {
                buf.fill(0);
                Ok(buf.len())
            }
            Behaviour::Random => {
                time::fill_random(buf);
                Ok(buf.len())
            }
            Behaviour::Console | Behaviour::ConsoleItself => console_inode().read_at(offset, buf),
        }
    }

    fn write_at(&self, offset: u64, data: &[u8], append: bool) -> Result<(usize, u64)> {
        if let Place::Block(_) = self.place {
            return Err(Errno::ENXIO);
        }
        let device = self.device().ok_or(Errno::EISDIR)?;
        match device.behaviour {
            Behaviour::Null | Behaviour::Zero | Behaviour::Random => Ok((data.len(), offset)),
            Behaviour::Full => Err(Errno::ENOSPC),
            Behaviour::Console | Behaviour::ConsoleItself => {
                console_inode().write_at(offset, data, append)
            }
        }
    }

    fn lookup(&self, name: &[u8]) -> Result<Arc<dyn Inode>> {
        if self.place != Place::Root {
            return Err(Errno::ENOTDIR);
        }
        if let Some(index) = DEVICES.iter().position(|device| device.name == name) {
            return Ok(node(index, self.made));
        }
        let disk = disk_named(name).ok_or(Errno::ENOENT)?;
        Ok(Arc::new(Node {
            place: Place::Block(disk.node()),
            made: self.made,
        }))
    }

    /// The static nodes by index, then the disks by serial; the module's
    /// documentation says why that keeps a listing in pieces stable.
    fn read_dir(&self, cursor: u64, emit: &mut dyn FnMut(DirEntry<'_>) -> bool) -> Result<()> {
        if self.place != Place::Root {
            return Err(Errno::ENOTDIR);
        }
        if cursor < BLOCK_CURSOR_BASE {
            let first = usize::try_from(cursor.saturating_sub(FIRST_CURSOR)).unwrap_or(usize::MAX);
            for (index, device) in DEVICES.iter().enumerate().skip(first) {
                let ino = if device.behaviour == Behaviour::ConsoleItself {
                    console_inode().metadata().ino
                } else {
                    ino_of(index)
                };
                let entry = DirEntry {
                    ino,
                    kind: FileType::CharDevice,
                    name: device.name,
                    next: FIRST_CURSOR.saturating_add(index as u64 + 1),
                };
                if !emit(entry) {
                    return Ok(());
                }
            }
        }
        // Copied out, so `emit` — which may copy to a program's memory and
        // sleep on a fault — runs with the registry unlocked.
        for disk in disks_from(cursor.saturating_sub(BLOCK_CURSOR_BASE)) {
            let entry = DirEntry {
                ino: block_ino(disk.serial),
                kind: FileType::BlockDevice,
                name: disk.name(),
                next: BLOCK_CURSOR_BASE
                    .saturating_add(disk.serial)
                    .saturating_add(1),
            };
            if !emit(entry) {
                break;
            }
        }
        Ok(())
    }
}

/// The inode `DEVICES[index]` is, stamped `made`.
///
/// `/dev/console` is the console itself rather than a node standing for it.
/// `fs::console::open_console` names a process's descriptors `/dev/console`
/// only when that name reaches this very inode, and `stat` then reports the
/// console's own number and identity.
fn node(index: usize, made: Timespec) -> Arc<dyn Inode> {
    if DEVICES
        .get(index)
        .is_some_and(|device| device.behaviour == Behaviour::ConsoleItself)
    {
        return console_inode();
    }
    Arc::new(Node {
        place: Place::Device(index),
        made,
    })
}

/// What reads and writes of the character device numbered `rdev` go to: the
/// devfs node with that number, opened as an open of it in `/dev` would be.
///
/// # Errors
///
/// `ENXIO` for a number no device here has.
pub(crate) fn open_char_device(rdev: u64) -> Result<Arc<dyn Inode>> {
    let index = DEVICES
        .iter()
        .position(|device| makedev(device.major, device.minor) == rdev)
        .ok_or(Errno::ENXIO)?;
    // The stamp is never seen: `stat` reports the node that was opened, and
    // this inode only takes its reads and writes.
    let device = node(index, Timespec::default());
    Ok(device.open()?.unwrap_or(device))
}

/// An open file of a device node, made to read and write the device its
/// number names. Anything else -- a devfs node, which already is its device,
/// and a node opened with `O_PATH`, which is a handle on the name -- comes
/// back as it was.
///
/// The open keeps its location and inode, so `fstat` reports the node that
/// was opened, with its own inode number and `st_rdev`, as `/dev/tty` reports
/// its own while reading and writing the console.
///
/// # Errors
///
/// `ENXIO` for a character number no device here has, and for every block
/// device, registered or not: a mount reaches a disk through
/// [`block_device`], and nothing reads a block node's descriptor yet.
pub(crate) fn attach_device(file: Arc<OpenFile>) -> Result<Arc<OpenFile>> {
    if file.is_path() {
        return Ok(file);
    }
    match file.kind() {
        FileType::CharDevice => {}
        FileType::BlockDevice => return Err(Errno::ENXIO),
        _ => return Ok(file),
    }
    let inode = file.inode();
    if Arc::clone(inode).into_any().is::<Node>() || Arc::ptr_eq(inode, &console_inode()) {
        return Ok(file);
    }
    let device = open_char_device(inode.metadata().rdev)?;
    Ok(file.with_io(device))
}

/// Mount a devfs on `/dev`, making the directory if the archive had none.
pub(crate) fn mount() -> Result<()> {
    let ns = fs::namespace();
    let ctx = ns.context();
    match ns.mkdir(&ctx, None, b"/dev", 0o755) {
        Ok(()) | Err(Errno::EEXIST) => {}
        Err(errno) => return Err(errno),
    }
    let at = ns.resolve(&ctx, None, b"/dev", true)?;
    ns.mount(Arc::new(Devfs::new()), &at).map(drop)
}
