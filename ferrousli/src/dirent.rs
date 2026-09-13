//! `dirent.h`: reading directories.
//!
//! A `DIR` is a [`Dir`] from `malloc`: the descriptor and a buffer that
//! `getdents64` fills with records. `readdir` returns a pointer into the
//! buffer, valid until the next call on the same stream, as POSIX allows.
//! The design is musl 1.2.5's (MIT), `src/dirent/`.
//!
//! # The record layout
//!
//! `getdents64` writes `struct linux_dirent64`: a 64-bit inode number, a
//! 64-bit offset, a 16-bit record length, a type byte and the NUL-terminated
//! name, padded to a multiple of 8. The kernel does not export that structure
//! to its UAPI headers; its layout is in `getdents64(2)` and is the prefix of
//! glibc's `struct dirent64` in `/usr/include/x86_64-linux-gnu/bits/dirent.h`.
//! musl's `struct dirent` in `include/bits/dirent.h` is the same with a
//! 256-byte name, so a record can be handed to the program as it is. A record
//! is only `d_reclen` bytes long, not `sizeof(struct dirent)`; `readdir_r` and
//! `scandir` copy that many.
//!
//! glibc's `struct dirent` and `struct dirent64` have this layout too on
//! x86-64, so `readdir64` and the other `64` names glibc exports are the same
//! functions.
//!
//! # Offsets
//!
//! `telldir` returns the kernel's `d_off` of the last record returned, which
//! `seekdir` gives back to `lseek`. The value is the file system's cookie, not
//! a count, as POSIX allows.

use core::ffi::{c_char, c_int, c_long, c_uint, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;

use crate::errno;
use crate::fcntl::{AT_FDCWD, O_DIRECTORY};
use crate::growable::{Growable, sort_by};
use crate::lock::SpinLock;
use crate::malloc::{calloc, free, malloc};
use crate::stat::{Stat, fstat};
use crate::string::{memcpy, strcoll, strverscmp};
use crate::syscall::{self, nr};

/// `struct dirent`, as `include/bits/dirent.h` lays it out.
#[repr(C)]
#[derive(Debug)]
pub struct Dirent {
    /// The inode number.
    pub d_ino: u64,
    /// The file system's cookie for the next record.
    pub d_off: i64,
    /// This record's length.
    pub d_reclen: u16,
    /// The file's type, a `DT_` value.
    pub d_type: u8,
    /// The name, NUL-terminated.
    pub d_name: [c_char; 256],
}

const _: () = assert!(size_of::<Dirent>() == 280);
const _: () = assert!(offset_of!(Dirent, d_off) == 8);
const _: () = assert!(offset_of!(Dirent, d_reclen) == 16);
const _: () = assert!(offset_of!(Dirent, d_type) == 18);
const _: () = assert!(offset_of!(Dirent, d_name) == 19);

/// The buffer's size in words. 8 KiB holds dozens of records, and is aligned
/// to 8 as the records are.
const BUFFER_WORDS: usize = 1024;
/// The buffer's size in bytes.
const BUFFER_BYTES: usize = BUFFER_WORDS * 8;

/// `O_CLOEXEC`, from `asm-generic/fcntl.h`.
const O_CLOEXEC: c_int = 0o2_000_000;
/// `O_PATH`, from `asm-generic/fcntl.h`.
const O_PATH: usize = 0o10_000_000;
/// `F_GETFL`, from `asm-generic/fcntl.h`.
const F_GETFL: usize = 3;
/// `SEEK_SET`, from `linux/fs.h`.
const SEEK_SET: usize = 0;
/// `S_IFMT`, from `linux/stat.h`.
pub(crate) const S_IFMT: c_uint = 0o170_000;
/// `S_IFDIR`, from `linux/stat.h`.
pub(crate) const S_IFDIR: c_uint = 0o040_000;

/// `DIR`: an open directory stream.
#[repr(C)]
#[derive(Debug)]
pub struct Dir {
    /// Held by the calls that change the position.
    lock: SpinLock,
    /// The directory's descriptor.
    fd: c_int,
    /// Where the next record starts in `buf`.
    pos: usize,
    /// How many bytes of `buf` the last `getdents64` filled.
    end: usize,
    /// The offset `telldir` reports.
    tell: i64,
    /// The records, in words so that they are aligned as the kernel aligns
    /// them.
    buf: [u64; BUFFER_WORDS],
}

/// Closes `fd`, ignoring any error: used where a failure is already being
/// reported.
pub(crate) fn close_quietly(fd: c_int) {
    // SAFETY: `close` reads no memory.
    let _ = unsafe { syscall::syscall3(nr::CLOSE, fd as usize, 0, 0) };
}

/// A stream for `fd`, which the stream then owns, or null with `errno` set.
fn new_stream(fd: c_int) -> *mut Dir {
    let dir = calloc(1, size_of::<Dir>()).cast::<Dir>();
    if dir.is_null() {
        return null_mut();
    }
    // SAFETY: a zeroed `Dir` is valid: an unheld lock, an empty buffer. The
    // allocation is new, so writing a field aliases nothing.
    unsafe { (*dir).fd = fd };
    dir
}

/// Opens the directory `name`.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn opendir(name: *const c_char) -> *mut Dir {
    // SAFETY: the kernel reads the NUL-terminated path.
    let ret = unsafe {
        syscall::syscall4(
            nr::OPENAT,
            AT_FDCWD as usize,
            name.addr(),
            (O_DIRECTORY | O_CLOEXEC) as usize,
            0,
        )
    };
    let fd = match errno::decode(ret) {
        Ok(fd) => fd as c_int,
        Err(e) => {
            errno::set(e);
            return null_mut();
        }
    };
    let dir = new_stream(fd);
    if dir.is_null() {
        close_quietly(fd);
    }
    dir
}

/// A stream for the open directory `fd`. The stream owns the descriptor from
/// then on. The descriptor's flags, close-on-exec included, are left as they
/// are, as glibc leaves them.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fdopendir(fd: c_int) -> *mut Dir {
    let mut st = Stat::default();
    // SAFETY: `st` is a writable `struct stat`.
    if unsafe { fstat(fd, &raw mut st) } < 0 {
        return null_mut();
    }
    // SAFETY: `F_GETFL` reads no memory.
    let flags = unsafe { syscall::syscall3(nr::FCNTL, fd as usize, F_GETFL, 0) };
    match errno::decode(flags) {
        Err(e) => {
            errno::set(e);
            return null_mut();
        }
        Ok(flags) if flags & O_PATH != 0 => {
            errno::set(errno::EBADF);
            return null_mut();
        }
        Ok(_) => {}
    }
    if st.st_mode & S_IFMT != S_IFDIR {
        errno::set(errno::ENOTDIR);
        return null_mut();
    }
    new_stream(fd)
}

/// Closes the stream and its descriptor, and frees it.
///
/// # Safety
///
/// `dir` must be a stream from [`opendir`] or [`fdopendir`], not used again.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn closedir(dir: *mut Dir) -> c_int {
    if dir.is_null() {
        errno::set(errno::EBADF);
        return -1;
    }
    // SAFETY: the caller passes a valid stream.
    let fd = unsafe { (*dir).fd };
    // SAFETY: the stream came from `calloc`, and nothing uses it again.
    unsafe { free(dir.cast()) };
    // SAFETY: `close` reads no memory.
    let ret = unsafe { syscall::syscall3(nr::CLOSE, fd as usize, 0, 0) };
    errno::from_syscall(ret) as c_int
}

/// The next record of `dir`, or null at the end or on an error, which sets
/// `errno`. The caller holds the stream's lock or is its only user.
///
/// # Safety
///
/// `dir` must be a valid stream.
unsafe fn next_record(dir: *mut Dir) -> *mut Dirent {
    // SAFETY: the caller passes a valid stream it has to itself.
    let d = unsafe { &mut *dir };
    if d.pos >= d.end {
        // SAFETY: the kernel fills at most `BUFFER_BYTES` of the buffer.
        let ret = unsafe {
            syscall::syscall3(
                nr::GETDENTS64,
                d.fd as usize,
                d.buf.as_mut_ptr().addr(),
                BUFFER_BYTES,
            )
        };
        match errno::decode(ret) {
            // A directory that was removed while open reads as empty.
            Err(errno::ENOENT) | Ok(0) => return null_mut(),
            Err(e) => {
                errno::set(e);
                return null_mut();
            }
            Ok(len) => {
                d.end = len.min(BUFFER_BYTES);
                d.pos = 0;
            }
        }
    }
    let record = d.buf.as_mut_ptr().wrapping_byte_add(d.pos).cast::<Dirent>();
    // SAFETY: the kernel wrote whole records, and `pos` is where one starts.
    let reclen = unsafe { (*record).d_reclen };
    // SAFETY: as above.
    let off = unsafe { (*record).d_off };
    if reclen == 0 {
        // A corrupt buffer would otherwise return this record forever.
        d.pos = d.end;
        errno::set(errno::EIO);
        return null_mut();
    }
    d.pos += usize::from(reclen);
    d.tell = off;
    record
}

/// The next entry of the directory, or null at its end or on an error, which
/// sets `errno`. The entry is valid until the next call on the stream.
///
/// # Safety
///
/// `dir` must be a valid stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn readdir(dir: *mut Dir) -> *mut Dirent {
    // SAFETY: the caller passes a valid stream.
    let _guard = unsafe { (*dir).lock.lock() };
    // SAFETY: as above, and the lock is held.
    unsafe { next_record(dir) }
}

/// `readdir` under glibc's large-file name.
///
/// # Safety
///
/// As [`readdir`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn readdir64(dir: *mut Dir) -> *mut Dirent {
    // SAFETY: the same contract.
    unsafe { readdir(dir) }
}

/// Copies the next entry into `entry` and points `*result` at it, or sets
/// `*result` to null at the end. Returns zero, or an error number.
///
/// # Safety
///
/// `dir` must be a valid stream, `entry` writable for a `struct dirent`, and
/// `result` writable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn readdir_r(
    dir: *mut Dir,
    entry: *mut Dirent,
    result: *mut *mut Dirent,
) -> c_int {
    // SAFETY: the caller passes a valid stream.
    let _guard = unsafe { (*dir).lock.lock() };
    let saved = get_errno();
    errno::set(0);
    // SAFETY: as above, and the lock is held.
    let record = unsafe { next_record(dir) };
    let error = get_errno();
    if error != 0 {
        return error;
    }
    errno::set(saved);
    let out = if record.is_null() {
        null_mut()
    } else {
        // SAFETY: the record is a whole record.
        let len = usize::from(unsafe { (*record).d_reclen }).min(size_of::<Dirent>());
        // SAFETY: the record is `len` bytes, and `entry` a separate buffer of
        // at least that size.
        let _ = unsafe { memcpy(entry.cast(), record.cast::<c_void>(), len) };
        entry
    };
    // SAFETY: the caller passes a writable pointer.
    unsafe { result.write(out) };
    0
}

/// `readdir_r` under glibc's large-file name.
///
/// # Safety
///
/// As [`readdir_r`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn readdir64_r(
    dir: *mut Dir,
    entry: *mut Dirent,
    result: *mut *mut Dirent,
) -> c_int {
    // SAFETY: the same contract.
    unsafe { readdir_r(dir, entry, result) }
}

/// Moves the stream to `offset` in the directory, and empties its buffer.
///
/// # Safety
///
/// `dir` must be a valid stream.
unsafe fn seek(dir: *mut Dir, offset: i64) {
    // SAFETY: the caller passes a valid stream.
    let d = unsafe { &mut *dir };
    let _guard = d.lock.lock();
    // SAFETY: `lseek` reads no memory.
    let ret = unsafe { syscall::syscall3(nr::LSEEK, d.fd as usize, offset as usize, SEEK_SET) };
    d.tell = match errno::decode(ret) {
        Ok(at) => at as i64,
        Err(_) => -1,
    };
    d.pos = 0;
    d.end = 0;
}

/// Moves the stream back to the directory's first entry.
///
/// # Safety
///
/// `dir` must be a valid stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn rewinddir(dir: *mut Dir) {
    // SAFETY: the caller passes a valid stream.
    unsafe { seek(dir, 0) };
}

/// Moves the stream to a position [`telldir`] returned.
///
/// # Safety
///
/// `dir` must be a valid stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn seekdir(dir: *mut Dir, offset: c_long) {
    // SAFETY: the caller passes a valid stream.
    unsafe { seek(dir, offset) };
}

/// The stream's position, for [`seekdir`].
///
/// # Safety
///
/// `dir` must be a valid stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn telldir(dir: *mut Dir) -> c_long {
    // SAFETY: the caller passes a valid stream.
    let d = unsafe { &*dir };
    let _guard = d.lock.lock();
    d.tell
}

/// The stream's descriptor.
///
/// # Safety
///
/// `dir` must be a valid stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn dirfd(dir: *mut Dir) -> c_int {
    // SAFETY: the caller passes a valid stream.
    unsafe { (*dir).fd }
}

/// A `scandir` filter.
type Filter = Option<unsafe extern "C" fn(*const Dirent) -> c_int>;
/// A `scandir` comparison.
type Compare = Option<unsafe extern "C" fn(*const *const Dirent, *const *const Dirent) -> c_int>;

/// Frees the entries collected so far.
fn free_entries(entries: &Growable<*mut Dirent>) {
    for &entry in entries.as_slice() {
        // SAFETY: each entry came from `malloc` and is not used again.
        unsafe { free(entry.cast()) };
    }
}

/// Reads the directory `path`, keeping the entries `filter` accepts (all if it
/// is null), sorted by `compar` if it is not null. Stores an array from
/// `malloc` of entries from `malloc` in `*namelist`, and returns their number,
/// or -1 with `errno` set.
///
/// # Safety
///
/// `path` must be a NUL-terminated string, `namelist` writable, and the
/// functions must accept entries.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn scandir(
    path: *const c_char,
    namelist: *mut *mut *mut Dirent,
    filter: Filter,
    compar: Compare,
) -> c_int {
    // SAFETY: the caller passes a string.
    let dir = unsafe { opendir(path) };
    if dir.is_null() {
        return -1;
    }
    let saved = get_errno();
    let mut entries: Growable<*mut Dirent> = Growable::new();
    let failed = loop {
        errno::set(0);
        // SAFETY: `dir` is this call's own stream.
        let record = unsafe { next_record(dir) };
        if record.is_null() {
            break get_errno() != 0;
        }
        if let Some(filter) = filter
            // SAFETY: the program's filter accepts an entry.
            && unsafe { filter(record) } == 0
        {
            continue;
        }
        // SAFETY: `record` is a whole record.
        let len = usize::from(unsafe { (*record).d_reclen });
        let copy = malloc(len).cast::<Dirent>();
        if copy.is_null() {
            break true;
        }
        // SAFETY: both hold `len` bytes and do not overlap.
        let _ = unsafe { memcpy(copy.cast(), record.cast::<c_void>(), len) };
        if entries.len() >= c_int::MAX as usize || !entries.push(copy) {
            // SAFETY: the copy came from `malloc` and is not in the array.
            unsafe { free(copy.cast()) };
            errno::set(errno::ENOMEM);
            break true;
        }
    };
    let error = get_errno();
    // SAFETY: the stream is not used again.
    let _ = unsafe { closedir(dir) };
    if failed {
        free_entries(&entries);
        errno::set(error);
        return -1;
    }
    errno::set(saved);
    if let Some(compar) = compar {
        sort_by(entries.as_mut_slice(), |a, b| {
            let (a, b) = (a.cast_const(), b.cast_const());
            // SAFETY: the program's comparison accepts pointers to entries.
            unsafe { compar(&raw const a, &raw const b) }.cmp(&0)
        });
    }
    let count = entries.len();
    let (mut array, _) = entries.into_raw();
    if array.is_null() {
        // No entries: still give the program an array it can free.
        array = malloc(size_of::<*mut Dirent>()).cast();
        if array.is_null() {
            return -1;
        }
    }
    // SAFETY: the caller passes a writable pointer.
    unsafe { namelist.write(array) };
    count as c_int
}

/// `scandir` under glibc's large-file name.
///
/// # Safety
///
/// As [`scandir`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn scandir64(
    path: *const c_char,
    namelist: *mut *mut *mut Dirent,
    filter: Filter,
    compar: Compare,
) -> c_int {
    // SAFETY: the same contract.
    unsafe { scandir(path, namelist, filter, compar) }
}

/// The name of the entry `*entry` points to.
///
/// # Safety
///
/// `entry` must point to a pointer to a valid entry.
unsafe fn name_of(entry: *const *const Dirent) -> *const c_char {
    // SAFETY: the caller passes a pointer to an entry pointer.
    let entry = unsafe { entry.read() };
    entry.wrapping_byte_add(offset_of!(Dirent, d_name)).cast()
}

/// Orders two entries by name in the locale's collation order.
///
/// # Safety
///
/// Both must point to pointers to valid entries.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn alphasort(a: *const *const Dirent, b: *const *const Dirent) -> c_int {
    // SAFETY: the caller passes valid entries.
    let x = unsafe { name_of(a) };
    // SAFETY: as above.
    let y = unsafe { name_of(b) };
    // SAFETY: an entry's name is a string.
    unsafe { strcoll(x, y) }
}

/// `alphasort` under glibc's large-file name.
///
/// # Safety
///
/// As [`alphasort`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn alphasort64(a: *const *const Dirent, b: *const *const Dirent) -> c_int {
    // SAFETY: the same contract.
    unsafe { alphasort(a, b) }
}

/// Orders two entries by name as version numbers, with `strverscmp`.
///
/// # Safety
///
/// Both must point to pointers to valid entries.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn versionsort(a: *const *const Dirent, b: *const *const Dirent) -> c_int {
    // SAFETY: the caller passes valid entries.
    let x = unsafe { name_of(a) };
    // SAFETY: as above.
    let y = unsafe { name_of(b) };
    // SAFETY: an entry's name is a string.
    unsafe { strverscmp(x, y) }
}

/// `versionsort` under glibc's large-file name.
///
/// # Safety
///
/// As [`versionsort`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn versionsort64(a: *const *const Dirent, b: *const *const Dirent) -> c_int {
    // SAFETY: the same contract.
    unsafe { versionsort(a, b) }
}

/// The calling thread's `errno`.
pub(crate) fn get_errno() -> c_int {
    // SAFETY: the pointer is the calling thread's `errno`, which lives as long
    // as the thread.
    unsafe { errno::__errno_location().read() }
}
