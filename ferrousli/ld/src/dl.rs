//! `dlfcn.h`, the loader's half: `dlopen`, `dlsym`, `dlclose`, `dlerror`,
//! `dladdr` and `link.h`'s `dl_iterate_phdr`, over the scope the loader built;
//! and glibc's `dladdr1` and `dlinfo`, with the `struct link_map` they hand
//! out.
//!
//! The C library exports the same names and, loaded by this loader, forwards
//! to these through [`crate::interface`]; the loader exports them too, so
//! that a program with no C library can call them, which is how
//! `tests/link.rs` does. `dladdr1` and `dlinfo` are the loader's alone: the
//! loader's names carry no version, so a program linked against glibc binds
//! them here whatever version it asks for, and a static program has nothing
//! loaded to ask about.
//!
//! # Handles
//!
//! A handle is the address of the object's slot in the scope, which is a
//! fixed array in a `static` and never moves. Every handle given back is
//! checked against the array, so a stale or invented one is an error rather
//! than a wild read.
//!
//! # One lock, never held across anyone else's code
//!
//! The scope is written only by `dlopen`, under [`LOCK`]; every reader takes
//! the lock just long enough to copy what it needs out. Constructors, ifunc
//! resolvers found by `dlsym`, and `dl_iterate_phdr`'s callback all run with
//! the lock released, so any of them may call back in -- a constructor that
//! `dlopen`s, an unwinder's callback that `dladdr`s. The one exception is a
//! library's own `IRELATIVE` resolvers, which run while `dlopen` relocates it,
//! as they run before `main` at start-up.
//!
//! # What is not glibc's
//!
//! * Nothing is ever unloaded, as with musl: `dlclose` answers 0 and the
//!   object stays, which is what makes a handle's lifetime simple.
//! * Every object is global. `RTLD_LOCAL` is taken as `RTLD_GLOBAL`: a later
//!   library can bind to an earlier one's symbols whatever it was opened
//!   with. `RTLD_NOLOAD` is honoured, and `RTLD_LAZY` is `RTLD_NOW`.
//! * `RTLD_NEXT` is refused, with a message: it needs the caller's object,
//!   and a C library forwarding the call has lost it.
//! * A library with thread-local storage of its own cannot be `dlopen`ed yet:
//!   every block is in static TLS (see [`crate::tls`]), laid out before the
//!   program started. It is refused by name, and nothing of it is kept.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int, c_void};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};

use crate::elf::{PT_DYNAMIC, PT_LOAD, Phdr, STT_GNU_IFUNC, st_type};
use crate::object::{MAX_OBJECTS, Object};
use crate::report::Error;
use crate::scope::Scope;
use crate::sym::Found;
use crate::{Arguments, start};

/// `RTLD_NOLOAD`: only answer an object already loaded.
const RTLD_NOLOAD: c_int = 4;

/// `RTLD_NEXT`, as a handle.
const RTLD_NEXT: usize = usize::MAX;

/// `STT_TLS`: a symbol naming thread-local storage.
const STT_TLS: u8 = 6;

/// Held while the scope is read or written. A spin lock: `dlopen` is rare,
/// every other holder copies a few words, and the loader has no futex
/// wrapper of its own.
static LOCK: AtomicBool = AtomicBool::new(false);

/// The lock, released when dropped.
struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        LOCK.store(false, Ordering::Release);
    }
}

/// Take [`LOCK`].
fn lock() -> Guard {
    while LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    Guard
}

/// Run `f` on the scope with the lock held.
pub(crate) fn with_scope<R>(f: impl FnOnce(&Scope) -> R) -> R {
    let _guard = lock();
    f(start::scope())
}

/// What start-up leaves for `dlopen`: the process's arguments, which
/// initialisers are given, and the page size.
struct Startup(UnsafeCell<(Arguments, usize)>);

// SAFETY: written once by `set_arguments` before the program runs, read-only
// afterwards.
unsafe impl Sync for Startup {}

/// See [`Startup`].
static STARTUP: Startup = Startup(UnsafeCell::new((
    (0, core::ptr::null_mut(), core::ptr::null_mut()),
    4096,
)));

/// Record the process's arguments and page size for `dlopen`.
///
/// # Safety
///
/// Called once, by the only thread, before the program runs.
pub(crate) unsafe fn set_arguments(args: Arguments, page_size: usize) {
    // SAFETY: the caller's promise: nothing reads it yet.
    unsafe { STARTUP.0.get().write((args, page_size)) };
}

/// See [`Startup`].
fn startup() -> (Arguments, usize) {
    // SAFETY: written before the program ran, and never again.
    unsafe { STARTUP.0.get().read() }
}

/// The order objects were initialised in, by scope index, so that `dl_fini`
/// finishes them in the reverse: a `dlopen`ed library, initialised after
/// everything loaded at start-up, is finished before any of them.
static ORDER: [AtomicU8; MAX_OBJECTS] = [const { AtomicU8::new(0) }; MAX_OBJECTS];

/// How many entries of [`ORDER`] are written.
static ORDERED: AtomicUsize = AtomicUsize::new(0);

/// Record that object `index` is being initialised.
pub(crate) fn record_initialised(index: usize) {
    let at = ORDERED.fetch_add(1, Ordering::AcqRel);
    if let (Some(slot), Ok(index)) = (ORDER.get(at), u8::try_from(index)) {
        slot.store(index, Ordering::Release);
    }
}

/// The scope indices in the order they were initialised.
pub(crate) fn initialised() -> impl DoubleEndedIterator<Item = usize> {
    let count = ORDERED.load(Ordering::Acquire).min(MAX_OBJECTS);
    ORDER
        .iter()
        .take(count)
        .map(|slot| usize::from(slot.load(Ordering::Acquire)))
}

/// How many objects have ever been added, for `dl_iterate_phdr`'s
/// `dlpi_adds`: an unwinder caches what it found and looks again when this
/// moves.
static ADDS: AtomicU64 = AtomicU64::new(0);

/// Bytes for the names `dlopen` is given, which are the caller's and may be
/// freed once it returns. Names from a `DT_NEEDED` live in their object's
/// string table and need no copy.
const NAME_POOL: usize = 16 * 1024;

/// See [`NAME_POOL`]; used only under [`LOCK`].
struct Names(UnsafeCell<([u8; NAME_POOL], usize)>);

// SAFETY: only touched with `LOCK` held.
unsafe impl Sync for Names {}

/// See [`NAME_POOL`].
static NAMES: Names = Names(UnsafeCell::new(([0; NAME_POOL], 0)));

/// Copy a NUL-terminated name into [`NAMES`], or `None` when the pool is full.
///
/// # Safety
///
/// `LOCK` must be held, and `name` a NUL-terminated string.
unsafe fn keep_name(name: *const c_char) -> Option<*const c_char> {
    // SAFETY: the caller holds the lock, so nothing else has the pool.
    let (pool, used) = unsafe { &mut *NAMES.0.get() };
    let start = *used;
    let mut at = start;
    let mut from = name;
    loop {
        // SAFETY: the name is NUL-terminated, and this stops at the NUL.
        let byte = unsafe { from.read() } as u8;
        *pool.get_mut(at)? = byte;
        at += 1;
        if byte == 0 {
            break;
        }
        from = from.wrapping_add(1);
    }
    *used = at;
    Some(pool.get(start..)?.as_ptr().cast())
}

/// The message `dlerror` gives next, and whether there is one.
struct Message(UnsafeCell<[u8; 512]>);

// SAFETY: written and read only under `LOCK`.
unsafe impl Sync for Message {}

/// See [`Message`].
static MESSAGE: Message = Message(UnsafeCell::new([0; 512]));

/// Whether [`MESSAGE`] holds a failure `dlerror` has not reported.
static PENDING: AtomicBool = AtomicBool::new(false);

/// Record a failure for `dlerror`: `text`, then `": "` and `subject` when
/// there is one, as glibc words it the other way round.
fn fail(text: &str, subject: Option<*const c_char>) {
    let _guard = lock();
    // SAFETY: the lock is held.
    let buffer = unsafe { &mut *MESSAGE.0.get() };
    let mut at = 0;
    // One byte is always left for the terminator.
    let room = buffer.len() - 1;
    let mut put = |byte: u8| {
        if at < room
            && let Some(slot) = buffer.get_mut(at)
        {
            *slot = byte;
            at += 1;
        }
    };
    if let Some(mut name) = subject.filter(|name| !name.is_null()) {
        loop {
            // SAFETY: a subject is a NUL-terminated name, and this stops at
            // the NUL.
            let byte = unsafe { name.read() } as u8;
            if byte == 0 {
                break;
            }
            put(byte);
            name = name.wrapping_add(1);
        }
        put(b':');
        put(b' ');
    }
    for byte in text.bytes() {
        put(byte);
    }
    if let Some(end) = buffer.get_mut(at) {
        *end = 0;
    }
    PENDING.store(true, Ordering::Release);
}

/// [`fail`] from a loader error.
fn fail_with(error: Error) {
    fail(error.text(), error.subject());
}

/// `dlopen(path, flags)`.
///
/// # Safety
///
/// `path` must be null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void {
    if path.is_null() {
        // The program, whose `dlsym` searches everything: glibc's answer.
        return with_scope(|scope| scope.handle(0));
    }
    let (args, page_size) = startup();
    let opened = {
        let _guard = lock();
        // SAFETY: the lock is held, so this is the only reference.
        let scope = unsafe { &mut *start::scope_mut() };
        if flags & RTLD_NOLOAD != 0 {
            // SAFETY: `path` is a NUL-terminated string.
            return match scope.find(path) {
                Some(index) => scope.handle(index),
                None => core::ptr::null_mut(),
            };
        }
        let before = scope.len();
        // SAFETY: the lock is held, and `path` is a NUL-terminated string.
        let Some(name) = (unsafe { keep_name(path) }) else {
            drop(_guard);
            fail("too many names opened", Some(path));
            return core::ptr::null_mut();
        };
        let result = scope.open(name, page_size).and_then(|index| {
            refuse_tls(scope, before)?;
            // SAFETY: every object in the scope is mapped, and those from
            // `before` on are new and not yet relocated.
            unsafe { crate::relocate(scope, before, page_size)? };
            Ok(index)
        });
        match result {
            Ok(index) => {
                let after = scope.len();
                let _ = ADDS.fetch_add((after - before) as u64, Ordering::Relaxed);
                Ok((index, before, after, scope.handle(index)))
            }
            Err(error) => {
                scope.truncate(before);
                Err(error)
            }
        }
    };
    let (_, before, after, handle) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            fail_with(error);
            return core::ptr::null_mut();
        }
    };
    // Dependencies first, as at start-up: the last loaded is initialised
    // first, each without the lock.
    // SAFETY: the new objects are mapped and relocated.
    unsafe { crate::run_range(before, after, args) };
    handle
}

/// Refuse objects from `before` on that bring TLS of their own: there is no
/// room left in anybody's static TLS for them.
fn refuse_tls(scope: &Scope, before: usize) -> Result<(), Error> {
    for index in before..scope.len() {
        if scope.get(index).is_some_and(|object| object.tls.is_some()) {
            return Err(Error::NotAnObject(scope.name_at(index)));
        }
    }
    Ok(())
}

/// `dlsym(handle, name)`.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    if handle.addr() == RTLD_NEXT {
        fail("RTLD_NEXT is not supported", None);
        return core::ptr::null_mut();
    }
    let found = with_scope(|scope| {
        if handle.is_null() {
            // SAFETY: every object in the scope is mapped.
            return Ok(unsafe { scope.lookup(name, None, 0) });
        }
        match scope.index_of_handle(handle) {
            // SAFETY: as above.
            Some(index) => Ok(unsafe { scope.lookup_from(index, name) }),
            None => Err(()),
        }
    });
    let found: Found = match found {
        Ok(Some(found)) => found,
        Ok(None) => {
            fail("undefined symbol", Some(name));
            return core::ptr::null_mut();
        }
        Err(()) => {
            fail("not a handle from dlopen", None);
            return core::ptr::null_mut();
        }
    };
    let address = match st_type(found.info) {
        // A thread-local variable's address is the calling thread's copy.
        STT_TLS => found.tls_offset.map_or(0, |offset| {
            crate::tls::thread_pointer()
                .wrapping_add_signed(offset)
                .wrapping_add(found.value)
        }),
        STT_GNU_IFUNC => {
            // SAFETY: an `STT_GNU_IFUNC` symbol's address is a resolver
            // taking nothing, which picks the implementation.
            let resolver = unsafe {
                core::mem::transmute::<usize, unsafe extern "C" fn() -> usize>(found.address)
            };
            // SAFETY: calling it is how it answers; the lock is released.
            unsafe { resolver() }
        }
        _ => found.address,
    };
    address as *mut c_void
}

/// `dlclose(handle)`: 0 for a handle `dlopen` gave, which stays loaded.
#[unsafe(no_mangle)]
pub(crate) extern "C" fn dlclose(handle: *mut c_void) -> c_int {
    if with_scope(|scope| scope.index_of_handle(handle)).is_some() {
        return 0;
    }
    fail("not a handle from dlopen", None);
    -1
}

/// Where `dlerror` hands its message out from, so that the next failure does
/// not overwrite what a caller is still reading.
static REPORTED: Message = Message(UnsafeCell::new([0; 512]));

/// `dlerror()`: the last failure's message, once, then null until the next.
#[unsafe(no_mangle)]
pub(crate) extern "C" fn dlerror() -> *mut c_char {
    let _guard = lock();
    if !PENDING.swap(false, Ordering::AcqRel) {
        return core::ptr::null_mut();
    }
    // SAFETY: both buffers are only touched under the lock, which is held.
    let message = unsafe { MESSAGE.0.get().read() };
    // SAFETY: as above.
    unsafe { REPORTED.0.get().write(message) };
    REPORTED.0.get().cast()
}

/// `Dl_info`.
#[repr(C)]
#[derive(Debug)]
pub(crate) struct DlInfo {
    /// The object's name.
    dli_fname: *const c_char,
    /// Where it is loaded.
    dli_fbase: *mut c_void,
    /// The nearest symbol at or below the address, or null.
    dli_sname: *const c_char,
    /// That symbol's address, or null.
    dli_saddr: *mut c_void,
}

/// Whether `address` is in one of `object`'s loadable segments.
fn contains(object: &Object, address: usize) -> bool {
    headers(object).any(|header| {
        let start = object.base.wrapping_add(header.p_vaddr as usize);
        header.p_type == PT_LOAD && address >= start && address - start < header.p_memsz as usize
    })
}

/// `object`'s program headers, from where it was mapped.
fn headers(object: &Object) -> impl Iterator<Item = Phdr> + '_ {
    (0..object.phnum).map(move |index| {
        let at = (object.phdr as *const Phdr).wrapping_add(index);
        // SAFETY: `phnum` headers are mapped at `phdr`, which whoever mapped
        // the object recorded.
        unsafe { at.read_unaligned() }
    })
}

/// `dladdr(address, info)`: the object holding `address` and the nearest
/// symbol at or below it. Nonzero when `info` was filled.
///
/// # Safety
///
/// `info` must be valid for writing a `Dl_info`.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn dladdr(address: *const c_void, info: *mut DlInfo) -> c_int {
    let wanted = address.addr();
    let answer = with_scope(|scope| {
        let index = (0..scope.len()).find(|&index| {
            scope
                .get(index)
                .is_some_and(|object| object.phdr != 0 && contains(object, wanted))
        })?;
        let object = scope.get(index)?;
        let base = headers(object)
            .filter(|header| header.p_type == PT_LOAD)
            .map(|header| object.base.wrapping_add(header.p_vaddr as usize))
            .min()
            .unwrap_or(object.base);
        let (sname, saddr) = crate::sym::nearest(object, wanted).unwrap_or_default();
        Some(DlInfo {
            dli_fname: scope.name_at(index),
            dli_fbase: base as *mut c_void,
            dli_sname: sname,
            dli_saddr: saddr as *mut c_void,
        })
    });
    let Some(answer) = answer else {
        return 0;
    };
    // SAFETY: the caller vouches for `info`.
    unsafe { info.write(answer) };
    1
}

/// glibc's `struct link_map`, the part `<link.h>` shows: a loaded object's
/// load bias, name and dynamic section, and its neighbours in load order.
#[repr(C)]
#[derive(Debug)]
pub(crate) struct LinkMap {
    /// The load bias.
    l_addr: usize,
    /// The name it was loaded as.
    l_name: *const c_char,
    /// Its `PT_DYNAMIC`, at its run-time address, or 0.
    l_ld: usize,
    /// The object loaded after it.
    l_next: *mut LinkMap,
    /// The object loaded before it.
    l_prev: *mut LinkMap,
}

/// One [`LinkMap`] for each object in the scope, rebuilt under [`LOCK`]
/// before one is handed out, so that the chain follows every `dlopen`.
struct LinkMaps(UnsafeCell<[LinkMap; MAX_OBJECTS]>);

// SAFETY: written only under `LOCK`; a caller reads its entries as C does,
// and they only ever change to describe more objects.
unsafe impl Sync for LinkMaps {}

/// See [`LinkMaps`].
static LINK_MAPS: LinkMaps = LinkMaps(UnsafeCell::new(
    [const {
        LinkMap {
            l_addr: 0,
            l_name: core::ptr::null(),
            l_ld: 0,
            l_next: core::ptr::null_mut(),
            l_prev: core::ptr::null_mut(),
        }
    }; MAX_OBJECTS],
));

/// The link map of object `index`, after bringing every entry up to date.
/// Called with [`LOCK`] held, through [`with_scope`].
fn link_map(scope: &Scope, index: usize) -> *mut LinkMap {
    let maps = LINK_MAPS.0.get().cast::<LinkMap>();
    let count = scope.len().min(MAX_OBJECTS);
    for i in 0..count {
        let Some(object) = scope.get(i) else {
            continue;
        };
        let dynamic = headers(object)
            .find(|header| header.p_type == PT_DYNAMIC)
            .map_or(0, |header| {
                object.base.wrapping_add(header.p_vaddr as usize)
            });
        let entry = LinkMap {
            l_addr: object.base,
            l_name: scope.name_at(i),
            l_ld: dynamic,
            l_next: if i + 1 < count {
                maps.wrapping_add(i + 1)
            } else {
                core::ptr::null_mut()
            },
            l_prev: if i > 0 {
                maps.wrapping_add(i - 1)
            } else {
                core::ptr::null_mut()
            },
        };
        // SAFETY: `i` is inside the array, and the lock is held.
        unsafe { maps.wrapping_add(i).write(entry) };
    }
    if index < count {
        maps.wrapping_add(index)
    } else {
        core::ptr::null_mut()
    }
}

/// `dladdr1`'s `RTLD_DL_SYMENT`: the symbol's table entry.
const RTLD_DL_SYMENT: c_int = 1;
/// `dladdr1`'s `RTLD_DL_LINKMAP`: the object's link map.
const RTLD_DL_LINKMAP: c_int = 2;

/// `dladdr1(address, info, extra, flags)`: [`dladdr`], and with
/// `RTLD_DL_SYMENT` the nearest symbol's table entry, or with
/// `RTLD_DL_LINKMAP` the object's link map, in `*extra`. libasound calls
/// it.
///
/// # Safety
///
/// `info` must be valid for writing a `Dl_info`, and `extra` for writing a
/// pointer when `flags` asks for one.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn dladdr1(
    address: *const c_void,
    info: *mut DlInfo,
    extra: *mut *mut c_void,
    flags: c_int,
) -> c_int {
    // SAFETY: the caller vouches for `info`.
    if unsafe { dladdr(address, info) } == 0 {
        return 0;
    }
    if flags != RTLD_DL_SYMENT && flags != RTLD_DL_LINKMAP {
        return 1;
    }
    let wanted = address.addr();
    let answer = with_scope(|scope| {
        let index = (0..scope.len()).find(|&index| {
            scope
                .get(index)
                .is_some_and(|object| object.phdr != 0 && contains(object, wanted))
        })?;
        if flags == RTLD_DL_LINKMAP {
            return Some(link_map(scope, index).cast::<c_void>());
        }
        let object = scope.get(index)?;
        crate::sym::nearest_entry(object, wanted).map(|entry| entry.cast_mut().cast())
    });
    // SAFETY: the caller vouches for `extra`.
    unsafe { extra.write(answer.unwrap_or(core::ptr::null_mut())) };
    1
}

/// `dlinfo`'s requests: the namespace, the link map, the origin, and the
/// TLS module number.
const RTLD_DI_LMID: c_int = 1;
/// See [`RTLD_DI_LMID`].
const RTLD_DI_LINKMAP: c_int = 2;
/// See [`RTLD_DI_LMID`].
const RTLD_DI_ORIGIN: c_int = 6;
/// See [`RTLD_DI_LMID`].
const RTLD_DI_TLS_MODID: c_int = 9;

/// `dlinfo(handle, request, arg)`: what `request` asks about the object
/// `handle` names, written to `arg`. 0, or -1 with the reason for
/// `dlerror`. The namespace is always the base one, and the origin is the
/// directory the object was loaded from, as its name gives it. libasound
/// calls it.
///
/// # Safety
///
/// `arg` must be valid for what `request` writes: a `Lmid_t`, a pointer, a
/// `size_t`, or for `RTLD_DI_ORIGIN` a buffer of `PATH_MAX` bytes.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn dlinfo(
    handle: *mut c_void,
    request: c_int,
    arg: *mut c_void,
) -> c_int {
    enum Answer {
        Word(usize),
        Origin(*const c_char),
    }
    let answer = with_scope(|scope| {
        let index = scope.index_of_handle(handle)?;
        let object = scope.get(index)?;
        Some(match request {
            RTLD_DI_LMID => Some(Answer::Word(0)),
            RTLD_DI_LINKMAP => Some(Answer::Word(link_map(scope, index).addr())),
            RTLD_DI_TLS_MODID => Some(Answer::Word(if object.tls.is_some() {
                index + 1
            } else {
                0
            })),
            RTLD_DI_ORIGIN => Some(Answer::Origin(scope.name_at(index))),
            _ => None,
        })
    });
    let answer = match answer {
        None => {
            fail("not a handle from dlopen", None);
            return -1;
        }
        Some(None) => {
            fail("unsupported dlinfo request", None);
            return -1;
        }
        Some(Some(answer)) => answer,
    };
    match answer {
        Answer::Word(word) => {
            // SAFETY: the caller vouches for a word at `arg`.
            unsafe { arg.cast::<usize>().write(word) };
        }
        Answer::Origin(name) => {
            // The name up to its last `/`, or `.` for a name without one.
            let out = arg.cast::<u8>();
            let mut end = None;
            let mut len = 0;
            // SAFETY: the name is NUL-terminated, and read up to its NUL.
            while !name.is_null() && unsafe { name.wrapping_add(len).read() } != 0 {
                // SAFETY: as above.
                if unsafe { name.wrapping_add(len).read() } == b'/' as c_char {
                    end = Some(len);
                }
                len += 1;
            }
            match end {
                Some(0) => {
                    // SAFETY: the caller's buffer holds `PATH_MAX` bytes.
                    unsafe { out.write(b'/') };
                    // SAFETY: as above.
                    unsafe { out.wrapping_add(1).write(0) };
                }
                Some(end) => {
                    for i in 0..end {
                        // SAFETY: `i` is inside the name and the buffer.
                        let byte = unsafe { name.wrapping_add(i).read() } as u8;
                        // SAFETY: as above.
                        unsafe { out.wrapping_add(i).write(byte) };
                    }
                    // SAFETY: as above.
                    unsafe { out.wrapping_add(end).write(0) };
                }
                None => {
                    // SAFETY: as above.
                    unsafe { out.write(b'.') };
                    // SAFETY: as above.
                    unsafe { out.wrapping_add(1).write(0) };
                }
            }
        }
    }
    0
}

/// `struct dl_phdr_info`, glibc's.
#[repr(C)]
#[derive(Debug)]
pub(crate) struct DlPhdrInfo {
    /// The object's load bias.
    dlpi_addr: usize,
    /// Its name.
    dlpi_name: *const c_char,
    /// Its program headers.
    dlpi_phdr: *const Phdr,
    /// How many.
    dlpi_phnum: u16,
    /// Objects ever added to the process.
    dlpi_adds: u64,
    /// Objects ever removed: none, here.
    dlpi_subs: u64,
    /// Its TLS module number, or 0.
    dlpi_tls_modid: usize,
    /// This thread's block of it, or null.
    dlpi_tls_data: *mut c_void,
}

/// A `dl_iterate_phdr` callback.
type Callback = unsafe extern "C" fn(*mut DlPhdrInfo, usize, *mut c_void) -> c_int;

/// `dl_iterate_phdr(callback, data)`: `callback` for every loaded object in
/// load order, until one returns nonzero, which is then returned.
///
/// # Safety
///
/// `callback` must be sound to call with each object's description and
/// `data`.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn dl_iterate_phdr(
    callback: Option<Callback>,
    data: *mut c_void,
) -> c_int {
    let Some(callback) = callback else {
        return 0;
    };
    let mut index = 0;
    loop {
        let described = with_scope(|scope| {
            if index >= scope.len() {
                return None;
            }
            let object = scope.get(index)?;
            Some((object.phdr != 0).then(|| DlPhdrInfo {
                dlpi_addr: object.base,
                dlpi_name: scope.name_at(index),
                dlpi_phdr: object.phdr as *const Phdr,
                dlpi_phnum: u16::try_from(object.phnum).unwrap_or(0),
                dlpi_adds: ADDS.load(Ordering::Relaxed).max(scope.len() as u64),
                dlpi_subs: 0,
                dlpi_tls_modid: if object.tls.is_some() { index + 1 } else { 0 },
                dlpi_tls_data: object.tls.map_or(core::ptr::null_mut(), |tls| {
                    crate::tls::thread_pointer().wrapping_add_signed(tls.offset) as *mut c_void
                }),
            }))
        });
        let Some(described) = described else {
            return 0;
        };
        index += 1;
        let Some(mut info) = described else {
            continue;
        };
        // SAFETY: the caller vouches for the callback; `info` lives across
        // it, and the lock is released.
        let result = unsafe { callback(&raw mut info, size_of::<DlPhdrInfo>(), data) };
        if result != 0 {
            return result;
        }
    }
}
