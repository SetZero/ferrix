//! tmpfs: a filesystem that is only memory.
//!
//! The root filesystem at boot, `/tmp`, and what an initramfs is unpacked
//! into. It is the first filesystem Ferrix has, and it is written against the
//! same [`Inode`] contract btrfs will be.
//!
//! # Where file contents live
//!
//! Not in a `Vec<u8>`. A regular file's bytes are held by a [`Pages`] object
//! the kernel supplies, which in the kernel is a VMO: a sparse list of frames
//! committed on first write. That is what `docs/ARCHITECTURE.md` means by the
//! page cache being unified with VMOs, and it is what will let `mmap` of a
//! tmpfs file map the file's own pages rather than a copy. A byte vector would
//! have worked today and been unpicked the day `MAP_SHARED` met a file.
//!
//! [`HeapStorage`] is the same interface over heap pages, for the host tests
//! and the fuzzer.
//!
//! # Locking
//!
//! One spin lock per inode, holding everything about it. An operation that
//! needs more than one — `link`, `unlink`, `rmdir`, `rename` — takes them in
//! ascending inode number, after looking the names up under the directory
//! lock alone and then re-checking them once everything is held. That order
//! is the only rule, and it is what keeps `rename` of a directory over a
//! sibling from deadlocking against `rmdir` inside it.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use ferrix_linux_abi::errno::Errno;
use ferrix_sync::{SpinLock, SpinLockGuard};

use crate::Result;
use crate::node::{
    Clock, DirEntry, FIRST_CURSOR, FileSystem, FileType, Inode, Metadata, NewNode, SetAttributes,
    StatFs, Timespec,
};
use crate::path::PATH_MAX;

/// The block size tmpfs reports.
pub const BLOCK_SIZE: u32 = 4096;

/// The page size [`HeapPages`] stores in.
const HEAP_PAGE: u64 = 4096;

/// `TMPFS_MAGIC`, from `include/uapi/linux/magic.h`.
pub const TMPFS_MAGIC: u64 = 0x0102_1994;

/// What Linux's tmpfs counts each directory entry as, for a directory's size.
const DIRENT_SIZE: u64 = 20;

/// A regular file's contents: a sparse array of bytes.
///
/// The file's length is tmpfs's to keep; this only holds bytes. Reading a
/// range nothing was written to yields zeros.
pub trait Pages: Send + Sync + fmt::Debug {
    /// Fill `buf` from `offset`.
    ///
    /// # Errors
    ///
    /// `EIO` if the store cannot produce a page it holds.
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<()>;

    /// Store `data` at `offset`.
    ///
    /// # Errors
    ///
    /// `ENOSPC` or `ENOMEM` if memory ran out, with nothing stored past the
    /// point of failure being relied on.
    fn write(&self, offset: u64, data: &[u8]) -> Result<()>;

    /// Forget everything from `offset` on, so that it reads as zeros if the
    /// file grows again: whole pages are released and the tail of a partial
    /// one is cleared.
    fn discard_from(&self, offset: u64);

    /// Memory actually committed, in bytes.
    fn committed_bytes(&self) -> u64;
}

/// Where new files get their [`Pages`].
pub trait Storage: Send + Sync + fmt::Debug {
    /// A store for a new, empty file.
    ///
    /// # Errors
    ///
    /// `ENOMEM`.
    fn allocate(&self) -> Result<Box<dyn Pages>>;

    /// The largest a file may grow, which is `EFBIG` past.
    fn max_file_size(&self) -> u64;

    /// Pages in total and pages free, for `statfs`. Unknown by default, which
    /// `df` shows as a filesystem of no size rather than an invented one.
    fn capacity(&self) -> (u64, u64) {
        (0, 0)
    }
}

/// [`Storage`] on the heap.
#[derive(Debug, Clone, Copy)]
pub struct HeapStorage {
    max_file_size: u64,
}

impl HeapStorage {
    /// Heap storage whose files may grow to `max_file_size` bytes.
    #[must_use]
    pub const fn new(max_file_size: u64) -> HeapStorage {
        HeapStorage { max_file_size }
    }
}

impl Storage for HeapStorage {
    fn allocate(&self) -> Result<Box<dyn Pages>> {
        Ok(Box::new(HeapPages::default()))
    }

    fn max_file_size(&self) -> u64 {
        self.max_file_size
    }
}

/// [`Pages`] on the heap, a page at a time.
#[derive(Debug, Default)]
pub struct HeapPages {
    pages: SpinLock<BTreeMap<u64, Box<[u8]>>>,
}

/// Visit each page-sized piece of `[offset, offset + len)`: its page index,
/// the offset within the page, and the offset within the caller's buffer.
fn pieces(
    offset: u64,
    len: usize,
    mut visit: impl FnMut(u64, usize, core::ops::Range<usize>) -> Result<()>,
) -> Result<()> {
    let mut done = 0_usize;
    while done < len {
        let at = offset.checked_add(done as u64).ok_or(Errno::EFBIG)?;
        let within = usize::try_from(at % HEAP_PAGE).map_err(|_| Errno::EIO)?;
        let take = (HEAP_PAGE as usize - within).min(len - done);
        visit(at / HEAP_PAGE, within, done..done + take)?;
        done += take;
    }
    Ok(())
}

impl Pages for HeapPages {
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let pages = self.pages.lock();
        pieces(offset, buf.len(), |index, within, range| {
            let len = range.len();
            let out = buf.get_mut(range).ok_or(Errno::EIO)?;
            match pages.get(&index) {
                Some(page) => {
                    out.copy_from_slice(page.get(within..within + len).ok_or(Errno::EIO)?);
                }
                None => out.fill(0),
            }
            Ok(())
        })
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<()> {
        let mut pages = self.pages.lock();
        pieces(offset, data.len(), |index, within, range| {
            let len = range.len();
            let page = pages
                .entry(index)
                .or_insert_with(|| alloc::vec![0_u8; HEAP_PAGE as usize].into_boxed_slice());
            let slot = page.get_mut(within..within + len).ok_or(Errno::EIO)?;
            slot.copy_from_slice(data.get(range).ok_or(Errno::EIO)?);
            Ok(())
        })
    }

    fn discard_from(&self, offset: u64) {
        let mut pages = self.pages.lock();
        let first_whole = offset.div_ceil(HEAP_PAGE);
        drop(pages.split_off(&first_whole));
        let within = (offset % HEAP_PAGE) as usize;
        if within != 0
            && let Some(page) = pages.get_mut(&(offset / HEAP_PAGE))
            && let Some(tail) = page.get_mut(within..)
        {
            tail.fill(0);
        }
    }

    fn committed_bytes(&self) -> u64 {
        (self.pages.lock().len() as u64).saturating_mul(HEAP_PAGE)
    }
}

/// What every inode of one tmpfs instance shares.
#[derive(Debug)]
struct Shared {
    device: u64,
    clock: Arc<dyn Clock>,
    storage: Arc<dyn Storage>,
    next_ino: AtomicU64,
}

/// One tmpfs instance.
#[derive(Debug)]
pub struct Tmpfs {
    shared: Arc<Shared>,
    root: Arc<Node>,
}

impl Tmpfs {
    /// An empty tmpfs whose root directory has `permissions`.
    #[must_use]
    pub fn new(
        device: u64,
        clock: Arc<dyn Clock>,
        storage: Arc<dyn Storage>,
        permissions: u32,
    ) -> Arc<Tmpfs> {
        let shared = Arc::new(Shared {
            device,
            clock,
            storage,
            next_ino: AtomicU64::new(1),
        });
        let now = shared.clock.now();
        let root = Node::new(&shared, Body::Dir(Dir::new()), permissions, now);
        Arc::new(Tmpfs { shared, root })
    }
}

impl FileSystem for Tmpfs {
    fn root(&self) -> Arc<dyn Inode> {
        Arc::clone(&self.root) as Arc<dyn Inode>
    }

    fn name(&self) -> &'static str {
        "tmpfs"
    }

    fn device(&self) -> u64 {
        self.shared.device
    }

    fn statfs(&self) -> StatFs {
        let (blocks, free) = self.shared.storage.capacity();
        let files = self
            .shared
            .next_ino
            .load(Ordering::Relaxed)
            .saturating_sub(1);
        StatFs {
            magic: TMPFS_MAGIC,
            block_size: u64::from(BLOCK_SIZE),
            blocks,
            blocks_free: free,
            blocks_available: free,
            files,
            files_free: 0,
            name_max: crate::path::NAME_MAX as u64,
        }
    }
}

/// One tmpfs inode.
pub struct Node {
    ino: u64,
    shared: Arc<Shared>,
    state: SpinLock<State>,
}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("tmpfs::Node")
            .field("ino", &self.ino)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct State {
    permissions: u32,
    uid: u32,
    gid: u32,
    nlink: u32,
    atime: Timespec,
    mtime: Timespec,
    ctime: Timespec,
    body: Body,
}

impl State {
    fn touch(&mut self, now: Timespec) {
        self.mtime = now;
        self.ctime = now;
    }

    fn dir(&mut self) -> Result<&mut Dir> {
        match &mut self.body {
            Body::Dir(dir) => Ok(dir),
            _ => Err(Errno::ENOTDIR),
        }
    }

    fn is_dir(&self) -> bool {
        matches!(self.body, Body::Dir(_))
    }
}

#[derive(Debug)]
enum Body {
    File { pages: Box<dyn Pages>, len: u64 },
    Dir(Dir),
    Symlink(Box<[u8]>),
    Special { kind: FileType, rdev: u64 },
}

/// A directory's names.
///
/// Indexed twice: by name for lookups, and by a cursor handed out in creation
/// order for `getdents64`. A cursor is never reused, so a program reading a
/// directory while another creates and removes names in it sees each
/// surviving entry exactly once — which a position-counting cursor cannot
/// promise, and which `rm -rf` depends on.
#[derive(Debug)]
struct Dir {
    by_name: BTreeMap<Box<[u8]>, u64>,
    by_cursor: BTreeMap<u64, Entry>,
    next_cursor: u64,
    /// Removed: nothing may be created in it again.
    dead: bool,
}

#[derive(Debug)]
struct Entry {
    name: Box<[u8]>,
    ino: u64,
    kind: FileType,
    node: Arc<Node>,
}

impl Dir {
    fn new() -> Dir {
        Dir {
            by_name: BTreeMap::new(),
            by_cursor: BTreeMap::new(),
            next_cursor: FIRST_CURSOR,
            dead: false,
        }
    }

    fn get(&self, name: &[u8]) -> Option<&Entry> {
        self.by_name
            .get(name)
            .and_then(|cursor| self.by_cursor.get(cursor))
    }

    fn insert(&mut self, name: &[u8], node: Arc<Node>, kind: FileType) -> Result<()> {
        let cursor = self.next_cursor;
        self.next_cursor = cursor.checked_add(1).ok_or(Errno::ENOSPC)?;
        let entry = Entry {
            name: Box::from(name),
            ino: node.ino,
            kind,
            node,
        };
        let _ = self.by_name.insert(Box::from(name), cursor);
        let _ = self.by_cursor.insert(cursor, entry);
        Ok(())
    }

    fn remove(&mut self, name: &[u8]) -> Option<Entry> {
        let cursor = self.by_name.remove(name)?;
        self.by_cursor.remove(&cursor)
    }

    fn len(&self) -> usize {
        self.by_name.len()
    }

    fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// Several inode locks, taken in ascending inode number.
struct Locked<'a> {
    guards: Vec<(u64, SpinLockGuard<'a, State>)>,
}

impl<'a> Locked<'a> {
    fn new(nodes: &[&'a Node]) -> Locked<'a> {
        let mut sorted: Vec<&'a Node> = nodes.to_vec();
        sorted.sort_by_key(|node| node.ino);
        sorted.dedup_by_key(|node| node.ino);
        Locked {
            guards: sorted
                .into_iter()
                .map(|node| (node.ino, node.state.lock()))
                .collect(),
        }
    }

    fn state(&mut self, ino: u64) -> Result<&mut State> {
        self.guards
            .iter_mut()
            .find(|(held, _)| *held == ino)
            .map(|(_, guard)| &mut **guard)
            .ok_or(Errno::EIO)
    }
}

impl Node {
    fn new(shared: &Arc<Shared>, body: Body, permissions: u32, now: Timespec) -> Arc<Node> {
        let nlink = if matches!(body, Body::Dir(_)) { 2 } else { 1 };
        Arc::new(Node {
            ino: shared.next_ino.fetch_add(1, Ordering::Relaxed),
            shared: Arc::clone(shared),
            state: SpinLock::new(State {
                permissions: permissions & 0o7777,
                uid: 0,
                gid: 0,
                nlink,
                atime: now,
                mtime: now,
                ctime: now,
                body,
            }),
        })
    }

    fn now(&self) -> Timespec {
        self.shared.clock.now()
    }

    /// The child node called `name`.
    fn child(&self, name: &[u8]) -> Result<Arc<Node>> {
        let mut state = self.state.lock();
        let dir = state.dir()?;
        dir.get(name)
            .map(|entry| Arc::clone(&entry.node))
            .ok_or(Errno::ENOENT)
    }

    /// `other`, if it is an inode of this same tmpfs instance.
    fn ours(&self, other: &Arc<dyn Inode>) -> Result<Arc<Node>> {
        let node = Arc::clone(other)
            .into_any()
            .downcast::<Node>()
            .map_err(|_| Errno::EXDEV)?;
        if Arc::ptr_eq(&node.shared, &self.shared) {
            Ok(node)
        } else {
            Err(Errno::EXDEV)
        }
    }

    /// Whether `name` in this directory still names `ino`, and its kind.
    fn still_names(state: &mut State, name: &[u8], ino: Option<u64>) -> Result<Option<FileType>> {
        let dir = state.dir()?;
        let entry = dir.get(name);
        if entry.map(|entry| entry.ino) == ino {
            Ok(Some(entry.map_or(FileType::Regular, |entry| entry.kind)))
        } else {
            Ok(None)
        }
    }

    fn body_for(&self, node: NewNode<'_>) -> Result<Body> {
        Ok(match node {
            NewNode::Regular => Body::File {
                pages: self.shared.storage.allocate()?,
                len: 0,
            },
            NewNode::Directory => Body::Dir(Dir::new()),
            NewNode::Symlink(target) => {
                if target.len() >= PATH_MAX {
                    return Err(Errno::ENAMETOOLONG);
                }
                Body::Symlink(Box::from(target))
            }
            NewNode::Device { kind, rdev } => {
                if !matches!(kind, FileType::CharDevice | FileType::BlockDevice) {
                    return Err(Errno::EINVAL);
                }
                Body::Special { kind, rdev }
            }
            NewNode::Fifo => Body::Special {
                kind: FileType::Fifo,
                rdev: 0,
            },
            NewNode::Socket => Body::Special {
                kind: FileType::Socket,
                rdev: 0,
            },
        })
    }

    fn unlink_once(&self, name: &[u8]) -> Result<bool> {
        let child = self.child(name)?;
        let mut locked = Locked::new(&[self, &child]);
        let now = self.now();
        {
            let state = locked.state(self.ino)?;
            let Some(kind) = Node::still_names(state, name, Some(child.ino))? else {
                return Ok(false);
            };
            if kind == FileType::Directory {
                return Err(Errno::EISDIR);
            }
            let _ = state.dir()?.remove(name);
            state.touch(now);
        }
        let victim = locked.state(child.ino)?;
        victim.nlink = victim.nlink.saturating_sub(1);
        victim.ctime = now;
        Ok(true)
    }

    fn rmdir_once(&self, name: &[u8]) -> Result<bool> {
        let child = self.child(name)?;
        if child.ino == self.ino {
            return Err(Errno::EINVAL);
        }
        let mut locked = Locked::new(&[self, &child]);
        let now = self.now();
        if Node::still_names(locked.state(self.ino)?, name, Some(child.ino))?.is_none() {
            return Ok(false);
        }
        {
            let victim = locked.state(child.ino)?;
            let Body::Dir(dir) = &mut victim.body else {
                return Err(Errno::ENOTDIR);
            };
            if !dir.is_empty() {
                return Err(Errno::ENOTEMPTY);
            }
            dir.dead = true;
            victim.nlink = 0;
            victim.ctime = now;
        }
        let state = locked.state(self.ino)?;
        let _ = state.dir()?.remove(name);
        state.nlink = state.nlink.saturating_sub(1);
        state.touch(now);
        Ok(true)
    }

    fn rename_once(
        &self,
        old: &[u8],
        new_parent: &Arc<Node>,
        new: &[u8],
        replace: bool,
    ) -> Result<bool> {
        let source = self.child(old)?;
        let victim = match new_parent.child(new) {
            Ok(victim) => Some(victim),
            Err(Errno::ENOENT) => None,
            Err(other) => return Err(other),
        };
        if victim
            .as_ref()
            .is_some_and(|victim| victim.ino == source.ino)
        {
            return Ok(true);
        }
        if victim.is_some() && !replace {
            return Err(Errno::EEXIST);
        }

        let mut nodes: Vec<&Node> = alloc::vec![self, new_parent, &source];
        if let Some(victim) = &victim {
            nodes.push(victim);
        }
        let mut locked = Locked::new(&nodes);
        let now = self.now();

        let Some(kind) = Node::still_names(locked.state(self.ino)?, old, Some(source.ino))? else {
            return Ok(false);
        };
        let victim_ino = victim.as_ref().map(|victim| victim.ino);
        {
            let target_dir = locked.state(new_parent.ino)?;
            if target_dir.dir()?.dead {
                return Err(Errno::ENOENT);
            }
            if Node::still_names(target_dir, new, victim_ino)?.is_none() {
                return Ok(false);
            }
        }
        let moving_dir = kind == FileType::Directory;
        let victim_dir = match victim_ino {
            Some(ino) => {
                let state = locked.state(ino)?;
                match (&state.body, moving_dir) {
                    (Body::Dir(dir), true) if !dir.is_empty() => return Err(Errno::ENOTEMPTY),
                    (Body::Dir(_), false) => return Err(Errno::EISDIR),
                    (Body::Dir(_), true) => true,
                    (_, true) => return Err(Errno::ENOTDIR),
                    (_, false) => false,
                }
            }
            None => false,
        };
        let crossing = self.ino != new_parent.ino;

        let entry = {
            let state = locked.state(self.ino)?;
            let entry = state.dir()?.remove(old).ok_or(Errno::EIO)?;
            if moving_dir && crossing {
                state.nlink = state.nlink.saturating_sub(1);
            }
            state.touch(now);
            entry
        };
        {
            let state = locked.state(new_parent.ino)?;
            let dir = state.dir()?;
            if victim_ino.is_some() {
                let _ = dir.remove(new);
            }
            dir.insert(new, entry.node, entry.kind)?;
            if victim_dir {
                state.nlink = state.nlink.saturating_sub(1);
            }
            if moving_dir && crossing {
                state.nlink = state.nlink.saturating_add(1);
            }
            state.touch(now);
        }
        if let Some(ino) = victim_ino {
            let state = locked.state(ino)?;
            if let Body::Dir(dir) = &mut state.body {
                dir.dead = true;
                state.nlink = 0;
            } else {
                state.nlink = state.nlink.saturating_sub(1);
            }
            state.ctime = now;
        }
        locked.state(source.ino)?.ctime = now;
        Ok(true)
    }
}

impl Inode for Node {
    fn metadata(&self) -> Metadata {
        let state = self.state.lock();
        let (kind, size, blocks, rdev) = match &state.body {
            Body::File { pages, len } => {
                (FileType::Regular, *len, pages.committed_bytes() / 512, 0)
            }
            Body::Dir(dir) => (
                FileType::Directory,
                (dir.len() as u64)
                    .saturating_add(2)
                    .saturating_mul(DIRENT_SIZE),
                0,
                0,
            ),
            Body::Symlink(target) => (FileType::Symlink, target.len() as u64, 0, 0),
            Body::Special { kind, rdev } => (*kind, 0, 0, *rdev),
        };
        Metadata {
            ino: self.ino,
            kind,
            permissions: state.permissions,
            nlink: state.nlink,
            uid: state.uid,
            gid: state.gid,
            size,
            rdev,
            blocks,
            block_size: BLOCK_SIZE,
            atime: state.atime,
            mtime: state.mtime,
            ctime: state.ctime,
        }
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn set_attributes(&self, change: &SetAttributes) -> Result<()> {
        let now = self.now();
        let mut state = self.state.lock();
        if let Some(permissions) = change.permissions {
            state.permissions = permissions & 0o7777;
        }
        if let Some(uid) = change.uid {
            state.uid = uid;
        }
        if let Some(gid) = change.gid {
            state.gid = gid;
        }
        if let Some(atime) = change.atime {
            state.atime = atime;
        }
        if let Some(mtime) = change.mtime {
            state.mtime = mtime;
        }
        state.ctime = now;
        Ok(())
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let state = self.state.lock();
        match &state.body {
            Body::File { pages, len } => {
                if offset >= *len {
                    return Ok(0);
                }
                let available = usize::try_from(*len - offset).unwrap_or(usize::MAX);
                let take = buf.len().min(available);
                pages.read(offset, buf.get_mut(..take).ok_or(Errno::EIO)?)?;
                Ok(take)
            }
            Body::Dir(_) => Err(Errno::EISDIR),
            _ => Err(Errno::EINVAL),
        }
    }

    fn write_at(&self, offset: u64, data: &[u8], append: bool) -> Result<(usize, u64)> {
        let now = self.now();
        let max = self.shared.storage.max_file_size();
        let mut state = self.state.lock();
        let end = match &mut state.body {
            Body::File { pages, len } => {
                let start = if append { *len } else { offset };
                if data.is_empty() {
                    return Ok((0, start));
                }
                let end = start.checked_add(data.len() as u64).ok_or(Errno::EFBIG)?;
                if end > max {
                    return Err(Errno::EFBIG);
                }
                pages.write(start, data)?;
                *len = (*len).max(end);
                end
            }
            Body::Dir(_) => return Err(Errno::EISDIR),
            _ => return Err(Errno::EINVAL),
        };
        state.touch(now);
        Ok((data.len(), end))
    }

    fn set_len(&self, new_len: u64) -> Result<()> {
        let now = self.now();
        if new_len > self.shared.storage.max_file_size() {
            return Err(Errno::EFBIG);
        }
        let mut state = self.state.lock();
        match &mut state.body {
            Body::File { pages, len } => {
                if new_len < *len {
                    pages.discard_from(new_len);
                }
                *len = new_len;
            }
            Body::Dir(_) => return Err(Errno::EISDIR),
            _ => return Err(Errno::EINVAL),
        }
        state.touch(now);
        Ok(())
    }

    fn grow_to(&self, new_len: u64) -> Result<()> {
        let now = self.now();
        if new_len > self.shared.storage.max_file_size() {
            return Err(Errno::EFBIG);
        }
        let mut state = self.state.lock();
        match &mut state.body {
            // Decided under the lock every write takes, so a writer that
            // extends the file in the meantime is never cut back.
            Body::File { len, .. } if *len >= new_len => return Ok(()),
            // Nothing to clear: a shrink zeroes what it cuts off, so the bytes
            // this uncovers already read as zeros.
            Body::File { len, .. } => *len = new_len,
            Body::Dir(_) => return Err(Errno::EISDIR),
            _ => return Err(Errno::EINVAL),
        }
        state.touch(now);
        Ok(())
    }

    fn lookup(&self, name: &[u8]) -> Result<Arc<dyn Inode>> {
        let mut state = self.state.lock();
        let dir = state.dir()?;
        if dir.dead {
            return Err(Errno::ENOENT);
        }
        dir.get(name)
            .map(|entry| Arc::clone(&entry.node) as Arc<dyn Inode>)
            .ok_or(Errno::ENOENT)
    }

    fn create(&self, name: &[u8], node: NewNode<'_>, permissions: u32) -> Result<Arc<dyn Inode>> {
        let now = self.now();
        let kind = node.kind();
        let child = Node::new(&self.shared, self.body_for(node)?, permissions, now);
        let mut state = self.state.lock();
        {
            let dir = state.dir()?;
            if dir.dead {
                return Err(Errno::ENOENT);
            }
            if dir.get(name).is_some() {
                return Err(Errno::EEXIST);
            }
            dir.insert(name, Arc::clone(&child), kind)?;
        }
        if kind == FileType::Directory {
            state.nlink = state.nlink.saturating_add(1);
        }
        state.touch(now);
        Ok(child)
    }

    fn link(&self, name: &[u8], target: &Arc<dyn Inode>) -> Result<()> {
        let target = self.ours(target)?;
        if target.ino == self.ino {
            return Err(Errno::EPERM);
        }
        let mut locked = Locked::new(&[self, &target]);
        let now = self.now();
        let kind = {
            let state = locked.state(target.ino)?;
            if state.is_dir() {
                return Err(Errno::EPERM);
            }
            if state.nlink == 0 {
                return Err(Errno::ENOENT);
            }
            match &state.body {
                Body::File { .. } => FileType::Regular,
                Body::Symlink(_) => FileType::Symlink,
                Body::Special { kind, .. } => *kind,
                Body::Dir(_) => FileType::Directory,
            }
        };
        {
            let state = locked.state(self.ino)?;
            let dir = state.dir()?;
            if dir.dead {
                return Err(Errno::ENOENT);
            }
            if dir.get(name).is_some() {
                return Err(Errno::EEXIST);
            }
            dir.insert(name, Arc::clone(&target), kind)?;
            state.touch(now);
        }
        let state = locked.state(target.ino)?;
        state.nlink = state.nlink.saturating_add(1);
        state.ctime = now;
        Ok(())
    }

    fn unlink(&self, name: &[u8]) -> Result<()> {
        while !self.unlink_once(name)? {}
        Ok(())
    }

    fn rmdir(&self, name: &[u8]) -> Result<()> {
        while !self.rmdir_once(name)? {}
        Ok(())
    }

    fn rename(
        &self,
        old: &[u8],
        new_parent: &Arc<dyn Inode>,
        new: &[u8],
        replace: bool,
    ) -> Result<()> {
        let new_parent = self.ours(new_parent)?;
        while !self.rename_once(old, &new_parent, new, replace)? {}
        Ok(())
    }

    fn read_dir(&self, cursor: u64, emit: &mut dyn FnMut(DirEntry<'_>) -> bool) -> Result<()> {
        let mut state = self.state.lock();
        let dir = state.dir()?;
        for (&at, entry) in dir.by_cursor.range(cursor.max(FIRST_CURSOR)..) {
            let accepted = emit(DirEntry {
                ino: entry.ino,
                kind: entry.kind,
                name: &entry.name,
                next: at.saturating_add(1),
            });
            if !accepted {
                break;
            }
        }
        Ok(())
    }

    fn read_link(&self) -> Result<Vec<u8>> {
        match &self.state.lock().body {
            Body::Symlink(target) => Ok(target.to_vec()),
            _ => Err(Errno::EINVAL),
        }
    }
}
