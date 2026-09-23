//! `search.h`: a hash table (`hsearch`), a balanced binary tree (`tsearch`),
//! linear search (`lsearch`) and doubly linked queues (`insque`).
//!
//! The designs are musl 1.2.5's (MIT): `src/search/hsearch.c`, `tsearch.c`,
//! `tdelete.c`, `tfind.c`, `twalk.c`, `tdestroy.c`, `lsearch.c` and
//! `insque.c`.
//!
//! # The hash table
//!
//! Open addressing over a power-of-two table, probing quadratically, and
//! growing to twice the number of entries once it is three quarters full. The
//! table never shrinks and entries cannot be removed, as POSIX's interface
//! allows. A failed growth leaves the table usable.
//!
//! `struct hsearch_data` in musl's header is a pointer and two `unsigned int`s,
//! 16 bytes, which is also the size of glibc's. The pointer names this
//! library's own table, so a structure can only be passed through these
//! calls, not shared with glibc's code. `hcreate` refuses to replace a table
//! that exists, as glibc does; musl leaks it instead.
//!
//! # The tree
//!
//! An AVL tree. Each node is a [`Node`] whose first member is the key pointer,
//! because callers read a node returned by `tsearch`, `tfind` or `twalk` as a
//! pointer to the key. Its height stays below `1.44 * log2(n + 2)`, which is
//! what bounds [`Path`]. `tdelete` returns the removed node's parent, or, for
//! the root, the root pointer's address, as musl does.

use core::ffi::{c_char, c_int, c_uint, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::errno;
use crate::malloc::{calloc, free, malloc};
use crate::string::{memcpy, strcmp};

/// A comparison function from the program.
type Compare = Option<unsafe extern "C" fn(*const c_void, *const c_void) -> c_int>;

// ---------------------------------------------------------------------------
// hsearch
// ---------------------------------------------------------------------------

/// `ENTRY`: a key string and its data.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    /// The key, a NUL-terminated string. Null marks an empty slot.
    pub key: *mut c_char,
    /// The program's data.
    pub data: *mut c_void,
}

/// `struct hsearch_data`, the state of a reentrant table.
#[repr(C)]
#[derive(Debug)]
pub struct HsearchData {
    /// The table, or null before `hcreate_r`.
    tab: *mut Table,
    /// Unused, as in musl.
    unused1: c_uint,
    /// Unused, as in musl.
    unused2: c_uint,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Entry>() == 16);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<HsearchData>() == 16);

/// `FIND`, the `ACTION` that only looks.
const FIND: c_int = 0;

/// A hash table.
#[derive(Debug)]
struct Table {
    /// `mask + 1` slots.
    entries: *mut Entry,
    /// The slot count less one; the count is a power of two.
    mask: usize,
    /// How many slots hold an entry.
    used: usize,
}

/// The smallest table.
const MIN_SIZE: usize = 8;
/// The largest table, the largest power of two a `size_t` holds.
const MAX_SIZE: usize = (usize::MAX >> 1) + 1;

/// The table behind `hcreate`, `hsearch` and `hdestroy`.
static GLOBAL: AtomicPtr<Table> = AtomicPtr::new(null_mut());

/// The hash of `key`.
///
/// # Safety
///
/// `key` must be a NUL-terminated string.
unsafe fn key_hash(key: *const c_char) -> usize {
    let mut h: usize = 0;
    let mut p = key;
    loop {
        // SAFETY: the string is NUL-terminated and the loop stops at its end.
        let byte = unsafe { p.read() } as u8;
        if byte == 0 {
            return h;
        }
        h = h.wrapping_mul(31).wrapping_add(usize::from(byte));
        p = p.wrapping_add(1);
    }
}

/// The slot holding `key`, or the empty slot where it would go.
///
/// # Safety
///
/// `table` must be a valid table with at least one empty slot, and `key` a
/// NUL-terminated string.
unsafe fn lookup(table: &Table, key: *const c_char, hash: usize) -> *mut Entry {
    let mut i = hash;
    let mut step = 1usize;
    loop {
        let e = table.entries.wrapping_add(i & table.mask);
        // SAFETY: `i & mask` is a slot of the table.
        let existing = unsafe { (*e).key };
        // SAFETY: a slot's key is null or a string.
        if existing.is_null() || unsafe { strcmp(existing, key) } == 0 {
            return e;
        }
        i = i.wrapping_add(step);
        step = step.wrapping_add(1);
    }
}

/// Moves `table` to a new array of at least `wanted` slots. Returns `false`,
/// leaving the table as it was, if there is no memory.
///
/// # Safety
///
/// `table.entries` must be null or a valid array of `mask + 1` slots from
/// `malloc`.
unsafe fn resize(table: &mut Table, wanted: usize) -> bool {
    let wanted = wanted.min(MAX_SIZE);
    let mut size = MIN_SIZE;
    while size < wanted {
        size <<= 1;
    }
    let entries = calloc(size, size_of::<Entry>()).cast::<Entry>();
    if entries.is_null() {
        errno::set(errno::ENOMEM);
        return false;
    }
    let old = table.entries;
    let old_size = table.mask.wrapping_add(1);
    table.entries = entries;
    table.mask = size - 1;
    if old.is_null() {
        return true;
    }
    let mut i = 0;
    while i < old_size {
        // SAFETY: `i` is a slot of the old array.
        let entry = unsafe { old.wrapping_add(i).read() };
        if !entry.key.is_null() {
            // SAFETY: the key is a string.
            let hash = unsafe { key_hash(entry.key) };
            // SAFETY: the new table has more slots than entries.
            let slot = unsafe { lookup(table, entry.key, hash) };
            // SAFETY: `slot` is a slot of the new array.
            unsafe { slot.write(entry) };
        }
        i += 1;
    }
    // SAFETY: the old array came from `calloc` and nothing refers to it now.
    unsafe { free(old.cast()) };
    true
}

/// Creates a table for about `nel` entries in `htab`. Returns nonzero on
/// success, and zero with `errno` set otherwise.
///
/// # Safety
///
/// `htab` must be null or point to a writable `struct hsearch_data`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn hcreate_r(nel: usize, htab: *mut HsearchData) -> c_int {
    if htab.is_null() {
        errno::set(errno::EINVAL);
        return 0;
    }
    let table = calloc(1, size_of::<Table>()).cast::<Table>();
    if table.is_null() {
        return 0;
    }
    // SAFETY: `calloc` returned a zeroed `Table`, which is valid with null
    // entries, and nothing else refers to it.
    let table_ref = unsafe { &mut *table };
    // SAFETY: the entries are null.
    if !unsafe { resize(table_ref, nel) } {
        // SAFETY: the table came from `calloc` and was never shared.
        unsafe { free(table.cast()) };
        return 0;
    }
    // SAFETY: the caller passes a writable structure.
    unsafe { (*htab).tab = table };
    1
}

/// Frees the table in `htab`, but not the keys or data in it.
///
/// # Safety
///
/// `htab` must be null or point to a `struct hsearch_data` that is zeroed or
/// was given to [`hcreate_r`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn hdestroy_r(htab: *mut HsearchData) {
    if htab.is_null() {
        return;
    }
    // SAFETY: the caller passes a valid structure.
    let table = unsafe { (*htab).tab };
    if !table.is_null() {
        // SAFETY: a table in the structure is valid.
        let entries = unsafe { (*table).entries };
        // SAFETY: the entries and the table came from `calloc`.
        unsafe { free(entries.cast()) };
        // SAFETY: as above.
        unsafe { free(table.cast()) };
    }
    // SAFETY: as above.
    unsafe { (*htab).tab = null_mut() };
}

/// Looks `item.key` up in `table`, adding `item` if `action` is `ENTER` and
/// the key is not there. Returns the entry, or null.
///
/// # Safety
///
/// `table` must be null or valid, and `item.key` a NUL-terminated string.
unsafe fn search(table: *mut Table, item: Entry, action: c_int) -> *mut Entry {
    if table.is_null() || item.key.is_null() {
        errno::set(errno::ESRCH);
        return null_mut();
    }
    // SAFETY: the caller passes a valid table, which only this call uses.
    let table = unsafe { &mut *table };
    // SAFETY: the key is a string.
    let hash = unsafe { key_hash(item.key) };
    // SAFETY: the table always keeps an empty slot.
    let e = unsafe { lookup(table, item.key, hash) };
    // SAFETY: `e` is a slot of the table.
    if !unsafe { (*e).key }.is_null() {
        return e;
    }
    if action == FIND {
        errno::set(errno::ESRCH);
        return null_mut();
    }
    // SAFETY: as above.
    unsafe { e.write(item) };
    table.used += 1;
    if table.used > table.mask - table.mask / 4 {
        // SAFETY: the table's entries are valid.
        if !unsafe { resize(table, table.used.saturating_mul(2)) } {
            table.used -= 1;
            // SAFETY: the failed resize left the old array, and `e`, in place.
            unsafe { (*e).key = null_mut() };
            return null_mut();
        }
        // SAFETY: the grown table has empty slots, and holds the key.
        return unsafe { lookup(table, item.key, hash) };
    }
    e
}

/// Looks `item.key` up in `htab`, adding `item` if `action` is `ENTER`.
/// Stores the entry, or null, in `*retval`, and returns nonzero if there is
/// one.
///
/// # Safety
///
/// `htab` must hold a table from [`hcreate_r`], `item.key` must be a
/// NUL-terminated string that outlives the table, and `retval` writable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn hsearch_r(
    item: Entry,
    action: c_int,
    retval: *mut *mut Entry,
    htab: *mut HsearchData,
) -> c_int {
    let table = if htab.is_null() {
        null_mut()
    } else {
        // SAFETY: the caller passes a valid structure.
        unsafe { (*htab).tab }
    };
    // SAFETY: the caller's contract covers the table and the key.
    let e = unsafe { search(table, item, action) };
    if !retval.is_null() {
        // SAFETY: the caller passes a writable pointer.
        unsafe { retval.write(e) };
    }
    c_int::from(!e.is_null())
}

/// Creates the process's table for about `nel` entries. Returns nonzero on
/// success, and zero if there is no memory or a table already exists.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn hcreate(nel: usize) -> c_int {
    if !GLOBAL.load(Ordering::Relaxed).is_null() {
        return 0;
    }
    let mut data = HsearchData {
        tab: null_mut(),
        unused1: 0,
        unused2: 0,
    };
    // SAFETY: `data` is a valid structure.
    let ok = unsafe { hcreate_r(nel, &raw mut data) };
    GLOBAL.store(data.tab, Ordering::Relaxed);
    ok
}

/// Frees the process's table.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn hdestroy() {
    let mut data = HsearchData {
        tab: GLOBAL.swap(null_mut(), Ordering::Relaxed),
        unused1: 0,
        unused2: 0,
    };
    // SAFETY: the table, if any, came from `hcreate_r`.
    unsafe { hdestroy_r(&raw mut data) };
}

/// Looks `item.key` up in the process's table, adding `item` if `action` is
/// `ENTER`. Returns the entry, or null.
///
/// # Safety
///
/// `item.key` must be a NUL-terminated string that outlives the table.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn hsearch(item: Entry, action: c_int) -> *mut Entry {
    // SAFETY: the table is null or came from `hcreate`, and the caller passes
    // a string key.
    unsafe { search(GLOBAL.load(Ordering::Relaxed), item, action) }
}

// ---------------------------------------------------------------------------
// tsearch
// ---------------------------------------------------------------------------

/// A tree node. The key comes first: callers read a node as `*(void **)`.
#[repr(C)]
#[derive(Debug)]
struct Node {
    /// The program's key.
    key: *const c_void,
    /// The left and right children.
    child: [*mut Node; 2],
    /// The height of the subtree rooted here; a leaf's is 1.
    height: c_int,
}

/// A bound on an AVL tree's height with as many nodes as memory can hold, and
/// so on the length of a path from the root.
const MAX_HEIGHT: usize = usize::BITS as usize * 3 / 2;

/// A root-to-node path of child slots, as `tsearch` and `tdelete` record it.
struct Path {
    /// The slots; the first `len` are set.
    slots: [*mut *mut Node; MAX_HEIGHT + 3],
    /// How many are set.
    len: usize,
}

impl Path {
    /// An empty path.
    const fn new() -> Self {
        Self {
            slots: [null_mut(); MAX_HEIGHT + 3],
            len: 0,
        }
    }

    /// Appends `slot`. Returns `false` if the path is impossibly long, which
    /// only a corrupt tree makes it.
    fn push(&mut self, slot: *mut *mut Node) -> bool {
        let Some(entry) = self.slots.get_mut(self.len) else {
            return false;
        };
        *entry = slot;
        self.len += 1;
        true
    }

    /// Slot `i`, or null.
    fn get(&self, i: usize) -> *mut *mut Node {
        self.slots.get(i).copied().unwrap_or(null_mut())
    }

    /// Rebalances slots `from` down to `lowest`, stopping once a subtree's
    /// height is unchanged, since nothing above it can then change.
    ///
    /// # Safety
    ///
    /// Each of those slots must hold a node whose subtrees are balanced once
    /// the slots below it are.
    unsafe fn rebalance(&self, from: usize, lowest: usize) {
        let mut i = from;
        while i >= lowest {
            let slot = self.get(i);
            // SAFETY: the caller vouches for the slot, and `balance` accepts
            // no null.
            if slot.is_null() || unsafe { balance(slot) } == 0 || i == 0 {
                return;
            }
            i -= 1;
        }
    }
}

/// `node`'s height, 0 for null.
///
/// # Safety
///
/// `node` must be null or a valid node.
unsafe fn height(node: *mut Node) -> c_int {
    if node.is_null() {
        0
    } else {
        // SAFETY: the caller passes a valid node.
        unsafe { (*node).height }
    }
}

/// Child `side` (0 or 1) of `node`.
///
/// # Safety
///
/// `node` must be a valid node.
unsafe fn child(node: *mut Node, side: usize) -> *mut Node {
    // SAFETY: the caller passes a valid node.
    let children = unsafe { (*node).child };
    children.get(side).copied().unwrap_or(null_mut())
}

/// The key of `node`.
///
/// # Safety
///
/// `node` must be a valid node.
unsafe fn key_of(node: *mut Node) -> *const c_void {
    // SAFETY: the caller passes a valid node.
    unsafe { (*node).key }
}

/// Where child `side` (0 or 1) of `node` is stored.
fn child_slot(node: *mut Node, side: usize) -> *mut *mut Node {
    node.wrapping_byte_add(offset_of!(Node, child))
        .cast::<*mut Node>()
        .wrapping_add(side & 1)
}

/// Sets child `side` of `node`.
///
/// # Safety
///
/// `node` must be a valid node.
unsafe fn set_child(node: *mut Node, side: usize, value: *mut Node) {
    // SAFETY: the caller passes a valid node, and `side & 1` is 0 or 1.
    unsafe { child_slot(node, side).write(value) };
}

/// Sets `node`'s height.
///
/// # Safety
///
/// `node` must be a valid node.
unsafe fn set_height(node: *mut Node, h: c_int) {
    // SAFETY: the caller passes a valid node.
    unsafe { (*node).height = h };
}

/// Rotates the subtree at `*slot`, rooted at `x`, whose `dir` side is two
/// deeper than the other. Returns how much the subtree's height changed.
///
/// # Safety
///
/// `slot` must hold `x`, a valid node whose `dir` subtree is at least 2 high.
unsafe fn rotate(slot: *mut *mut Node, x: *mut Node, dir: usize) -> c_int {
    // Every node reached below is a child of a valid node, so valid or null,
    // and each is known not to be null from the heights before it is used.
    let other = 1 - dir;
    // SAFETY: the caller passes a valid `x`.
    let y = unsafe { child(x, dir) };
    // SAFETY: `y` is not null, since its subtree is at least 2 high.
    let z = unsafe { child(y, other) };
    // SAFETY: as above.
    let y_dir = unsafe { child(y, dir) };
    // SAFETY: `x` is valid.
    let hx = unsafe { height(x) };
    // SAFETY: `z` is valid or null.
    let hz = unsafe { height(z) };
    // SAFETY: `y_dir` is valid or null.
    let new_root = if hz > unsafe { height(y_dir) } {
        // Double rotation: `z`, which is not null, becomes the root.
        // SAFETY: as above.
        let z_other = unsafe { child(z, other) };
        // SAFETY: as above.
        let z_dir = unsafe { child(z, dir) };
        // SAFETY: `x`, `y` and `z` are valid.
        unsafe { set_child(x, dir, z_other) };
        // SAFETY: as above.
        unsafe { set_child(y, other, z_dir) };
        // SAFETY: as above.
        unsafe { set_child(z, other, x) };
        // SAFETY: as above.
        unsafe { set_child(z, dir, y) };
        // SAFETY: as above.
        unsafe { set_height(x, hz) };
        // SAFETY: as above.
        unsafe { set_height(y, hz) };
        // SAFETY: as above.
        unsafe { set_height(z, hz + 1) };
        z
    } else {
        // Single rotation: `y` becomes the root.
        // SAFETY: `x` and `y` are valid.
        unsafe { set_child(x, dir, z) };
        // SAFETY: as above.
        unsafe { set_child(y, other, x) };
        // SAFETY: as above.
        unsafe { set_height(x, hz + 1) };
        // SAFETY: as above.
        unsafe { set_height(y, hz + 2) };
        y
    };
    // SAFETY: the caller passes a writable slot.
    unsafe { slot.write(new_root) };
    // SAFETY: `new_root` is valid.
    let h = unsafe { height(new_root) };
    h - hx
}

/// Rebalances the subtree at `*slot`. Returns zero if its height did not
/// change.
///
/// # Safety
///
/// `slot` must hold a valid node whose subtrees are balanced.
unsafe fn balance(slot: *mut *mut Node) -> c_int {
    // SAFETY: the caller passes a valid slot.
    let n = unsafe { slot.read() };
    // SAFETY: it holds a valid node.
    let left = unsafe { child(n, 0) };
    // SAFETY: as above.
    let right = unsafe { child(n, 1) };
    // SAFETY: children of a valid node are valid or null.
    let h0 = unsafe { height(left) };
    // SAFETY: as above.
    let h1 = unsafe { height(right) };
    if (h0 - h1).abs() < 2 {
        // SAFETY: `n` is valid.
        let old = unsafe { height(n) };
        let new = h0.max(h1) + 1;
        // SAFETY: as above.
        unsafe { set_height(n, new) };
        return new - old;
    }
    // SAFETY: the deeper side is at least 2 high.
    unsafe { rotate(slot, n, usize::from(h0 < h1)) }
}

/// The side a comparison result sends a search down.
fn side(c: c_int) -> usize {
    usize::from(c > 0)
}

/// Finds `key` in the tree at `*rootp`, adding it if it is not there. Returns
/// the node, which points to the key, or null if a node could not be
/// allocated.
///
/// # Safety
///
/// `rootp` must be null or point to the root of a tree built by these
/// functions, and `compar` must accept the keys.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn tsearch(
    key: *const c_void,
    rootp: *mut *mut c_void,
    compar: Compare,
) -> *mut c_void {
    let Some(compar) = compar else {
        return null_mut();
    };
    if rootp.is_null() {
        return null_mut();
    }
    let mut path = Path::new();
    let mut slot = rootp.cast::<*mut Node>();
    loop {
        if !path.push(slot) {
            return null_mut();
        }
        // SAFETY: `slot` is the root pointer or a child slot of a valid node.
        let n = unsafe { slot.read() };
        if n.is_null() {
            break;
        }
        // SAFETY: `n` is valid.
        let n_key = unsafe { key_of(n) };
        // SAFETY: the program's comparison accepts its keys.
        let c = unsafe { compar(key, n_key) };
        if c == 0 {
            return n.cast();
        }
        slot = child_slot(n, side(c));
    }
    let r = malloc(size_of::<Node>()).cast::<Node>();
    if r.is_null() {
        return null_mut();
    }
    // SAFETY: `r` is a new allocation of a node's size.
    unsafe {
        r.write(Node {
            key,
            child: [null_mut(); 2],
            height: 1,
        });
    }
    // SAFETY: `slot` is the empty slot the search ended at.
    unsafe { slot.write(r) };
    // The new node's own slot is the last; its ancestors' slots are the ones
    // before, down to the root pointer at 0.
    if let Some(from) = path.len.checked_sub(2) {
        // SAFETY: those slots hold the new node's ancestors.
        unsafe { path.rebalance(from, 0) };
    }
    r.cast()
}

/// Finds `key` in the tree at `*rootp`. Returns its node, or null.
///
/// # Safety
///
/// As [`tsearch`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn tfind(
    key: *const c_void,
    rootp: *const *mut c_void,
    compar: Compare,
) -> *mut c_void {
    let Some(compar) = compar else {
        return null_mut();
    };
    if rootp.is_null() {
        return null_mut();
    }
    // SAFETY: the caller passes a valid root pointer.
    let mut n = unsafe { rootp.read() }.cast::<Node>();
    while !n.is_null() {
        // SAFETY: `n` is a valid node.
        let n_key = unsafe { key_of(n) };
        // SAFETY: the comparison accepts the keys.
        let c = unsafe { compar(key, n_key) };
        if c == 0 {
            break;
        }
        // SAFETY: as above.
        n = unsafe { child(n, side(c)) };
    }
    n.cast()
}

/// Removes `key` from the tree at `*rootp`. Returns the removed node's
/// parent, the root pointer's address if it was the root, or null if the key
/// was not there.
///
/// # Safety
///
/// As [`tsearch`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn tdelete(
    key: *const c_void,
    rootp: *mut *mut c_void,
    compar: Compare,
) -> *mut c_void {
    let Some(compar) = compar else {
        return null_mut();
    };
    if rootp.is_null() {
        return null_mut();
    }
    let root_slot = rootp.cast::<*mut Node>();
    // Slot 0 stands for the root's parent: it holds the root pointer's
    // address, which is what deleting the root returns.
    let mut path = Path::new();
    if !path.push(root_slot) || !path.push(root_slot) {
        return null_mut();
    }
    // SAFETY: the caller passes a valid root pointer.
    let mut n = unsafe { root_slot.read() };
    loop {
        if n.is_null() {
            return null_mut();
        }
        // SAFETY: `n` is a valid node.
        let n_key = unsafe { key_of(n) };
        // SAFETY: the comparison accepts the keys.
        let c = unsafe { compar(key, n_key) };
        if c == 0 {
            break;
        }
        let slot = child_slot(n, side(c));
        if !path.push(slot) {
            return null_mut();
        }
        // SAFETY: `slot` is a child slot of a valid node.
        n = unsafe { slot.read() };
    }
    let parent: *mut c_void = if path.len == 2 {
        rootp.cast()
    } else {
        // SAFETY: the slot before the node's own holds its parent.
        unsafe { path.get(path.len - 2).read() }.cast()
    };
    // SAFETY: `n` is a valid node.
    let left = unsafe { child(n, 0) };
    let replacement = if left.is_null() {
        // SAFETY: as above.
        unsafe { child(n, 1) }
    } else {
        // Remove the in-order predecessor instead, moving its key here.
        let deleted = n;
        if !path.push(child_slot(n, 0)) {
            return null_mut();
        }
        n = left;
        loop {
            // SAFETY: `n` is a valid node.
            let right = unsafe { child(n, 1) };
            if right.is_null() {
                break;
            }
            if !path.push(child_slot(n, 1)) {
                return null_mut();
            }
            n = right;
        }
        // SAFETY: `n` is valid.
        let moved = unsafe { key_of(n) };
        // SAFETY: `deleted` is valid.
        unsafe { (*deleted).key = moved };
        // SAFETY: as above.
        unsafe { child(n, 0) }
    };
    // SAFETY: `n` came from `malloc`, and its slot is overwritten below.
    unsafe { free(n.cast()) };
    let own = path.len - 1;
    // SAFETY: the last slot held `n`, which had at most this one child.
    unsafe { path.get(own).write(replacement) };
    if own >= 2 {
        // SAFETY: slots 1 to `own - 1` hold the ancestors of the removed node.
        unsafe { path.rebalance(own - 1, 1) };
    }
    parent
}

/// A `twalk` callback.
type Action = Option<unsafe extern "C" fn(*const c_void, c_int, c_int)>;

/// `preorder`: before a node's children.
const PREORDER: c_int = 0;
/// `postorder`: between a node's children.
const POSTORDER: c_int = 1;
/// `endorder`: after a node's children.
const ENDORDER: c_int = 2;
/// `leaf`: a node without children.
const LEAF: c_int = 3;

/// Visits the subtree at `node`, `depth` below the root.
///
/// # Safety
///
/// `node` must be null or a valid node, and `action` must accept the nodes.
unsafe fn walk(
    node: *mut Node,
    action: unsafe extern "C" fn(*const c_void, c_int, c_int),
    depth: c_int,
) {
    if node.is_null() {
        return;
    }
    // SAFETY: `node` is valid.
    if unsafe { height(node) } == 1 {
        // SAFETY: the program's callback accepts a node.
        unsafe { action(node.cast(), LEAF, depth) };
        return;
    }
    // SAFETY: as above.
    let left = unsafe { child(node, 0) };
    // SAFETY: as above.
    let right = unsafe { child(node, 1) };
    // SAFETY: the callback accepts a node, and the children are valid or null.
    unsafe { action(node.cast(), PREORDER, depth) };
    // SAFETY: as above.
    unsafe { walk(left, action, depth + 1) };
    // SAFETY: as above.
    unsafe { action(node.cast(), POSTORDER, depth) };
    // SAFETY: as above.
    unsafe { walk(right, action, depth + 1) };
    // SAFETY: as above.
    unsafe { action(node.cast(), ENDORDER, depth) };
}

/// Calls `action` for each node of the tree at `root`: once for a leaf, and
/// before, between and after the children of any other node.
///
/// # Safety
///
/// `root` must be null or a tree built by these functions, and `action` must
/// not change the tree.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn twalk(root: *const c_void, action: Action) {
    if let Some(action) = action {
        // SAFETY: the caller passes a valid tree and callback.
        unsafe { walk(root.cast_mut().cast(), action, 0) };
    }
}

/// Frees every node of the tree at `root`, calling `freekey` on each key
/// first if it is not null.
///
/// # Safety
///
/// `root` must be null or a tree built by these functions, not used again.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn tdestroy(
    root: *mut c_void,
    freekey: Option<unsafe extern "C" fn(*mut c_void)>,
) {
    let node = root.cast::<Node>();
    if node.is_null() {
        return;
    }
    // SAFETY: `node` is valid.
    let left = unsafe { child(node, 0) };
    // SAFETY: as above.
    let right = unsafe { child(node, 1) };
    // SAFETY: the children are valid or null. The recursion is only as deep
    // as the balanced tree.
    unsafe { tdestroy(left.cast(), freekey) };
    // SAFETY: as above.
    unsafe { tdestroy(right.cast(), freekey) };
    if let Some(freekey) = freekey {
        // SAFETY: `node` is valid.
        let key = unsafe { key_of(node) };
        // SAFETY: the program's function accepts its keys.
        unsafe { freekey(key.cast_mut()) };
    }
    // SAFETY: the node came from `malloc` and is not used again.
    unsafe { free(node.cast()) };
}

// ---------------------------------------------------------------------------
// lsearch
// ---------------------------------------------------------------------------

/// The first of `*nelp` elements of `width` bytes at `base` that `compar`
/// finds equal to `key`, or null.
///
/// # Safety
///
/// `base` must hold `*nelp` elements of `width` bytes, and `nelp` be valid.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn lfind(
    key: *const c_void,
    base: *const c_void,
    nelp: *mut usize,
    width: usize,
    compar: Compare,
) -> *mut c_void {
    let Some(compar) = compar else {
        return null_mut();
    };
    // SAFETY: the caller passes a valid count.
    let n = unsafe { nelp.read() };
    let mut i = 0;
    while i < n {
        let element = base.wrapping_byte_add(i.wrapping_mul(width));
        // SAFETY: element `i` is in the array, and the comparison accepts it.
        if unsafe { compar(key, element) } == 0 {
            return element.cast_mut();
        }
        i += 1;
    }
    null_mut()
}

/// As [`lfind`], but appends `key` and increments `*nelp` if it is not found.
///
/// # Safety
///
/// As [`lfind`], and `base` must have room for one more element.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn lsearch(
    key: *const c_void,
    base: *mut c_void,
    nelp: *mut usize,
    width: usize,
    compar: Compare,
) -> *mut c_void {
    // SAFETY: the same contract.
    let found = unsafe { lfind(key, base, nelp, width, compar) };
    if !found.is_null() || compar.is_none() {
        return found;
    }
    // SAFETY: the caller passes a valid count.
    let n = unsafe { nelp.read() };
    // SAFETY: as above.
    unsafe { nelp.write(n.wrapping_add(1)) };
    let end = base.wrapping_byte_add(n.wrapping_mul(width));
    // SAFETY: the array has room for element `n`, which the key does not
    // overlap.
    unsafe { memcpy(end, key, width) }
}

// ---------------------------------------------------------------------------
// insque
// ---------------------------------------------------------------------------

/// The links at the start of every queue element.
#[repr(C)]
#[derive(Debug)]
struct Link {
    /// The next element, or null.
    next: *mut Link,
    /// The previous element, or null.
    prev: *mut Link,
}

/// Inserts `element` after `pred` in a queue. A null `pred` starts a new queue
/// of one element.
///
/// # Safety
///
/// Both must be null or begin with two writable link pointers, and `pred`'s
/// neighbours must be valid.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn insque(element: *mut c_void, pred: *mut c_void) {
    let e = element.cast::<Link>();
    let p = pred.cast::<Link>();
    if p.is_null() {
        // SAFETY: the caller passes a writable element.
        unsafe {
            e.write(Link {
                next: null_mut(),
                prev: null_mut(),
            });
        }
        return;
    }
    // SAFETY: the caller passes valid elements.
    let next = unsafe { (*p).next };
    // SAFETY: as above.
    unsafe { e.write(Link { next, prev: p }) };
    // SAFETY: as above.
    unsafe { (*p).next = e };
    if !next.is_null() {
        // SAFETY: the queue's elements are valid.
        unsafe { (*next).prev = e };
    }
}

/// Removes `element` from its queue.
///
/// # Safety
///
/// `element` must be in a queue of valid elements.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn remque(element: *mut c_void) {
    let e = element.cast::<Link>();
    // SAFETY: the caller passes a valid element.
    let Link { next, prev } = unsafe { e.read() };
    if !next.is_null() {
        // SAFETY: the queue's elements are valid.
        unsafe { (*next).prev = prev };
    }
    if !prev.is_null() {
        // SAFETY: as above.
        unsafe { (*prev).next = next };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn compare_ints(a: *const c_void, b: *const c_void) -> c_int {
        // SAFETY: the tests pass pointers to `i32`s.
        let x = unsafe { a.cast::<i32>().read() };
        // SAFETY: as above.
        let y = unsafe { b.cast::<i32>().read() };
        x.cmp(&y) as c_int
    }

    /// Checks the AVL invariants below `n`, and returns its height.
    fn check(n: *mut Node) -> c_int {
        if n.is_null() {
            return 0;
        }
        // SAFETY: the tree is valid.
        let l = unsafe { child(n, 0) };
        // SAFETY: as above.
        let r = unsafe { child(n, 1) };
        // SAFETY: as above.
        let h = unsafe { height(n) };
        let (hl, hr) = (check(l), check(r));
        assert!((hl - hr).abs() < 2, "unbalanced");
        assert_eq!(h, hl.max(hr) + 1, "stale height");
        h
    }

    #[test]
    fn the_tree_stays_balanced_through_inserts_and_deletes() {
        let keys: Vec<i32> = (0..2000).map(|i| (i * 7919) % 2000).collect();
        let mut root: *mut c_void = null_mut();
        for k in &keys {
            let key: *const i32 = k;
            // SAFETY: the keys outlive the tree, and the comparison fits them.
            let node = unsafe { tsearch(key.cast(), &raw mut root, Some(compare_ints)) };
            assert!(!node.is_null());
        }
        assert!(check(root.cast()) <= 16);
        for k in keys.iter().step_by(2) {
            let key: *const i32 = k;
            // SAFETY: as above.
            let parent = unsafe { tdelete(key.cast(), &raw mut root, Some(compare_ints)) };
            assert!(!parent.is_null());
            let _ = check(root.cast());
        }
        for (i, k) in keys.iter().enumerate() {
            let key: *const i32 = k;
            // SAFETY: as above.
            let found = unsafe { tfind(key.cast(), &raw const root, Some(compare_ints)) };
            assert_eq!(found.is_null(), i % 2 == 0);
        }
        // SAFETY: the tree was built by `tsearch` and is not used again.
        unsafe { tdestroy(root, None) };
    }

    #[test]
    fn the_hash_table_grows_and_keeps_its_entries() {
        let mut data = HsearchData {
            tab: null_mut(),
            unused1: 0,
            unused2: 0,
        };
        // SAFETY: `data` is a valid structure.
        assert_eq!(unsafe { hcreate_r(1, &raw mut data) }, 1);
        let keys: Vec<std::ffi::CString> = (0..500)
            .map(|i| std::ffi::CString::new(format!("key{i}")).unwrap_or_default())
            .collect();
        for (i, k) in keys.iter().enumerate() {
            let item = Entry {
                key: k.as_ptr().cast_mut(),
                data: core::ptr::without_provenance_mut(i),
            };
            let mut out = null_mut();
            // SAFETY: the keys outlive the table.
            let found = unsafe { hsearch_r(item, 1, &raw mut out, &raw mut data) };
            assert_eq!(found, 1);
        }
        for (i, k) in keys.iter().enumerate() {
            let item = Entry {
                key: k.as_ptr().cast_mut(),
                data: null_mut(),
            };
            let mut out = null_mut();
            // SAFETY: as above.
            let found = unsafe { hsearch_r(item, FIND, &raw mut out, &raw mut data) };
            assert_eq!(found, 1);
            // SAFETY: a found entry is a slot of the table.
            assert_eq!(unsafe { (*out).data }.addr(), i);
        }
        // SAFETY: the table came from `hcreate_r`.
        unsafe { hdestroy_r(&raw mut data) };
    }
}
