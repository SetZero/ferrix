//! Names, as opposed to the things they name.
//!
//! An inode does not know what it is called. A hard link means it is called
//! several things; a rename means what it is called changes while it stays
//! the same object; and a mount point means a name in one filesystem leads to
//! the root of another. The dentry is where all three are recorded, which is
//! why `..`, `getcwd` and `/proc/self/fd` are answered from dentries and never
//! from inodes.
//!
//! # Negative entries
//!
//! A dentry with no inode records that a name does *not* exist. A compiler
//! probes dozens of include directories for every header and misses in most
//! of them, so the miss is the common case and is worth caching as much as
//! the hit — `docs/ARCHITECTURE.md` §8 names this as a performance
//! requirement. It is also what `open(O_CREAT)` fills in: the walk finds the
//! negative dentry, the filesystem creates the file, and the dentry becomes
//! positive in place.
//!
//! # Ownership
//!
//! A child holds its parent strongly, so `..` always works from anything a
//! program holds. A parent holds its children *weakly*, so the tree does not
//! keep itself alive: a dentry nobody refers to goes away. What keeps recently
//! used ones is the namespace's cache, a bounded queue of strong references,
//! which is the whole of the eviction policy and is replaceable without
//! touching anything here.
//!
//! # The generation, and the race it closes
//!
//! A lookup that misses the cache asks the filesystem with no lock held, then
//! inserts what it learned. A create that completes in between would have its
//! result overwritten by a stale negative entry, and the file would be
//! invisible until the entry was evicted. So every change to a directory's
//! names bumps its generation under the children lock, and a lookup inserts
//! only if the generation it read before asking is still current. A lookup
//! that loses the race still returns a correct answer for itself; it just
//! does not cache it.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use core::fmt;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use ferrix_sync::SpinLock;

use crate::node::Inode;

/// Source of dentry identifiers, which the mount table keys on.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// One name in the tree.
pub struct Dentry {
    /// Unique for the life of the system, so the mount table can name a mount
    /// point without holding a pointer's address as a key.
    id: u64,
    /// The mutable part: what it is called, where, and what it names.
    state: SpinLock<State>,
    /// Children looked up so far, weakly.
    children: SpinLock<BTreeMap<Box<[u8]>, Weak<Dentry>>>,
    /// Bumped by every change to this directory's names; see the module
    /// documentation.
    generation: AtomicU64,
    /// How many mounts sit on this dentry. Checked on every step of every
    /// walk, so it is a counter rather than a question for the mount table.
    mounts: AtomicU32,
}

/// The parts of a dentry a rename or an unlink changes.
struct State {
    name: Box<[u8]>,
    parent: Option<Arc<Dentry>>,
    inode: Option<Arc<dyn Inode>>,
    /// Removed from its parent: unlinked, or replaced by a rename. Whoever
    /// still holds it keeps the inode, but no walk will find it again.
    unhashed: bool,
}

impl Dentry {
    /// A root: no parent, and an inode.
    pub(crate) fn root(inode: Arc<dyn Inode>) -> Arc<Dentry> {
        Dentry::new(Box::from(&b"/"[..]), None, Some(inode))
    }

    fn new(
        name: Box<[u8]>,
        parent: Option<Arc<Dentry>>,
        inode: Option<Arc<dyn Inode>>,
    ) -> Arc<Dentry> {
        Arc::new(Dentry {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            state: SpinLock::new(State {
                name,
                parent,
                inode,
                unhashed: false,
            }),
            children: SpinLock::new(BTreeMap::new()),
            generation: AtomicU64::new(0),
            mounts: AtomicU32::new(0),
        })
    }

    /// Unique identifier.
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// What this dentry is called in its parent.
    #[must_use]
    pub fn name(&self) -> Box<[u8]> {
        self.state.lock().name.clone()
    }

    /// The directory holding it, or `None` for a filesystem's root.
    #[must_use]
    pub fn parent(&self) -> Option<Arc<Dentry>> {
        self.state.lock().parent.clone()
    }

    /// What it names, or `None` if the name does not exist.
    #[must_use]
    pub fn inode(&self) -> Option<Arc<dyn Inode>> {
        self.state.lock().inode.clone()
    }

    /// Whether it has been removed from the tree.
    #[must_use]
    pub fn is_unhashed(&self) -> bool {
        self.state.lock().unhashed
    }

    /// Whether anything is mounted on it.
    pub(crate) fn is_mountpoint(&self) -> bool {
        self.mounts.load(Ordering::Relaxed) != 0
    }

    pub(crate) fn add_mount(&self) {
        let _ = self.mounts.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn remove_mount(&self) {
        let _ = self.mounts.fetch_sub(1, Ordering::Relaxed);
    }

    /// The cached child called `name`, if one is alive.
    pub(crate) fn cached_child(&self, name: &[u8]) -> Option<Arc<Dentry>> {
        self.children.lock().get(name).and_then(Weak::upgrade)
    }

    /// The generation a lookup must see unchanged to cache its answer.
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Record what a filesystem lookup found, if nothing changed meanwhile.
    ///
    /// Returns the dentry the caller should use: one that raced in ahead of it
    /// if there is one, otherwise a new one — cached if `generation` is still
    /// current and private to this walk if not.
    pub(crate) fn insert_looked_up(
        self: &Arc<Self>,
        name: &[u8],
        inode: Option<Arc<dyn Inode>>,
        generation: u64,
    ) -> (Arc<Dentry>, bool) {
        let mut children = self.children.lock();
        if let Some(existing) = children.get(name).and_then(Weak::upgrade) {
            return (existing, false);
        }
        let child = Dentry::new(Box::from(name), Some(Arc::clone(self)), inode);
        if self.generation() != generation {
            return (child, false);
        }
        children.retain(|_, weak| weak.strong_count() > 0);
        let _ = children.insert(Box::from(name), Arc::downgrade(&child));
        (child, true)
    }

    /// A name was created in this directory: make its dentry positive.
    ///
    /// `walked` is the (negative) dentry the caller's walk found. If a cached
    /// one exists under the name it is updated instead, so every holder sees
    /// the new inode.
    pub(crate) fn fill(self: &Arc<Self>, name: &[u8], walked: &Arc<Dentry>, inode: Arc<dyn Inode>) {
        let mut children = self.children.lock();
        let target = children
            .get(name)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| Arc::clone(walked));
        {
            let mut state = target.state.lock();
            state.inode = Some(Arc::clone(&inode));
            state.unhashed = false;
        }
        if !Arc::ptr_eq(&target, walked) {
            walked.state.lock().inode = Some(inode);
        }
        let _ = children.insert(Box::from(name), Arc::downgrade(&target));
        let _ = self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// A name was removed from this directory: unhash whatever held it.
    pub(crate) fn remove_name(&self, name: &[u8]) {
        let mut children = self.children.lock();
        if let Some(child) = children.remove(name).and_then(|weak| weak.upgrade()) {
            child.state.lock().unhashed = true;
        }
        let _ = self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Move `child` from this directory to `new_parent` under `new_name`.
    ///
    /// The caller holds the namespace's rename lock, so no other move is in
    /// progress, and has already made the filesystem agree.
    pub(crate) fn move_child(
        self: &Arc<Self>,
        old_name: &[u8],
        child: &Arc<Dentry>,
        new_parent: &Arc<Dentry>,
        new_name: &[u8],
    ) {
        self.remove_name(old_name);
        new_parent.remove_name(new_name);
        {
            let mut state = child.state.lock();
            state.name = Box::from(new_name);
            state.parent = Some(Arc::clone(new_parent));
            state.unhashed = false;
        }
        let mut children = new_parent.children.lock();
        let _ = children.insert(Box::from(new_name), Arc::downgrade(child));
        let _ = new_parent.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Whether `self` is `other` or one of its ancestors, within one
    /// filesystem.
    pub(crate) fn is_ancestor_of(self: &Arc<Self>, other: &Arc<Dentry>) -> bool {
        let mut at = Some(Arc::clone(other));
        while let Some(dentry) = at {
            if Arc::ptr_eq(self, &dentry) {
                return true;
            }
            at = dentry.parent();
        }
        false
    }
}

impl fmt::Debug for Dentry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock();
        f.debug_struct("Dentry")
            .field("id", &self.id)
            .field("name", &alloc::string::String::from_utf8_lossy(&state.name))
            .field("positive", &state.inode.is_some())
            .field("unhashed", &state.unhashed)
            .finish_non_exhaustive()
    }
}
