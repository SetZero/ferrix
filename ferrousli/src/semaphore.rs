//! `semaphore.h`: unnamed semaphores, and named ones in `/dev/shm`.
//!
//! Adapted from musl (MIT). A semaphore is a value word, whose sign bit says
//! a thread may be sleeping on it, a count of waiters, and the private-futex
//! flag. Posting to a semaphore with waiters wakes one; posting the last
//! value past the waiters wakes them all, so that none sleeps with the value
//! above zero.
//!
//! A named semaphore is a 32-byte file in `/dev/shm`, mapped shared. It is
//! created in a temporary file and linked into place, so no process ever
//! maps a half-written one. Opening the same file again in a process returns
//! the same mapping, counted, as POSIX requires; a table of 256 entries, which
//! is `SEM_NSEMS_MAX`, records them.

use core::ffi::{c_char, c_int, c_uint, c_void};
use core::mem::{MaybeUninit, size_of};
use core::ptr::{null_mut, with_exposed_provenance_mut};
use core::sync::atomic::{AtomicI32, AtomicPtr, AtomicU64, Ordering};

use crate::cancel::{self, Cleanup};
use crate::lock::SpinLock;
use crate::stat::Stat;
use crate::syscall::{self, nr};
use crate::time::{CLOCK_REALTIME, Timespec};
use crate::{errno, futex};

/// `SEM_VALUE_MAX`, from `limits.h`.
const VALUE_MAX: c_int = c_int::MAX;
/// `SEM_NSEMS_MAX`, from `limits.h`.
const NSEMS_MAX: usize = 256;
/// `SEM_FAILED`.
const FAILED: *mut Sem = null_mut();

/// `sem_t`: eight `int`s, of which the first three are used.
#[repr(C)]
#[derive(Debug)]
pub struct Sem {
    /// The value, with the sign bit set while threads may sleep.
    value: AtomicI32,
    /// Threads waiting.
    waiters: AtomicI32,
    /// 128 for a private semaphore, 0 for a shared one.
    private: AtomicI32,
    /// Unused.
    rest: [AtomicI32; 5],
}

const _: () = assert!(size_of::<Sem>() == 32);

impl Sem {
    /// A semaphore holding `value`.
    const fn new(value: c_int, shared: bool) -> Self {
        Self {
            value: AtomicI32::new(value),
            waiters: AtomicI32::new(0),
            private: AtomicI32::new(if shared { 0 } else { 128 }),
            rest: [const { AtomicI32::new(0) }; 5],
        }
    }

    /// Takes one from the value if it is above zero.
    fn try_take(&self) -> bool {
        loop {
            let value = self.value.load(Ordering::SeqCst);
            if value & VALUE_MAX == 0 {
                return false;
            }
            if futex::cas(&self.value, value, value - 1) == value {
                return true;
            }
        }
    }
}

/// Reports `error` through `errno` and returns -1.
fn fail(error: c_int) -> c_int {
    errno::set(error);
    -1
}

/// Initialises `*sem` with `value`, shared between processes if `shared` is
/// nonzero. A value above `SEM_VALUE_MAX` fails with `EINVAL`.
///
/// # Safety
///
/// `sem` must be valid for a write of a `sem_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sem_init(sem: *mut Sem, shared: c_int, value: c_uint) -> c_int {
    let Ok(value) = c_int::try_from(value) else {
        return fail(errno::EINVAL);
    };
    // SAFETY: the caller vouches for `sem`.
    unsafe { sem.write(Sem::new(value, shared != 0)) };
    0
}

/// Destroys `*sem`, which holds nothing to free.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sem_destroy(_sem: *mut Sem) -> c_int {
    0
}

/// Stores the value of `*sem` in `*value`.
///
/// # Safety
///
/// `sem` must be an initialised semaphore and `value` valid for a write of an
/// `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sem_getvalue(sem: *mut Sem, value: *mut c_int) -> c_int {
    // SAFETY: the caller vouches for the semaphore.
    let current = unsafe { &*sem }.value.load(Ordering::SeqCst) & VALUE_MAX;
    // SAFETY: the caller vouches for `value`.
    unsafe { value.write(current) };
    0
}

/// Adds one to `*sem`, waking a waiter. Fails with `EOVERFLOW` at
/// `SEM_VALUE_MAX`.
///
/// # Safety
///
/// `sem` must be an initialised semaphore.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sem_post(sem: *mut Sem) -> c_int {
    // SAFETY: the caller vouches for the semaphore, which is all atomics.
    let sem = unsafe { &*sem };
    let private = sem.private.load(Ordering::SeqCst) != 0;
    let (value, waiters) = loop {
        let value = sem.value.load(Ordering::SeqCst);
        let waiters = sem.waiters.load(Ordering::SeqCst);
        if value & VALUE_MAX == VALUE_MAX {
            return fail(errno::EOVERFLOW);
        }
        let mut new = value.wrapping_add(1);
        if waiters <= 1 {
            new &= VALUE_MAX;
        }
        if futex::cas(&sem.value, value, new) == value {
            break (value, waiters);
        }
    };
    if value < 0 {
        futex::wake(
            &raw const sem.value,
            if waiters > 1 { 1 } else { -1 },
            private,
        );
    }
    0
}

/// Takes one from `*sem` if its value is above zero, or fails with `EAGAIN`.
///
/// # Safety
///
/// `sem` must be an initialised semaphore.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sem_trywait(sem: *mut Sem) -> c_int {
    // SAFETY: the caller vouches for the semaphore.
    if unsafe { &*sem }.try_take() {
        0
    } else {
        fail(errno::EAGAIN)
    }
}

/// Uncounts a waiter whose wait was cancelled.
unsafe extern "C" fn uncount(waiters: *mut c_void) {
    // SAFETY: the waiting thread registered its semaphore's waiter count,
    // which outlives the wait.
    let _ = unsafe { AtomicI32::from_ptr(waiters.cast()) }.fetch_sub(1, Ordering::SeqCst);
}

/// Waits to take one from `*sem`, until the absolute time `*at` on `clock` if
/// `at` is not null. A cancellation point.
///
/// # Safety
///
/// `sem` must be an initialised semaphore, and `at` null or valid for a read
/// of a `struct timespec`.
unsafe fn clockwait(sem: *mut Sem, clock: c_int, at: *const Timespec) -> c_int {
    cancel::testcancel();
    // SAFETY: the caller vouches for the semaphore, which is all atomics.
    let sem = unsafe { &*sem };
    if sem.try_take() {
        return 0;
    }
    let mut spins = 100;
    while spins > 0
        && sem.value.load(Ordering::SeqCst) & VALUE_MAX == 0
        && sem.waiters.load(Ordering::SeqCst) == 0
    {
        core::hint::spin_loop();
        spins -= 1;
    }
    while !sem.try_take() {
        let private = sem.private.load(Ordering::SeqCst) != 0;
        let _ = sem.waiters.fetch_add(1, Ordering::SeqCst);
        let _ = futex::cas(&sem.value, 0, c_int::MIN);
        let mut cb = Cleanup {
            func: None,
            arg: null_mut(),
            next: null_mut(),
        };
        // SAFETY: `cb` lives until the matching pop, and the waiter count
        // outlives the wait.
        unsafe {
            cancel::_pthread_cleanup_push(&raw mut cb, Some(uncount), sem.waiters.as_ptr().cast())
        };
        // SAFETY: the caller vouches for `at`.
        let r = unsafe { futex::timedwait_cp(&sem.value, c_int::MIN, clock, at, private) };
        // SAFETY: `cb` is the handler pushed above; running it uncounts.
        unsafe { cancel::_pthread_cleanup_pop(&raw mut cb, 1) };
        if r != 0 {
            return fail(r);
        }
    }
    0
}

/// Waits to take one from `*sem`. A cancellation point.
///
/// # Safety
///
/// `sem` must be an initialised semaphore.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sem_wait(sem: *mut Sem) -> c_int {
    // SAFETY: the caller vouches for the semaphore, and there is no limit.
    unsafe { clockwait(sem, CLOCK_REALTIME, core::ptr::null()) }
}

/// Waits to take one from `*sem` until the absolute time `*at` on
/// `CLOCK_REALTIME`, then fails with `ETIMEDOUT`. A cancellation point.
///
/// # Safety
///
/// `sem` must be an initialised semaphore, and `at` valid for a read of a
/// `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sem_timedwait(sem: *mut Sem, at: *const Timespec) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { clockwait(sem, CLOCK_REALTIME, at) }
}

/// As [`sem_timedwait`], measuring `*at` against `clock`. From glibc.
///
/// # Safety
///
/// As [`sem_timedwait`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sem_clockwait(sem: *mut Sem, clock: c_int, at: *const Timespec) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { clockwait(sem, clock, at) }
}

/// Guards the table of named semaphores.
static TABLE_LOCK: SpinLock = SpinLock::new();
/// Each open named semaphore's mapping, a reservation, or null.
static TABLE_SEM: [AtomicPtr<Sem>; NSEMS_MAX] = [const { AtomicPtr::new(null_mut()) }; NSEMS_MAX];
/// Each open named semaphore's inode.
static TABLE_INO: [AtomicU64; NSEMS_MAX] = [const { AtomicU64::new(0) }; NSEMS_MAX];
/// How many times each named semaphore is open.
static TABLE_REFS: [AtomicI32; NSEMS_MAX] = [const { AtomicI32::new(0) }; NSEMS_MAX];

/// A table entry being set up.
fn reserved() -> *mut Sem {
    core::ptr::without_provenance_mut(usize::MAX)
}

/// `O_RDWR | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK`, from `asm-generic/fcntl.h`.
const OPEN_FLAGS: c_int = 0o2 | 0o400_000 | 0o2_000_000 | 0o4_000;
/// `O_CREAT`.
const O_CREAT: c_int = 0o100;
/// `O_EXCL`.
const O_EXCL: c_int = 0o200;
/// `AT_FDCWD`, from `linux/fcntl.h`.
const AT_FDCWD: usize = -100_isize as usize;
/// `PROT_READ | PROT_WRITE`.
const PROT_READ_WRITE: usize = 0x1 | 0x2;
/// `MAP_SHARED`, from `linux/mman.h`.
const MAP_SHARED: usize = 0x01;
/// The longest name, `NAME_MAX`.
const NAME_MAX: usize = 255;
/// Where named semaphores live.
const DIRECTORY: &[u8] = b"/dev/shm/";

/// A path buffer: the directory, a name and a NUL.
type PathBuf = [u8; 9 + NAME_MAX + 1];

/// Writes `/dev/shm/<name>` into `buf` for a semaphore `name`, which may
/// start with slashes but hold no other. musl's `__shm_mapname`.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
unsafe fn map_name(name: *const c_char, buf: &mut PathBuf) -> Result<(), c_int> {
    let mut at = name;
    // SAFETY: the caller passes a NUL-terminated string, and `at` stays
    // inside it.
    while unsafe { at.read() } == b'/' as c_char {
        at = at.wrapping_add(1);
    }
    let mut len = 0;
    loop {
        // SAFETY: as above.
        let byte = unsafe { at.wrapping_add(len).read() } as u8;
        if byte == 0 {
            break;
        }
        if byte == b'/' {
            return Err(errno::EINVAL);
        }
        len += 1;
        if len > NAME_MAX {
            return Err(errno::ENAMETOOLONG);
        }
    }
    // SAFETY: as above; `len` bytes were read.
    let first = unsafe { at.read() } as u8;
    // SAFETY: as above.
    let last = unsafe { at.wrapping_add(len.saturating_sub(1)).read() } as u8;
    if len == 0 || (len <= 2 && first == b'.' && last == b'.') {
        return Err(errno::EINVAL);
    }
    let mut out = 0;
    for &byte in DIRECTORY {
        if let Some(slot) = buf.get_mut(out) {
            *slot = byte;
        }
        out += 1;
    }
    for i in 0..=len {
        // SAFETY: as above, including the NUL.
        let byte = unsafe { at.wrapping_add(i).read() } as u8;
        if let Some(slot) = buf.get_mut(out) {
            *slot = byte;
        }
        out += 1;
    }
    Ok(())
}

/// Opens `path` with `flags` and `mode`, returning the descriptor.
fn open(path: *const u8, flags: c_int, mode: c_uint) -> Result<usize, c_int> {
    // SAFETY: the kernel only reads the NUL-terminated path.
    let ret = unsafe {
        syscall::syscall4(
            nr::OPENAT,
            AT_FDCWD,
            path.addr(),
            flags as usize,
            mode as usize,
        )
    };
    errno::decode(ret)
}

/// Closes a descriptor this module opened.
fn close(fd: usize) {
    // SAFETY: the descriptor is this module's.
    let _ = unsafe { syscall::syscall2(nr::CLOSE, fd, 0) };
}

/// Unlinks `path`.
fn unlink(path: *const u8) -> Result<(), c_int> {
    // SAFETY: the kernel only reads the NUL-terminated path.
    let ret = unsafe { syscall::syscall3(nr::UNLINKAT, AT_FDCWD, path.addr(), 0) };
    errno::decode(ret).map(|_| ())
}

/// The inode of `fd`, and a shared mapping of a semaphore in it.
fn map_file(fd: usize) -> Result<(u64, *mut Sem), c_int> {
    let mut st = MaybeUninit::<Stat>::uninit();
    // SAFETY: the kernel writes a `struct stat` to the live local.
    let ret = unsafe { syscall::syscall2(nr::FSTAT, fd, st.as_mut_ptr().addr()) };
    let _: usize = errno::decode(ret)?;
    // SAFETY: `fstat` succeeded, so it wrote the structure.
    let ino = unsafe { st.assume_init_ref() }.st_ino;
    // SAFETY: a new shared mapping of the file aliases nothing in this
    // process but other mappings of the same semaphore, whose every access
    // is atomic.
    let ret = unsafe {
        syscall::syscall6(
            nr::MMAP,
            0,
            size_of::<Sem>(),
            PROT_READ_WRITE,
            MAP_SHARED,
            fd,
            0,
        )
    };
    let map = errno::decode(ret)?;
    Ok((ino, with_exposed_provenance_mut(map)))
}

/// Unmaps a named semaphore's mapping.
fn unmap(sem: *mut Sem) {
    // SAFETY: only mappings this module made are passed, once unused.
    let _ = unsafe { syscall::syscall2(nr::MUNMAP, sem.addr(), size_of::<Sem>()) };
}

/// Opens or creates the file of semaphore `name`, returning its inode and
/// mapping.
fn open_or_create(
    path: &PathBuf,
    flags: c_int,
    mode: c_uint,
    value: c_uint,
) -> Result<(u64, *mut Sem), c_int> {
    let exclusive = flags == O_CREAT | O_EXCL;
    if exclusive {
        // SAFETY: the kernel only reads the NUL-terminated path.
        let ret = unsafe { syscall::syscall3(nr::FACCESSAT, AT_FDCWD, path.as_ptr().addr(), 0) };
        if ret == 0 {
            return Err(errno::EEXIST);
        }
    }
    let mut first = true;
    let mut initial = Sem::new(0, true);
    loop {
        if !exclusive {
            match open(path.as_ptr(), OPEN_FLAGS, 0) {
                Ok(fd) => {
                    let mapped = map_file(fd);
                    close(fd);
                    return mapped;
                }
                Err(errno::ENOENT) => {}
                Err(error) => return Err(error),
            }
        }
        if flags & O_CREAT == 0 {
            return Err(errno::ENOENT);
        }
        if first {
            first = false;
            let Ok(value) = c_int::try_from(value) else {
                return Err(errno::EINVAL);
            };
            initial = Sem::new(value, true);
        }
        // Build it under a temporary name, then link it into place.
        let mut now = Timespec::default();
        // SAFETY: the kernel writes a `struct timespec` to the live local.
        let _ = unsafe {
            syscall::syscall2(
                nr::CLOCK_GETTIME,
                CLOCK_REALTIME as usize,
                (&raw mut now).addr(),
            )
        };
        let mut tmp = [0_u8; 40];
        temporary_name(now.tv_nsec, &mut tmp);
        let fd = match open(tmp.as_ptr(), O_CREAT | O_EXCL | OPEN_FLAGS, mode & 0o666) {
            Ok(fd) => fd,
            Err(errno::EEXIST) => continue,
            Err(error) => return Err(error),
        };
        // SAFETY: the kernel reads the 32 bytes of the live local.
        let wrote = unsafe {
            syscall::syscall3(nr::WRITE, fd, (&raw const initial).addr(), size_of::<Sem>())
        };
        let mapped = if wrote == size_of::<Sem>() as isize {
            map_file(fd)
        } else {
            Err(errno::decode(wrote).err().unwrap_or(errno::EIO))
        };
        close(fd);
        let (ino, map) = match mapped {
            Ok(found) => found,
            Err(error) => {
                let _ = unlink(tmp.as_ptr());
                return Err(error);
            }
        };
        // SAFETY: the kernel only reads the two NUL-terminated paths.
        let linked = unsafe {
            syscall::syscall6(
                nr::LINKAT,
                AT_FDCWD,
                tmp.as_ptr().addr(),
                AT_FDCWD,
                path.as_ptr().addr(),
                0,
                0,
            )
        };
        let _ = unlink(tmp.as_ptr());
        match errno::decode(linked) {
            Ok(_) => return Ok((ino, map)),
            Err(error) => {
                unmap(map);
                // Someone else created it first: open theirs, unless the
                // caller wanted it new.
                if error != errno::EEXIST || exclusive {
                    return Err(error);
                }
            }
        }
    }
}

/// Writes `/dev/shm/tmp-<nsec>` and a NUL into `buf`.
fn temporary_name(nsec: i64, buf: &mut [u8; 40]) {
    let mut out = 0;
    let mut put = |byte: u8| {
        if let Some(slot) = buf.get_mut(out) {
            *slot = byte;
            out += 1;
        }
    };
    for &byte in b"/dev/shm/tmp-" {
        put(byte);
    }
    let mut digits = [0_u8; 20];
    let mut count = 0;
    let mut value = nsec.unsigned_abs();
    loop {
        if let Some(slot) = digits.get_mut(count) {
            *slot = b'0' + (value % 10) as u8;
        }
        count += 1;
        value /= 10;
        if value == 0 || count == digits.len() {
            break;
        }
    }
    while count > 0 {
        count -= 1;
        put(digits.get(count).copied().unwrap_or(b'0'));
    }
    put(0);
}

/// Opens the named semaphore `name`, creating it with permissions `mode` and
/// value `value` if `flags` holds `O_CREAT` and it does not exist. Returns
/// `SEM_FAILED` with `errno` set on failure.
///
/// C declares it variadic, and reads `mode` and `value` only with `O_CREAT`.
/// On x86-64 variadic integers arrive where fixed ones do, so they are
/// parameters here, as in [`crate::fcntl`].
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sem_open(
    name: *const c_char,
    flags: c_int,
    mode: c_uint,
    value: c_uint,
) -> *mut Sem {
    let mut path: PathBuf = [0; 9 + NAME_MAX + 1];
    // SAFETY: the caller passes a NUL-terminated string.
    if let Err(error) = unsafe { map_name(name, &mut path) } {
        errno::set(error);
        return FAILED;
    }

    // Reserve a slot first: after the file is created, nothing may fail.
    let slot = {
        let _guard = TABLE_LOCK.lock();
        let mut total: i64 = 0;
        let mut free = None;
        for (i, entry) in TABLE_SEM.iter().enumerate() {
            total += i64::from(TABLE_REFS.get(i).map_or(0, |r| r.load(Ordering::SeqCst)));
            if free.is_none() && entry.load(Ordering::SeqCst).is_null() {
                free = Some(i);
            }
        }
        match free {
            Some(i) if total < i64::from(c_int::MAX) => {
                if let Some(entry) = TABLE_SEM.get(i) {
                    entry.store(reserved(), Ordering::SeqCst);
                }
                i
            }
            _ => {
                errno::set(errno::EMFILE);
                return FAILED;
            }
        }
    };

    let old = cancel::set_state(cancel::DISABLE);
    let opened = open_or_create(&path, flags & (O_CREAT | O_EXCL), mode, value);
    let _ = cancel::set_state(old);

    let _guard = TABLE_LOCK.lock();
    let (ino, map) = match opened {
        Ok(found) => found,
        Err(error) => {
            if let Some(entry) = TABLE_SEM.get(slot) {
                entry.store(null_mut(), Ordering::SeqCst);
            }
            errno::set(error);
            return FAILED;
        }
    };
    // The same file already mapped: share that mapping.
    let existing = TABLE_SEM.iter().enumerate().position(|(i, entry)| {
        let sem = entry.load(Ordering::SeqCst);
        i != slot
            && !sem.is_null()
            && sem != reserved()
            && TABLE_INO
                .get(i)
                .is_some_and(|n| n.load(Ordering::SeqCst) == ino)
    });
    let (index, sem) = match existing {
        Some(i) => {
            unmap(map);
            if let Some(entry) = TABLE_SEM.get(slot) {
                entry.store(null_mut(), Ordering::SeqCst);
            }
            (
                i,
                TABLE_SEM.get(i).map_or(map, |e| e.load(Ordering::SeqCst)),
            )
        }
        None => (slot, map),
    };
    if let (Some(entry), Some(inode), Some(refs)) = (
        TABLE_SEM.get(index),
        TABLE_INO.get(index),
        TABLE_REFS.get(index),
    ) {
        entry.store(sem, Ordering::SeqCst);
        inode.store(ino, Ordering::SeqCst);
        let _ = refs.fetch_add(1, Ordering::SeqCst);
    }
    sem
}

/// Closes a named semaphore `sem_open` returned, unmapping it once every
/// open of it is closed. Fails with `EINVAL` for anything else.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sem_close(sem: *mut Sem) -> c_int {
    let guard = TABLE_LOCK.lock();
    let Some(i) = TABLE_SEM
        .iter()
        .position(|entry| !sem.is_null() && entry.load(Ordering::SeqCst) == sem)
    else {
        drop(guard);
        return fail(errno::EINVAL);
    };
    let (Some(entry), Some(inode), Some(refs)) =
        (TABLE_SEM.get(i), TABLE_INO.get(i), TABLE_REFS.get(i))
    else {
        return 0;
    };
    if refs.fetch_sub(1, Ordering::SeqCst) > 1 {
        return 0;
    }
    entry.store(null_mut(), Ordering::SeqCst);
    inode.store(0, Ordering::SeqCst);
    drop(guard);
    unmap(sem);
    0
}

/// Removes the named semaphore `name`. Processes that have it open keep it.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sem_unlink(name: *const c_char) -> c_int {
    let mut path: PathBuf = [0; 9 + NAME_MAX + 1];
    // SAFETY: the caller passes a NUL-terminated string.
    if let Err(error) = unsafe { map_name(name, &mut path) } {
        return fail(error);
    }
    match unlink(path.as_ptr()) {
        Ok(()) => 0,
        Err(error) => fail(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapped(name: &core::ffi::CStr) -> Result<Vec<u8>, c_int> {
        let mut buf: PathBuf = [0xff; 9 + NAME_MAX + 1];
        // SAFETY: `name` is NUL-terminated.
        unsafe { map_name(name.as_ptr(), &mut buf) }?;
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Ok(buf.get(..end).unwrap_or(&[]).to_vec())
    }

    #[test]
    fn names_map_into_dev_shm_and_bad_ones_are_refused() {
        assert_eq!(mapped(c"//sem").as_deref(), Ok(&b"/dev/shm/sem"[..]));
        assert_eq!(mapped(c"a/b"), Err(errno::EINVAL));
        assert_eq!(mapped(c"/"), Err(errno::EINVAL));
        assert_eq!(mapped(c".."), Err(errno::EINVAL));
        assert_eq!(mapped(c"."), Err(errno::EINVAL));
        assert_eq!(mapped(c"..."), Ok(b"/dev/shm/...".to_vec()));
        let long = std::ffi::CString::new(vec![b'x'; 256]).unwrap_or_default();
        assert_eq!(mapped(&long), Err(errno::ENAMETOOLONG));
    }

    #[test]
    fn a_semaphore_counts_and_refuses_when_empty_or_full() {
        let sem = Sem::new(1, false);
        assert!(sem.try_take());
        assert!(!sem.try_take());
        let p = (&raw const sem).cast_mut();
        // SAFETY: `sem` is a live local, here and below.
        assert_eq!(unsafe { sem_post(p) }, 0);
        let mut value = -1;
        // SAFETY: as above, and `value` is a live local.
        assert_eq!(unsafe { sem_getvalue(p, &raw mut value) }, 0);
        assert_eq!(value, 1);
        let full = Sem::new(VALUE_MAX, false);
        // SAFETY: `full` is a live local.
        assert_eq!(unsafe { sem_post((&raw const full).cast_mut()) }, -1);
    }

    #[test]
    fn a_temporary_name_holds_the_nanoseconds() {
        let mut buf = [0xff_u8; 40];
        temporary_name(123_456_789, &mut buf);
        assert!(buf.starts_with(b"/dev/shm/tmp-123456789\0"));
    }
}
