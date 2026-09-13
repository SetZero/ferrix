//! Multibyte and wide characters: the conversions in `stdlib.h`, `wchar.h`
//! and `uchar.h`.
//!
//! The algorithms are musl 1.2.5's (MIT), from `src/multibyte/`.
//!
//! # Encodings
//!
//! The encoding is the calling thread's `LC_CTYPE`, from [`crate::locale`]:
//!
//! * **UTF-8**, strictly. A character is the shortest form of a scalar value:
//!   no overlong forms, no surrogates, nothing above U+10FFFF. Anything else
//!   is `EILSEQ`.
//! * **C**, where every byte is one character. 0x00 to 0x7f are themselves,
//!   and 0x80 to 0xff are U+DF80 to U+DFFF, musl's "code units". A byte string
//!   is never invalid, and only those 128 wide characters and ASCII convert
//!   back.
//!
//! # The conversion state
//!
//! `mbstate_t` is two `unsigned`s, of which only the first is used. Zero is
//! the initial state. In the middle of a UTF-8 sequence it holds musl's
//! decoder state, whose top bit is then always set:
//!
//! * The low bits collect the character's value.
//! * Above them, a lead byte sets two marker bits for each continuation byte
//!   still to come. Each continuation byte shifts the state six bits left and
//!   adds its payload, and when the markers have shifted out of bit 31 the
//!   character is complete.
//! * The top six bits are a negative offset that bounds the next byte. That
//!   is how E0, ED, F0 and F4 narrow their second byte's range to exclude
//!   overlong forms, surrogates and values above U+10FFFF, without a table.
//!
//! `mbrtoc16` keeps a pending low surrogate in the same word, as a positive
//! value, and `c16rtomb` a pending high surrogate's contribution.
//!
//! A function given a null state uses one of its own. Each has a separate
//! one, and none is shared between threads safely, as C permits.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int};
use core::mem::size_of;
use core::ptr::null_mut;

use crate::errno;
use crate::locale;
use crate::string::strlen;

/// `wchar_t`: a signed 32-bit integer on x86-64.
pub type WChar = i32;

/// `wint_t`: an unsigned 32-bit integer.
pub type WInt = u32;

/// `WEOF`.
pub const WEOF: WInt = 0xffff_ffff;

/// `EOF`.
const EOF: c_int = -1;

/// `(size_t)-1`: an invalid sequence.
const INVALID: usize = usize::MAX;
/// `(size_t)-2`: an incomplete sequence, all of which was consumed.
const INCOMPLETE: usize = usize::MAX - 1;
/// `(size_t)-3`: a character from an earlier call, consuming nothing.
const EARLIER: usize = usize::MAX - 2;

/// `MB_LEN_MAX`: the most bytes one character takes in any locale.
const MB_LEN_MAX: usize = 4;

/// `mbstate_t`, from `bits/alltypes.h`.
#[repr(C)]
#[derive(Debug)]
pub struct MbState {
    /// The conversion state described in the module documentation.
    state: u32,
    /// Unused.
    unused: u32,
}

const _: () = assert!(size_of::<MbState>() == 8);

/// A function's own conversion state, for when it is given a null one.
struct InternalState(UnsafeCell<MbState>);

// SAFETY: C and POSIX do not require these functions to be safe to call from
// several threads with a null state; a program that does races on its own
// account, exactly as with musl.
unsafe impl Sync for InternalState {}

impl InternalState {
    /// The initial state.
    const fn new() -> Self {
        Self(UnsafeCell::new(MbState {
            state: 0,
            unused: 0,
        }))
    }

    /// `given`, or this state if `given` is null.
    fn or(&self, given: *mut MbState) -> *mut MbState {
        if given.is_null() { self.0.get() } else { given }
    }
}

/// Reads the state word.
///
/// # Safety
///
/// `st` must be a valid `mbstate_t`.
unsafe fn load(st: *const MbState) -> u32 {
    // SAFETY: the caller passes a valid state.
    unsafe { st.cast::<u32>().read() }
}

/// Writes the state word.
///
/// # Safety
///
/// `st` must be a valid `mbstate_t`.
unsafe fn save(st: *mut MbState, state: u32) {
    // SAFETY: the caller passes a valid state.
    unsafe { st.cast::<u32>().write(state) }
}

/// The lowest UTF-8 lead byte of a multibyte character: C0 and C1 could only
/// begin overlong forms.
const LEAD_MIN: u8 = 0xc2;
/// The highest: F5 and above could only begin values above U+10FFFF.
const LEAD_MAX: u8 = 0xf4;

/// musl's `R(a, b)`: the state's top bits that bound the next byte to
/// `[a, b)`, where `a` is 0x80 or `b` is 0xc0.
const fn bound(a: u32, b: u32) -> u32 {
    (if a == 0x80 {
        0x40_u32.wrapping_sub(b)
    } else {
        0_u32.wrapping_sub(a)
    }) << 23
}

/// The state after the lead byte `byte`, which is from [`LEAD_MIN`] to
/// [`LEAD_MAX`]: musl's `bittab` entry, computed rather than looked up.
const fn lead_state(byte: u8) -> u32 {
    let any = bound(0x80, 0xc0);
    let byte = byte as u32;
    if byte < 0xe0 {
        // Two bytes: five bits of value, and one continuation byte.
        any | (byte - 0xc0)
    } else if byte < 0xf0 {
        // Three bytes. E0 would be overlong below A0, and ED a surrogate from
        // A0.
        let low = byte - 0xe0;
        let first = if low == 0 {
            bound(0xa0, 0xc0)
        } else if low == 0xd {
            bound(0x80, 0xa0)
        } else {
            any
        };
        first | (any >> 6) | low
    } else {
        // Four bytes. F0 would be overlong below 90, and F4 above U+10FFFF
        // from 90.
        let low = byte - 0xf0;
        let first = if low == 0 {
            bound(0x90, 0xc0)
        } else if low == 4 {
            bound(0x80, 0x90)
        } else {
            any
        };
        first | (any >> 6) | (any >> 12) | low
    }
}

/// musl's `OOB(c, b)`: whether `byte` is outside the range the state allows
/// for the next byte.
fn out_of_bounds(state: u32, byte: u8) -> bool {
    let high = i32::from(byte >> 3);
    let offset = state.cast_signed() >> 26;
    ((high - 0x10) | (high + offset)) & !7 != 0
}

/// What one more byte of a UTF-8 sequence does.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// The character is complete, with this value.
    Complete(u32),
    /// More bytes are needed; this is the new state.
    Partial(u32),
    /// The byte cannot continue the sequence.
    Invalid,
}

/// Feeds the continuation byte `byte` to a partial state.
fn continue_sequence(state: u32, byte: u8) -> Step {
    if out_of_bounds(state, byte) {
        return Step::Invalid;
    }
    let next = (state << 6) | u32::from(byte & 0x3f);
    if next & (1 << 31) == 0 {
        Step::Complete(next)
    } else {
        Step::Partial(next)
    }
}

/// The wide character the byte `byte` is in the C locale.
fn code_unit(byte: u8) -> u32 {
    if byte < 0x80 {
        u32::from(byte)
    } else {
        0xdf00 | u32::from(byte)
    }
}

/// Whether `wc` is one of the C locale's code units for 0x80 to 0xff.
fn is_code_unit(wc: u32) -> bool {
    wc.wrapping_sub(0xdf80) < 0x80
}

/// Reports an invalid sequence: `errno` is `EILSEQ`, and the result -1.
fn invalid() -> usize {
    errno::set(errno::EILSEQ);
    INVALID
}

/// [`mbrtowc`] with the state already chosen.
///
/// # Safety
///
/// As [`mbrtowc`], with `st` valid.
unsafe fn decode(wc: *mut WChar, s: *const c_char, n: usize, st: *mut MbState) -> usize {
    // SAFETY: the caller passes a valid state.
    let mut state = unsafe { load(st) };
    let bytes = s.cast::<u8>();
    if s.is_null() {
        if state != 0 {
            // SAFETY: as above.
            unsafe { save(st, 0) };
            return invalid();
        }
        return 0;
    }
    if n == 0 {
        return INCOMPLETE;
    }
    let mut i = 0;
    if state == 0 {
        // SAFETY: `n > 0`, so the first byte is readable.
        let byte = unsafe { bytes.read() };
        let value = if byte < 0x80 {
            u32::from(byte)
        } else if !locale::current_is_utf8() {
            code_unit(byte)
        } else if byte.wrapping_sub(LEAD_MIN) > LEAD_MAX - LEAD_MIN {
            return invalid();
        } else {
            state = lead_state(byte);
            0
        };
        if state == 0 {
            if !wc.is_null() {
                // SAFETY: the caller passes room for a character, or null.
                unsafe { wc.write(value.cast_signed()) };
            }
            return usize::from(value != 0);
        }
        i = 1;
    }
    while i < n {
        // SAFETY: `i < n`, and the caller vouches for `n` bytes.
        let byte = unsafe { bytes.wrapping_add(i).read() };
        i += 1;
        match continue_sequence(state, byte) {
            Step::Invalid => {
                // SAFETY: the caller passes a valid state.
                unsafe { save(st, 0) };
                return invalid();
            }
            Step::Complete(value) => {
                // SAFETY: as above.
                unsafe { save(st, 0) };
                if !wc.is_null() {
                    // SAFETY: the caller passes room for a character, or
                    // null.
                    unsafe { wc.write(value.cast_signed()) };
                }
                return i;
            }
            Step::Partial(next) => state = next,
        }
    }
    // SAFETY: the caller passes a valid state.
    unsafe { save(st, state) };
    INCOMPLETE
}

/// `mbrtowc`'s own state.
static MBRTOWC_STATE: InternalState = InternalState::new();

/// Converts the character at `s`, of at most `n` bytes, to `*wc`, continuing
/// from and updating the state `st`.
///
/// Returns the bytes this call consumed, or 0 for the NUL character. Returns
/// `(size_t)-2` if all `n` bytes were consumed without completing a
/// character, and `(size_t)-1` with `errno` set to `EILSEQ` for an invalid
/// sequence, which also resets the state. A null `s` resets the state, and
/// fails if it was in the middle of a character. A null `wc` discards the
/// character, and a null `st` uses this function's own state.
///
/// # Safety
///
/// `s` must be null or valid for `n` bytes, or up to the end of a character.
/// `wc` must be null or writable. `st` must be null or a valid state.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mbrtowc(
    wc: *mut WChar,
    s: *const c_char,
    n: usize,
    st: *mut MbState,
) -> usize {
    // SAFETY: the caller's contract, with a state now chosen.
    unsafe { decode(wc, s, n, MBRTOWC_STATE.or(st)) }
}

/// `mbrlen`'s own state.
static MBRLEN_STATE: InternalState = InternalState::new();

/// [`mbrtowc`] without storing the character, and with a separate state of
/// its own.
///
/// # Safety
///
/// As [`mbrtowc`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mbrlen(s: *const c_char, n: usize, st: *mut MbState) -> usize {
    // SAFETY: the caller's contract, with a state now chosen.
    unsafe { decode(null_mut(), s, n, MBRLEN_STATE.or(st)) }
}

/// Converts the character at `s`, of at most `n` bytes, to `*wc` without any
/// state: an incomplete character is invalid.
///
/// Returns the bytes used, 0 for the NUL character or a null `s`, or -1 with
/// `errno` set to `EILSEQ`.
///
/// # Safety
///
/// `s` must be null or valid for `n` bytes, or up to the end of a character.
/// `wc` must be null or writable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mbtowc(wc: *mut WChar, s: *const c_char, n: usize) -> c_int {
    if s.is_null() {
        return 0;
    }
    if n == 0 {
        let _ = invalid();
        return -1;
    }
    let bytes = s.cast::<u8>();
    // SAFETY: `n > 0`, so the first byte is readable.
    let byte = unsafe { bytes.read() };
    let store = |value: u32| {
        if !wc.is_null() {
            // SAFETY: the caller passes room for a character, or null.
            unsafe { wc.write(value.cast_signed()) };
        }
    };
    if byte < 0x80 {
        store(u32::from(byte));
        return c_int::from(byte != 0);
    }
    if !locale::current_is_utf8() {
        store(code_unit(byte));
        return 1;
    }
    if byte.wrapping_sub(LEAD_MIN) > LEAD_MAX - LEAD_MIN {
        let _ = invalid();
        return -1;
    }
    let mut state = lead_state(byte);
    // Whether `n` bytes can hold the whole character: after `n - 1`
    // continuation bytes the markers must have left bit 31.
    if n < MB_LEN_MAX && (state << (6 * n - 6)) & (1 << 31) != 0 {
        let _ = invalid();
        return -1;
    }
    let mut used = 1;
    loop {
        // SAFETY: the check above means the character ends within `n` bytes,
        // and each byte read so far continued it.
        let next = unsafe { bytes.wrapping_add(used).read() };
        used += 1;
        match continue_sequence(state, next) {
            Step::Invalid => {
                let _ = invalid();
                return -1;
            }
            Step::Complete(value) => {
                store(value);
                return used as c_int;
            }
            Step::Partial(partial) => state = partial,
        }
    }
}

/// [`mbtowc`] without storing the character: the length of the character at
/// `s`.
///
/// # Safety
///
/// As [`mbtowc`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mblen(s: *const c_char, n: usize) -> c_int {
    // SAFETY: the caller's contract is `mbtowc`'s.
    unsafe { mbtowc(null_mut(), s, n) }
}

/// Encodes `wc` into `s`, which must have room for `MB_CUR_MAX` bytes.
/// Returns the bytes written, or `(size_t)-1` with `errno` set to `EILSEQ`
/// for a character the locale cannot encode. A null `s` returns 1. The
/// encodings have no shift state, so `st` is not used.
///
/// # Safety
///
/// `s` must be null or valid for writing `MB_CUR_MAX` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcrtomb(s: *mut c_char, wc: WChar, st: *mut MbState) -> usize {
    let _ = st;
    if s.is_null() {
        return 1;
    }
    let wc = wc.cast_unsigned();
    let out = s.cast::<u8>();
    let put = |i: usize, byte: u32| {
        // SAFETY: `i` is below the length this character needs, which is at
        // most `MB_CUR_MAX`, and the caller passes that much room.
        unsafe { out.wrapping_add(i).write(byte as u8) };
    };
    if wc < 0x80 {
        put(0, wc);
        1
    } else if !locale::current_is_utf8() {
        if !is_code_unit(wc) {
            return invalid();
        }
        put(0, wc & 0xff);
        1
    } else if wc < 0x800 {
        put(0, 0xc0 | (wc >> 6));
        put(1, 0x80 | (wc & 0x3f));
        2
    } else if wc < 0xd800 || wc.wrapping_sub(0xe000) < 0x2000 {
        put(0, 0xe0 | (wc >> 12));
        put(1, 0x80 | ((wc >> 6) & 0x3f));
        put(2, 0x80 | (wc & 0x3f));
        3
    } else if wc.wrapping_sub(0x10000) < 0x10_0000 {
        put(0, 0xf0 | (wc >> 18));
        put(1, 0x80 | ((wc >> 12) & 0x3f));
        put(2, 0x80 | ((wc >> 6) & 0x3f));
        put(3, 0x80 | (wc & 0x3f));
        4
    } else {
        invalid()
    }
}

/// [`wcrtomb`] without a state: the bytes written, 0 for a null `s`, or -1
/// with `errno` set to `EILSEQ`.
///
/// # Safety
///
/// As [`wcrtomb`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wctomb(s: *mut c_char, wc: WChar) -> c_int {
    if s.is_null() {
        return 0;
    }
    // SAFETY: the caller's contract is `wcrtomb`'s.
    match unsafe { wcrtomb(s, wc, null_mut()) } {
        INVALID => -1,
        len => len as c_int,
    }
}

/// Whether `st` is null or in the initial state.
///
/// # Safety
///
/// `st` must be null or a valid state.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mbsinit(st: *const MbState) -> c_int {
    // SAFETY: the caller passes a valid state, or null.
    c_int::from(st.is_null() || unsafe { load(st) } == 0)
}

/// The wide character the single byte `c` is, or `WEOF` if it does not stand
/// alone. As in musl, only `c`'s low eight bits are looked at, except that
/// `EOF` is `WEOF`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn btowc(c: c_int) -> WInt {
    let byte = c as u8;
    if byte < 0x80 {
        u32::from(byte)
    } else if !locale::current_is_utf8() && c != EOF {
        code_unit(byte)
    } else {
        WEOF
    }
}

/// The single byte the wide character `wc` encodes as, or `EOF` if it takes
/// more than one or none.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn wctob(wc: WInt) -> c_int {
    if wc < 0x80 {
        wc as c_int
    } else if !locale::current_is_utf8() && is_code_unit(wc) {
        (wc & 0xff) as c_int
    } else {
        EOF
    }
}

/// Converts the string `*src` to at most `wn` wide characters in `ws`.
///
/// Stops at the NUL, which it stores unless `wn` ran out first, and then sets
/// `*src` to null; or when `wn` characters are stored, leaving `*src` at the
/// next byte; or at an invalid sequence, leaving `*src` at its first byte and
/// returning `(size_t)-1` with `errno` set to `EILSEQ`. Returns the
/// characters stored, not counting the NUL. With a null `ws` it only counts,
/// with no limit, and leaves `*src` alone.
///
/// A state in the middle of a character is continued. When `ws` is not null it
/// is reset to the initial state, which is where the conversion leaves it.
///
/// # Safety
///
/// `*src` must be a NUL-terminated string. `ws` must be null or valid for
/// writing `wn` characters, and `st` null or a valid state.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mbsrtowcs(
    ws: *mut WChar,
    src: *mut *const c_char,
    wn: usize,
    st: *mut MbState,
) -> usize {
    // SAFETY: the caller passes a valid `src`.
    let start = unsafe { src.read() }.cast::<u8>();
    let mut state = if st.is_null() {
        0
    } else {
        // SAFETY: the caller passes a valid state.
        unsafe { load(st) }
    };
    let mut s = start;
    let mut count = 0;

    if state == 0 && !locale::current_is_utf8() {
        if ws.is_null() {
            // SAFETY: the caller passes a NUL-terminated string.
            return unsafe { strlen(start.cast()) };
        }
        loop {
            if count == wn {
                // SAFETY: the caller passes a valid `src`.
                unsafe { src.write(s.cast()) };
                return wn;
            }
            // SAFETY: no NUL has been passed, so `s` is within the string.
            let byte = unsafe { s.read() };
            if byte == 0 {
                break;
            }
            // SAFETY: `count < wn`, and the caller passes room for `wn`.
            unsafe { ws.wrapping_add(count).write(code_unit(byte).cast_signed()) };
            s = s.wrapping_add(1);
            count += 1;
        }
        // SAFETY: the string ended before `wn` characters were stored.
        unsafe { ws.wrapping_add(count).write(0) };
        // SAFETY: the caller passes a valid `src`.
        unsafe { src.write(core::ptr::null()) };
        return count;
    }

    // Where the character being decoded began.
    let mut char_start = s;
    loop {
        if !ws.is_null() && count == wn {
            // SAFETY: the caller passes a valid `src`.
            unsafe { src.write(s.cast()) };
            return wn;
        }
        if state == 0 {
            // SAFETY: no NUL has been passed, so `s` is within the string.
            let byte = unsafe { s.read() };
            if byte == 0 {
                break;
            }
            if byte < 0x80 {
                if !ws.is_null() {
                    // SAFETY: `count < wn`, and the caller passes room.
                    unsafe { ws.wrapping_add(count).write(i32::from(byte)) };
                }
                s = s.wrapping_add(1);
                count += 1;
                continue;
            }
            if byte.wrapping_sub(LEAD_MIN) > LEAD_MAX - LEAD_MIN {
                break;
            }
            char_start = s;
            state = lead_state(byte);
            s = s.wrapping_add(1);
        } else if !ws.is_null() && !st.is_null() {
            // The character begun in an earlier call is being completed and
            // stored, so the caller's state is consumed.
            // SAFETY: the caller passes a valid state.
            unsafe { save(st, 0) };
        }
        loop {
            // SAFETY: every byte before `s` continued a character, so none
            // was the NUL, and `s` is within the string.
            let byte = unsafe { s.read() };
            match continue_sequence(state, byte) {
                Step::Invalid => {
                    s = char_start;
                    break;
                }
                Step::Complete(value) => {
                    if !ws.is_null() {
                        // SAFETY: `count < wn`, and the caller passes room.
                        unsafe { ws.wrapping_add(count).write(value.cast_signed()) };
                    }
                    s = s.wrapping_add(1);
                    count += 1;
                    state = 0;
                    break;
                }
                Step::Partial(next) => {
                    state = next;
                    s = s.wrapping_add(1);
                }
            }
        }
        if state != 0 {
            break;
        }
    }

    // SAFETY: `s` is within the string.
    if state == 0 && unsafe { s.read() } == 0 {
        if !ws.is_null() {
            // SAFETY: `count < wn`, checked at the top of the loop.
            unsafe { ws.wrapping_add(count).write(0) };
            // SAFETY: the caller passes a valid `src`.
            unsafe { src.write(core::ptr::null()) };
        }
        return count;
    }
    if !ws.is_null() {
        // SAFETY: as above.
        unsafe { src.write(s.cast()) };
    }
    invalid()
}

/// [`mbsrtowcs`] without a state, from the string `s`.
///
/// # Safety
///
/// `s` must be a NUL-terminated string, and `ws` null or valid for writing
/// `wn` characters.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mbstowcs(ws: *mut WChar, s: *const c_char, wn: usize) -> usize {
    let mut at = s;
    // SAFETY: the caller's contract is `mbsrtowcs`'s.
    unsafe { mbsrtowcs(ws, &raw mut at, wn, null_mut()) }
}

/// `mbsnrtowcs`'s own state.
static MBSNRTOWCS_STATE: InternalState = InternalState::new();

/// [`mbsrtowcs`] reading at most `n` bytes of `*src`.
///
/// If the bytes run out in the middle of a character, as in musl, the state
/// is reset and `*src` left at that character's first byte, for the caller to
/// retry with more. An invalid sequence leaves `*src` at it and returns
/// `(size_t)-1`. A null `st` uses this function's own state.
///
/// # Safety
///
/// `*src` must be valid for `n` bytes or up to its NUL. `ws` must be null or
/// valid for writing `wn` characters, and `st` null or a valid state.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mbsnrtowcs(
    ws: *mut WChar,
    src: *mut *const c_char,
    n: usize,
    wn: usize,
    st: *mut MbState,
) -> usize {
    let st = MBSNRTOWCS_STATE.or(st);
    // SAFETY: the caller passes a valid `src`.
    let mut s = unsafe { src.read() };
    let mut left = n;
    let mut count = 0;
    while !s.is_null() && left > 0 && (ws.is_null() || count < wn) {
        let mut wc = 0;
        // SAFETY: `left` bytes of `s` are readable, and the state is valid.
        let len = unsafe { decode(&raw mut wc, s, left, st) };
        match len {
            INVALID => {
                count = INVALID;
                break;
            }
            INCOMPLETE => {
                // SAFETY: as above.
                unsafe { save(st, 0) };
                break;
            }
            0 => {
                if !ws.is_null() {
                    // SAFETY: `count < wn`.
                    unsafe { ws.wrapping_add(count).write(0) };
                }
                s = core::ptr::null();
                break;
            }
            _ => {
                if !ws.is_null() {
                    // SAFETY: `count < wn`.
                    unsafe { ws.wrapping_add(count).write(wc) };
                }
                s = s.wrapping_add(len);
                left -= len;
                count += 1;
            }
        }
    }
    if !ws.is_null() {
        // SAFETY: the caller passes a valid `src`.
        unsafe { src.write(s) };
    }
    count
}

/// Converts the wide string `*ws` to at most `n` bytes in `s`.
///
/// Stops at the NUL, which it stores if it fits, then setting `*ws` to null;
/// or before a character that does not fit, leaving `*ws` at it; or at a
/// character the locale cannot encode, leaving `*ws` at it and returning
/// `(size_t)-1` with `errno` set to `EILSEQ`. Returns the bytes stored, not
/// counting the NUL. With a null `s` it only counts. `st` is not used.
///
/// # Safety
///
/// `*ws` must be a NUL-terminated wide string, and `s` null or valid for
/// writing `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsrtombs(
    s: *mut c_char,
    ws: *mut *const WChar,
    n: usize,
    st: *mut MbState,
) -> usize {
    let _ = st;
    let mut buf = [0 as c_char; MB_LEN_MAX];
    // SAFETY: the caller passes a valid `ws`.
    let mut w = unsafe { ws.read() };
    if s.is_null() {
        let mut total = 0_usize;
        loop {
            // SAFETY: no NUL has been passed, so `w` is within the string.
            let wc = unsafe { w.read() };
            if wc == 0 {
                return total;
            }
            if wc.cast_unsigned() >= 0x80 {
                // SAFETY: `buf` has room for any character.
                let len = unsafe { wcrtomb(buf.as_mut_ptr(), wc, null_mut()) };
                if len == INVALID {
                    return INVALID;
                }
                total = total.wrapping_add(len);
            } else {
                total = total.wrapping_add(1);
            }
            w = w.wrapping_add(1);
        }
    }

    let mut out = s;
    let mut left = n;
    while left > 0 {
        // SAFETY: no NUL has been passed, so `w` is within the string.
        let wc = unsafe { w.read() };
        if wc == 0 {
            // SAFETY: `left > 0`.
            unsafe { out.write(0) };
            // SAFETY: the caller passes a valid `ws`.
            unsafe { ws.write(core::ptr::null()) };
            return n - left;
        }
        if wc.cast_unsigned() < 0x80 {
            // SAFETY: `left > 0`.
            unsafe { out.write(wc as c_char) };
            out = out.wrapping_add(1);
            left -= 1;
        } else {
            // SAFETY: `buf` has room for any character.
            let len = unsafe { wcrtomb(buf.as_mut_ptr(), wc, null_mut()) };
            if len == INVALID {
                // SAFETY: the caller passes a valid `ws`.
                unsafe { ws.write(w) };
                return INVALID;
            }
            if len > left {
                break;
            }
            let mut i = 0;
            while i < len {
                let byte = buf.get(i).copied().unwrap_or(0);
                // SAFETY: `i < len <= left`, within `out`.
                unsafe { out.wrapping_add(i).write(byte) };
                i += 1;
            }
            out = out.wrapping_add(len);
            left -= len;
        }
        w = w.wrapping_add(1);
    }
    // SAFETY: the caller passes a valid `ws`.
    unsafe { ws.write(w) };
    n - left
}

/// [`wcsrtombs`] without a state, from the wide string `ws`.
///
/// # Safety
///
/// As [`wcsrtombs`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcstombs(s: *mut c_char, ws: *const WChar, n: usize) -> usize {
    let mut at = ws;
    // SAFETY: the caller's contract is `wcsrtombs`'s.
    unsafe { wcsrtombs(s, &raw mut at, n, null_mut()) }
}

/// [`wcsrtombs`] reading at most `wn` wide characters of `*ws`. The NUL, if
/// reached, is stored if it fits but not counted.
///
/// # Safety
///
/// `*ws` must be valid for `wn` characters or up to its NUL, and `dst` null
/// or valid for writing `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsnrtombs(
    dst: *mut c_char,
    ws: *mut *const WChar,
    wn: usize,
    n: usize,
    st: *mut MbState,
) -> usize {
    let _ = st;
    // SAFETY: the caller passes a valid `ws`.
    let mut w = unsafe { ws.read() };
    let mut out = dst;
    let mut left = if dst.is_null() { 0 } else { n };
    let mut chars = wn;
    let mut count = 0_usize;
    let mut tmp = [0 as c_char; MB_LEN_MAX];
    while !w.is_null() && chars > 0 {
        // SAFETY: `chars > 0`, so `w` is within the string.
        let wc = unsafe { w.read() };
        let direct = left >= MB_LEN_MAX;
        let target = if direct { out } else { tmp.as_mut_ptr() };
        // SAFETY: `target` has room for `MB_LEN_MAX` bytes.
        let len = unsafe { wcrtomb(target, wc, null_mut()) };
        if len == INVALID {
            count = INVALID;
            break;
        }
        if !dst.is_null() {
            if !direct {
                if len > left {
                    break;
                }
                let mut i = 0;
                while i < len {
                    let byte = tmp.get(i).copied().unwrap_or(0);
                    // SAFETY: `i < len <= left`, within `out`.
                    unsafe { out.wrapping_add(i).write(byte) };
                    i += 1;
                }
            }
            out = out.wrapping_add(len);
            left -= len;
        }
        if wc == 0 {
            w = core::ptr::null();
            break;
        }
        w = w.wrapping_add(1);
        chars -= 1;
        count = count.wrapping_add(len);
    }
    if !dst.is_null() {
        // SAFETY: the caller passes a valid `ws`.
        unsafe { ws.write(w) };
    }
    count
}

/// `char16_t`.
pub type Char16 = u16;

/// `char32_t`.
pub type Char32 = u32;

/// `mbrtoc16`'s own state.
static MBRTOC16_STATE: InternalState = InternalState::new();

/// [`mbrtowc`] into UTF-16. A character above U+FFFF gives its high surrogate
/// first; the next call gives the low one and returns `(size_t)-3`, consuming
/// nothing.
///
/// # Safety
///
/// As [`mbrtowc`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mbrtoc16(
    pc16: *mut Char16,
    s: *const c_char,
    n: usize,
    ps: *mut MbState,
) -> usize {
    let ps = MBRTOC16_STATE.or(ps);
    if s.is_null() {
        // SAFETY: the empty string is one readable byte.
        return unsafe { mbrtoc16(null_mut(), c"".as_ptr(), 1, ps) };
    }
    // SAFETY: the state is valid.
    let pending = unsafe { load(ps) };
    if pending.cast_signed() > 0 {
        if !pc16.is_null() {
            // SAFETY: the caller passes room for a unit, or null.
            unsafe { pc16.write(pending as u16) };
        }
        // SAFETY: as above.
        unsafe { save(ps, 0) };
        return EARLIER;
    }
    let mut wc: WChar = 0;
    // SAFETY: the caller's contract, with a valid state.
    let len = unsafe { decode(&raw mut wc, s, n, ps) };
    if len <= MB_LEN_MAX {
        let mut unit = wc.cast_unsigned();
        if unit >= 0x10000 {
            // SAFETY: the state is valid.
            unsafe { save(ps, (unit & 0x3ff) + 0xdc00) };
            unit = 0xd7c0 + (unit >> 10);
        }
        if !pc16.is_null() {
            // SAFETY: the caller passes room for a unit, or null.
            unsafe { pc16.write(unit as u16) };
        }
    }
    len
}

/// `c16rtomb`'s own state.
static C16RTOMB_STATE: InternalState = InternalState::new();

/// [`wcrtomb`] from UTF-16. A high surrogate is kept in the state and writes
/// nothing; the low surrogate that must follow completes the character. A
/// lone or reversed surrogate is `EILSEQ`.
///
/// # Safety
///
/// `s` must be null or valid for writing `MB_CUR_MAX` bytes, and `ps` null or
/// a valid state.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn c16rtomb(s: *mut c_char, c16: Char16, ps: *mut MbState) -> usize {
    let ps = C16RTOMB_STATE.or(ps);
    // SAFETY: the state is valid.
    let high = unsafe { load(ps) };
    let unit = u32::from(c16);
    let fail = || {
        // SAFETY: the state is valid.
        unsafe { save(ps, 0) };
        invalid()
    };
    if s.is_null() {
        return if high != 0 { fail() } else { 1 };
    }
    if high == 0 && unit.wrapping_sub(0xd800) < 0x400 {
        // SAFETY: the state is valid.
        unsafe { save(ps, (unit - 0xd7c0) << 10) };
        return 0;
    }
    let wc = if high != 0 {
        if unit.wrapping_sub(0xdc00) >= 0x400 {
            return fail();
        }
        // SAFETY: the state is valid.
        unsafe { save(ps, 0) };
        high + unit - 0xdc00
    } else {
        unit
    };
    // SAFETY: the caller passes room for `MB_CUR_MAX` bytes.
    unsafe { wcrtomb(s, wc.cast_signed(), null_mut()) }
}

/// `mbrtoc32`'s own state.
static MBRTOC32_STATE: InternalState = InternalState::new();

/// [`mbrtowc`] into a `char32_t`.
///
/// # Safety
///
/// As [`mbrtowc`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mbrtoc32(
    pc32: *mut Char32,
    s: *const c_char,
    n: usize,
    ps: *mut MbState,
) -> usize {
    let ps = MBRTOC32_STATE.or(ps);
    if s.is_null() {
        // SAFETY: the empty string is one readable byte.
        return unsafe { mbrtoc32(null_mut(), c"".as_ptr(), 1, ps) };
    }
    let mut wc: WChar = 0;
    // SAFETY: the caller's contract, with a valid state.
    let len = unsafe { decode(&raw mut wc, s, n, ps) };
    if len <= MB_LEN_MAX && !pc32.is_null() {
        // SAFETY: the caller passes room for a character, or null.
        unsafe { pc32.write(wc.cast_unsigned()) };
    }
    len
}

/// [`wcrtomb`] from a `char32_t`.
///
/// # Safety
///
/// As [`wcrtomb`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn c32rtomb(s: *mut c_char, c32: Char32, ps: *mut MbState) -> usize {
    // SAFETY: the caller's contract is `wcrtomb`'s.
    unsafe { wcrtomb(s, c32.cast_signed(), ps) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decodes all of `bytes` from the initial state, in the UTF-8 decoder
    /// alone: the value, or `None` if it is invalid or incomplete.
    fn utf8(bytes: &[u8]) -> Option<u32> {
        let (&lead, rest) = bytes.split_first()?;
        if lead < 0x80 {
            return rest.is_empty().then_some(u32::from(lead));
        }
        if lead.wrapping_sub(LEAD_MIN) > LEAD_MAX - LEAD_MIN {
            return None;
        }
        let mut state = lead_state(lead);
        for (i, &byte) in rest.iter().enumerate() {
            match continue_sequence(state, byte) {
                Step::Invalid => return None,
                Step::Complete(value) => return (i + 1 == rest.len()).then_some(value),
                Step::Partial(next) => state = next,
            }
        }
        None
    }

    #[test]
    fn the_decoder_accepts_exactly_the_shortest_forms_of_scalar_values() {
        for scalar in (0..0xd800).chain(0xe000..0x11_0000) {
            let c = char::from_u32(scalar).unwrap_or_default();
            let mut buf = [0; 4];
            let encoded = c.encode_utf8(&mut buf).as_bytes();
            assert_eq!(utf8(encoded), Some(scalar), "U+{scalar:04X}");
        }
        // Every two-byte sequence Rust rejects, the decoder rejects.
        for a in 0x80..=0xff_u8 {
            for b in 0..=0xff_u8 {
                let bytes = [a, b];
                assert_eq!(
                    utf8(&bytes).is_some(),
                    core::str::from_utf8(&bytes).is_ok(),
                    "{bytes:02x?}"
                );
            }
        }
        // Three- and four-byte prefixes at every boundary.
        let agrees = |bytes: &[u8]| {
            let one_char = core::str::from_utf8(bytes)
                .ok()
                .filter(|text| text.chars().count() == 1)
                .and_then(|text| text.chars().next())
                .map(u32::from);
            assert_eq!(utf8(bytes), one_char, "{bytes:02x?}");
        };
        let edges = [0x7f_u8, 0x80, 0xbf, 0xc0];
        let prefixes = (0xe0..=0xff_u8).flat_map(|a| (0x70..=0xd0_u8).map(move |b| [a, b]));
        for [a, b] in prefixes {
            for c in edges {
                agrees(&[a, b, c]);
                for d in edges {
                    agrees(&[a, b, c, d]);
                }
            }
        }
    }

    #[test]
    fn a_partial_state_always_has_its_top_bit_set() {
        for lead in LEAD_MIN..=LEAD_MAX {
            assert_ne!(lead_state(lead) & (1 << 31), 0, "{lead:02x}");
        }
        let state = lead_state(0xf0);
        let Step::Partial(state) = continue_sequence(state, 0x90) else {
            panic!("F0 90 is a prefix");
        };
        assert_ne!(state & (1 << 31), 0);
    }

    #[test]
    fn code_units_cover_the_high_bytes_once() {
        for byte in 0x80..=0xff_u8 {
            let wc = code_unit(byte);
            assert!(is_code_unit(wc));
            assert_eq!(wc & 0xff, u32::from(byte));
        }
        assert!(!is_code_unit(0xdf7f));
        assert!(!is_code_unit(0xe000));
    }
}
