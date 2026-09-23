//! `locale.h` and `langinfo.h`: locales, as musl has them.
//!
//! The design is musl 1.2.5's (MIT), from `src/locale/`, and the behaviour a
//! program sees is meant to be musl's exactly.
//!
//! # Two locales
//!
//! Only two locales are built in. A locale is six categories, and each
//! category is either the C locale, a null [`Map`] pointer, or a [`Map`] that
//! holds a name. Where it matters, and so far that is only `LC_CTYPE`, a
//! non-null map means UTF-8:
//!
//! * **C** (also `POSIX`) is byte-transparent. `MB_CUR_MAX` is 1, and every
//!   byte is a character: 0x00 to 0x7f are ASCII, and 0x80 to 0xff are the wide
//!   characters U+DF80 to U+DFFF, which a UTF-8 decoder can never produce, so
//!   no byte string is ever invalid.
//! * **C.UTF-8** has UTF-8 in `LC_CTYPE`, and is C in every other category.
//!
//! There are no locale files. As in musl without `MUSL_LOCPATH`, any other
//! name is accepted: it gets a [`Map`] recording the name, so that
//! `setlocale` can give it back, and behaves as C.UTF-8. So `en_US.UTF-8`
//! works, and so does `de_DE.ISO-8859-1`, which is then UTF-8 all the same. A
//! name of 24 bytes or more, with a `/`, or starting with `.` is taken as
//! C.UTF-8. Nothing can fail to load, so `setlocale` only fails for a category
//! that does not exist.
//!
//! An empty name is looked up in the environment: `LC_ALL`, then the
//! category's own `LC_*` variable, then `LANG`, and C.UTF-8 when none of them
//! is set and non-empty. A program starts in the C locale until it calls
//! `setlocale`.
//!
//! `setlocale(LC_ALL, …)` names a locale whose categories differ as their six
//! names joined by `;`, in category order, as in `C.UTF-8;C;C;C;C;C`, and
//! accepts that string back.
//!
//! # The current locale
//!
//! The global locale is what `setlocale` changes. A thread may instead use a
//! locale object of its own through `uselocale`. The thread control block
//! holds that object, or null while the thread follows the global locale,
//! which `uselocale` reports as `LC_GLOBAL_LOCALE`.
//!
//! `newlocale` returns one of four static locales without allocating when the
//! result equals one: C, C.UTF-8, the environment's default, or C with the
//! default's `LC_CTYPE`. `freelocale` frees only what was allocated.
//!
//! # Where this departs from musl
//!
//! musl dereferences whatever `locale_t` it is given. Here `LC_GLOBAL_LOCALE`
//! passed to `newlocale`, `duplocale` or an `_l` function means the global
//! locale, and a null `locale_t` passed to an `_l` function means C. A null
//! name given to `newlocale`, or a null locale given to `duplocale`, fails with
//! `EINVAL`. POSIX leaves all of these undefined, and musl faults.
//!
//! Message catalogues and `MUSL_LOCPATH` are not supported: every string is
//! the C locale's.

use core::cell::UnsafeCell;
use core::ffi::{CStr, c_char, c_int};
use core::mem::size_of;
use core::ptr::{null, null_mut, without_provenance_mut};
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use crate::errno;
use crate::lock::SpinLock;
use crate::malloc;
use crate::stdlib::getenv;

/// `LC_CTYPE`: character classes and the multibyte encoding.
pub const LC_CTYPE: c_int = 0;
/// `LC_NUMERIC`: the decimal point.
pub const LC_NUMERIC: c_int = 1;
/// `LC_TIME`: day and month names and date formats.
pub const LC_TIME: c_int = 2;
/// `LC_COLLATE`: string ordering.
pub const LC_COLLATE: c_int = 3;
/// `LC_MONETARY`: currency formatting.
pub const LC_MONETARY: c_int = 4;
/// `LC_MESSAGES`: messages and yes/no answers.
pub const LC_MESSAGES: c_int = 5;
/// `LC_ALL`: every category.
pub const LC_ALL: c_int = 6;

/// How many categories a locale has.
const CATEGORIES: usize = 6;

/// The longest locale name kept, as musl's `LOCALE_NAME_MAX`.
const NAME_MAX: usize = 23;

/// `LC_GLOBAL_LOCALE`: `(locale_t)-1`.
pub const LC_GLOBAL_LOCALE: *mut Locale = without_provenance_mut(usize::MAX);

/// `CODESET`, the `nl_langinfo` item naming the character encoding.
const CODESET: c_int = 14;

/// A category's data: its name, and nothing else, since there are no locale
/// files. Maps are never freed, so a name `setlocale` returns stays valid.
#[repr(C)]
#[derive(Debug)]
pub struct Map {
    /// The name, NUL-terminated.
    name: [u8; NAME_MAX + 1],
    /// The next map made for a name, or null.
    next: *const Map,
}

// SAFETY: a map is never written after it is made and linked in, so threads
// may share it.
unsafe impl Sync for Map {}

/// `name` in a NUL-padded name buffer. `name` must be shorter than the buffer.
#[allow(
    clippy::indexing_slicing,
    reason = "evaluated at compile time, where an index out of range fails the build"
)]
const fn name_buffer(name: &[u8]) -> [u8; NAME_MAX + 1] {
    let mut buffer = [0; NAME_MAX + 1];
    let mut i = 0;
    while i < name.len() {
        buffer[i] = name[i];
        i += 1;
    }
    buffer
}

/// The name C.UTF-8 is given.
const C_UTF8_NAME: &[u8] = b"C.UTF-8";

/// C.UTF-8's `LC_CTYPE`.
static C_DOT_UTF8: Map = Map {
    name: name_buffer(C_UTF8_NAME),
    next: null(),
};

/// The name of a category that is the C locale.
const C_NAME: &CStr = c"C";

/// `struct __locale_struct`, which a `locale_t` points at: one [`Map`] per
/// category, null for the C locale.
///
/// A program never looks inside, so the layout is this library's. Each
/// category is an atomic so that the global locale can be changed while other
/// threads read it.
#[repr(C)]
#[derive(Debug)]
pub struct Locale {
    /// Each category's map.
    categories: [AtomicPtr<Map>; CATEGORIES],
}

impl Locale {
    /// A locale with these categories.
    const fn new(categories: [*const Map; CATEGORIES]) -> Self {
        let [ctype, numeric, time, collate, monetary, messages] = categories;
        Self {
            categories: [
                AtomicPtr::new(ctype.cast_mut()),
                AtomicPtr::new(numeric.cast_mut()),
                AtomicPtr::new(time.cast_mut()),
                AtomicPtr::new(collate.cast_mut()),
                AtomicPtr::new(monetary.cast_mut()),
                AtomicPtr::new(messages.cast_mut()),
            ],
        }
    }

    /// Every category's map.
    fn load(&self) -> [*const Map; CATEGORIES] {
        let mut maps = [null(); CATEGORIES];
        for (map, category) in maps.iter_mut().zip(&self.categories) {
            *map = category.load(Ordering::Relaxed).cast_const();
        }
        maps
    }

    /// Sets every category's map.
    fn store(&self, maps: [*const Map; CATEGORIES]) {
        for (category, map) in self.categories.iter().zip(maps) {
            category.store(map.cast_mut(), Ordering::Relaxed);
        }
    }

    /// Category `category`'s map, or null for C or a category that does not
    /// exist.
    fn category(&self, category: usize) -> *const Map {
        self.categories
            .get(category)
            .map_or(null(), |map| map.load(Ordering::Relaxed).cast_const())
    }

    /// Sets category `category`'s map.
    fn set_category(&self, category: usize, map: *const Map) {
        if let Some(slot) = self.categories.get(category) {
            slot.store(map.cast_mut(), Ordering::Relaxed);
        }
    }

    /// Whether `LC_CTYPE` is UTF-8.
    pub fn is_utf8(&self) -> bool {
        !self.category(LC_CTYPE as usize).is_null()
    }
}

/// The C locale.
static C_LOCALE: Locale = Locale::new([null(); CATEGORIES]);

/// C.UTF-8.
static UTF8_LOCALE: Locale = Locale::new([
    &raw const C_DOT_UTF8,
    null(),
    null(),
    null(),
    null(),
    null(),
]);

/// The locale the environment names, once [`DEFAULTS_MADE`] is set.
static DEFAULT_LOCALE: Locale = Locale::new([null(); CATEGORIES]);

/// C, with [`DEFAULT_LOCALE`]'s `LC_CTYPE`, once [`DEFAULTS_MADE`] is set.
static DEFAULT_CTYPE_LOCALE: Locale = Locale::new([null(); CATEGORIES]);

/// Whether [`DEFAULT_LOCALE`] and [`DEFAULT_CTYPE_LOCALE`] have been filled in.
static DEFAULTS_MADE: AtomicBool = AtomicBool::new(false);

/// The global locale, which `setlocale` changes. A program starts in C.
static GLOBAL: Locale = Locale::new([null(); CATEGORIES]);

/// The newest map made for a name, the head of a list linked by `next`.
static NAMED_MAPS: AtomicPtr<Map> = AtomicPtr::new(null_mut());

/// Guards changes to the global locale, the name list and the defaults. It is
/// held across `malloc`, which may make a system call, as musl holds its
/// locale lock.
static LOCK: SpinLock = SpinLock::new();

/// The environment variable for each category, in category order.
const CATEGORY_VARIABLES: [&CStr; CATEGORIES] = [
    c"LC_CTYPE",
    c"LC_NUMERIC",
    c"LC_TIME",
    c"LC_COLLATE",
    c"LC_MONETARY",
    c"LC_MESSAGES",
];

/// The non-empty value of the environment variable `name`, if it has one.
fn environment(name: &CStr) -> Option<&'static [u8]> {
    // SAFETY: `name` is NUL-terminated, and the environment is the program's.
    let value = unsafe { getenv(name.as_ptr()) };
    if value.is_null() {
        return None;
    }
    // SAFETY: `getenv` returns a NUL-terminated string.
    let bytes = unsafe { CStr::from_ptr(value) }.to_bytes();
    (!bytes.is_empty()).then_some(bytes)
}

/// The name a category given `name` gets: the environment's choice for an
/// empty name, and C.UTF-8 for a name that is too long, contains `/` or
/// starts with `.`.
fn effective_name(category: usize, name: &[u8]) -> &[u8] {
    let name = if name.is_empty() {
        environment(c"LC_ALL")
            .or_else(|| {
                CATEGORY_VARIABLES
                    .get(category)
                    .and_then(|v| environment(v))
            })
            .or_else(|| environment(c"LANG"))
            .unwrap_or(C_UTF8_NAME)
    } else {
        name
    };
    let kept = name
        .iter()
        .take(NAME_MAX)
        .take_while(|&&byte| byte != b'/')
        .count();
    if name.first() == Some(&b'.') || kept < name.len() {
        C_UTF8_NAME
    } else {
        name
    }
}

/// A map's name, without its NUL.
fn map_name(map: &Map) -> &[u8] {
    let len = map
        .name
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(NAME_MAX);
    map.name.get(..len).unwrap_or_default()
}

/// The map for category `category` named `name`: musl's `__get_locale`.
///
/// C and POSIX are null. C.UTF-8 is [`C_DOT_UTF8`] for `LC_CTYPE`, and null
/// for every other category, where it is C. Any other name gets a map of its
/// own, made once and kept. If the map cannot be allocated the category is C,
/// or C.UTF-8 for `LC_CTYPE`, which is never C unless C was asked for.
///
/// The caller holds [`LOCK`].
fn get_locale(category: usize, name: &[u8]) -> *const Map {
    let name = effective_name(category, name);
    if name == b"C" || name == b"POSIX" || name == C_UTF8_NAME {
        return if category == LC_CTYPE as usize && name == C_UTF8_NAME {
            &raw const C_DOT_UTF8
        } else {
            null()
        };
    }

    let mut at = NAMED_MAPS.load(Ordering::Relaxed).cast_const();
    while !at.is_null() {
        // SAFETY: every map on the list was made by this function and is
        // never freed.
        let map = unsafe { &*at };
        if map_name(map) == name {
            return at;
        }
        at = map.next;
    }

    let new = malloc::malloc(size_of::<Map>()).cast::<Map>();
    if new.is_null() {
        return if category == LC_CTYPE as usize {
            &raw const C_DOT_UTF8
        } else {
            null()
        };
    }
    let mut buffer = [0; NAME_MAX + 1];
    for (to, &from) in buffer.iter_mut().zip(name) {
        *to = from;
    }
    // SAFETY: `malloc` returned room for a map, aligned for anything.
    unsafe {
        new.write(Map {
            name: buffer,
            next: NAMED_MAPS.load(Ordering::Relaxed),
        });
    }
    NAMED_MAPS.store(new, Ordering::Relaxed);
    new
}

/// The string `name`, as bytes. A null pointer is treated as empty.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string that outlives the use.
unsafe fn bytes<'a>(name: *const c_char) -> &'a [u8] {
    if name.is_null() {
        return &[];
    }
    // SAFETY: the caller passes a NUL-terminated string.
    unsafe { CStr::from_ptr(name) }.to_bytes()
}

/// The name `setlocale` gives a category with this map.
fn category_name(map: *const Map) -> *mut c_char {
    if map.is_null() {
        return C_NAME.as_ptr().cast_mut();
    }
    // SAFETY: a non-null category map is a static or a map `get_locale` made,
    // and neither is freed.
    unsafe { &*map }.name.as_ptr().cast::<c_char>().cast_mut()
}

/// The buffer `setlocale(LC_ALL, …)` composes a mixed locale's name in: six
/// names of at most 23 bytes, each followed by `;` or the final NUL.
struct CompositeName(UnsafeCell<[u8; CATEGORIES * (NAME_MAX + 1)]>);

// SAFETY: only `setlocale` writes the buffer, while holding `LOCK`.
unsafe impl Sync for CompositeName {}

/// Where `setlocale(LC_ALL, …)` returns a mixed locale's name.
static COMPOSITE_NAME: CompositeName =
    CompositeName(UnsafeCell::new([0; CATEGORIES * (NAME_MAX + 1)]));

/// Sets the global locale from a name `setlocale(LC_ALL, name)` was given:
/// one name for every category, or six joined by `;`.
///
/// As in musl, a part too long to be a name leaves that category with the
/// previous part and is not stepped past, and a string with fewer than six
/// parts gives the last one to the remaining categories.
///
/// The caller holds [`LOCK`].
fn set_all(name: &[u8]) {
    let mut part = [0; NAME_MAX];
    let mut part_len = 0;
    for (to, &from) in part.iter_mut().zip(C_UTF8_NAME) {
        *to = from;
        part_len += 1;
    }
    let mut rest = name;
    let mut maps = [null(); CATEGORIES];
    for (category, map) in maps.iter_mut().enumerate() {
        let end = rest
            .iter()
            .position(|&byte| byte == b';')
            .unwrap_or(rest.len());
        if end <= NAME_MAX {
            part_len = 0;
            for (to, &from) in part.iter_mut().zip(rest.iter().take(end)) {
                *to = from;
                part_len += 1;
            }
            if end < rest.len() {
                rest = rest.get(end + 1..).unwrap_or_default();
            }
        }
        *map = get_locale(category, part.get(..part_len).unwrap_or_default());
    }
    GLOBAL.store(maps);
}

/// The global locale's name for `LC_ALL`: one name if every category has the
/// same map, and otherwise the six names joined by `;` in [`COMPOSITE_NAME`].
///
/// The caller holds [`LOCK`].
fn all_name() -> *mut c_char {
    let maps = GLOBAL.load();
    if maps.iter().all(|&map| map == maps[0]) {
        return category_name(maps[0]);
    }
    let buffer = COMPOSITE_NAME.0.get().cast::<u8>();
    let mut at = 0;
    for map in maps {
        let name = if map.is_null() {
            C_NAME.to_bytes()
        } else {
            // SAFETY: as in `category_name`.
            map_name(unsafe { &*map })
        };
        for &byte in name {
            // SAFETY: each name is at most 23 bytes and six of them with
            // their separators fill the buffer at most.
            unsafe { buffer.wrapping_add(at).write(byte) };
            at += 1;
        }
        // SAFETY: as above.
        unsafe { buffer.wrapping_add(at).write(b';') };
        at += 1;
    }
    // SAFETY: the last separator becomes the NUL.
    unsafe { buffer.wrapping_add(at - 1).write(0) };
    buffer.cast()
}

/// Sets or queries the global locale's category `category`, or all of them
/// for `LC_ALL`.
///
/// With a null `name` it only returns the current name. Returns null for a
/// category that does not exist. The string returned is static and may be
/// overwritten by the next call.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn setlocale(category: c_int, name: *const c_char) -> *mut c_char {
    let Some(category) = usize::try_from(category)
        .ok()
        .filter(|&c| c <= LC_ALL as usize)
    else {
        return null_mut();
    };
    let _guard = LOCK.lock();
    if category == LC_ALL as usize {
        if !name.is_null() {
            // SAFETY: the caller passes a NUL-terminated string.
            set_all(unsafe { bytes(name) });
        }
        return all_name();
    }
    let map = if name.is_null() {
        GLOBAL.category(category)
    } else {
        // SAFETY: the caller passes a NUL-terminated string.
        let map = get_locale(category, unsafe { bytes(name) });
        GLOBAL.set_category(category, map);
        map
    };
    category_name(map)
}

/// `struct lconv`, from `locale.h`.
#[repr(C)]
#[derive(Debug)]
pub struct Lconv {
    decimal_point: *const c_char,
    thousands_sep: *const c_char,
    grouping: *const c_char,
    int_curr_symbol: *const c_char,
    currency_symbol: *const c_char,
    mon_decimal_point: *const c_char,
    mon_thousands_sep: *const c_char,
    mon_grouping: *const c_char,
    positive_sign: *const c_char,
    negative_sign: *const c_char,
    int_frac_digits: c_char,
    frac_digits: c_char,
    p_cs_precedes: c_char,
    p_sep_by_space: c_char,
    n_cs_precedes: c_char,
    n_sep_by_space: c_char,
    p_sign_posn: c_char,
    n_sign_posn: c_char,
    int_p_cs_precedes: c_char,
    int_p_sep_by_space: c_char,
    int_n_cs_precedes: c_char,
    int_n_sep_by_space: c_char,
    int_p_sign_posn: c_char,
    int_n_sign_posn: c_char,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Lconv>() == 96);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<Lconv>() == 56);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::offset_of!(Lconv, int_frac_digits) == 80);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::offset_of!(Lconv, int_n_sign_posn) == 93);

// SAFETY: the one `Lconv` is static, its strings are static, and neither is
// ever written.
unsafe impl Sync for Lconv {}

/// The C locale's conventions: POSIX's values, as in musl. No category but C
/// exists for `LC_NUMERIC` or `LC_MONETARY`, so these hold in every locale.
static POSIX_LCONV: Lconv = Lconv {
    decimal_point: c".".as_ptr(),
    thousands_sep: c"".as_ptr(),
    grouping: c"".as_ptr(),
    int_curr_symbol: c"".as_ptr(),
    currency_symbol: c"".as_ptr(),
    mon_decimal_point: c"".as_ptr(),
    mon_thousands_sep: c"".as_ptr(),
    mon_grouping: c"".as_ptr(),
    positive_sign: c"".as_ptr(),
    negative_sign: c"".as_ptr(),
    int_frac_digits: c_char::MAX,
    frac_digits: c_char::MAX,
    p_cs_precedes: c_char::MAX,
    p_sep_by_space: c_char::MAX,
    n_cs_precedes: c_char::MAX,
    n_sep_by_space: c_char::MAX,
    p_sign_posn: c_char::MAX,
    n_sign_posn: c_char::MAX,
    int_p_cs_precedes: c_char::MAX,
    int_p_sep_by_space: c_char::MAX,
    int_n_cs_precedes: c_char::MAX,
    int_n_sep_by_space: c_char::MAX,
    int_p_sign_posn: c_char::MAX,
    int_n_sign_posn: c_char::MAX,
};

/// The numeric and monetary conventions of the current locale, which are
/// always the C locale's. The structure must not be written.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn localeconv() -> *mut Lconv {
    (&raw const POSIX_LCONV).cast_mut()
}

/// Where the calling thread keeps its `uselocale` locale: null while it
/// follows the global locale.
#[cfg(not(test))]
fn thread_slot() -> *mut *mut Locale {
    crate::thread::locale_location()
}

#[cfg(test)]
std::thread_local! {
    static THREAD_LOCALE: core::cell::Cell<*mut Locale> = const { core::cell::Cell::new(null_mut()) };
}

/// Where the calling thread keeps its `uselocale` locale: in unit tests, a
/// Rust thread-local, since the thread pointer belongs to the host's C
/// library.
#[cfg(test)]
fn thread_slot() -> *mut *mut Locale {
    THREAD_LOCALE.with(core::cell::Cell::as_ptr)
}

/// The calling thread's current locale.
pub fn current() -> &'static Locale {
    // SAFETY: the slot is the calling thread's, and lives as long as it.
    let locale = unsafe { thread_slot().read() };
    if locale.is_null() {
        &GLOBAL
    } else {
        // SAFETY: `uselocale` stored a locale object, which POSIX requires
        // the program to keep valid while any thread uses it.
        unsafe { &*locale }
    }
}

/// The locale a `locale_t` argument names: the global locale for
/// `LC_GLOBAL_LOCALE`, and C for null.
///
/// # Safety
///
/// `locale` must be null, `LC_GLOBAL_LOCALE`, or a locale object that stays
/// valid while the result is used.
pub unsafe fn from_c(locale: *mut Locale) -> &'static Locale {
    if locale.is_null() {
        &C_LOCALE
    } else if locale == LC_GLOBAL_LOCALE {
        &GLOBAL
    } else {
        // SAFETY: the caller passes a valid locale object.
        unsafe { &*locale }
    }
}

/// Whether the calling thread's `LC_CTYPE` is UTF-8, so that `MB_CUR_MAX` is
/// 4, rather than C, where it is 1.
pub fn current_is_utf8() -> bool {
    current().is_utf8()
}

/// `MB_CUR_MAX`, which `stdlib.h` defines as a call to this: the most bytes
/// one character takes in the current locale.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __ctype_get_mb_cur_max() -> usize {
    if current_is_utf8() { 4 } else { 1 }
}

/// Whether `locale` is one `newlocale` or `duplocale` allocated, and so may be
/// changed in place and freed.
fn is_allocated(locale: *const Locale) -> bool {
    !locale.is_null()
        && locale != LC_GLOBAL_LOCALE.cast_const()
        && locale != &raw const C_LOCALE
        && locale != &raw const UTF8_LOCALE
        && locale != &raw const DEFAULT_LOCALE
        && locale != &raw const DEFAULT_CTYPE_LOCALE
        && locale != &raw const GLOBAL
}

/// A new locale object holding `maps`, or null with `errno` set to `ENOMEM`.
fn allocate(maps: [*const Map; CATEGORIES]) -> *mut Locale {
    let new = malloc::malloc(size_of::<Locale>()).cast::<Locale>();
    if !new.is_null() {
        // SAFETY: `malloc` returned room for a locale, aligned for anything.
        unsafe { new.write(Locale::new(maps)) };
    }
    new
}

/// musl's `do_newlocale`. The caller holds [`LOCK`].
///
/// # Safety
///
/// As [`newlocale`], with `name` checked to be non-null where used.
unsafe fn make_locale(mask: c_int, name: &[u8], base: *mut Locale) -> *mut Locale {
    let base_maps = if base.is_null() {
        None
    } else {
        // SAFETY: the caller passes a valid locale object or
        // `LC_GLOBAL_LOCALE`.
        Some(unsafe { from_c(base) }.load())
    };
    let mut maps = [null(); CATEGORIES];
    for (category, map) in maps.iter_mut().enumerate() {
        let chosen = mask & (1 << category) != 0;
        *map = match base_maps {
            Some(base_maps) if !chosen => base_maps.get(category).copied().unwrap_or(null()),
            _ => get_locale(category, if chosen { name } else { &[] }),
        };
    }

    if is_allocated(base) {
        // SAFETY: an allocated locale is a valid object the program owns.
        unsafe { &*base }.store(maps);
        return base;
    }

    for builtin in [&C_LOCALE, &UTF8_LOCALE] {
        if builtin.load() == maps {
            return (&raw const *builtin).cast_mut();
        }
    }
    if !DEFAULTS_MADE.load(Ordering::Relaxed) {
        let mut defaults = [null(); CATEGORIES];
        for (category, map) in defaults.iter_mut().enumerate() {
            *map = get_locale(category, &[]);
        }
        DEFAULT_LOCALE.store(defaults);
        DEFAULT_CTYPE_LOCALE.set_category(LC_CTYPE as usize, defaults[0]);
        DEFAULTS_MADE.store(true, Ordering::Relaxed);
    }
    for builtin in [&DEFAULT_LOCALE, &DEFAULT_CTYPE_LOCALE] {
        if builtin.load() == maps {
            return (&raw const *builtin).cast_mut();
        }
    }
    allocate(maps)
}

/// A locale with the categories in `mask` from the locale `name`, and the
/// rest from `base`, or from the environment if `base` is null.
///
/// A `base` that `newlocale` or `duplocale` allocated is changed in place and
/// returned. Otherwise the result is a static locale when one matches, or a
/// new object. Returns null with `errno` set to `ENOMEM` if it cannot be
/// allocated, or to `EINVAL` if `name` is null and `mask` names a category.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string. `base` must be null,
/// `LC_GLOBAL_LOCALE` or a valid locale object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn newlocale(
    mask: c_int,
    name: *const c_char,
    base: *mut Locale,
) -> *mut Locale {
    if name.is_null() && mask & ((1 << CATEGORIES) - 1) != 0 {
        errno::set(errno::EINVAL);
        return null_mut();
    }
    let _guard = LOCK.lock();
    // SAFETY: the caller passes a NUL-terminated string or null.
    let name = unsafe { bytes(name) };
    // SAFETY: the caller passes a valid `base`.
    unsafe { make_locale(mask, name, base) }
}

/// A new locale object with `old`'s categories, or null with `errno` set to
/// `ENOMEM`. `LC_GLOBAL_LOCALE` copies the global locale. A null `old` fails
/// with `EINVAL`.
///
/// # Safety
///
/// `old` must be null, `LC_GLOBAL_LOCALE` or a valid locale object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn duplocale(old: *mut Locale) -> *mut Locale {
    if old.is_null() {
        errno::set(errno::EINVAL);
        return null_mut();
    }
    // SAFETY: the caller passes a valid locale object or `LC_GLOBAL_LOCALE`.
    allocate(unsafe { from_c(old) }.load())
}

/// Frees a locale object `newlocale` or `duplocale` made. The static locales
/// they may return are left alone.
///
/// # Safety
///
/// `locale` must be a locale object no thread still uses, and not already
/// freed.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn freelocale(locale: *mut Locale) {
    if is_allocated(locale) {
        // SAFETY: an allocated locale came from `malloc`, and the caller no
        // longer uses it.
        unsafe { malloc::free(locale.cast()) };
    }
}

/// Makes `new` the calling thread's locale, unless it is null, and returns the
/// previous one. `LC_GLOBAL_LOCALE` makes the thread follow the global locale
/// again, and is what a thread following it gets back.
///
/// # Safety
///
/// `new` must be null, `LC_GLOBAL_LOCALE` or a locale object that stays valid
/// while the thread uses it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn uselocale(new: *mut Locale) -> *mut Locale {
    let slot = thread_slot();
    // SAFETY: the slot is the calling thread's.
    let old = unsafe { slot.read() };
    if !new.is_null() {
        let stored = if new == LC_GLOBAL_LOCALE {
            null_mut()
        } else {
            new
        };
        // SAFETY: as above.
        unsafe { slot.write(stored) };
    }
    if old.is_null() { LC_GLOBAL_LOCALE } else { old }
}

/// `LC_TIME`'s strings in the C locale, indexed by item number: the days,
/// months, AM and PM, the formats, and the era items, as musl gives them.
const C_TIME: [&CStr; 0x32] = [
    c"Sun",
    c"Mon",
    c"Tue",
    c"Wed",
    c"Thu",
    c"Fri",
    c"Sat",
    c"Sunday",
    c"Monday",
    c"Tuesday",
    c"Wednesday",
    c"Thursday",
    c"Friday",
    c"Saturday",
    c"Jan",
    c"Feb",
    c"Mar",
    c"Apr",
    c"May",
    c"Jun",
    c"Jul",
    c"Aug",
    c"Sep",
    c"Oct",
    c"Nov",
    c"Dec",
    c"January",
    c"February",
    c"March",
    c"April",
    c"May",
    c"June",
    c"July",
    c"August",
    c"September",
    c"October",
    c"November",
    c"December",
    c"AM",
    c"PM",
    c"%a %b %e %T %Y",
    c"%m/%d/%y",
    c"%H:%M:%S",
    c"%I:%M:%S %p",
    c"",
    c"",
    c"%m/%d/%y",
    c"0123456789",
    c"%a %b %e %T %Y",
    c"%H:%M:%S",
];

/// `LC_NUMERIC`'s strings: `RADIXCHAR` and `THOUSEP`.
const C_NUMERIC: [&CStr; 2] = [c".", c""];

/// `LC_MESSAGES`'s strings: `YESEXPR`, `NOEXPR`, `YESSTR` and `NOSTR`.
const C_MESSAGES: [&CStr; 4] = [c"^[yY]", c"^[nN]", c"yes", c"no"];

/// The string for `nl_langinfo` item `item` in `locale`.
fn langinfo(item: c_int, locale: &Locale) -> &'static CStr {
    if item == CODESET {
        return if locale.is_utf8() { c"UTF-8" } else { c"ASCII" };
    }
    let category = item >> 16;
    let Ok(index) = usize::try_from(item & 0xffff) else {
        return c"";
    };
    if index == 0xffff
        && let Some(category) = usize::try_from(category).ok().filter(|&c| c < CATEGORIES)
    {
        // glibc's `_NL_LOCALE_NAME(category)`: the category's name.
        let map = locale.category(category);
        if map.is_null() {
            return C_NAME;
        }
        // SAFETY: category maps are static or never freed.
        let map = unsafe { &*map };
        // Names are at most 23 bytes, so the buffer always holds a NUL.
        return CStr::from_bytes_until_nul(&map.name).unwrap_or(C_NAME);
    }
    let table: &[&'static CStr] = match category {
        LC_NUMERIC => &C_NUMERIC,
        LC_TIME => &C_TIME,
        LC_MESSAGES => &C_MESSAGES,
        _ => &[],
    };
    table.get(index).copied().unwrap_or(c"")
}

/// The string describing `item` in the current locale, such as a month's
/// name. An unknown item gives the empty string. The string must not be
/// written.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn nl_langinfo(item: c_int) -> *mut c_char {
    langinfo(item, current()).as_ptr().cast_mut()
}

/// [`nl_langinfo`] in the locale `locale`.
///
/// # Safety
///
/// `locale` must be null, `LC_GLOBAL_LOCALE` or a valid locale object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn nl_langinfo_l(item: c_int, locale: *mut Locale) -> *mut c_char {
    // SAFETY: the caller passes a valid locale.
    langinfo(item, unsafe { from_c(locale) })
        .as_ptr()
        .cast_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The string at `p`.
    fn text(p: *const c_char) -> &'static str {
        // SAFETY: every pointer tested is to a NUL-terminated static string.
        unsafe { CStr::from_ptr(p) }
            .to_str()
            .unwrap_or("<not UTF-8>")
    }

    #[test]
    fn names_resolve_to_c_c_utf8_or_a_named_utf8_map() {
        let _guard = LOCK.lock();
        assert!(get_locale(0, b"C").is_null());
        assert!(get_locale(0, b"POSIX").is_null());
        assert_eq!(get_locale(0, b"C.UTF-8"), &raw const C_DOT_UTF8);
        assert!(get_locale(1, b"C.UTF-8").is_null());
        let named = get_locale(0, b"en_US.UTF-8");
        assert!(!named.is_null());
        assert_ne!(named, &raw const C_DOT_UTF8);
        // The same name gets the same map, in any category.
        assert_eq!(get_locale(3, b"en_US.UTF-8"), named);
        assert_eq!(text(category_name(named)), "en_US.UTF-8");
        // Names that cannot be names are C.UTF-8.
        assert_eq!(get_locale(0, b"../etc"), &raw const C_DOT_UTF8);
        assert_eq!(get_locale(0, b".hidden"), &raw const C_DOT_UTF8);
        assert_eq!(
            get_locale(0, b"abcdefghijklmnopqrstuvwxyz"),
            &raw const C_DOT_UTF8
        );
        // 23 bytes is still a name.
        let longest = get_locale(0, b"abcdefghijklmnopqrstuvw");
        assert_eq!(text(category_name(longest)), "abcdefghijklmnopqrstuvw");
        // Case matters: this is not the built-in name.
        assert_ne!(get_locale(0, b"c.utf-8"), &raw const C_DOT_UTF8);
    }

    #[test]
    fn langinfo_gives_the_c_locales_strings() {
        let c = &C_LOCALE;
        assert_eq!(langinfo(CODESET, c), c"ASCII");
        assert_eq!(langinfo(CODESET, &UTF8_LOCALE), c"UTF-8");
        assert_eq!(langinfo(0x20000, c), c"Sun");
        assert_eq!(langinfo(0x2000D, c), c"Saturday");
        assert_eq!(langinfo(0x20025, c), c"December");
        assert_eq!(langinfo(0x20028, c), c"%a %b %e %T %Y");
        assert_eq!(langinfo(0x2002F, c), c"0123456789");
        assert_eq!(langinfo(0x20031, c), c"%H:%M:%S");
        assert_eq!(langinfo(0x20032, c), c"");
        assert_eq!(langinfo(0x10000, c), c".");
        assert_eq!(langinfo(0x10001, c), c"");
        assert_eq!(langinfo(0x4000F, c), c"");
        assert_eq!(langinfo(0x50000, c), c"^[yY]");
        assert_eq!(langinfo(0x50003, c), c"no");
        assert_eq!(langinfo(0x50004, c), c"");
        assert_eq!(langinfo(0xffff, &UTF8_LOCALE), c"C.UTF-8");
        assert_eq!(langinfo(0x1ffff, &UTF8_LOCALE), c"C");
        assert_eq!(langinfo(0x6ffff, c), c"");
        assert_eq!(langinfo(-1, c), c"");
        assert_eq!(langinfo(c_int::MIN, c), c"");
    }

    #[test]
    fn uselocale_swaps_the_threads_locale_and_reports_the_global_one() {
        let utf8 = (&raw const UTF8_LOCALE).cast_mut();
        // SAFETY: every locale passed is null, static or `LC_GLOBAL_LOCALE`.
        assert_eq!(unsafe { uselocale(null_mut()) }, LC_GLOBAL_LOCALE);
        // SAFETY: as above.
        assert_eq!(unsafe { uselocale(utf8) }, LC_GLOBAL_LOCALE);
        assert!(current_is_utf8());
        assert_eq!(__ctype_get_mb_cur_max(), 4);
        // SAFETY: as above.
        assert_eq!(unsafe { uselocale(null_mut()) }, utf8);
        // SAFETY: as above.
        assert_eq!(unsafe { uselocale(LC_GLOBAL_LOCALE) }, utf8);
        // SAFETY: as above.
        assert_eq!(unsafe { uselocale(null_mut()) }, LC_GLOBAL_LOCALE);
        assert!(!current_is_utf8());
    }

    #[test]
    fn localeconv_is_posixs() {
        let conv = localeconv();
        // SAFETY: `localeconv` returns a static structure.
        let conv = unsafe { &*conv };
        assert_eq!(text(conv.decimal_point), ".");
        assert_eq!(text(conv.grouping), "");
        // CHAR_MAX: 127 where `char` is signed, 255 on Arm.
        assert_eq!(conv.int_n_sign_posn, c_char::MAX);
    }
}
