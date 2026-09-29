//! Mounts, and every operation that takes a path.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use ferrix_kmem::{Charge, arc_footprint};
use ferrix_linux_abi::errno::Errno;
use ferrix_sync::{Parker, SleepLock, SpinLock};

use crate::Result;
use crate::access::{Access, MAY_READ, MAY_WRITE};
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

/// What one mount allows, apart from what its filesystem does: Linux's
/// per-mount `MNT_*` flags, which `mount(2)` sets from its `MS_*` flags and
/// `MS_REMOUNT` changes.
///
/// Four are enforced where the operation they govern is decided -- see
/// [`Location::require_writable`], [`Mount::no_devices`],
/// [`Mount::no_exec`] and [`Mount::no_set_id`] -- and the three access-time
/// ones are recorded and shown, and change nothing, because no filesystem
/// here keeps an access time apart from the others.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MountFlags(u32);

impl MountFlags {
    /// None: read-write, set-id bits and devices honoured, programs run.
    pub const NONE: MountFlags = MountFlags(0);
    /// `MS_RDONLY`: nothing on it may be changed through this mount.
    pub const READ_ONLY: MountFlags = MountFlags(1);
    /// `MS_NOSUID`: set-user-id and set-group-id bits are ignored.
    pub const NOSUID: MountFlags = MountFlags(1 << 1);
    /// `MS_NODEV`: its device nodes cannot be opened.
    pub const NODEV: MountFlags = MountFlags(1 << 2);
    /// `MS_NOEXEC`: nothing on it runs, or is mapped executable.
    pub const NOEXEC: MountFlags = MountFlags(1 << 3);
    /// `MS_NOATIME`.
    pub const NOATIME: MountFlags = MountFlags(1 << 4);
    /// `MS_NODIRATIME`.
    pub const NODIRATIME: MountFlags = MountFlags(1 << 5);
    /// `MS_RELATIME`.
    pub const RELATIME: MountFlags = MountFlags(1 << 6);
    /// The access-time flags, which a remount that names none keeps.
    pub const ATIME: MountFlags = MountFlags((1 << 4) | (1 << 5) | (1 << 6));
    /// Every flag there is.
    const ALL: u32 = (1 << 7) - 1;

    /// The flags `bits` holds, from [`MountFlags::bits`].
    #[must_use]
    pub const fn from_bits(bits: u32) -> MountFlags {
        MountFlags(bits & Self::ALL)
    }

    /// The flags as a word.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Whether every flag of `other` is set.
    #[must_use]
    pub const fn contains(self, other: MountFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// Both sets.
    #[must_use]
    pub const fn union(self, other: MountFlags) -> MountFlags {
        MountFlags(self.0 | other.0)
    }

    /// These, less `other`'s.
    #[must_use]
    pub const fn without(self, other: MountFlags) -> MountFlags {
        MountFlags(self.0 & !other.0)
    }

    /// `statfs`'s `f_flags` for a mount with these: `ST_VALID` and the
    /// `ST_*` bit of each flag, Linux's `flags_by_mnt`.
    #[must_use]
    pub fn statfs_flags(self) -> u64 {
        use ferrix_linux_abi::types::{
            ST_NOATIME, ST_NODEV, ST_NODIRATIME, ST_NOEXEC, ST_NOSUID, ST_RDONLY, ST_RELATIME,
            ST_VALID,
        };
        [
            (Self::READ_ONLY, ST_RDONLY),
            (Self::NOSUID, ST_NOSUID),
            (Self::NODEV, ST_NODEV),
            (Self::NOEXEC, ST_NOEXEC),
            (Self::NOATIME, ST_NOATIME),
            (Self::NODIRATIME, ST_NODIRATIME),
            (Self::RELATIME, ST_RELATIME),
        ]
        .into_iter()
        .filter(|&(flag, _)| self.contains(flag))
        .fold(ST_VALID, |bits, (_, bit)| bits | bit)
    }

    /// The options `/proc/mounts` and `mountinfo` print for them, in
    /// Linux's order (`show_vfsmnt`, `show_mnt_opts`): `rw` or `ro`, then
    /// the others that are set.
    #[must_use]
    pub fn options(self) -> Vec<u8> {
        let mut out = Vec::from(if self.contains(Self::READ_ONLY) {
            &b"ro"[..]
        } else {
            &b"rw"[..]
        });
        for (flag, name) in [
            (Self::NOSUID, &b",nosuid"[..]),
            (Self::NODEV, b",nodev"),
            (Self::NOEXEC, b",noexec"),
            (Self::NOATIME, b",noatime"),
            (Self::NODIRATIME, b",nodiratime"),
            (Self::RELATIME, b",relatime"),
        ] {
            if self.contains(flag) {
                out.extend_from_slice(name);
            }
        }
        out
    }
}

/// A filesystem instance attached to the tree.
pub struct Mount {
    id: u64,
    /// Its [`MountFlags`], which a remount changes in place: an atomic, so
    /// that every check reads them without a lock and sees one remount
    /// whole.
    flags: AtomicU32,
    fs: Arc<dyn FileSystem>,
    root: Arc<Dentry>,
    /// The mount it is on and the dentry it covers; `None` for the root.
    parent: Option<(Arc<Mount>, Arc<Dentry>)>,
    /// Where a sleeping lock made for something on this mount waits: the
    /// namespace's, so that an open file reaches it through its location
    /// and nothing that opens a file has to be told.
    parker: Arc<dyn Parker>,
    /// The kernel heap it holds, charged to the job that mounted it, or that
    /// made the pipe or socket a detached one is for (F-37).
    _charge: Charge,
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

    /// Where a sleeping lock for something on this mount waits.
    #[must_use]
    pub fn parker(&self) -> &Arc<dyn Parker> {
        &self.parker
    }

    /// Its flags, as the last `mount` or remount left them.
    #[must_use]
    pub fn flags(&self) -> MountFlags {
        MountFlags::from_bits(self.flags.load(Ordering::Acquire))
    }

    /// Whether nothing may be changed through it: `MS_RDONLY`.
    #[must_use]
    pub fn read_only(&self) -> bool {
        self.flags().contains(MountFlags::READ_ONLY)
    }

    /// Whether its device nodes may not be opened: `MS_NODEV`, Linux's
    /// `path_nodev`.
    #[must_use]
    pub fn no_devices(&self) -> bool {
        self.flags().contains(MountFlags::NODEV)
    }

    /// Whether nothing on it may run or be mapped executable: `MS_NOEXEC`,
    /// Linux's `path_noexec`.
    #[must_use]
    pub fn no_exec(&self) -> bool {
        self.flags().contains(MountFlags::NOEXEC)
    }

    /// Whether a program on it runs without its set-id bits: `MS_NOSUID`,
    /// Linux's `mnt_may_suid`.
    #[must_use]
    pub fn no_set_id(&self) -> bool {
        self.flags().contains(MountFlags::NOSUID)
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

    /// `..`, with no root to stop at but the top of the tree: the namespace's
    /// root, or a [`Location::detached`] one, which is its own parent.
    #[must_use]
    pub fn parent(&self) -> Location {
        up(self, None)
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
    /// reports `pipe:[1234]`. `parker` is where the open file's sleeping
    /// lock waits; a stream never takes it, but every description has one.
    ///
    /// # Errors
    ///
    /// `ENOMEM` past the job's memory limit.
    pub fn detached(
        fs: Arc<dyn FileSystem>,
        inode: Arc<dyn Inode>,
        name: &[u8],
        parker: Arc<dyn Parker>,
    ) -> Result<Location> {
        let charge = crate::charge(arc_footprint::<Mount>())?;
        let dentry = Dentry::named_root(Box::from(name), inode)?;
        let mount = Arc::new(Mount {
            id: DETACHED_MOUNT,
            flags: AtomicU32::new(0),
            fs,
            root: Arc::clone(&dentry),
            parent: None,
            parker,
            _charge: charge,
        });
        Ok(Location { mount, dentry })
    }

    /// Whether this is a [`Location::detached`] one.
    #[must_use]
    pub fn is_detached(&self) -> bool {
        self.mount.id == DETACHED_MOUNT
    }

    /// `EROFS` if its mount is read-only: Linux's `mnt_want_write`, which
    /// every change to a file through a path or a descriptor asks first.
    ///
    /// # Errors
    ///
    /// `EROFS`.
    pub fn require_writable(&self) -> Result<()> {
        if self.mount.read_only() {
            Err(Errno::EROFS)
        } else {
            Ok(())
        }
    }

    /// Whether this is the root of its mount, the one place `MS_REMOUNT`
    /// and `umount2` act on.
    #[must_use]
    pub fn is_mount_root(&self) -> bool {
        Arc::ptr_eq(&self.dentry, &self.mount.root)
    }
}

/// The mount identifier every [`Location::detached`] mount carries. A
/// namespace numbers its own from one, so no mount in a tree is ever this.
const DETACHED_MOUNT: u64 = 0;

/// The two places a relative and an absolute path start from, and who is
/// walking.
///
/// A process's, in the kernel: `chroot` changes `root`, `chdir` changes
/// `cwd`, and `who` is its filesystem ids. The namespace takes them as an
/// argument rather than knowing about processes, which is what lets the host
/// tests be two processes -- or two users -- at once.
#[derive(Clone, Debug)]
pub struct Context {
    /// Where `/` is.
    pub root: Location,
    /// Where a relative path starts.
    pub cwd: Location,
    /// The identity every permission check along the way is made against.
    pub who: Access,
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
    ///
    /// A sleeping lock, because it is held across both of the rename's path
    /// walks, and a walk into a filesystem that does I/O -- btrfs -- may wait
    /// for a disk. The kernel lends the wait through the namespace's
    /// [`Parker`]; on the host it spins.
    rename_lock: SleepLock<()>,
    /// Where a lock that sleeps waits: lent by the kernel, and handed on to
    /// every lock this namespace and its open files make.
    parker: Arc<dyn Parker>,
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
    /// A namespace whose root is `fs`, whose sleeping locks wait as `parker`
    /// says: the kernel's wait queues, or [`ferrix_sync::SpinParker`] on the
    /// host.
    #[must_use]
    pub fn new(fs: Arc<dyn FileSystem>, parker: Arc<dyn Parker>) -> Namespace {
        Namespace::with_cache(fs, DEFAULT_CACHE, parker)
    }

    /// As [`Namespace::new`], keeping at most `cache_limit` unused dentries.
    #[must_use]
    pub fn with_cache(
        fs: Arc<dyn FileSystem>,
        cache_limit: usize,
        parker: Arc<dyn Parker>,
    ) -> Namespace {
        // Made before any program runs: nobody to charge, and nothing to
        // refuse.
        let root = Arc::new(Mount {
            id: 1,
            flags: AtomicU32::new(0),
            root: Dentry::uncharged_root(fs.root()),
            fs,
            parent: None,
            parker: Arc::clone(&parker),
            _charge: Charge::none(),
        });
        Namespace {
            root,
            mounts: SpinLock::new(BTreeMap::new()),
            next_mount: AtomicU64::new(2),
            rename_lock: SleepLock::new((), parker.as_ref()),
            cache: SpinLock::new(VecDeque::new()),
            cache_limit,
            parker,
        }
    }

    /// Where this namespace's sleeping locks wait, and every mount's in it.
    #[must_use]
    pub fn parker(&self) -> &Arc<dyn Parker> {
        &self.parker
    }

    /// The root of the tree.
    #[must_use]
    pub fn root(&self) -> Location {
        Location {
            mount: Arc::clone(&self.root),
            dentry: Arc::clone(&self.root.root),
        }
    }

    /// A context with both root and working directory at the root, acting as
    /// root.
    #[must_use]
    pub fn context(&self) -> Context {
        Context {
            root: self.root(),
            cwd: self.root(),
            who: Access::root(),
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
    /// As `Namespace::walk`, and `ENOENT` if the last component is missing.
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

    /// The path from `root` to `at`, or `None` when `at` is not at or below
    /// `root`: what `mountinfo` prints a mount point as, and how it leaves
    /// out a mount the reader's root cannot reach, as Linux's
    /// `seq_path_root` does.
    #[must_use]
    pub fn path_within(&self, at: &Location, root: &Location) -> Option<Vec<u8>> {
        if at.is_detached() {
            return None;
        }
        let mut here = at.clone();
        loop {
            if here.same(root) {
                return Some(self.path_of(at, root));
            }
            let up = up(&here, None);
            if up.same(&here) {
                return None;
            }
            here = up;
        }
    }

    /// The path of `mount`'s root inside its own filesystem: `/` for a
    /// mount of the whole filesystem, which is every mount until a bind
    /// mounts a part of one. `mountinfo`'s fourth field.
    #[must_use]
    pub fn root_path(mount: &Mount) -> Vec<u8> {
        let mut parts: Vec<Box<[u8]>> = Vec::new();
        let mut here = Arc::clone(&mount.root);
        while let Some(parent) = here.parent() {
            parts.push(here.name());
            here = parent;
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
        // How many times a create may lose to another one before `EEXIST` is
        // passed on after all. Linux never loses: it looks the name up and
        // creates it under the directory's lock. Here the walk and the create
        // are separate steps, so two `echo x >> log` on a new file can both
        // find nothing and both create, and the loser must open what the
        // winner made rather than say "File exists" -- which without
        // `O_EXCL` it never may. Each lost race means another create
        // completed, and losing again means the name was removed and made
        // again in the gap. The bound is there so that a name the cache and
        // the filesystem disagree about fails rather than spins.
        const CREATE_RACES: u32 = 8;

        let exclusive_create = flags.create && flags.exclusive;
        let follow = !(flags.nofollow || exclusive_create);
        let mut lost = 0;
        let (walked, inode) = loop {
            let walked = self.walk(ctx, Self::start(ctx, start), path, follow)?;
            if let Some(inode) = walked.found.dentry.inode() {
                break (walked, inode);
            }
            if !flags.create {
                return Err(Errno::ENOENT);
            }
            if walked.must_be_dir {
                return Err(Errno::EISDIR);
            }
            match self.create_at(ctx, &walked, NewNode::Regular, permissions) {
                Ok(dentry) => {
                    let created = Location {
                        mount: walked.found.mount,
                        dentry,
                    };
                    return OpenFile::new(created, flags);
                }
                Err(Errno::EEXIST) if !exclusive_create && lost < CREATE_RACES => lost += 1,
                Err(other) => return Err(other),
            }
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
        // Linux's `may_open`: read for reading, write for writing or
        // truncating. A file this call just created is not checked, and
        // neither is an `O_PATH` handle, which allows no I/O.
        if !flags.path {
            let mut want = 0;
            if flags.read {
                want |= MAY_READ;
            }
            if flags.write || flags.truncate {
                want |= MAY_WRITE;
            }
            ctx.who.require(&inode.metadata(), want)?;
            // Linux's `may_open` and `do_dentry_open`, in that order: a
            // device on a `nodev` mount is `EACCES`, and writing anything
            // but a device, a pipe or a socket through a read-only mount is
            // `EROFS` -- those three are not the filesystem's to change.
            let special = matches!(
                kind,
                FileType::CharDevice | FileType::BlockDevice | FileType::Fifo | FileType::Socket
            );
            if matches!(kind, FileType::CharDevice | FileType::BlockDevice)
                && walked.found.mount.no_devices()
            {
                return Err(Errno::EACCES);
            }
            if (flags.write || flags.truncate) && !special {
                walked.found.require_writable()?;
            }
        }
        if flags.truncate && flags.write && kind == FileType::Regular {
            inode.set_len(0)?;
        }
        OpenFile::new(walked.found, flags)
    }

    // -- Changing the tree --------------------------------------------------

    /// Create `node` at the negative name a walk found, returning the dentry
    /// that now names it.
    ///
    /// The directory must allow the context to write and search it, and the
    /// new object belongs to the context: its filesystem user id, and its
    /// filesystem group id or a set-group-id directory's group. A filesystem
    /// makes what it creates root's, so the owner is given afterwards; one
    /// that keeps no owners refuses, and what it made stays as it made it.
    fn create_at(
        &self,
        ctx: &Context,
        walked: &Walked,
        node: NewNode<'_>,
        permissions: u32,
    ) -> Result<Arc<Dentry>> {
        let name = walked.name_or(Errno::EEXIST)?;
        if walked.found.dentry.inode().is_some() {
            return Err(Errno::EEXIST);
        }
        // A name that exists is `EEXIST` whatever the mount; a new one on a
        // read-only mount is `EROFS` before any permission is asked, as
        // Linux's `filename_create` has it.
        walked.parent.require_writable()?;
        let dir = walked.parent.inode()?;
        let dir_meta = dir.metadata();
        ctx.who.may_create(&dir_meta)?;
        let (uid, gid, permissions) = ctx.who.new_owner(&dir_meta, node.kind(), permissions);
        let inode = dir.create(name, node, permissions)?;
        let made = inode.metadata();
        if made.uid != uid || made.gid != gid {
            let owner = SetAttributes {
                uid: Some(uid),
                gid: Some(gid),
                ..SetAttributes::default()
            };
            let _ = inode.set_attributes(&owner);
        }
        Ok(walked.parent.dentry.fill(name, &walked.found.dentry, inode))
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
        self.create_at(ctx, &walked, NewNode::Directory, permissions)
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
        self.mknod_at(ctx, start, path, node, permissions).map(drop)
    }

    /// `mknodat`, answering where the new node is.
    ///
    /// [`Namespace::mknod`] is this and a `drop`. A caller that has to use
    /// what it made -- binding an `AF_UNIX` socket to a name, which keys its
    /// table by the node the bind created -- needs that node, and looking the
    /// path up again afterwards would find whatever is there by then rather
    /// than what this call made.
    ///
    /// # Errors
    ///
    /// As [`Namespace::mknod`].
    pub fn mknod_at(
        &self,
        ctx: &Context,
        start: Option<&Location>,
        path: &[u8],
        node: NewNode<'_>,
        permissions: u32,
    ) -> Result<Location> {
        let walked = self.walk(ctx, Self::start(ctx, start), path, false)?;
        if walked.must_be_dir && walked.found.dentry.inode().is_none() {
            return Err(Errno::ENOENT);
        }
        let dentry = self.create_at(ctx, &walked, node, permissions)?;
        Ok(Location {
            mount: Arc::clone(&walked.found.mount),
            dentry,
        })
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
        dest.parent.require_writable()?;
        if !Arc::ptr_eq(&source.found.mount, &dest.parent.mount) {
            return Err(Errno::EXDEV);
        }
        let dir = dest.parent.inode()?;
        ctx.who.may_create(&dir.metadata())?;
        dir.link(name, &inode)?;
        let _ = dest.parent.dentry.fill(name, &dest.found.dentry, inode);
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
        walked.parent.require_writable()?;
        let inode = walked.found.inode()?;
        ctx.who
            .may_delete(&walked.parent.inode()?.metadata(), &inode.metadata())?;
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
        walked.parent.require_writable()?;
        let inode = walked.found.inode()?;
        ctx.who
            .may_delete(&walked.parent.inode()?.metadata(), &inode.metadata())?;
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
        // `do_renameat2`'s order: across mounts is `EXDEV`, and a read-only
        // mount `EROFS`, before either name is looked at.
        if !Arc::ptr_eq(&source.parent.mount, &dest.parent.mount) {
            return Err(Errno::EXDEV);
        }
        source.parent.require_writable()?;
        let moving = source.found.inode()?;
        let moving_meta = moving.metadata();
        let moving_dir = moving_meta.kind == FileType::Directory;

        if source.is_mountpoint() || dest.is_mountpoint() {
            return Err(Errno::EBUSY);
        }
        if (source.must_be_dir || dest.must_be_dir) && !moving_dir {
            return Err(Errno::ENOTDIR);
        }
        let mut replacing_dir = false;
        if let Some(target) = dest.found.dentry.inode() {
            if mode == RenameMode::NoReplace {
                return Err(Errno::EEXIST);
            }
            let target_meta = target.metadata();
            if target_meta.ino == moving_meta.ino {
                // Two names for one file: rename(2) says do nothing.
                return Ok(());
            }
            if dest.found.dentry.is_ancestor_of(&source.parent.dentry) {
                return Err(Errno::ENOTEMPTY);
            }
            replacing_dir = target_meta.kind == FileType::Directory;
        }
        if moving_dir && source.found.dentry.is_ancestor_of(&dest.parent.dentry) {
            return Err(Errno::EINVAL);
        }

        // Linux's `vfs_rename`: the source may be deleted from its directory,
        // the destination made or replaced in its own, and a directory moving
        // to a new parent must be writable itself, because its `..` changes.
        let who = &ctx.who;
        who.may_delete(&source.parent.inode()?.metadata(), &moving_meta)?;
        match dest.found.dentry.inode() {
            Some(target) => who.may_delete(&dest.parent.inode()?.metadata(), &target.metadata())?,
            None => who.may_create(&dest.parent.inode()?.metadata())?,
        }
        if moving_dir && !Arc::ptr_eq(&source.parent.dentry, &dest.parent.dentry) {
            who.require(&moving_meta, MAY_WRITE)?;
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
        // the miss the walk cached, which holds the destination directory. A
        // replaced directory was empty, and goes as `rmdir` sends one: with
        // the misses cached inside it, each of which holds it.
        if replacing_dir {
            self.forget_with_children(&dest.found.dentry);
        } else {
            self.forget(&dest.found.dentry);
        }
        Ok(())
    }

    /// `chmod`, `chown` and `utimensat`, on a location.
    ///
    /// # Errors
    ///
    /// Whatever the filesystem refuses.
    pub fn set_attributes(&self, at: &Location, change: &SetAttributes) -> Result<()> {
        at.require_writable()?;
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
            FileType::Regular => {
                at.require_writable()?;
                inode.set_len(len)
            }
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
    /// mounted exactly there, `ENOMEM` past the job's memory limit.
    pub fn mount(&self, fs: Arc<dyn FileSystem>, at: &Location) -> Result<Arc<Mount>> {
        self.mount_with(fs, at, MountFlags::NONE)
    }

    /// Mount `fs` on the directory `at` with `flags`.
    ///
    /// # Errors
    ///
    /// As [`Namespace::mount`].
    pub fn mount_with(
        &self,
        fs: Arc<dyn FileSystem>,
        at: &Location,
        flags: MountFlags,
    ) -> Result<Arc<Mount>> {
        if at.inode()?.metadata().kind != FileType::Directory {
            return Err(Errno::ENOTDIR);
        }
        let key = (at.mount.id, at.dentry.id());
        // Charged before the table is locked: a refusal drops `fs`, which
        // may be its last reference.
        let charge = crate::charge(arc_footprint::<Mount>())?;
        let root = Dentry::root(fs.root())?;
        let mut mounts = self.mounts.lock();
        if mounts.contains_key(&key) {
            return Err(Errno::EBUSY);
        }
        let mount = Arc::new(Mount {
            id: self.next_mount.fetch_add(1, Ordering::Relaxed),
            flags: AtomicU32::new(flags.bits()),
            root,
            fs,
            parent: Some((Arc::clone(&at.mount), Arc::clone(&at.dentry))),
            parker: Arc::clone(&self.parker),
            _charge: charge,
        });
        let _ = mounts.insert(key, Arc::clone(&mount));
        at.dentry.add_mount();
        Ok(mount)
    }

    /// Give the mount whose root `at` is the flags `flags`: `MS_REMOUNT`.
    /// The flags replace the old ones whole; which to keep is the caller's
    /// to decide, as `mount(2)`'s rules say.
    ///
    /// # Errors
    ///
    /// `EINVAL` if `at` is not the root of a mount.
    pub fn remount(&self, at: &Location, flags: MountFlags) -> Result<()> {
        if !at.is_mount_root() {
            return Err(Errno::EINVAL);
        }
        at.mount.flags.store(flags.bits(), Ordering::Release);
        Ok(())
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
        {
            let mut mounts = self.mounts.lock();
            if mounts.keys().any(|&(on, _)| on == at.mount.id) {
                return Err(Errno::EBUSY);
            }
            let _ = mounts.remove(&(parent.id, covered.id()));
        }
        covered.remove_mount();
        // Outside the mount table's lock: the cache takes its own.
        self.forget_tree(&at.mount.root);
        Ok(())
    }

    /// Drop the cache's references to every dentry of a tree that has just
    /// been unmounted: its root and anything whose parents lead back to it.
    ///
    /// Nothing can reach that tree by name any more, but a cached dentry holds
    /// its inode and, through its parents, every directory up to the root —
    /// so without this an unmounted filesystem stayed allocated, a dentry at a
    /// time, until the cache got round to evicting each one. A procfs or a
    /// devtmpfs mounted and unmounted again left one behind per cycle. Linux
    /// prunes a mount's dentries when it is unmounted for the same reason.
    ///
    /// A walk up each cached dentry's parents, paid only by unmount. Nothing is
    /// freed under the lock: the entries taken out are dropped after it is
    /// released, since the last reference to one releases its parent chain.
    fn forget_tree(&self, root: &Arc<Dentry>) {
        let released: Vec<Arc<Dentry>> = {
            let mut cache = self.cache.lock();
            let mut released = Vec::new();
            cache.retain(|cached| {
                let gone = root.is_ancestor_of(cached);
                if gone {
                    released.push(Arc::clone(cached));
                }
                !gone
            });
            released
        };
        drop(released);
    }
}
