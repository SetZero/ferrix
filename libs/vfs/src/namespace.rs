//! Mounts, and every operation that takes a path.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use ferrix_linux_abi::errno::Errno;
use ferrix_sync::SpinLock;

use crate::Result;
use crate::dentry::Dentry;
use crate::file::{OpenFile, OpenFlags};
use crate::node::{FileSystem, FileType, Inode, Metadata, NewNode, SetAttributes, StatFs};
use crate::walk::{LastPart, Walked, up};

/// How many dentries the namespace keeps alive that nothing else refers to.
///
/// The cache's whole eviction policy: a queue of this many strong references,
/// oldest dropped first. Sized for a shell and a boot, not for a compiler;
/// stage 16 will want it scaled to memory, which is a change to this number's
/// source and not to the structure.
pub const DEFAULT_CACHE: usize = 4096;

/// A filesystem instance attached to the tree.
pub struct Mount {
    id: u64,
    fs: Arc<dyn FileSystem>,
    root: Arc<Dentry>,
    /// The mount it is on and the dentry it covers; `None` for the root.
    parent: Option<(Arc<Mount>, Arc<Dentry>)>,
}

impl Mount {
    /// Identifier, unique within the namespace.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The filesystem mounted here.
    #[must_use]
    pub fn filesystem(&self) -> &Arc<dyn FileSystem> {
        &self.fs
    }

    /// The dentry of the filesystem's root directory.
    #[must_use]
    pub fn root(&self) -> &Arc<Dentry> {
        &self.root
    }

    /// The mount this one is on, and the dentry it covers.
    #[must_use]
    pub fn parent(&self) -> Option<&(Arc<Mount>, Arc<Dentry>)> {
        self.parent.as_ref()
    }
}

impl fmt::Debug for Mount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mount")
            .field("id", &self.id)
            .field("fs", &self.fs.name())
            .finish_non_exhaustive()
    }
}

/// A place in the tree: a dentry, and the mount it was reached through.
///
/// Both halves, because a dentry alone is ambiguous — the same filesystem can
/// be mounted twice — and `..` from a mount's root depends on which mount.
#[derive(Clone, Debug)]
pub struct Location {
    /// The mount.
    pub mount: Arc<Mount>,
    /// The name within it.
    pub dentry: Arc<Dentry>,
}

impl Location {
    /// What it names.
    ///
    /// # Errors
    ///
    /// `ENOENT` for a negative dentry.
    pub fn inode(&self) -> Result<Arc<dyn Inode>> {
        self.dentry.inode().ok_or(Errno::ENOENT)
    }

    /// Whether two locations are the same place.
    #[must_use]
    pub fn same(&self, other: &Location) -> bool {
        Arc::ptr_eq(&self.mount, &other.mount) && Arc::ptr_eq(&self.dentry, &other.dentry)
    }

    /// `..`, with no root to stop at but the namespace's own.
    #[must_use]
    pub fn parent(&self) -> Location {
        up(self, self)
    }

    /// A place for an object no directory holds: a pipe.
    ///
    /// An open file needs a location -- `fstat` takes the device from its
    /// mount, `fstatfs` the filesystem, `/proc/self/fd` the name -- and `pipe`
    /// makes two open files for an inode that has none. This is one: a mount
    /// of `fs` on nothing, whose root dentry is `inode`, called `name`. It is
    /// in no namespace's table, so no walk can reach it, `..` from it stays
    /// where it is, and nothing can be mounted on it.
    /// [`Namespace::path_of`] reports it as `name` alone, which is how Linux
    /// reports `pipe:[1234]`.
    #[must_use]
    pub fn detached(fs: Arc<dyn FileSystem>, inode: Arc<dyn Inode>, name: &[u8]) -> Location {
        let dentry = Dentry::named_root(Box::from(name), inode);
        let mount = Arc::new(Mount {
            id: DETACHED_MOUNT,
            fs,
            root: Arc::clone(&dentry),
            parent: None,
        });
        Location { mount, dentry }
    }

    /// Whether this is a [`Location::detached`] one.
    #[must_use]
    pub fn is_detached(&self) -> bool {
        self.mount.id == DETACHED_MOUNT
    }
}

/// The mount identifier every [`Location::detached`] mount carries. A
/// namespace numbers its own from one, so no mount in a tree is ever this.
const DETACHED_MOUNT: u64 = 0;

/// The two places a relative and an absolute path start from.
///
/// A process's, in the kernel: `chroot` changes `root` and `chdir` changes
/// `cwd`. The namespace takes them as an argument rather than knowing about
/// processes, which is what lets the host tests be two processes at once.
#[derive(Clone, Debug)]
pub struct Context {
    /// Where `/` is.
    pub root: Location,
    /// Where a relative path starts.
    pub cwd: Location,
}

/// What `stat` reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    /// The device of the filesystem holding it.
    pub dev: u64,
    /// Everything else.
    pub metadata: Metadata,
}

/// Whether a rename may replace an existing name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameMode {
    /// Replace it, as plain `rename` does.
    Replace,
    /// Refuse with `EEXIST`, as `RENAME_NOREPLACE` does.
    NoReplace,
}

/// A mount table with a root.
pub struct Namespace {
    root: Arc<Mount>,
    /// Keyed by the mount a mount point is in and the mount point's dentry.
    mounts: SpinLock<BTreeMap<(u64, u64), Arc<Mount>>>,
    next_mount: AtomicU64,
    /// Held across a rename, so that the ancestry checks it makes are not
    /// invalidated by another rename moving a directory underneath it.
    /// Linux's `s_vfs_rename_mutex`, one per namespace rather than per
    /// filesystem because renames across filesystems are refused anyway.
    rename_lock: SpinLock<()>,
    cache: SpinLock<VecDeque<Arc<Dentry>>>,
    cache_limit: usize,
}

impl fmt::Debug for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Namespace")
            .field("root", &self.root)
            .field("mounts", &self.mounts.lock().len())
            .finish_non_exhaustive()
    }
}

impl Namespace {
    /// A namespace whose root is `fs`.
    #[must_use]
    pub fn new(fs: Arc<dyn FileSystem>) -> Namespace {
        Namespace::with_cache(fs, DEFAULT_CACHE)
    }

    /// As [`Namespace::new`], keeping at most `cache_limit` unused dentries.
    #[must_use]
    pub fn with_cache(fs: Arc<dyn FileSystem>, cache_limit: usize) -> Namespace {
        let root = Arc::new(Mount {
            id: 1,
            root: Dentry::root(fs.root()),
            fs,
            parent: None,
        });
        Namespace {
            root,
            mounts: SpinLock::new(BTreeMap::new()),
            next_mount: AtomicU64::new(2),
            rename_lock: SpinLock::new(()),
            cache: SpinLock::new(VecDeque::new()),
            cache_limit,
        }
    }

    /// The root of the tree.
    #[must_use]
    pub fn root(&self) -> Location {
        Location {
            mount: Arc::clone(&self.root),
            dentry: Arc::clone(&self.root.root),
        }
    }

    /// A context with both root and working directory at the root.
    #[must_use]
    pub fn context(&self) -> Context {
        Context {
            root: self.root(),
            cwd: self.root(),
        }
    }

    /// Every mount, the root first.
    #[must_use]
    pub fn mounts(&self) -> Vec<Arc<Mount>> {
        let mut all = Vec::new();
        all.push(Arc::clone(&self.root));
        all.extend(self.mounts.lock().values().cloned());
        all
    }

    /// How many unused dentries the cache is holding.
    #[must_use]
    pub fn cached(&self) -> usize {
        self.cache.lock().len()
    }

    pub(crate) fn remember(&self, dentry: &Arc<Dentry>) {
        let evicted = {
            let mut cache = self.cache.lock();
            cache.push_back(Arc::clone(dentry));
            if cache.len() > self.cache_limit {
                cache.pop_front()
            } else {
                None
            }
        };
        // Dropped with the lock released: the last reference to a dentry
        // releases its parent, and that chain is unbounded.
        drop(evicted);
    }

    /// Drop the cache's references to a dentry that has left the tree.
    ///
    /// An unlinked file's dentry still names its inode, so a cache entry
    /// keeping that dentry alive would keep the file's contents allocated until
    /// it happened to be evicted: up to the cache's size in deleted files that
    /// nobody can reach. A linear scan of a bounded queue, paid only by the
    /// operations that remove a name. The caller still holds the dentry, so
    /// nothing is freed under the lock.
    fn forget(&self, dentry: &Arc<Dentry>) {
        self.cache
            .lock()
            .retain(|cached| !Arc::ptr_eq(cached, dentry));
    }

    /// As [`Namespace::forget`], for a directory that has just been removed,
    /// and every cached child of it too.
    ///
    /// A removed directory was empty, so its only cached children are misses
    /// -- names somebody looked for inside it. Each holds its parent strongly,
    /// which is what makes `..` work, and so each would keep the removed
    /// directory's dentry, and through it the directory's inode, alive until
    /// the cache got round to evicting it. Linux prunes them on `rmdir` for
    /// the same reason.
    ///
    /// Nothing is freed under the lock: the caller holds the directory, and
    /// the children taken out are dropped after the lock is released.
    fn forget_with_children(&self, directory: &Arc<Dentry>) {
        let released: Vec<Arc<Dentry>> = {
            let mut cache = self.cache.lock();
            let mut released = Vec::new();
            cache.retain(|cached| {
                let gone = Arc::ptr_eq(cached, directory)
                    || cached
                        .parent()
                        .is_some_and(|parent| Arc::ptr_eq(&parent, directory));
                if gone {
                    released.push(Arc::clone(cached));
                }
                !gone
            });
            released
        };
        drop(released);
    }

    pub(crate) fn mounted_on(&self, at: &Location) -> Option<Arc<Mount>> {
        self.mounts
            .lock()
            .get(&(at.mount.id, at.dentry.id()))
            .cloned()
    }

    fn start<'a>(ctx: &'a Context, start: Option<&'a Location>) -> &'a Location {
        start.unwrap_or(&ctx.cwd)
    }

    // -- Reading the tree ---------------------------------------------------

    /// Resolve a path to something that exists.
    ///
    /// # Errors
    ///
    /// As [`Namespace::walk`], and `ENOENT` if the last component is missing.
    pub fn resolve(
        &self,
        ctx: &Context,
        start: Option<&Location>,
        path: &[u8],
        follow: bool,
    ) -> Result<Location> {
        let walked = self.walk(ctx, Self::start(ctx, start), path, follow)?;
        let _ = walked.found.inode()?;
        Ok(walked.found)
    }

    /// `stat` on a location.
    ///
    /// # Errors
    ///
    /// `ENOENT` for a negative dentry.
    pub fn stat(&self, at: &Location) -> Result<Stat> {
        Ok(Stat {
            dev: at.mount.fs.device(),
            metadata: at.inode()?.metadata(),
        })
    }

    /// `statfs` on a location: the filesystem it is on.
    #[must_use]
    pub fn statfs(&self, at: &Location) -> StatFs {
        at.mount.fs.statfs()
    }

    /// A symbolic link's target, without following it.
    ///
    /// # Errors
    ///
    /// `EINVAL` if the last component is not a symbolic link.
    pub fn read_link(
        &self,
        ctx: &Context,
        start: Option<&Location>,
        path: &[u8],
    ) -> Result<Vec<u8>> {
        let at = self.resolve(ctx, start, path, false)?;
        let inode = at.inode()?;
        if inode.metadata().kind != FileType::Symlink {
            return Err(Errno::EINVAL);
        }
        inode.read_link()
    }

    /// The path from `root` to `at`, as `getcwd` and `/proc/self/fd` report it.
    ///
    /// A location outside `root` — reached before a `chroot`, say — is
    /// reported from the namespace's root instead, which is what Linux does
    /// short of prefixing it with `(unreachable)`.
    #[must_use]
    pub fn path_of(&self, at: &Location, root: &Location) -> Vec<u8> {
        if at.is_detached() {
            // Not in any tree, so there is no path to build: the name it was
            // given is the whole answer, and a `/` in front would claim a
            // place it does not have.
            return at.dentry.name().into_vec();
        }
        let mut parts: Vec<Box<[u8]>> = Vec::new();
        let mut here = at.clone();
        loop {
            if here.same(root) {
                break;
            }
            if Arc::ptr_eq(&here.dentry, &here.mount.root) {
                match &here.mount.parent {
                    Some((mount, dentry)) => {
                        here = Location {
                            mount: Arc::clone(mount),
                            dentry: Arc::clone(dentry),
                        };
                        continue;
                    }
                    None => break,
                }
            }
            parts.push(here.dentry.name());
            match here.dentry.parent() {
                Some(parent) => here.dentry = parent,
                None => break,
            }
        }
        if parts.is_empty() {
            return Vec::from(&b"/"[..]);
        }
        let mut path = Vec::new();
        for part in parts.iter().rev() {
            path.push(b'/');
            path.extend_from_slice(part);
        }
        path
    }

    // -- Opening ------------------------------------------------------------

    /// `openat`.
    ///
    /// # Errors
    ///
    /// The ones `open(2)` documents: `ENOENT`, `EEXIST` for `O_CREAT|O_EXCL`
    /// on an existing name, `ELOOP` for `O_NOFOLLOW` on a link, `ENOTDIR` for
    /// `O_DIRECTORY` on something else, `EISDIR` for writing a directory, and
    /// whatever the walk refuses.
    pub fn open(
        &self,
        ctx: &Context,
        start: Option<&Location>,
        path: &[u8],
        flags: &OpenFlags,
        permissions: u32,
    ) -> Result<Arc<OpenFile>> {
        let exclusive_create = flags.create && flags.exclusive;
        let follow = !(flags.nofollow || exclusive_create);
        let walked = self.walk(ctx, Self::start(ctx, start), path, follow)?;

        let Some(inode) = walked.found.dentry.inode() else {
            if !flags.create {
                return Err(Errno::ENOENT);
            }
            if walked.must_be_dir {
                return Err(Errno::EISDIR);
            }
            let _ = self.create_at(&walked, NewNode::Regular, permissions)?;
            return OpenFile::new(walked.found, flags);
        };

        if exclusive_create {
            return Err(Errno::EEXIST);
        }
        let kind = inode.metadata().kind;
        if kind == FileType::Symlink && !flags.path {
            return Err(Errno::ELOOP);
        }
        if flags.directory && kind != FileType::Directory {
            return Err(Errno::ENOTDIR);
        }
        if kind == FileType::Directory && (flags.write || flags.create) && !flags.path {
            return Err(Errno::EISDIR);
        }
        if flags.truncate && flags.write && kind == FileType::Regular {
            inode.set_len(0)?;
        }
        OpenFile::new(walked.found, flags)
    }

    // -- Changing the tree --------------------------------------------------

    /// Create `node` at the negative name a walk found.
    fn create_at(
        &self,
        walked: &Walked,
        node: NewNode<'_>,
        permissions: u32,
    ) -> Result<Arc<dyn Inode>> {
        let name = walked.name_or(Errno::EEXIST)?;
        if walked.found.dentry.inode().is_some() {
            return Err(Errno::EEXIST);
        }
        let dir = walked.parent.inode()?;
        let inode = dir.create(name, node, permissions)?;
        walked
            .parent
            .dentry
            .fill(name, &walked.found.dentry, Arc::clone(&inode));
        Ok(inode)
    }

    /// `mkdirat`.
    ///
    /// # Errors
    ///
    /// `EEXIST` if the name exists, including `.` and `..`.
    pub fn mkdir(
        &self,
        ctx: &Context,
        start: Option<&Location>,
        path: &[u8],
        permissions: u32,
    ) -> Result<()> {
        let walked = self.walk(ctx, Self::start(ctx, start), path, false)?;
        self.create_at(&walked, NewNode::Directory, permissions)
            .map(drop)
    }

    /// `mknodat`, for everything but a directory.
    ///
    /// # Errors
    ///
    /// `EEXIST` if the name exists, `ENOENT` for a path ending in a slash.
    pub fn mknod(
        &self,
        ctx: &Context,
        start: Option<&Location>,
        path: &[u8],
        node: NewNode<'_>,
        permissions: u32,
    ) -> Result<()> {
        let walked = self.walk(ctx, Self::start(ctx, start), path, false)?;
        if walked.must_be_dir && walked.found.dentry.inode().is_none() {
            return Err(Errno::ENOENT);
        }
        self.create_at(&walked, node, permissions).map(drop)
    }

    /// `symlinkat`: make `path` a link to `target`.
    ///
    /// # Errors
    ///
    /// `ENOENT` for an empty target, `EEXIST` if the name exists.
    pub fn symlink(
        &self,
        ctx: &Context,
        start: Option<&Location>,
        path: &[u8],
        target: &[u8],
    ) -> Result<()> {
        if target.is_empty() {
            return Err(Errno::ENOENT);
        }
        self.mknod(ctx, start, path, NewNode::Symlink(target), 0o777)
    }

    /// `linkat`: give what `old` names the further name `new`.
    ///
    /// # Errors
    ///
    /// `EPERM` for a directory, `EXDEV` across mounts, `EEXIST` if `new`
    /// exists.
    pub fn link(
        &self,
        ctx: &Context,
        old: (Option<&Location>, &[u8]),
        follow: bool,
        new: (Option<&Location>, &[u8]),
    ) -> Result<()> {
        let source = self.walk(ctx, Self::start(ctx, old.0), old.1, follow)?;
        let inode = source.found.inode()?;
        if inode.metadata().kind == FileType::Directory {
            return Err(Errno::EPERM);
        }
        let dest = self.walk(ctx, Self::start(ctx, new.0), new.1, false)?;
        let name = dest.name_or(Errno::EEXIST)?;
        if dest.found.dentry.inode().is_some() {
            return Err(Errno::EEXIST);
        }
        if !Arc::ptr_eq(&source.found.mount, &dest.parent.mount) {
            return Err(Errno::EXDEV);
        }
        dest.parent.inode()?.link(name, &inode)?;
        dest.parent.dentry.fill(name, &dest.found.dentry, inode);
        Ok(())
    }

    /// `unlinkat` without `AT_REMOVEDIR`.
    ///
    /// # Errors
    ///
    /// `EISDIR` for a directory, `EBUSY` for a mount point.
    pub fn unlink(&self, ctx: &Context, start: Option<&Location>, path: &[u8]) -> Result<()> {
        let walked = self.walk(ctx, Self::start(ctx, start), path, false)?;
        let name = walked.name_or(Errno::EISDIR)?;
        let inode = walked.found.inode()?;
        if inode.metadata().kind == FileType::Directory {
            return Err(Errno::EISDIR);
        }
        if walked.must_be_dir {
            return Err(Errno::ENOTDIR);
        }
        if walked.is_mountpoint() {
            return Err(Errno::EBUSY);
        }
        walked.parent.inode()?.unlink(name)?;
        walked.parent.dentry.remove_name(name);
        self.forget(&walked.found.dentry);
        Ok(())
    }

    /// `unlinkat` with `AT_REMOVEDIR`.
    ///
    /// # Errors
    ///
    /// `EINVAL` for `.`, `ENOTEMPTY` for `..` or a directory with entries,
    /// `ENOTDIR` for something else, `EBUSY` for a mount point or the root.
    pub fn rmdir(&self, ctx: &Context, start: Option<&Location>, path: &[u8]) -> Result<()> {
        let walked = self.walk(ctx, Self::start(ctx, start), path, false)?;
        let name = match &walked.last {
            LastPart::Name(name) => name,
            LastPart::Dot => return Err(Errno::EINVAL),
            LastPart::DotDot => return Err(Errno::ENOTEMPTY),
            LastPart::Root => return Err(Errno::EBUSY),
        };
        let inode = walked.found.inode()?;
        if inode.metadata().kind != FileType::Directory {
            return Err(Errno::ENOTDIR);
        }
        if walked.is_mountpoint() {
            return Err(Errno::EBUSY);
        }
        walked.parent.inode()?.rmdir(name)?;
        walked.parent.dentry.remove_name(name);
        self.forget_with_children(&walked.found.dentry);
        Ok(())
    }

    /// `renameat2`, without `RENAME_EXCHANGE`.
    ///
    /// # Errors
    ///
    /// `EXDEV` across mounts, `EBUSY` for a mount point or `.`/`..`,
    /// `EINVAL` for moving a directory into itself, `ENOTEMPTY` for replacing
    /// a directory with entries — including an ancestor of the source —
    /// `EEXIST` under [`RenameMode::NoReplace`], and the kind mismatches
    /// `rename(2)` lists.
    pub fn rename(
        &self,
        ctx: &Context,
        old: (Option<&Location>, &[u8]),
        new: (Option<&Location>, &[u8]),
        mode: RenameMode,
    ) -> Result<()> {
        let _serialised = self.rename_lock.lock();
        let source = self.walk(ctx, Self::start(ctx, old.0), old.1, false)?;
        let dest = self.walk(ctx, Self::start(ctx, new.0), new.1, false)?;
        let old_name = source.name_or(Errno::EBUSY)?;
        let new_name = dest.name_or(Errno::EBUSY)?;
        let moving = source.found.inode()?;
        let moving_meta = moving.metadata();
        let moving_dir = moving_meta.kind == FileType::Directory;

        if !Arc::ptr_eq(&source.parent.mount, &dest.parent.mount) {
            return Err(Errno::EXDEV);
        }
        if source.is_mountpoint() || dest.is_mountpoint() {
            return Err(Errno::EBUSY);
        }
        if (source.must_be_dir || dest.must_be_dir) && !moving_dir {
            return Err(Errno::ENOTDIR);
        }
        if let Some(target) = dest.found.dentry.inode() {
            if mode == RenameMode::NoReplace {
                return Err(Errno::EEXIST);
            }
            if target.metadata().ino == moving_meta.ino {
                // Two names for one file: rename(2) says do nothing.
                return Ok(());
            }
            if dest.found.dentry.is_ancestor_of(&source.parent.dentry) {
                return Err(Errno::ENOTEMPTY);
            }
        }
        if moving_dir && source.found.dentry.is_ancestor_of(&dest.parent.dentry) {
            return Err(Errno::EINVAL);
        }

        let new_dir = dest.parent.inode()?;
        source
            .parent
            .inode()?
            .rename(old_name, &new_dir, new_name, mode == RenameMode::Replace)?;
        source.parent.dentry.move_child(
            old_name,
            &source.found.dentry,
            &dest.parent.dentry,
            new_name,
        );
        // Whatever stood at the destination has left the tree: a replaced
        // file, whose pages it would keep, or -- for a rename to a new name --
        // the miss the walk cached, which holds the destination directory.
        self.forget(&dest.found.dentry);
        Ok(())
    }

    /// `chmod`, `chown` and `utimensat`, on a location.
    ///
    /// # Errors
    ///
    /// Whatever the filesystem refuses.
    pub fn set_attributes(&self, at: &Location, change: &SetAttributes) -> Result<()> {
        at.inode()?.set_attributes(change)
    }

    /// `truncate` on a location.
    ///
    /// # Errors
    ///
    /// `EISDIR` for a directory, `EINVAL` for anything else not a regular
    /// file.
    pub fn truncate(&self, at: &Location, len: u64) -> Result<()> {
        let inode = at.inode()?;
        match inode.metadata().kind {
            FileType::Regular => inode.set_len(len),
            FileType::Directory => Err(Errno::EISDIR),
            _ => Err(Errno::EINVAL),
        }
    }

    // -- Mounting -----------------------------------------------------------

    /// Mount `fs` on the directory `at`.
    ///
    /// # Errors
    ///
    /// `ENOTDIR` if `at` is not a directory, `EBUSY` if something is already
    /// mounted exactly there.
    pub fn mount(&self, fs: Arc<dyn FileSystem>, at: &Location) -> Result<Arc<Mount>> {
        if at.inode()?.metadata().kind != FileType::Directory {
            return Err(Errno::ENOTDIR);
        }
        let key = (at.mount.id, at.dentry.id());
        let mut mounts = self.mounts.lock();
        if mounts.contains_key(&key) {
            return Err(Errno::EBUSY);
        }
        let mount = Arc::new(Mount {
            id: self.next_mount.fetch_add(1, Ordering::Relaxed),
            root: Dentry::root(fs.root()),
            fs,
            parent: Some((Arc::clone(&at.mount), Arc::clone(&at.dentry))),
        });
        let _ = mounts.insert(key, Arc::clone(&mount));
        at.dentry.add_mount();
        Ok(mount)
    }

    /// Unmount the filesystem whose root `at` is.
    ///
    /// Lazy, in the sense of `MNT_DETACH`: files already open on it keep
    /// working, and it goes away when the last of them closes.
    ///
    /// # Errors
    ///
    /// `EINVAL` if `at` is not the root of a mount, or is the namespace's
    /// root; `EBUSY` if something is mounted inside it.
    pub fn unmount(&self, at: &Location) -> Result<()> {
        if !Arc::ptr_eq(&at.dentry, &at.mount.root) {
            return Err(Errno::EINVAL);
        }
        let Some((parent, covered)) = &at.mount.parent else {
            return Err(Errno::EINVAL);
        };
        let mut mounts = self.mounts.lock();
        if mounts.keys().any(|&(on, _)| on == at.mount.id) {
            return Err(Errno::EBUSY);
        }
        let _ = mounts.remove(&(parent.id, covered.id()));
        covered.remove_mount();
        Ok(())
    }
}
