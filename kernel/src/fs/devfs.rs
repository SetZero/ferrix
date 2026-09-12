//! devfs: the device nodes every program expects to find in `/dev`.
//!
//! A fixed table, not a directory nodes can be created in. What is in `/dev`
//! is the kernel's statement of which devices exist, and until drivers come
//! and go — stage 10's — that statement is a constant: the three memory
//! devices a C library and a shell reach for, the two random devices, and the
//! console under both of its names. A node made by `mknod` in a tmpfs names a
//! device by number and reaches nothing until the VFS routes numbers to
//! drivers; here the node *is* the device, which is why this is a filesystem
//! of its own rather than a tmpfs with nodes unpacked into it.
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
//! # Why it is called devfs
//!
//! `/proc/mounts` says what a filesystem is, and this is not Linux's
//! `devtmpfs`: nothing can be created in it. `devfs` is the name Linux gave a
//! kernel-populated `/dev` before `devtmpfs` replaced it, so a program that
//! looks is told something true in a word it may know.
//!
//! # Streams
//!
//! Every node reports [`Inode::is_stream`], so the VFS passes no offsets and
//! `lseek` is `ESPIPE`. Linux lets a program seek `/dev/null` to a position
//! that means nothing; refusing is the answer every character device here can
//! give alike, including the console, which cannot seek on Linux either.

use alloc::sync::Arc;
use core::any::Any;

use ferrix_vfs::initramfs::makedev;
use ferrix_vfs::{
    DirEntry, Errno, FIRST_CURSOR, FileSystem, FileType, Inode, Metadata, Result, Timespec,
};

use crate::fs;
use crate::fs::console::console_inode;
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

/// Everything in `/dev`, in the order a listing reports it.
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
        "devfs"
    }

    fn device(&self) -> u64 {
        self.device
    }
}

/// Which object a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// `/dev` itself.
    Root,
    /// `DEVICES[index]`.
    Device(usize),
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
    /// The device this node is, or `None` for the directory.
    fn device(&self) -> Option<&'static Device> {
        match self.place {
            Place::Root => None,
            Place::Device(index) => DEVICES.get(index),
        }
    }
}

/// A device's inode number.
fn ino_of(index: usize) -> u64 {
    (index as u64).saturating_add(ROOT_INO + 1)
}

impl Inode for Node {
    fn metadata(&self) -> Metadata {
        let (ino, kind, permissions, nlink, rdev) = match (self.place, self.device()) {
            (Place::Device(index), Some(device)) => (
                ino_of(index),
                FileType::CharDevice,
                device.permissions,
                1,
                makedev(device.major, device.minor),
            ),
            _ => (ROOT_INO, FileType::Directory, 0o755, 2, 0),
        };
        Metadata {
            ino,
            kind,
            permissions,
            nlink,
            uid: 0,
            gid: 0,
            size: 0,
            rdev,
            blocks: 0,
            block_size: 4096,
            atime: self.made,
            mtime: self.made,
            ctime: self.made,
        }
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn is_stream(&self) -> bool {
        self.device().is_some()
    }

    /// The console's own inode takes the reads and writes, so that whatever
    /// the console layer keys on — its object — is what an open of either
    /// name reaches, while `stat` still reports this node's number.
    fn open(&self) -> Result<Option<Arc<dyn Inode>>> {
        Ok(self
            .device()
            .filter(|device| device.behaviour == Behaviour::Console)
            .map(|_| console_inode()))
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
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
        if self.device().is_some() {
            return Err(Errno::ENOTDIR);
        }
        let index = DEVICES
            .iter()
            .position(|device| device.name == name)
            .ok_or(Errno::ENOENT)?;
        // `/dev/console` is the console itself rather than a node standing for
        // it. `fs::console::open_console` names a process's descriptors
        // `/dev/console` only when that name reaches this very inode, and
        // `stat` then reports the console's own number and identity.
        if DEVICES
            .get(index)
            .is_some_and(|device| device.behaviour == Behaviour::ConsoleItself)
        {
            return Ok(console_inode());
        }
        Ok(Arc::new(Node {
            place: Place::Device(index),
            made: self.made,
        }))
    }

    fn read_dir(&self, cursor: u64, emit: &mut dyn FnMut(DirEntry<'_>) -> bool) -> Result<()> {
        if self.device().is_some() {
            return Err(Errno::ENOTDIR);
        }
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
                break;
            }
        }
        Ok(())
    }
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
