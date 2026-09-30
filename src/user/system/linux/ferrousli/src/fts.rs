//! `fts.h`, BSD's file hierarchy walk, as glibc exports it: `fts_open`,
//! `fts_read`, `fts_set` and `fts_close`, with their `fts64_` names.
//! libselinux walks trees with them. musl has no `fts`.
//!
//! The structures are glibc's, whose fields the caller reads: `FTS` and
//! `FTSENT`, with `fts_statp` pointing at a `struct stat`, the kernel's on
//! both 64-bit architectures. ARMv7-A's glibc hands out its own 32-bit
//! `time_t` structure there, which this library does not have, so `fts` is
//! not built for it.
//!
//! The walk is written from the interface BSD documents, not from glibc's
//! or BSD's source. A directory is returned before its entries as `FTS_D`
//! and after them as `FTS_DP`, or as `FTS_DNR` after `FTS_D` if it cannot
//! be read; `fts_set` can ask for an entry `FTS_AGAIN`, `FTS_FOLLOW` or
//! `FTS_SKIP`. Two things are simpler than BSD's, and neither changes what
//! a caller that uses the entries' paths sees:
//!
//! * The walk never changes the working directory, as if `FTS_NOCHDIR`
//!   were always given: `fts_accpath` is the whole path, which names the
//!   file from anywhere.
//! * `FTS_NOSTAT` is accepted and every entry is `stat`ed anyway, so an
//!   entry is never `FTS_NSOK`, which BSD allows but does not promise.
//!
//! An entry is valid until `fts_read` passes its directory's `FTS_DP`, or
//! `fts_close`: each directory's entries are kept until then.

use core::ffi::{c_char, c_int, c_long, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;

use crate::dirent::{closedir, opendir, readdir};
use crate::errno;
use crate::growable::{Growable, sort_by};
use crate::malloc::{free, malloc};
use crate::stat::{Stat, lstat, stat};
use crate::string::{memcpy, strlen};

/// Follow symbolic links named on the command line.
const FTS_COMFOLLOW: c_int = 0x1;
/// Follow every symbolic link.
const FTS_LOGICAL: c_int = 0x2;
/// Return `.` and `..`.
const FTS_SEEDOT: c_int = 0x20;
/// Do not descend into another file system.
const FTS_XDEV: c_int = 0x40;
/// Every option `fts_open` takes.
const FTS_OPTIONMASK: c_int = 0xff;

/// A directory, before its entries.
const FTS_D: u16 = 1;
/// A directory that is its own ancestor.
const FTS_DC: u16 = 2;
/// Anything that is not a directory, a regular file or a link.
const FTS_DEFAULT: u16 = 3;
/// A directory that cannot be read.
const FTS_DNR: u16 = 4;
/// `.` or `..`.
const FTS_DOT: u16 = 5;
/// A directory, after its entries.
const FTS_DP: u16 = 6;
/// A regular file.
const FTS_F: u16 = 8;
/// A file that cannot be `stat`ed.
const FTS_NS: u16 = 10;
/// A symbolic link.
const FTS_SL: u16 = 12;
/// A symbolic link to nothing.
const FTS_SLNONE: u16 = 13;

/// `fts_set`: return the entry again.
const FTS_AGAIN: c_int = 1;
/// `fts_set`: follow the symbolic link.
const FTS_FOLLOW: c_int = 2;
/// `fts_set`: no instruction.
const FTS_NOINSTR: c_int = 3;
/// `fts_set`: do not descend into the directory.
const FTS_SKIP: c_int = 4;

/// `nlink_t`: a `long` on x86-64, an `int` on AArch64.
#[cfg(target_arch = "x86_64")]
type Nlink = u64;
/// See above.
#[cfg(not(target_arch = "x86_64"))]
type Nlink = u32;

/// glibc's `FTSENT`.
#[repr(C)]
#[derive(Debug)]
pub struct Ftsent {
    /// The ancestor an `FTS_DC` directory repeats.
    pub fts_cycle: *mut Ftsent,
    /// The directory this entry is in.
    pub fts_parent: *mut Ftsent,
    /// The next entry in the same directory.
    pub fts_link: *mut Ftsent,
    /// The caller's number.
    pub fts_number: c_long,
    /// The caller's pointer.
    pub fts_pointer: *mut c_void,
    /// The path to reach the file by: the whole path here.
    pub fts_accpath: *mut c_char,
    /// The path from the starting point.
    pub fts_path: *mut c_char,
    /// The error, for `FTS_DNR`, `FTS_ERR` and `FTS_NS`.
    pub fts_errno: c_int,
    /// Unused here.
    pub fts_symfd: c_int,
    /// `strlen(fts_path)`.
    pub fts_pathlen: u16,
    /// `strlen(fts_name)`.
    pub fts_namelen: u16,
    /// The inode.
    pub fts_ino: u64,
    /// The device.
    pub fts_dev: u64,
    /// The links.
    pub fts_nlink: Nlink,
    /// The depth: -1 for the root's parent, 0 for the starting points.
    pub fts_level: i16,
    /// What the entry is: an `FTS_*` value.
    pub fts_info: u16,
    /// Private flags.
    pub fts_flags: u16,
    /// The caller's `fts_set` instruction.
    pub fts_instr: u16,
    /// The file's status.
    pub fts_statp: *mut Stat,
    /// The file's name, as long as `fts_namelen`, then a NUL.
    pub fts_name: [c_char; 1],
}

#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(Ftsent, fts_level) == 96);
const _: () = assert!(offset_of!(Ftsent, fts_statp) == 104);
const _: () = assert!(offset_of!(Ftsent, fts_name) == 112);

/// An entry, with what the walk keeps beside it. `entry` is last, so its
/// name runs on past the structure, followed by the path.
#[repr(C)]
struct Node {
    /// The first of this directory's entries, while it is being walked.
    children: *mut Ftsent,
    /// `fts_statp` points here.
    status: Stat,
    /// What the caller sees.
    entry: Ftsent,
}

/// `fts_open`'s comparison.
pub type Compare = unsafe extern "C" fn(*const *const Ftsent, *const *const Ftsent) -> c_int;

/// glibc's `FTS`.
#[repr(C)]
#[derive(Debug)]
pub struct Fts {
    /// The entry `fts_read` last returned.
    pub fts_cur: *mut Ftsent,
    /// Unused here.
    pub fts_child: *mut Ftsent,
    /// Unused here.
    pub fts_array: *mut *mut Ftsent,
    /// The device of the starting point being walked.
    pub fts_dev: u64,
    /// Unused here.
    pub fts_path: *mut c_char,
    /// Unused here.
    pub fts_rfd: c_int,
    /// Unused here.
    pub fts_pathlen: c_int,
    /// Unused here.
    pub fts_nitems: c_int,
    /// The comparison, or null.
    pub fts_compar: Option<Compare>,
    /// The options.
    pub fts_options: c_int,
}

/// A walk: glibc's `FTS`, then the walk's own state.
#[repr(C)]
struct Stream {
    /// What the caller sees.
    fts: Fts,
    /// The parent of the starting points, level -1, whose children they are.
    root: *mut Ftsent,
    /// Whether the walk has returned its first entry.
    started: bool,
}

/// The node an entry is in.
fn node_of(entry: *mut Ftsent) -> *mut Node {
    entry.wrapping_byte_sub(offset_of!(Node, entry)).cast()
}

/// A new entry named by the `name_len` bytes at `name`, whose path is the
/// `path_len` bytes at `path`, at `level` below `parent`. Null, with
/// `errno` set, if there is no memory or the name is too long.
///
/// # Safety
///
/// Both must be readable for their lengths.
unsafe fn new_entry(
    name: *const c_char,
    name_len: usize,
    path: *const c_char,
    path_len: usize,
    parent: *mut Ftsent,
    level: i16,
) -> *mut Ftsent {
    let (Ok(name_len16), Ok(path_len16)) = (u16::try_from(name_len), u16::try_from(path_len))
    else {
        errno::set(errno::ENAMETOOLONG);
        return null_mut();
    };
    let size = size_of::<Node>() + name_len + 1 + path_len + 1;
    let node = malloc(size).cast::<Node>();
    if node.is_null() {
        return null_mut();
    }
    // SAFETY: fresh memory for the node, its name and its path.
    unsafe { node.write_bytes(0, 1) };
    // SAFETY: as above; the entry is inside the node.
    let entry = unsafe { &raw mut (*node).entry };
    let name_at = entry
        .wrapping_byte_add(offset_of!(Ftsent, fts_name))
        .cast::<c_char>();
    let path_at = name_at.wrapping_add(name_len + 1);
    // SAFETY: the allocation holds the name and its NUL after the header.
    let _ = unsafe { memcpy(name_at.cast(), name.cast(), name_len) };
    // SAFETY: as above.
    unsafe { name_at.wrapping_add(name_len).write(0) };
    // SAFETY: and the path and its NUL after them.
    let _ = unsafe { memcpy(path_at.cast(), path.cast(), path_len) };
    // SAFETY: as above.
    unsafe { path_at.wrapping_add(path_len).write(0) };
    // SAFETY: the entry is this call's, set up field by field.
    let e = unsafe { &mut *entry };
    e.fts_parent = parent;
    e.fts_accpath = path_at;
    e.fts_path = path_at;
    e.fts_pathlen = path_len16;
    e.fts_namelen = name_len16;
    e.fts_level = level;
    e.fts_instr = FTS_NOINSTR as u16;
    // SAFETY: the status is inside the node.
    e.fts_statp = unsafe { &raw mut (*node).status };
    entry
}

/// Whether the `len` bytes at `name` are `.` or `..`.
///
/// # Safety
///
/// `name` must be readable for `len` bytes.
unsafe fn is_dots(name: *const c_char, len: usize) -> bool {
    if len == 0 || len > 2 {
        return false;
    }
    for i in 0..len {
        // SAFETY: `i` is below `len`.
        if unsafe { name.wrapping_add(i).read() } != b'.' as c_char {
            return false;
        }
    }
    true
}

/// Frees `list` and everything each of its entries still holds.
///
/// # Safety
///
/// `list` must be null or a list of entries from [`new_entry`] that
/// nothing uses again.
unsafe fn free_list(list: *mut Ftsent) {
    let mut entry = list;
    while !entry.is_null() {
        let node = node_of(entry);
        // SAFETY: a live node of this walk.
        let next = unsafe { (*entry).fts_link };
        // SAFETY: as above.
        let children = unsafe { (*node).children };
        // SAFETY: as above.
        unsafe { free_list(children) };
        // SAFETY: from `malloc`, not used again.
        unsafe { free(node.cast()) };
        entry = next;
    }
}

/// `stat`s `entry`, following a link if `follow`, and says what it is.
///
/// # Safety
///
/// `entry` must be a live entry of this walk.
unsafe fn classify(entry: *mut Ftsent, follow: bool, seedot: bool) -> u16 {
    // SAFETY: the caller's live entry.
    let e = unsafe { &mut *entry };
    let path = e.fts_path;
    let status = e.fts_statp;
    e.fts_errno = 0;
    let ret = if follow {
        // SAFETY: the path is NUL-terminated, and the status is the node's.
        unsafe { stat(path, status) }
    } else {
        // SAFETY: as above.
        unsafe { lstat(path, status) }
    };
    if ret < 0 {
        let error = errno::get();
        // SAFETY: as above.
        if follow && error == errno::ENOENT && unsafe { lstat(path, status) } == 0 {
            errno::set(0);
            return FTS_SLNONE;
        }
        e.fts_errno = error;
        // SAFETY: the status is the node's.
        unsafe { status.write(Stat::default()) };
        return FTS_NS;
    }
    // SAFETY: filled just now.
    let st = unsafe { status.read() };
    e.fts_ino = st.st_ino;
    e.fts_dev = st.st_dev;
    e.fts_nlink = st.st_nlink as Nlink;
    match st.st_mode & 0o170_000 {
        0o040_000 => {
            let name_len = usize::from(e.fts_namelen);
            let name = e.fts_name.as_ptr();
            // SAFETY: the name is `name_len` bytes and a NUL.
            let dot = e.fts_level > 0 && unsafe { is_dots(name, name_len) };
            if dot && seedot {
                return FTS_DOT;
            }
            let mut ancestor = e.fts_parent;
            // SAFETY: each ancestor is a live entry, up to the root's parent.
            while !ancestor.is_null() && unsafe { (*ancestor).fts_level } >= 0 {
                // SAFETY: as above.
                let a = unsafe { &*ancestor };
                if a.fts_dev == e.fts_dev && a.fts_ino == e.fts_ino {
                    e.fts_cycle = ancestor;
                    return FTS_DC;
                }
                ancestor = a.fts_parent;
            }
            FTS_D
        }
        0o120_000 => FTS_SL,
        0o100_000 => FTS_F,
        _ => FTS_DEFAULT,
    }
}

/// Sorts the list `list` with `compar`, if there is one, and returns its
/// new head.
///
/// # Safety
///
/// `list` must be a list of live entries.
unsafe fn sort(list: *mut Ftsent, compar: Option<Compare>) -> *mut Ftsent {
    let Some(compar) = compar else {
        return list;
    };
    let mut entries: Growable<*mut Ftsent> = Growable::new();
    let mut entry = list;
    while !entry.is_null() {
        if !entries.push(entry) {
            return list;
        }
        // SAFETY: a live entry.
        entry = unsafe { (*entry).fts_link };
    }
    sort_by(entries.as_mut_slice(), |a, b| {
        let (a, b) = (a.cast_const(), b.cast_const());
        // SAFETY: the program's comparison takes pointers to entries.
        unsafe { compar(&raw const a, &raw const b) }.cmp(&0)
    });
    let mut next = null_mut();
    for &entry in entries.as_slice().iter().rev() {
        // SAFETY: a live entry.
        unsafe { (*entry).fts_link = next };
        next = entry;
    }
    next
}

/// Reads the entries of the directory `dir` into a list, `stat`ed. Null
/// with `errno` set if it cannot be read, or with `errno` 0 if it is empty.
///
/// # Safety
///
/// `dir` must be a live entry of the walk `sp`.
unsafe fn read_children(sp: &Stream, dir: *mut Ftsent) -> *mut Ftsent {
    // SAFETY: a live entry.
    let d = unsafe { &*dir };
    // SAFETY: its path is NUL-terminated.
    let stream = unsafe { opendir(d.fts_path) };
    if stream.is_null() {
        return null_mut();
    }
    let options = sp.fts.fts_options;
    let seedot = options & FTS_SEEDOT != 0;
    let follow = options & FTS_LOGICAL != 0;
    let dir_len = usize::from(d.fts_pathlen);
    let slash = dir_len > 0 && {
        // SAFETY: the path holds `dir_len` bytes.
        (unsafe { d.fts_path.wrapping_add(dir_len - 1).read() }) == b'/' as c_char
    };
    let mut head: *mut Ftsent = null_mut();
    let mut tail: *mut Ftsent = null_mut();
    let failed = loop {
        errno::set(0);
        // SAFETY: this call's stream.
        let record = unsafe { readdir(stream) };
        if record.is_null() {
            break errno::get() != 0;
        }
        // SAFETY: a whole record.
        let name = unsafe { (*record).d_name.as_ptr() };
        // SAFETY: the name is NUL-terminated.
        let name_len = unsafe { strlen(name) };
        // SAFETY: the name holds `name_len` bytes.
        if !seedot && unsafe { is_dots(name, name_len) } {
            continue;
        }
        let mut path = [0 as c_char; 4096];
        let prefix = if slash { dir_len } else { dir_len + 1 };
        if prefix + name_len >= path.len() {
            errno::set(errno::ENAMETOOLONG);
            break true;
        }
        // SAFETY: the directory's path holds `dir_len` bytes, which fit.
        let _ = unsafe { memcpy(path.as_mut_ptr().cast(), d.fts_path.cast(), dir_len) };
        if let Some(separator) = path.get_mut(dir_len)
            && !slash
        {
            *separator = b'/' as c_char;
        }
        let at = path.as_mut_ptr().wrapping_add(prefix);
        // SAFETY: the name fits after the prefix, checked above.
        let _ = unsafe { memcpy(at.cast(), name.cast(), name_len) };
        // SAFETY: the name and path are readable for their lengths.
        let entry = unsafe {
            new_entry(
                name,
                name_len,
                path.as_ptr(),
                prefix + name_len,
                dir,
                d.fts_level + 1,
            )
        };
        if entry.is_null() {
            break true;
        }
        // SAFETY: a new live entry.
        let info = unsafe { classify(entry, follow, seedot) };
        // SAFETY: as above.
        unsafe { (*entry).fts_info = info };
        if tail.is_null() {
            head = entry;
        } else {
            // SAFETY: the list's last entry.
            unsafe { (*tail).fts_link = entry };
        }
        tail = entry;
    };
    let error = errno::get();
    // SAFETY: the stream is not used again.
    let _ = unsafe { closedir(stream) };
    if failed {
        // SAFETY: the list is this call's.
        unsafe { free_list(head) };
        errno::set(if error == 0 { errno::ENOMEM } else { error });
        return null_mut();
    }
    errno::set(0);
    // SAFETY: the list is live.
    unsafe { sort(head, sp.fts.fts_compar) }
}

/// Starts a walk of the files the null-terminated array `paths` names,
/// with `options`, visiting each directory's entries in `compar`'s order
/// if it is not null. Null with `errno` set on an error.
///
/// # Safety
///
/// `paths` must be a null-terminated array of NUL-terminated strings, and
/// `compar` sound to call with entries.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fts_open(
    paths: *const *const c_char,
    options: c_int,
    compar: Option<Compare>,
) -> *mut Fts {
    if options & !FTS_OPTIONMASK != 0 || paths.is_null() {
        errno::set(errno::EINVAL);
        return null_mut();
    }
    let sp = malloc(size_of::<Stream>()).cast::<Stream>();
    if sp.is_null() {
        return null_mut();
    }
    // SAFETY: an empty name and path, readable for zero bytes.
    let root = unsafe { new_entry(c"".as_ptr(), 0, c"".as_ptr(), 0, null_mut(), -1) };
    if root.is_null() {
        // SAFETY: from `malloc` above.
        unsafe { free(sp.cast()) };
        return null_mut();
    }
    let stream = Stream {
        fts: Fts {
            fts_cur: null_mut(),
            fts_child: null_mut(),
            fts_array: null_mut(),
            fts_dev: 0,
            fts_path: null_mut(),
            fts_rfd: -1,
            fts_pathlen: 0,
            fts_nitems: 0,
            fts_compar: compar,
            fts_options: options,
        },
        root,
        started: false,
    };
    // SAFETY: fresh memory for the stream.
    unsafe { sp.write(stream) };
    let follow = options & (FTS_LOGICAL | FTS_COMFOLLOW) != 0;
    let mut tail: *mut Ftsent = null_mut();
    let mut index = 0;
    loop {
        // SAFETY: the array is null-terminated.
        let path = unsafe { paths.wrapping_add(index).read() };
        if path.is_null() {
            break;
        }
        index += 1;
        // SAFETY: each path is NUL-terminated.
        let len = unsafe { strlen(path) };
        // SAFETY: as above.
        let entry = unsafe { new_entry(path, len, path, len, root, 0) };
        if len == 0 || entry.is_null() {
            if len == 0 {
                errno::set(errno::ENOENT);
            }
            // SAFETY: the walk is this call's, and not returned.
            unsafe { free_stream(sp) };
            return null_mut();
        }
        // SAFETY: a new live entry.
        let info = unsafe { classify(entry, follow, false) };
        // SAFETY: as above.
        unsafe { (*entry).fts_info = info };
        let node = node_of(root);
        if tail.is_null() {
            // SAFETY: the root's node.
            unsafe { (*node).children = entry };
        } else {
            // SAFETY: the list's last entry.
            unsafe { (*tail).fts_link = entry };
        }
        tail = entry;
    }
    let node = node_of(root);
    // SAFETY: the root's node.
    let roots = unsafe { (*node).children };
    // SAFETY: the root's list, live.
    let sorted = unsafe { sort(roots, compar) };
    // SAFETY: the root's node.
    unsafe { (*node).children = sorted };
    errno::set(0);
    sp.cast()
}

/// Frees the walk `sp` and every entry it still holds.
///
/// # Safety
///
/// `sp` must be a walk from [`fts_open`] that nothing uses again.
unsafe fn free_stream(sp: *mut Stream) {
    // SAFETY: the caller's walk.
    let root = unsafe { (*sp).root };
    // SAFETY: the root and its lists are the walk's.
    unsafe { free_list(root) };
    // SAFETY: from `malloc`.
    unsafe { free(sp.cast()) };
}

/// The next entry of the walk, or null at its end, with `errno` 0, or on an
/// error.
///
/// # Safety
///
/// `ftsp` must be a walk from [`fts_open`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fts_read(ftsp: *mut Fts) -> *mut Ftsent {
    let sp = ftsp.cast::<Stream>();
    // SAFETY: the caller's walk.
    let s = unsafe { &mut *sp };
    let options = s.fts.fts_options;
    let follow_all = options & FTS_LOGICAL != 0;
    let seedot = options & FTS_SEEDOT != 0;
    if !s.started {
        s.started = true;
        // SAFETY: the root's node.
        let first = unsafe { (*node_of(s.root)).children };
        if first.is_null() {
            return null_mut();
        }
        s.fts.fts_cur = first;
        // SAFETY: a live entry.
        s.fts.fts_dev = unsafe { (*first).fts_dev };
        return first;
    }
    let p = s.fts.fts_cur;
    if p.is_null() {
        return null_mut();
    }
    // SAFETY: the entry last returned, still live; nothing else refers to
    // it while this copy is read.
    let e = unsafe { p.read() };
    let instr = c_int::from(e.fts_instr);
    // SAFETY: as above.
    unsafe { (*p).fts_instr = FTS_NOINSTR as u16 };
    if instr == FTS_AGAIN {
        let follow = follow_all || (options & FTS_COMFOLLOW != 0 && e.fts_level == 0);
        // SAFETY: a live entry.
        let info = unsafe { classify(p, follow, seedot) };
        // SAFETY: as above.
        unsafe { (*p).fts_info = info };
        return p;
    }
    if instr == FTS_FOLLOW && (e.fts_info == FTS_SL || e.fts_info == FTS_SLNONE) {
        // SAFETY: a live entry.
        let info = unsafe { classify(p, true, seedot) };
        // SAFETY: as above.
        unsafe { (*p).fts_info = info };
        return p;
    }
    if e.fts_info == FTS_D {
        if instr == FTS_SKIP || (options & FTS_XDEV != 0 && e.fts_dev != s.fts.fts_dev) {
            // SAFETY: a live entry.
            unsafe { (*p).fts_info = FTS_DP };
            return p;
        }
        // SAFETY: a live directory entry of this walk.
        let children = unsafe { read_children(s, p) };
        if children.is_null() {
            let error = errno::get();
            let info = if error != 0 { FTS_DNR } else { FTS_DP };
            // SAFETY: a live entry.
            unsafe { (*p).fts_info = info };
            // SAFETY: as above.
            unsafe { (*p).fts_errno = error };
            return p;
        }
        // SAFETY: the directory's node.
        unsafe { (*node_of(p)).children = children };
        s.fts.fts_cur = children;
        return children;
    }
    // The next entry in the same directory, or the directory itself after
    // them.
    if !e.fts_link.is_null() {
        let next = e.fts_link;
        s.fts.fts_cur = next;
        // SAFETY: a live entry.
        let n = unsafe { &*next };
        if n.fts_level == 0 {
            s.fts.fts_dev = n.fts_dev;
        }
        return next;
    }
    let parent = e.fts_parent;
    // SAFETY: the parent is live; its level is -1 at the top.
    if unsafe { (*parent).fts_level } < 0 {
        s.fts.fts_cur = null_mut();
        errno::set(0);
        return null_mut();
    }
    let node = node_of(parent);
    // SAFETY: the directory's node.
    let children = unsafe { (*node).children };
    // SAFETY: the directory is done: its entries, `p` among them, go.
    unsafe { free_list(children) };
    // SAFETY: as above.
    unsafe { (*node).children = null_mut() };
    // SAFETY: a live entry.
    unsafe { (*parent).fts_info = FTS_DP };
    s.fts.fts_cur = parent;
    parent
}

/// Gives the walk an instruction for `entry`, which the next `fts_read`
/// follows. -1 with `EINVAL` for an instruction there is not.
///
/// # Safety
///
/// `entry` must be a live entry of the walk.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fts_set(_ftsp: *mut Fts, entry: *mut Ftsent, instr: c_int) -> c_int {
    if !matches!(instr, 0 | FTS_AGAIN | FTS_FOLLOW | FTS_NOINSTR | FTS_SKIP) {
        errno::set(errno::EINVAL);
        return -1;
    }
    // SAFETY: the caller's live entry.
    unsafe { (*entry).fts_instr = instr as u16 };
    0
}

/// Ends the walk, freeing it and its entries.
///
/// # Safety
///
/// `ftsp` must be null or a walk from [`fts_open`] that nothing uses again.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fts_close(ftsp: *mut Fts) -> c_int {
    if !ftsp.is_null() {
        // SAFETY: the caller's walk.
        unsafe { free_stream(ftsp.cast()) };
    }
    0
}

/// glibc's large-file name for [`fts_open`]: `struct stat` is already the
/// large-file one here.
///
/// # Safety
///
/// As [`fts_open`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fts64_open(
    paths: *const *const c_char,
    options: c_int,
    compar: Option<Compare>,
) -> *mut Fts {
    // SAFETY: the caller's contract is `fts_open`'s.
    unsafe { fts_open(paths, options, compar) }
}

/// glibc's large-file name for [`fts_read`].
///
/// # Safety
///
/// As [`fts_read`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fts64_read(ftsp: *mut Fts) -> *mut Ftsent {
    // SAFETY: the caller's contract is `fts_read`'s.
    unsafe { fts_read(ftsp) }
}

/// glibc's large-file name for [`fts_set`].
///
/// # Safety
///
/// As [`fts_set`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fts64_set(ftsp: *mut Fts, entry: *mut Ftsent, instr: c_int) -> c_int {
    // SAFETY: the caller's contract is `fts_set`'s.
    unsafe { fts_set(ftsp, entry, instr) }
}

/// glibc's large-file name for [`fts_close`].
///
/// # Safety
///
/// As [`fts_close`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fts64_close(ftsp: *mut Fts) -> c_int {
    // SAFETY: the caller's contract is `fts_close`'s.
    unsafe { fts_close(ftsp) }
}
