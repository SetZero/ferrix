//! `libintl.h`: message translation, as it behaves with no catalogue
//! installed. GLib, ATK, libmount and AT-SPI call these for every string
//! they show.
//!
//! There are no `.mo` catalogues here, so every lookup answers the message
//! it was given, as glibc does in the C locale or for a domain with no
//! catalogue: `gettext` its `msgid`, and `ngettext` `msgid` for 1 and
//! `msgid_plural` for any other count, the English rule glibc falls back
//! to. The bookkeeping is kept as glibc keeps it, because callers read it
//! back: `textdomain` names the current domain, "messages" until one is
//! set; `bindtextdomain` remembers each domain's directory, glibc's
//! `/usr/share/locale` until one is given; and `bind_textdomain_codeset`
//! each domain's code set, none until one is given.
//!
//! POSIX.1-2024's `_l` forms are here too. The answers were compared with
//! the host's glibc 2.43, which gives the same for a domain without a
//! catalogue.

use core::ffi::{c_char, c_int, c_ulong, c_void};
use core::mem::size_of;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::lock::SpinLock;
use crate::malloc::malloc;
use crate::string::{memcpy, strcmp, strlen};

/// The default domain.
const DEFAULT_DOMAIN: &core::ffi::CStr = c"messages";
/// Where glibc looks for catalogues when a domain has not been bound.
const DEFAULT_DIRECTORY: &core::ffi::CStr = c"/usr/share/locale";

/// The current domain, from `malloc`, or null for [`DEFAULT_DOMAIN`].
static DOMAIN: AtomicPtr<c_char> = AtomicPtr::new(null_mut());

/// One domain's bindings.
struct Binding {
    /// The domain, from `malloc`.
    domain: *mut c_char,
    /// Its directory, from `malloc`, or null for the default.
    directory: *mut c_char,
    /// Its code set, from `malloc`, or null for none.
    codeset: *mut c_char,
    /// The binding made before it.
    next: *mut Binding,
}

/// The bindings, newest first. Nothing is ever freed: a caller may hold
/// every string this module returned.
static BINDINGS: AtomicPtr<Binding> = AtomicPtr::new(null_mut());
/// Guards [`DOMAIN`] and [`BINDINGS`] while they change.
static LOCK: SpinLock = SpinLock::new();

/// A copy of the C string `s` from `malloc`, or null if there is no memory.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
unsafe fn copy(s: *const c_char) -> *mut c_char {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { strlen(s) } + 1;
    let out = malloc(len).cast::<c_char>();
    if !out.is_null() {
        // SAFETY: both hold `len` bytes.
        let _ = unsafe { memcpy(out.cast(), s.cast::<c_void>(), len) };
    }
    out
}

/// Whether `s` is null or the empty string.
///
/// # Safety
///
/// `s` must be null or a NUL-terminated string.
unsafe fn empty(s: *const c_char) -> bool {
    // SAFETY: as the caller says.
    s.is_null() || unsafe { s.read() } == 0
}

/// The message for `count` of a thing: `singular` for 1 and `plural`
/// otherwise.
fn choose(singular: *const c_char, plural: *const c_char, count: c_ulong) -> *mut c_char {
    if count == 1 { singular } else { plural }.cast_mut()
}

/// `msgid`, translated in the current domain: `msgid` itself.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn gettext(msgid: *const c_char) -> *mut c_char {
    msgid.cast_mut()
}

/// `msgid`, translated in `domain`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn dgettext(_domain: *const c_char, msgid: *const c_char) -> *mut c_char {
    msgid.cast_mut()
}

/// `msgid`, translated in `domain` for locale category `category`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn dcgettext(
    _domain: *const c_char,
    msgid: *const c_char,
    _category: c_int,
) -> *mut c_char {
    msgid.cast_mut()
}

/// The message for `n` of a thing: `msgid` for 1, `msgid_plural` else.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ngettext(
    msgid: *const c_char,
    msgid_plural: *const c_char,
    n: c_ulong,
) -> *mut c_char {
    choose(msgid, msgid_plural, n)
}

/// [`ngettext`] in `domain`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn dngettext(
    _domain: *const c_char,
    msgid: *const c_char,
    msgid_plural: *const c_char,
    n: c_ulong,
) -> *mut c_char {
    choose(msgid, msgid_plural, n)
}

/// [`ngettext`] in `domain` for locale category `category`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn dcngettext(
    _domain: *const c_char,
    msgid: *const c_char,
    msgid_plural: *const c_char,
    n: c_ulong,
    _category: c_int,
) -> *mut c_char {
    choose(msgid, msgid_plural, n)
}

/// [`gettext`] in `locale`, POSIX.1-2024's.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn gettext_l(msgid: *const c_char, _locale: *mut c_void) -> *mut c_char {
    msgid.cast_mut()
}

/// [`dgettext`] in `locale`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn dgettext_l(
    _domain: *const c_char,
    msgid: *const c_char,
    _locale: *mut c_void,
) -> *mut c_char {
    msgid.cast_mut()
}

/// [`dcgettext`] in `locale`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn dcgettext_l(
    _domain: *const c_char,
    msgid: *const c_char,
    _category: c_int,
    _locale: *mut c_void,
) -> *mut c_char {
    msgid.cast_mut()
}

/// [`ngettext`] in `locale`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ngettext_l(
    msgid: *const c_char,
    msgid_plural: *const c_char,
    n: c_ulong,
    _locale: *mut c_void,
) -> *mut c_char {
    choose(msgid, msgid_plural, n)
}

/// [`dngettext`] in `locale`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn dngettext_l(
    _domain: *const c_char,
    msgid: *const c_char,
    msgid_plural: *const c_char,
    n: c_ulong,
    _locale: *mut c_void,
) -> *mut c_char {
    choose(msgid, msgid_plural, n)
}

/// [`dcngettext`] in `locale`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn dcngettext_l(
    _domain: *const c_char,
    msgid: *const c_char,
    msgid_plural: *const c_char,
    n: c_ulong,
    _category: c_int,
    _locale: *mut c_void,
) -> *mut c_char {
    choose(msgid, msgid_plural, n)
}

/// Makes `domain` the current domain and returns it, or with null returns
/// the current one without changing it. The empty string sets the default,
/// "messages". Null if there is no memory.
///
/// # Safety
///
/// `domain` must be null or a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn textdomain(domain: *const c_char) -> *mut c_char {
    let _guard = LOCK.lock();
    if !domain.is_null() {
        // SAFETY: the caller passes a NUL-terminated string.
        let new = if unsafe { empty(domain) } {
            null_mut()
        } else {
            // SAFETY: as above.
            let new = unsafe { copy(domain) };
            if new.is_null() {
                return null_mut();
            }
            new
        };
        DOMAIN.store(new, Ordering::Release);
    }
    let current = DOMAIN.load(Ordering::Acquire);
    if current.is_null() {
        DEFAULT_DOMAIN.as_ptr().cast_mut()
    } else {
        current
    }
}

/// The binding of `domain`, made if `create` and there is none. Called with
/// [`LOCK`] held.
///
/// # Safety
///
/// `domain` must be a NUL-terminated string.
unsafe fn binding(domain: *const c_char, create: bool) -> *mut Binding {
    let mut node = BINDINGS.load(Ordering::Acquire);
    while !node.is_null() {
        // SAFETY: bindings live for the process.
        let b = unsafe { &*node };
        // SAFETY: both are NUL-terminated.
        if unsafe { strcmp(b.domain, domain) } == 0 {
            return node;
        }
        node = b.next;
    }
    if !create {
        return null_mut();
    }
    let node = malloc(size_of::<Binding>()).cast::<Binding>();
    // SAFETY: the caller passes a NUL-terminated string.
    let name = unsafe { copy(domain) };
    if node.is_null() || name.is_null() {
        return null_mut();
    }
    let binding = Binding {
        domain: name,
        directory: null_mut(),
        codeset: null_mut(),
        next: BINDINGS.load(Ordering::Acquire),
    };
    // SAFETY: fresh memory for one binding.
    unsafe { node.write(binding) };
    BINDINGS.store(node, Ordering::Release);
    node
}

/// Which setting of a binding [`bind`] reads or writes.
#[derive(Clone, Copy)]
enum Setting {
    /// The directory.
    Directory,
    /// The code set.
    Codeset,
}

/// Sets `setting` of `domain` to `value` and returns it, or with a null
/// `value` returns it unchanged, `fallback` if it was never set. Null for a
/// null or empty domain, or when there is no memory.
///
/// # Safety
///
/// Both must be null or NUL-terminated strings.
unsafe fn bind(
    domain: *const c_char,
    value: *const c_char,
    setting: Setting,
    fallback: *mut c_char,
) -> *mut c_char {
    // SAFETY: the caller passes null or a string.
    if unsafe { empty(domain) } {
        return null_mut();
    }
    let _guard = LOCK.lock();
    // SAFETY: as above.
    let node = unsafe { binding(domain, !value.is_null()) };
    if node.is_null() {
        return if value.is_null() {
            fallback
        } else {
            null_mut()
        };
    }
    // SAFETY: a binding, which lives for the process; the lock is held.
    let b = unsafe { &mut *node };
    let slot = match setting {
        Setting::Directory => &mut b.directory,
        Setting::Codeset => &mut b.codeset,
    };
    if !value.is_null() {
        // SAFETY: the caller passes a string.
        let new = unsafe { copy(value) };
        if new.is_null() {
            return null_mut();
        }
        *slot = new;
    }
    if slot.is_null() { fallback } else { *slot }
}

/// Makes `dirname` the directory `domain`'s catalogues are looked for in,
/// and returns it, or with a null `dirname` returns the current one,
/// glibc's `/usr/share/locale` by default. Null for a null or empty
/// domain.
///
/// # Safety
///
/// Both must be null or NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn bindtextdomain(
    domain: *const c_char,
    dirname: *const c_char,
) -> *mut c_char {
    let fallback = DEFAULT_DIRECTORY.as_ptr().cast_mut();
    // SAFETY: the caller's contract.
    unsafe { bind(domain, dirname, Setting::Directory, fallback) }
}

/// Makes `codeset` the code set `domain`'s translations are given in, and
/// returns it, or with a null `codeset` returns the current one, null if
/// none was set.
///
/// # Safety
///
/// Both must be null or NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn bind_textdomain_codeset(
    domain: *const c_char,
    codeset: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { bind(domain, codeset, Setting::Codeset, null_mut()) }
}
