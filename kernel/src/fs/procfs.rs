//! procfs: the kernel describing itself and its processes as files.
//!
//! Programs learn about themselves through `/proc` rather than through system
//! calls. `ps` is a listing of it, a C library counts processors in
//! `/proc/cpuinfo` and memory in `/proc/meminfo`, a sanitizer and a garbage
//! collector read `/proc/self/maps` to find their own stack, and a program
//! that wants its own path reads `/proc/self/exe`. None of these is optional
//! for the software the exit criterion runs, and the byte formats are pinned
//! in `libs/procfs`, where the host tests hold them against lines a real
//! Linux printed.
//!
//! # Rendered at open
//!
//! Every file is generated when it is opened, by [`Inode::open`] handing the
//! VFS a snapshot, and every read of that open file reads the snapshot. A
//! program reading `/proc/self/maps` a few bytes at a time — `busybox cat`
//! reads in pages, a `getline` loop in whatever its buffer is — sees one
//! consistent file, even though its own reads allocate and change the map
//! they are reading. Rendering at each read would hand it the first half of
//! one map and the second half of another.
//!
//! # Nothing is remembered
//!
//! Names here change without the VFS being told: a process starting adds
//! `/proc/<pid>`, one exiting removes it, and every `open` adds a name to
//! `/proc/<pid>/fd`. So every directory answers [`Inode::caches_lookups`]
//! with `false`, and a walk asks this filesystem each time. `/proc/self` is a
//! symbolic link whose target is the caller's pid, computed when the link is
//! read, which is what makes one name mean a different directory to every
//! process.
//!
//! # Extending it
//!
//! The top level and a process's directory are both tables of [`Entry`]: a
//! name, permission bits, and what the name is — a file rendered at open and
//! optionally writable, a link rendered when read, or the descriptor
//! directory. A new file is a row in [`TOP`] or [`PER_PROCESS`] and one
//! function; nothing else here needs to learn about it.
//!
//! # Inode numbers
//!
//! Computed from what a node is, so the same file has the same number every
//! time without a table of numbers to keep: the top level below 2³², and a
//! process's files above it with the pid in the upper half. `find` and `du`
//! treat two names with one number as one file, so the numbers are distinct;
//! nothing else about them is promised, as nothing is on Linux.

pub(crate) mod check;
mod render;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::fmt;

use ferrix_vfs::{
    DirEntry, Errno, FIRST_CURSOR, FileSystem, FileType, Inode, Metadata, Result, StatFs, Timespec,
};

use crate::fs;
use crate::panic::{catalog, fatal};
use crate::syscall::process::Process;
use crate::syscall::registry;

/// What an entry's functions are handed at the top level: nothing, because a
/// top-level file describes the kernel rather than a process.
pub(crate) type Kernel = ();

/// A write into a file in a table: the context, the bytes, and how many
/// were consumed.
pub(crate) type WriteFn<T> = fn(&T, &[u8]) -> Result<usize>;

/// What a name in a table is.
pub(crate) enum Content<T: 'static> {
    /// A regular file, rendered when opened, and written through `write` if
    /// it has one. A file with none refuses writes with `EINVAL`, as Linux
    /// does for a `/proc` file with no write handler.
    File {
        /// Produce the contents.
        render: fn(&T) -> Result<Vec<u8>>,
        /// Take a write, returning how much was consumed.
        write: Option<WriteFn<T>>,
    },
    /// A symbolic link, whose target is rendered each time it is read.
    Link(fn(&T) -> Result<Vec<u8>>),
    /// `/proc/<pid>/fd`: a link per open descriptor.
    Descriptors,
}

/// One name in a table.
pub(crate) struct Entry<T: 'static> {
    /// The name.
    pub(crate) name: &'static [u8],
    /// Permission bits, as `stat` reports them.
    pub(crate) permissions: u32,
    /// What it is.
    pub(crate) content: Content<T>,
}

impl<T> Entry<T> {
    /// Whether this entry is a file that takes writes.
    const fn takes_writes(&self) -> bool {
        matches!(self.content, Content::File { write: Some(_), .. })
    }

    /// The kind of object this entry is.
    const fn kind(&self) -> FileType {
        match self.content {
            Content::File { .. } => FileType::Regular,
            Content::Link(_) => FileType::Symlink,
            Content::Descriptors => FileType::Directory,
        }
    }
}

/// A file rendered at open that nothing may write.
const fn file<T>(name: &'static [u8], render: fn(&T) -> Result<Vec<u8>>) -> Entry<T> {
    Entry {
        name,
        permissions: 0o444,
        content: Content::File {
            render,
            write: None,
        },
    }
}

/// What a write-only file renders: nothing, so a read is end of file.
fn nothing(_: &Kernel) -> Result<Vec<u8>> {
    Ok(Vec::new())
}

/// `/proc/sysrq-trigger`: writing `c` panics the kernel, on purpose.
///
/// The one request of Linux's magic `SysRq` set that is implemented, because it
/// is the one somebody working on the failure path needs: a way to reach the
/// report, the backtrace, the screen and its QR code from a shell, without
/// building a kernel with a fault in it. Linux looks only at the first byte,
/// and so does this. Any other byte is accepted and does nothing.
fn sysrq_trigger(_: &Kernel, data: &[u8]) -> Result<usize> {
    if data.first() == Some(&b'c') {
        fatal!(
            catalog::SYSRQ_CRASH,
            "panic requested through /proc/sysrq-trigger"
        );
    }
    Ok(data.len())
}

/// `/proc`, less the process directories that follow these in a listing.
pub(crate) static TOP: [Entry<Kernel>; 9] = [
    Entry {
        name: b"self",
        permissions: 0o777,
        content: Content::Link(render::self_link),
    },
    file(b"cpuinfo", render::cpuinfo),
    file(b"filesystems", render::filesystems),
    file(b"meminfo", render::meminfo),
    file(b"mounts", render::mounts),
    file(b"stat", render::kstat),
    file(b"uptime", render::uptime),
    file(b"version", render::version),
    Entry {
        name: b"sysrq-trigger",
        permissions: 0o200,
        content: Content::File {
            render: nothing,
            write: Some(sysrq_trigger),
        },
    },
];

/// `/proc/<pid>`.
pub(crate) static PER_PROCESS: [Entry<Process>; 7] = [
    Entry {
        name: b"fd",
        permissions: 0o500,
        content: Content::Descriptors,
    },
    file(b"status", render::status),
    file(b"comm", render::comm),
    file(b"cmdline", render::cmdline),
    file(b"stat", render::stat),
    file(b"maps", render::maps),
    Entry {
        name: b"exe",
        permissions: 0o777,
        content: Content::Link(render::exe),
    },
];

/// Where the process directories start in the root's cursor space: above
/// every cursor [`TOP`] could use, so a listing resumed after the last table
/// entry begins at the lowest pid, and a cursor never names a pid and a table
/// entry both.
const PID_CURSORS: u64 = 1 << 32;

/// `statfs`'s `f_type` for procfs.
const PROC_SUPER_MAGIC: u64 = 0x9fa0;

/// A procfs instance.
#[derive(Debug)]
pub(crate) struct Procfs {
    /// `st_dev` for everything in it, and the timestamps.
    shared: Arc<Shared>,
}

/// What every node of one instance shares.
#[derive(Debug)]
struct Shared {
    /// `st_dev`.
    device: u64,
    /// Every timestamp: when it was mounted.
    made: Timespec,
}

impl Procfs {
    /// A procfs, stamped with the time it was made.
    pub(crate) fn new() -> Procfs {
        Procfs {
            shared: Arc::new(Shared {
                device: fs::anonymous_device(),
                made: fs::clock().now(),
            }),
        }
    }
}

impl FileSystem for Procfs {
    fn root(&self) -> Arc<dyn Inode> {
        Arc::new(Node {
            place: Place::Root,
            shared: Arc::clone(&self.shared),
        })
    }

    fn name(&self) -> &'static str {
        "proc"
    }

    fn device(&self) -> u64 {
        self.shared.device
    }

    /// `PROC_SUPER_MAGIC`, from `include/uapi/linux/magic.h`, which is how a
    /// program asks whether `/proc` really is procfs before trusting it.
    fn statfs(&self) -> StatFs {
        StatFs {
            magic: PROC_SUPER_MAGIC,
            block_size: 4096,
            name_max: 255,
            ..StatFs::default()
        }
    }
}

/// Which object a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// `/proc`.
    Root,
    /// `TOP[index]`.
    Top(usize),
    /// `/proc/<pid>`.
    Process(u32),
    /// `PER_PROCESS[index]` of the process.
    Entry(u32, usize),
    /// `/proc/<pid>/fd/<fd>`.
    Descriptor(u32, i32),
}

impl Place {
    /// The inode number; see the module documentation.
    fn ino(self) -> u64 {
        let pid = |pid: u32| u64::from(pid) << 32;
        match self {
            Place::Root => 1,
            Place::Top(index) => (index as u64).saturating_add(2),
            Place::Process(id) => pid(id) | 1,
            Place::Entry(id, index) => pid(id) | (0x100 + index as u64),
            Place::Descriptor(id, fd) => pid(id) | (0x1_0000 + u64::from(fd.unsigned_abs())),
        }
    }

    /// The kind and permission bits.
    fn kind(self) -> (FileType, u32) {
        let of = |kind: FileType, permissions: u32| (kind, permissions);
        match self {
            Place::Root | Place::Process(_) => of(FileType::Directory, 0o555),
            Place::Top(index) => TOP.get(index).map_or(of(FileType::Regular, 0), |entry| {
                of(entry.kind(), entry.permissions)
            }),
            Place::Entry(_, index) => PER_PROCESS
                .get(index)
                .map_or(of(FileType::Regular, 0), |entry| {
                    of(entry.kind(), entry.permissions)
                }),
            Place::Descriptor(..) => of(FileType::Symlink, 0o700),
        }
    }
}

/// A procfs inode.
#[derive(Debug)]
struct Node {
    /// Which object it is.
    place: Place,
    /// The instance's device and timestamps.
    shared: Arc<Shared>,
}

impl Node {
    /// Another node of the same instance.
    fn at(&self, place: Place) -> Arc<dyn Inode> {
        Arc::new(Node {
            place,
            shared: Arc::clone(&self.shared),
        })
    }

    /// Render this node's file, if it is one.
    fn snapshot(&self) -> Result<Option<Snapshot>> {
        let metadata = self.metadata();
        match self.place {
            Place::Top(index) => match TOP.get(index).map(|entry| &entry.content) {
                Some(Content::File { render, write }) => Ok(Some(Snapshot {
                    metadata,
                    bytes: render(&())?,
                    write: write.map(|write| -> Writer { Box::new(move |data| write(&(), data)) }),
                })),
                _ => Ok(None),
            },
            Place::Entry(pid, index) => match PER_PROCESS.get(index).map(|entry| &entry.content) {
                Some(Content::File { render, write }) => {
                    let process = alive(pid)?;
                    Ok(Some(Snapshot {
                        metadata,
                        bytes: render(&process)?,
                        write: write
                            .map(|write| -> Writer { Box::new(move |data| write(&process, data)) }),
                    }))
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    /// Whether this node is a file in a table that takes writes.
    fn writable(&self) -> bool {
        match self.place {
            Place::Top(index) => TOP.get(index).is_some_and(Entry::takes_writes),
            Place::Entry(_, index) => PER_PROCESS.get(index).is_some_and(Entry::takes_writes),
            _ => false,
        }
    }

    /// Whether this node is a process's descriptor directory, and whose.
    fn descriptors_of(&self) -> Option<u32> {
        match self.place {
            Place::Entry(pid, index) => PER_PROCESS
                .get(index)
                .filter(|entry| matches!(entry.content, Content::Descriptors))
                .map(|_| pid),
            _ => None,
        }
    }
}

/// The live process with this pid, or `ENOENT`: a directory a program is
/// holding for a process that has since exited is empty rather than an
/// error on every call.
fn alive(pid: u32) -> Result<Arc<Process>> {
    registry::find(pid).ok_or(Errno::ENOENT)
}

impl Inode for Node {
    fn metadata(&self) -> Metadata {
        let (kind, permissions) = self.place.kind();
        let made = self.shared.made;
        Metadata {
            ino: self.place.ino(),
            kind,
            permissions,
            nlink: if kind == FileType::Directory { 2 } else { 1 },
            uid: 0,
            gid: 0,
            // Linux reports zero for every generated file, and a program that
            // sized its buffer by `st_size` learns to read to end of file.
            size: 0,
            rdev: 0,
            blocks: 0,
            block_size: 4096,
            atime: made,
            mtime: made,
            ctime: made,
        }
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn caches_lookups(&self) -> bool {
        false
    }

    fn open(&self) -> Result<Option<Arc<dyn Inode>>> {
        Ok(self
            .snapshot()?
            .map(|snapshot| Arc::new(snapshot) as Arc<dyn Inode>))
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        match self.snapshot()? {
            Some(snapshot) => snapshot.read_at(offset, buf),
            None if self.place.kind().0 == FileType::Directory => Err(Errno::EISDIR),
            None => Err(Errno::EINVAL),
        }
    }

    fn write_at(&self, offset: u64, data: &[u8], append: bool) -> Result<(usize, u64)> {
        match self.snapshot()? {
            Some(snapshot) => snapshot.write_at(offset, data, append),
            None => Err(Errno::EINVAL),
        }
    }

    /// Accepted and ignored on a file that takes writes, as Linux does for its
    /// `/proc` files: a shell's `>` opens with `O_TRUNC`, and a generated file
    /// has no length of its own to cut. Refused on everything else.
    fn set_len(&self, _len: u64) -> Result<()> {
        if self.writable() {
            Ok(())
        } else {
            Err(Errno::EINVAL)
        }
    }

    fn lookup(&self, name: &[u8]) -> Result<Arc<dyn Inode>> {
        match self.place {
            Place::Root => {
                if let Some(index) = TOP.iter().position(|entry| entry.name == name) {
                    return Ok(self.at(Place::Top(index)));
                }
                let pid = number(name).ok_or(Errno::ENOENT)?;
                let _ = alive(pid)?;
                Ok(self.at(Place::Process(pid)))
            }
            Place::Process(pid) => {
                let _ = alive(pid)?;
                let index = PER_PROCESS
                    .iter()
                    .position(|entry| entry.name == name)
                    .ok_or(Errno::ENOENT)?;
                Ok(self.at(Place::Entry(pid, index)))
            }
            _ => {
                let pid = self.descriptors_of().ok_or(Errno::ENOTDIR)?;
                let fd = descriptor_number(name).ok_or(Errno::ENOENT)?;
                let open = alive(pid)?.files().lock().get(fd).is_ok();
                if !open {
                    return Err(Errno::ENOENT);
                }
                Ok(self.at(Place::Descriptor(pid, fd)))
            }
        }
    }

    fn read_dir(&self, cursor: u64, emit: &mut dyn FnMut(DirEntry<'_>) -> bool) -> Result<()> {
        match self.place {
            Place::Root => list_root(cursor, emit),
            Place::Process(pid) => {
                let _ = alive(pid)?;
                let _ = list_table(&PER_PROCESS, cursor, |index| Place::Entry(pid, index), emit);
                Ok(())
            }
            _ => {
                let pid = self.descriptors_of().ok_or(Errno::ENOTDIR)?;
                list_descriptors(pid, cursor, emit)
            }
        }
    }

    fn read_link(&self) -> Result<Vec<u8>> {
        match self.place {
            Place::Top(index) => match TOP.get(index).map(|entry| &entry.content) {
                Some(Content::Link(target)) => target(&()),
                _ => Err(Errno::EINVAL),
            },
            Place::Entry(pid, index) => match PER_PROCESS.get(index).map(|entry| &entry.content) {
                Some(Content::Link(target)) => target(&*alive(pid)?),
                _ => Err(Errno::EINVAL),
            },
            Place::Descriptor(pid, fd) => render::descriptor(&*alive(pid)?, fd),
            _ => Err(Errno::EINVAL),
        }
    }
}

/// Report a table's entries from `cursor` on.
fn list_table<T>(
    table: &[Entry<T>],
    cursor: u64,
    place: impl Fn(usize) -> Place,
    emit: &mut dyn FnMut(DirEntry<'_>) -> bool,
) -> bool {
    let first = usize::try_from(cursor.saturating_sub(FIRST_CURSOR)).unwrap_or(usize::MAX);
    for (index, entry) in table.iter().enumerate().skip(first) {
        let accepted = emit(DirEntry {
            ino: place(index).ino(),
            kind: entry.kind(),
            name: entry.name,
            next: FIRST_CURSOR.saturating_add(index as u64 + 1),
        });
        if !accepted {
            return false;
        }
    }
    true
}

/// `/proc`: the table, then a directory per live process in pid order.
fn list_root(cursor: u64, emit: &mut dyn FnMut(DirEntry<'_>) -> bool) -> Result<()> {
    if cursor < PID_CURSORS && !list_table(&TOP, cursor, Place::Top, emit) {
        return Ok(());
    }
    let from = cursor.saturating_sub(PID_CURSORS);
    let mut digits = [0_u8; 20];
    for process in registry::live() {
        let pid = u64::from(process.pid());
        if pid < from {
            continue;
        }
        let accepted = emit(DirEntry {
            ino: Place::Process(process.pid()).ino(),
            kind: FileType::Directory,
            name: decimal(pid, &mut digits),
            next: PID_CURSORS.saturating_add(pid).saturating_add(1),
        });
        if !accepted {
            break;
        }
    }
    Ok(())
}

/// `/proc/<pid>/fd`: a link per open descriptor, in descriptor order.
fn list_descriptors(
    pid: u32,
    cursor: u64,
    emit: &mut dyn FnMut(DirEntry<'_>) -> bool,
) -> Result<()> {
    let process = alive(pid)?;
    let from = cursor.saturating_sub(FIRST_CURSOR);
    // Collected first: `emit` copies into a buffer the caller owns, and the
    // table's lock is no place to do that from.
    let open: Vec<i32> = process.files().lock().iter().map(|(fd, _)| fd).collect();
    let mut digits = [0_u8; 20];
    for fd in open {
        let at = u64::from(fd.unsigned_abs());
        if at < from {
            continue;
        }
        let accepted = emit(DirEntry {
            ino: Place::Descriptor(pid, fd).ino(),
            kind: FileType::Symlink,
            name: decimal(at, &mut digits),
            next: FIRST_CURSOR.saturating_add(at).saturating_add(1),
        });
        if !accepted {
            break;
        }
    }
    Ok(())
}

/// A name that is a number as Linux writes one: decimal digits, no sign, no
/// leading zero, and not zero itself. `/proc/042` is not `/proc/42`.
fn number(name: &[u8]) -> Option<u32> {
    if name.first() == Some(&b'0') {
        return None;
    }
    let text = core::str::from_utf8(name).ok()?;
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A descriptor's name: as [`number`], except that `0` is one. Every process
/// has a descriptor 0, and a listing of `/proc/<pid>/fd` says so; a name the
/// listing reports that a walk then refuses is what `ls -R` prints an error
/// for.
fn descriptor_number(name: &[u8]) -> Option<i32> {
    if name == b"0" {
        return Some(0);
    }
    number(name).and_then(|fd| i32::try_from(fd).ok())
}

/// `value` in decimal, in `digits`.
fn decimal(value: u64, digits: &mut [u8; 20]) -> &[u8] {
    let mut at = digits.len();
    let mut rest = value;
    loop {
        at -= 1;
        if let Some(slot) = digits.get_mut(at) {
            *slot = b'0' + (rest % 10) as u8;
        }
        rest /= 10;
        if rest == 0 || at == 0 {
            break;
        }
    }
    digits.get(at..).unwrap_or_default()
}

/// A write into a file rendered at open.
type Writer = Box<dyn Fn(&[u8]) -> Result<usize> + Send + Sync>;

/// One open of a generated file: its contents as they were at open.
struct Snapshot {
    /// What `stat` would report, less the size, which is the snapshot's.
    metadata: Metadata,
    /// The contents.
    bytes: Vec<u8>,
    /// Where writes go, for a file that takes them.
    write: Option<Writer>,
}

impl fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("procfs::Snapshot")
            .field("ino", &self.metadata.ino)
            .field("len", &self.bytes.len())
            .field("writable", &self.write.is_some())
            .finish()
    }
}

impl Inode for Snapshot {
    /// The generated file's metadata with the snapshot's length, so that
    /// `SEEK_END` on an open file finds the end of what it can read.
    fn metadata(&self) -> Metadata {
        Metadata {
            size: self.bytes.len() as u64,
            ..self.metadata
        }
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let rest = self.bytes.get(start..).unwrap_or_default();
        let count = rest.len().min(buf.len());
        let (Some(to), Some(from)) = (buf.get_mut(..count), rest.get(..count)) else {
            return Ok(0);
        };
        to.copy_from_slice(from);
        Ok(count)
    }

    fn write_at(&self, offset: u64, data: &[u8], _append: bool) -> Result<(usize, u64)> {
        let write = self.write.as_ref().ok_or(Errno::EINVAL)?;
        let count = write(data)?;
        Ok((count, offset.saturating_add(count as u64)))
    }
}

/// Mount a procfs on `/proc`, making the directory if the archive had none.
pub(crate) fn mount() -> Result<()> {
    let ns = fs::namespace();
    let ctx = ns.context();
    match ns.mkdir(&ctx, None, b"/proc", 0o555) {
        Ok(()) | Err(Errno::EEXIST) => {}
        Err(errno) => return Err(errno),
    }
    let at = ns.resolve(&ctx, None, b"/proc", true)?;
    ns.mount(Arc::new(Procfs::new()), &at).map(drop)
}
