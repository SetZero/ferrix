//! Formatted input: `scanf`, `fscanf`, `sscanf` and their `v` forms, with
//! glibc's `__isoc99_` and `__isoc23_` names for each.
//!
//! This follows musl's `stdio/vfscanf.c`. White space in the format skips any
//! amount of white space in the input; any other byte, and `%%`, must match
//! the input; and a conversion reads a field. Every conversion but `%c`, `%[`
//! and `%n` skips white space first. A width limits how many bytes a field
//! takes, `*` reads a field without storing it, `%N$` stores through the Nth
//! argument, and `m` allocates the string of `%s`, `%c` and `%[`. The sizes
//! are `hh`, `h`, `l`, `ll`, `j`, `z`, `t` and `L`, and `%lc`, `%ls`, `%l[`,
//! `%C` and `%S` store wide characters converted with `mbrtowc`. The result is
//! the number of fields stored, or `EOF` when the input ended or failed before
//! the first.
//!
//! Numbers go through the parsers behind `strtol` and `strtod`. A number
//! conversion reads the longest sequence that could still begin a number, up
//! to the width, as C describes the input item, and hands it to the parser.
//! If the parser does not take all of it, as with `0x` or `1e+`, the field
//! fails to match and the bytes stay read, as in musl. `%i` does not read C23's
//! `0b` prefix, and there is no `%b`. A number field longer than 128 bytes is
//! cut there.
//!
//! A stream is read a byte at a time under its lock, with one byte given back
//! when a field ends, so the next read sees it.

use core::ffi::{c_char, c_int, c_long, c_void};
use core::mem::{MaybeUninit, size_of};
use core::ptr::null_mut;
use core::sync::atomic::Ordering;

use super::EOF;
use super::file::{self, File, Inner, stdin};
use crate::errno;
use crate::float::{BINARY32, BINARY64};
use crate::malloc::{free, malloc, realloc};
use crate::multibyte::{MbState, WChar, WInt, mbrtowc, mbsinit, wcrtomb};
use crate::scan::{CText, Input, digit, is_space};
use crate::strtod;
use crate::strtol;
use crate::va::{self, VaList, VaListArg, VaListTag};
use crate::wctype::iswspace;

/// The longest number field read.
const TOKEN: usize = 128;

/// Where a conversion reads from.
///
/// A wide source, for the `wscanf` family, gives one unit per wide character:
/// the character itself when it is ASCII, a space for any other white space,
/// and `0x80`, which no number or directive holds, for anything else. So widths
/// and `%n` count characters, and numbers read as they do from bytes. What the
/// last unit really was is [`Source::last_wide`].
pub(super) trait Source {
    /// The next byte, or `None` at the end of the input or on an error.
    fn next(&mut self) -> Option<u8>;
    /// Gives back `byte`, the last byte [`Source::next`] returned.
    fn back(&mut self, byte: u8);
    /// For a wide source, the wide character the last unit stood for.
    fn last_wide(&self) -> Option<WChar> {
        None
    }
}

/// A C string, for `sscanf`.
#[derive(Debug)]
struct Text {
    /// The string.
    start: *const c_char,
    /// How many bytes have been read. None of them is the NUL.
    pos: usize,
}

impl Source for Text {
    fn next(&mut self) -> Option<u8> {
        // SAFETY: `vsscanf` made this from a NUL-terminated string, and `pos`
        // only moves past bytes that are not the NUL, so it is at or before it.
        let byte = unsafe { self.start.wrapping_add(self.pos).read() } as u8;
        if byte == 0 {
            return None;
        }
        self.pos += 1;
        Some(byte)
    }

    fn back(&mut self, _byte: u8) {
        self.pos = self.pos.saturating_sub(1);
    }
}

/// A stream whose lock is held, for `fscanf`.
#[derive(Debug)]
struct Stream<'a> {
    /// The stream's state.
    inner: &'a mut Inner,
}

impl Source for Stream<'_> {
    fn next(&mut self) -> Option<u8> {
        self.inner.get_byte()
    }

    fn back(&mut self, byte: u8) {
        let _ = self.inner.unget(byte);
    }
}

/// Reads a source, counting the bytes taken for `%n`, and no further than a
/// field's width.
#[derive(Debug)]
struct Reader<'a, S> {
    /// The input.
    src: &'a mut S,
    /// How many bytes have been taken.
    count: usize,
    /// How many more bytes the field may take, or `None` without a limit.
    left: Option<usize>,
}

impl<S: Source> Reader<'_, S> {
    /// Limits the next field to `width` bytes, or lifts the limit for 0.
    fn limit(&mut self, width: usize) {
        self.left = (width != 0).then_some(width);
    }

    /// The next byte within the limit.
    fn next(&mut self) -> Option<u8> {
        if self.left == Some(0) {
            return None;
        }
        let byte = self.src.next()?;
        self.count += 1;
        if let Some(left) = &mut self.left {
            *left -= 1;
        }
        Some(byte)
    }

    /// Gives back the last byte read.
    fn back(&mut self, byte: u8) {
        self.src.back(byte);
        self.count = self.count.saturating_sub(1);
        if let Some(left) = &mut self.left {
            *left += 1;
        }
    }

    /// The next byte that is not white space, taking the white space before
    /// it.
    fn next_after_space(&mut self) -> Option<u8> {
        loop {
            match self.next() {
                Some(space) if is_space(space) => {}
                other => return other,
            }
        }
    }

    /// Skips white space, without a limit.
    fn skip_space(&mut self) {
        self.limit(0);
        while let Some(byte) = self.next() {
            if !is_space(byte) {
                self.back(byte);
                break;
            }
        }
    }
}

/// How a scan stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// The format ended.
    Done,
    /// The input did not match.
    Mismatch,
    /// The input ended or failed, the format was invalid, or memory ran out.
    Failure,
}

/// The size a conversion stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Size {
    /// `hh`.
    Char,
    /// `h`.
    Short,
    /// No size.
    Int,
    /// `l`, `z` and `t`.
    Long,
    /// `ll` and `j`.
    LongLong,
    /// `L`.
    LongDouble,
}

/// Stores `value` in an integer of `size` at `dest`, truncating it, unless
/// `dest` is null.
///
/// # Safety
///
/// `dest` must be null or valid for a write of that integer.
unsafe fn store_int(dest: *mut c_void, size: Size, value: u64) {
    if dest.is_null() {
        return;
    }
    let len = match size {
        Size::Char => 1,
        Size::Short => 2,
        Size::Int => 4,
        Size::Long => size_of::<c_long>(),
        Size::LongLong => 8,
        Size::LongDouble => return,
    };
    // The integer is its low bytes, least significant first.
    for (offset, byte) in value.to_le_bytes().into_iter().take(len).enumerate() {
        // SAFETY: the caller vouches for an integer of `len` bytes.
        unsafe { dest.cast::<u8>().wrapping_add(offset).write(byte) };
    }
}

/// Reads the longest sequence that could begin an integer in `base` into
/// `token`, and returns its length.
fn int_token<S: Source>(reader: &mut Reader<'_, S>, base: u32, token: &mut [u8; TOKEN]) -> usize {
    let mut len = 0;
    let mut hex = base == 16;
    while len < TOKEN {
        let Some(byte) = reader.next() else {
            break;
        };
        let signed = usize::from(matches!(token.first(), Some(b'+' | b'-')) && len > 0);
        let fits = match byte {
            b'+' | b'-' => len == 0,
            b'x' | b'X' => {
                let prefix = (base == 0 || base == 16)
                    && len == signed + 1
                    && token.get(signed) == Some(&b'0');
                hex |= prefix;
                prefix
            }
            _ => {
                // `%i` reads octal after a leading zero, so `08` is `0`
                // followed by an `8` the next field reads.
                let leading_zero = len > signed && token.get(signed) == Some(&b'0');
                let radix = match (hex, base) {
                    (true, _) => 16,
                    (false, 0) if leading_zero => 8,
                    (false, 0) => 10,
                    (false, other) => other,
                };
                digit(byte).is_some_and(|value| value < radix)
            }
        };
        if !fits {
            reader.back(byte);
            break;
        }
        if let Some(slot) = token.get_mut(len) {
            *slot = byte;
        }
        len += 1;
    }
    len
}

/// What a floating-point field has read so far.
#[derive(Debug, Default)]
struct FloatState {
    /// Whether a `0x` prefix was read.
    hex: bool,
    /// Whether a digit of the significand was read.
    digits: bool,
    /// Whether the radix point was read.
    dot: bool,
    /// Where the exponent's letter is, once read.
    exponent: Option<usize>,
    /// The word being read, `infinity` or `nan`, once its first letter is.
    word: Option<&'static [u8]>,
    /// Whether a NaN's parenthesis is open.
    paren: bool,
}

impl FloatState {
    /// Whether `byte` can continue the `len` bytes of `token`, updating what
    /// has been read.
    fn accepts(&mut self, token: &[u8], len: usize, byte: u8) -> bool {
        let signed = usize::from(matches!(token.first(), Some(b'+' | b'-')) && len > 0);
        let at = len - signed;
        let lower = byte | 0x20;
        if self.paren {
            return byte.is_ascii_alphanumeric() || byte == b'_' || byte == b')';
        }
        if let Some(word) = self.word {
            if word == b"nan" && at == 3 && byte == b'(' {
                self.paren = true;
                return true;
            }
            return word.get(at) == Some(&lower);
        }
        if at == 0 && (lower == b'i' || lower == b'n') {
            self.word = Some(if lower == b'i' { b"infinity" } else { b"nan" });
            return true;
        }
        match byte {
            b'+' | b'-' => len == 0 || self.exponent.is_some_and(|letter| letter + 1 == len),
            b'.' if !self.dot && self.exponent.is_none() => {
                self.dot = true;
                true
            }
            b'x' | b'X' if at == 1 && token.get(signed) == Some(&b'0') => {
                self.hex = true;
                true
            }
            _ if self.exponent.is_none() && self.hex && byte.is_ascii_hexdigit() => {
                self.digits = true;
                true
            }
            b'0'..=b'9' => {
                self.digits |= self.exponent.is_none();
                true
            }
            b'e' | b'E' if !self.hex && self.digits && self.exponent.is_none() => {
                self.exponent = Some(len);
                true
            }
            b'p' | b'P' if self.hex && self.digits && self.exponent.is_none() => {
                self.exponent = Some(len);
                true
            }
            _ => false,
        }
    }
}

/// Reads the longest sequence that could begin a floating-point number into
/// `token`, and returns its length.
fn float_token<S: Source>(reader: &mut Reader<'_, S>, token: &mut [u8; TOKEN]) -> usize {
    let mut state = FloatState::default();
    let mut len = 0;
    while len < TOKEN {
        let Some(byte) = reader.next() else {
            break;
        };
        if !state.accepts(token, len, byte) {
            reader.back(byte);
            break;
        }
        if let Some(slot) = token.get_mut(len) {
            *slot = byte;
        }
        len += 1;
        if state.paren && byte == b')' {
            break;
        }
    }
    len
}

/// Reads an integer field in `base` and stores it, as a pointer for `%p`.
///
/// # Safety
///
/// `dest` must be null or valid for a write of what the conversion stores.
unsafe fn integer<S: Source>(
    reader: &mut Reader<'_, S>,
    base: u32,
    pointer: bool,
    size: Size,
    dest: *mut c_void,
) -> Result<(), Stop> {
    let mut token = [0u8; TOKEN];
    let len = int_token(reader, base, &mut token);
    let text = token.get(..len).unwrap_or_default();
    let Some(scanned) =
        strtol::scan(text, base as c_int, false).filter(|scanned| scanned.end == len)
    else {
        return Err(Stop::Mismatch);
    };
    let magnitude = if scanned.overflow {
        errno::set(errno::ERANGE);
        u64::MAX
    } else {
        scanned.magnitude
    };
    let value = if scanned.negative {
        magnitude.wrapping_neg()
    } else {
        magnitude
    };
    if pointer && !dest.is_null() {
        let address = core::ptr::with_exposed_provenance_mut::<c_void>(value as usize);
        // SAFETY: the caller vouches for a pointer's room.
        unsafe { dest.cast::<*mut c_void>().write_unaligned(address) };
    } else {
        // SAFETY: the caller vouches for the integer's room.
        unsafe { store_int(dest, size, value) };
    }
    Ok(())
}

/// Reads a floating-point field and stores it as a `float`, a `double` for
/// `l`, or a `long double` for `L`. Other sizes store nothing, as in musl.
///
/// # Safety
///
/// `dest` must be null or valid for a write of what the size stores.
unsafe fn floating<S: Source>(
    reader: &mut Reader<'_, S>,
    size: Size,
    dest: *mut c_void,
) -> Result<(), Stop> {
    let mut token = [0u8; TOKEN];
    let len = float_token(reader, &mut token);
    let text = token.get(..len).unwrap_or_default();
    let format = match size {
        Size::Long => &BINARY64,
        Size::LongDouble => LONG_DOUBLE,
        _ => &BINARY32,
    };
    let parsed = strtod::parse(text, format);
    if parsed.end == 0 || parsed.end != len {
        return Err(Stop::Mismatch);
    }
    if parsed.error != 0 {
        errno::set(parsed.error);
    }
    if dest.is_null() {
        return Ok(());
    }
    match size {
        // SAFETY: the caller vouches for a `float`.
        Size::Int => unsafe {
            dest.cast::<f32>()
                .write_unaligned(f32::from_bits(parsed.bits as u32))
        },
        // SAFETY: the caller vouches for a `double`.
        Size::Long => unsafe {
            dest.cast::<f64>()
                .write_unaligned(f64::from_bits(parsed.bits as u64))
        },
        Size::LongDouble => {
            // An x87 `long double` is its ten low bytes, the rest padding; a
            // binary128 one all sixteen; ARMv7-A's a `double`'s eight.
            for (offset, byte) in parsed
                .bits
                .to_le_bytes()
                .into_iter()
                .take(LONG_DOUBLE_BYTES)
                .enumerate()
            {
                // SAFETY: the caller vouches for a `long double`.
                unsafe { dest.cast::<u8>().wrapping_add(offset).write(byte) };
            }
        }
        _ => {}
    }
    Ok(())
}

/// The format of a `long double`, and the bytes of it a conversion stores.
#[cfg(target_arch = "x86_64")]
const LONG_DOUBLE: &crate::float::Format = &crate::float::X87_EXTENDED;
/// The format of a `long double`, and the bytes of it a conversion stores.
#[cfg(target_arch = "x86_64")]
const LONG_DOUBLE_BYTES: usize = 10;
/// The format of a `long double`, and the bytes of it a conversion stores.
#[cfg(target_arch = "aarch64")]
const LONG_DOUBLE: &crate::float::Format = &crate::float::BINARY128;
/// The format of a `long double`, and the bytes of it a conversion stores.
#[cfg(target_arch = "aarch64")]
const LONG_DOUBLE_BYTES: usize = 16;
/// The format of a `long double`, and the bytes of it a conversion stores.
#[cfg(target_arch = "arm")]
const LONG_DOUBLE: &crate::float::Format = &BINARY64;
/// The format of a `long double`, and the bytes of it a conversion stores.
#[cfg(target_arch = "arm")]
const LONG_DOUBLE_BYTES: usize = 8;

/// Whether the wide character `wc` belongs in a `%s`, `%c` or `%[` field read
/// from a wide source: not white space for `%s`, anything for `%c`, and for
/// `%[` a member of the set written in the wide format from `start`, at the
/// `[`, to `end`, at the closing `]`, read as [`byte_set`] reads it.
///
/// # Safety
///
/// For `%[`, `format` must hold at least `end + 1` characters.
unsafe fn wide_member(
    format: *const WChar,
    start: usize,
    end: usize,
    conversion: u8,
    wc: WChar,
) -> bool {
    match conversion {
        b's' => iswspace(wc as WInt) == 0,
        b'[' => {
            // SAFETY: the caller vouches for the characters up to `end`.
            let at = |i: usize| unsafe { format.wrapping_add(i).read() };
            let mut i = start + 1;
            let invert = at(i) == WChar::from(b'^');
            if invert {
                i += 1;
            }
            let mut found = false;
            if at(i) == WChar::from(b'-') || at(i) == WChar::from(b']') {
                found |= at(i) == wc;
                i += 1;
            }
            while i < end {
                let c = at(i);
                if c == WChar::from(b'-') && i + 1 < end {
                    let (from, to) = (at(i - 1), at(i + 1));
                    found |= (from..=to).contains(&wc) || to == wc;
                    i += 2;
                    continue;
                }
                found |= c == wc;
                i += 1;
            }
            found != invert
        }
        _ => true,
    }
}

/// Builds the set of bytes a `%s`, `%c` or `%[` field may hold. For `%[`,
/// `p` is at the `[`, and is left at the closing `]`.
fn byte_set(format: &CText, p: &mut usize, conversion: u8) -> Result<[bool; 256], Stop> {
    let mut set = [true; 256];
    match conversion {
        b's' => {
            for space in [b'\t', b'\n', 0x0b, 0x0c, b'\r', b' '] {
                if let Some(slot) = set.get_mut(usize::from(space)) {
                    *slot = false;
                }
            }
        }
        b'[' => {
            *p += 1;
            let invert = format.at(*p) == b'^';
            if invert {
                *p += 1;
            }
            set = [invert; 256];
            let mark = |set: &mut [bool; 256], byte: u8| {
                if let Some(slot) = set.get_mut(usize::from(byte)) {
                    *slot = !invert;
                }
            };
            if matches!(format.at(*p), b'-' | b']') {
                mark(&mut set, format.at(*p));
                *p += 1;
            }
            while format.at(*p) != b']' {
                let byte = format.at(*p);
                if byte == 0 {
                    return Err(Stop::Failure);
                }
                let next = format.at(*p + 1);
                if byte == b'-' && next != 0 && next != b']' {
                    let from = format.at(*p - 1);
                    *p += 1;
                    for inside in from..format.at(*p) {
                        mark(&mut set, inside);
                    }
                }
                mark(&mut set, format.at(*p));
                *p += 1;
            }
        }
        _ => {}
    }
    Ok(set)
}

/// A field's destination for bytes or wide characters, the caller's or
/// allocated.
#[derive(Debug)]
struct Buffer {
    /// Where the next element goes, or null to store nothing.
    ptr: *mut c_void,
    /// How many elements it holds, when allocated.
    cap: usize,
    /// Whether it came from `malloc`.
    allocated: bool,
    /// The size of an element.
    element: usize,
}

impl Buffer {
    /// Stores `value`'s low `element` bytes as element `index`, growing an
    /// allocated buffer when it is full. False if there is no memory.
    fn push(&mut self, index: usize, value: i32) -> bool {
        if self.ptr.is_null() {
            return true;
        }
        if self.element == size_of::<WChar>() {
            // SAFETY: the caller's buffer holds the field, and an allocated one
            // has `cap` elements, of which `index` is below.
            unsafe {
                self.ptr
                    .cast::<WChar>()
                    .wrapping_add(index)
                    .write_unaligned(value)
            };
        } else {
            // SAFETY: as above.
            unsafe { self.ptr.cast::<u8>().wrapping_add(index).write(value as u8) };
        }
        if self.allocated && index + 1 == self.cap {
            let cap = self.cap + self.cap + 1;
            let Some(bytes) = cap.checked_mul(self.element) else {
                return false;
            };
            // SAFETY: the buffer came from `malloc`.
            let grown = unsafe { realloc(self.ptr, bytes) };
            if grown.is_null() {
                return false;
            }
            self.ptr = grown;
            self.cap = cap;
        }
        true
    }

    /// Frees an allocated buffer.
    fn release(&mut self) {
        if self.allocated {
            // SAFETY: the buffer came from `malloc`.
            unsafe { free(self.ptr) };
            self.ptr = null_mut();
        }
    }
}

/// Reads a `%s`, `%c` or `%[` field, of wide characters for `l`, and stores
/// it, allocating it for `m`.
///
/// # Safety
///
/// `dest` must be null or valid for writes of the field: a `char *` or
/// `wchar_t *` to its elements, or with `alloc`, a pointer to store one in.
#[allow(
    clippy::too_many_arguments,
    reason = "these are the conversion's parts"
)]
unsafe fn string<S: Source>(
    reader: &mut Reader<'_, S>,
    format: &CText,
    wide_format: *const WChar,
    p: &mut usize,
    conversion: u8,
    width: usize,
    size: Size,
    dest: *mut c_void,
    alloc: bool,
) -> Result<(), Stop> {
    let set_start = *p;
    let set = byte_set(format, p, conversion)?;
    let wide = size == Size::Long;
    let element = if wide { size_of::<WChar>() } else { 1 };
    let cap = if conversion == b'c' { width + 1 } else { 31 };
    let mut buffer = Buffer {
        ptr: dest,
        cap,
        allocated: false,
        element,
    };
    if alloc {
        let Some(bytes) = cap.checked_mul(element) else {
            return Err(Stop::Failure);
        };
        buffer.ptr = malloc(bytes);
        if buffer.ptr.is_null() {
            return Err(Stop::Failure);
        }
        buffer.allocated = true;
    }

    // SAFETY: an all-zero state is the initial conversion state.
    let mut state: MbState = unsafe { MaybeUninit::zeroed().assume_init() };
    let start = reader.count;
    let mut stored = 0;
    let mut result = Ok(());
    while let Some(byte) = reader.next() {
        if let Some(wc) = reader.src.last_wide() {
            // SAFETY: a wide source comes with the wide format, whose set runs
            // from `set_start` to `*p`.
            if !unsafe { wide_member(wide_format, set_start, *p, conversion, wc) } {
                reader.back(byte);
                break;
            }
            if wide {
                if !buffer.push(stored, wc) {
                    result = Err(Stop::Failure);
                    break;
                }
                stored += 1;
                continue;
            }
            let mut bytes = [0u8; 4];
            let mut encoding = MbState::new();
            // SAFETY: `bytes` has room for the longest sequence.
            let len = unsafe { wcrtomb(bytes.as_mut_ptr().cast(), wc, &raw mut encoding) };
            if len == usize::MAX {
                result = Err(Stop::Failure);
                break;
            }
            for &unit in bytes.get(..len).unwrap_or_default() {
                if !buffer.push(stored, i32::from(unit)) {
                    result = Err(Stop::Failure);
                    break;
                }
                stored += 1;
            }
            if result.is_err() {
                break;
            }
            continue;
        }
        if !set.get(usize::from(byte)).copied().unwrap_or(false) {
            reader.back(byte);
            break;
        }
        let value = if wide {
            let mut wc: WChar = 0;
            let one = byte as c_char;
            // SAFETY: `wc`, `one` and `state` are live locals.
            match unsafe { mbrtowc(&raw mut wc, &raw const one, 1, &raw mut state) } {
                usize::MAX => {
                    result = Err(Stop::Failure);
                    break;
                }
                partial if partial == usize::MAX - 1 => continue,
                _ => wc,
            }
        } else {
            i32::from(byte)
        };
        if !buffer.push(stored, value) {
            result = Err(Stop::Failure);
            break;
        }
        stored += 1;
    }
    // SAFETY: `state` is a live local.
    if result.is_ok() && wide && unsafe { mbsinit(&raw const state) } == 0 {
        result = Err(Stop::Failure);
    }
    let taken = reader.count - start;
    if result.is_ok() && (taken == 0 || (conversion == b'c' && taken != width)) {
        result = Err(Stop::Mismatch);
    }
    if result.is_err() {
        buffer.release();
        return result;
    }
    if conversion != b'c' {
        let _ = buffer.push(stored, 0);
    }
    if alloc {
        // SAFETY: with `m`, the caller passes a pointer to store the string in.
        unsafe { dest.cast::<*mut c_void>().write_unaligned(buffer.ptr) };
    }
    Ok(())
}

/// Argument `n`, counting from 1, of the list that started as `start`.
///
/// # Safety
///
/// The list must hold at least `n` pointers.
unsafe fn nth(start: &mut VaListTag, n: u8) -> *mut c_void {
    let base = VaList::from_tag(start);
    let mut tag = base.copy();
    let mut list = VaList::from_tag(&mut tag);
    for _ in 1..n {
        // SAFETY: the caller vouches for `n` pointers.
        let _ = unsafe { list.next_ptr::<c_void>() };
    }
    // SAFETY: as above.
    unsafe { list.next_ptr() }
}

/// Reads `src` by `format`, storing through the pointers in `ap`, and returns
/// how many fields were stored, or `EOF` if the input ended or failed, or the
/// format was invalid, before the first.
///
/// # Safety
///
/// `format` must be a NUL-terminated string, and `ap` hold a pointer of the
/// right type for each conversion that stores.
pub(super) unsafe fn scan<S: Source>(
    src: &mut S,
    format: *const c_char,
    wide_format: *const WChar,
    ap: VaListArg,
) -> c_int {
    // SAFETY: the caller passes a NUL-terminated format.
    let fmt = unsafe { CText::new(format) };
    // SAFETY: the caller passes its own list.
    let mut list = unsafe { VaList::from_raw(ap) };
    let mut start = list.copy();
    let mut reader = Reader {
        src,
        count: 0,
        left: None,
    };
    let mut stored: c_int = 0;
    let mut p = 0;
    let stop = loop {
        let byte = fmt.at(p);
        if byte == 0 {
            break Stop::Done;
        }
        if is_space(byte) {
            while is_space(fmt.at(p + 1)) {
                p += 1;
            }
            reader.skip_space();
            p += 1;
            continue;
        }
        if byte != b'%' || fmt.at(p + 1) == b'%' {
            reader.limit(0);
            let got = if byte == b'%' {
                p += 1;
                reader.next_after_space()
            } else {
                reader.next()
            };
            match got {
                Some(matched)
                    if matched == fmt.at(p)
                        && (matched < 0x80
                            || reader.src.last_wide().is_none()
                            // SAFETY: a wide source comes with the wide
                            // format, which has a character at `p`.
                            || reader.src.last_wide() == Some(unsafe { wide_format.wrapping_add(p).read() })) =>
                {
                    p += 1;
                    continue;
                }
                Some(other) => {
                    reader.back(other);
                    break Stop::Mismatch;
                }
                None => break Stop::Failure,
            }
        }

        p += 1;
        let dest: *mut c_void = if fmt.at(p) == b'*' {
            p += 1;
            null_mut()
        } else if fmt.at(p).is_ascii_digit() && fmt.at(p + 1) == b'$' {
            let n = fmt.at(p) - b'0';
            p += 2;
            // SAFETY: the caller's list holds the arguments the format names.
            unsafe { nth(&mut start, n) }
        } else {
            // SAFETY: as above.
            unsafe { list.next_ptr() }
        };
        let mut width = 0_usize;
        while fmt.at(p).is_ascii_digit() {
            width = width
                .saturating_mul(10)
                .saturating_add(usize::from(fmt.at(p) - b'0'));
            p += 1;
        }
        let alloc = fmt.at(p) == b'm';
        if alloc {
            p += 1;
        }
        let alloc = alloc && !dest.is_null();
        let mut size = match fmt.at(p) {
            b'h' if fmt.at(p + 1) == b'h' => {
                p += 2;
                Size::Char
            }
            b'h' => {
                p += 1;
                Size::Short
            }
            b'l' if fmt.at(p + 1) == b'l' => {
                p += 2;
                Size::LongLong
            }
            b'l' | b'z' | b't' => {
                p += 1;
                Size::Long
            }
            b'j' => {
                p += 1;
                Size::LongLong
            }
            b'L' => {
                p += 1;
                Size::LongDouble
            }
            _ => Size::Int,
        };
        let mut conversion = fmt.at(p);
        if matches!(conversion, b'C' | b'S') {
            conversion |= 0x20;
            size = Size::Long;
        }
        match conversion {
            b'c' => width = width.max(1),
            b'[' => {}
            b'n' => {
                // SAFETY: `%n` stores through an integer of its size.
                unsafe { store_int(dest, size, reader.count as u64) };
                p += 1;
                continue;
            }
            b'd' | b'i' | b'o' | b'u' | b'x' | b'X' | b'p' | b's' | b'a' | b'e' | b'f' | b'g'
            | b'A' | b'E' | b'F' | b'G' => reader.skip_space(),
            _ => break Stop::Failure,
        }
        reader.limit(width);
        match reader.next() {
            Some(first) => reader.back(first),
            None => break Stop::Failure,
        }
        let converted = match conversion {
            b's' | b'c' | b'[' => {
                // SAFETY: the caller's argument for this field matches it.
                unsafe {
                    string(
                        &mut reader,
                        &fmt,
                        wide_format,
                        &mut p,
                        conversion,
                        width,
                        size,
                        dest,
                        alloc,
                    )
                }
            }
            b'p' | b'x' | b'X' | b'o' | b'd' | b'u' | b'i' => {
                let base = match conversion {
                    b'p' | b'x' | b'X' => 16,
                    b'o' => 8,
                    b'i' => 0,
                    _ => 10,
                };
                // SAFETY: as above.
                unsafe { integer(&mut reader, base, conversion == b'p', size, dest) }
            }
            _ => {
                // SAFETY: as above.
                unsafe { floating(&mut reader, size, dest) }
            }
        };
        if let Err(stop) = converted {
            break stop;
        }
        p += 1;
        if !dest.is_null() {
            stored += 1;
        }
    };
    match stop {
        Stop::Failure if stored == 0 => EOF,
        _ => stored,
    }
}

/// Reads `stream` by `format`, storing through the pointers in `ap`.
///
/// # Safety
///
/// `stream` must be a live stream, `format` a NUL-terminated string, and `ap`
/// a `va_list` holding a pointer of the right type for each conversion.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vfscanf(stream: *mut File, format: *const c_char, ap: VaListArg) -> c_int {
    let op = |inner: &mut Inner| {
        let mut source = Stream { inner };
        // SAFETY: the caller passes a format and its arguments.
        unsafe { scan(&mut source, format, core::ptr::null(), ap) }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, op) }
}

/// Reads the string `s` by `format`, storing through the pointers in `ap`.
///
/// # Safety
///
/// `s` and `format` must be NUL-terminated strings, and `ap` a `va_list`
/// holding a pointer of the right type for each conversion.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vsscanf(s: *const c_char, format: *const c_char, ap: VaListArg) -> c_int {
    let mut source = Text { start: s, pos: 0 };
    // SAFETY: the caller passes a string, a format and its arguments.
    unsafe { scan(&mut source, format, core::ptr::null(), ap) }
}

/// Reads standard input by `format`, storing through the pointers in `ap`.
///
/// # Safety
///
/// As `vfscanf`, without the stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vscanf(format: *const c_char, ap: VaListArg) -> c_int {
    // SAFETY: `stdin` is a live stream, and the caller passes the rest.
    unsafe { vfscanf(stdin.load(Ordering::Relaxed), format, ap) }
}

/// `vfscanf`, under glibc's C99 name.
///
/// # Safety
///
/// As `vfscanf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __isoc99_vfscanf(
    stream: *mut File,
    format: *const c_char,
    ap: VaListArg,
) -> c_int {
    // SAFETY: the caller's promises are `vfscanf`'s.
    unsafe { vfscanf(stream, format, ap) }
}

/// `vsscanf`, under glibc's C99 name.
///
/// # Safety
///
/// As `vsscanf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __isoc99_vsscanf(
    s: *const c_char,
    format: *const c_char,
    ap: VaListArg,
) -> c_int {
    // SAFETY: the caller's promises are `vsscanf`'s.
    unsafe { vsscanf(s, format, ap) }
}

/// `vscanf`, under glibc's C99 name.
///
/// # Safety
///
/// As `vscanf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __isoc99_vscanf(format: *const c_char, ap: VaListArg) -> c_int {
    // SAFETY: the caller's promises are `vscanf`'s.
    unsafe { vscanf(format, ap) }
}

/// `vfscanf`, under glibc's C23 name.
///
/// # Safety
///
/// As `vfscanf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __isoc23_vfscanf(
    stream: *mut File,
    format: *const c_char,
    ap: VaListArg,
) -> c_int {
    // SAFETY: the caller's promises are `vfscanf`'s.
    unsafe { vfscanf(stream, format, ap) }
}

/// `vsscanf`, under glibc's C23 name.
///
/// # Safety
///
/// As `vsscanf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __isoc23_vsscanf(
    s: *const c_char,
    format: *const c_char,
    ap: VaListArg,
) -> c_int {
    // SAFETY: the caller's promises are `vsscanf`'s.
    unsafe { vsscanf(s, format, ap) }
}

/// `vscanf`, under glibc's C23 name.
///
/// # Safety
///
/// As `vscanf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __isoc23_vscanf(format: *const c_char, ap: VaListArg) -> c_int {
    // SAFETY: the caller's promises are `vscanf`'s.
    unsafe { vscanf(format, ap) }
}

va::variadic!(fscanf, 2, vfscanf);
va::variadic!(sscanf, 2, vsscanf);
va::variadic!(scanf, 1, vscanf);
va::variadic!(__isoc99_fscanf, 2, vfscanf);
va::variadic!(__isoc99_sscanf, 2, vsscanf);
va::variadic!(__isoc99_scanf, 1, vscanf);
va::variadic!(__isoc23_fscanf, 2, vfscanf);
va::variadic!(__isoc23_sscanf, 2, vsscanf);
va::variadic!(__isoc23_scanf, 1, vscanf);

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" {
        fn ferrousli_test_sscanf(s: *const c_char, format: *const c_char, ...) -> c_int;
    }

    #[test]
    fn integers_strings_and_counts() {
        let (mut a, mut b) = (0_i32, 0_u32);
        let mut word = [0u8; 16];
        let mut consumed = 0_i32;
        // SAFETY: the arguments match the conversions.
        let n = unsafe {
            ferrousli_test_sscanf(
                c"  -42 0x1f word".as_ptr(),
                c"%d %x %s%n".as_ptr(),
                &raw mut a,
                &raw mut b,
                word.as_mut_ptr(),
                &raw mut consumed,
            )
        };
        assert_eq!((n, a, b, consumed), (3, -42, 31, 15));
        assert_eq!(word.get(..5), Some(&b"word\0"[..]));
        // SAFETY: as above.
        let n = unsafe { ferrousli_test_sscanf(c"".as_ptr(), c"%d".as_ptr(), &raw mut a) };
        assert_eq!(n, EOF);
        // SAFETY: as above.
        let n = unsafe { ferrousli_test_sscanf(c"0x".as_ptr(), c"%x".as_ptr(), &raw mut b) };
        assert_eq!(n, 0);
    }

    #[test]
    fn floats_and_sets() {
        let mut d = 0.0_f64;
        let mut key = [0u8; 8];
        // SAFETY: the arguments match the conversions.
        let n = unsafe {
            ferrousli_test_sscanf(
                c"k=-2.5e2".as_ptr(),
                c"%[^=]=%lf".as_ptr(),
                key.as_mut_ptr(),
                &raw mut d,
            )
        };
        assert_eq!((n, d), (2, -250.0));
        assert_eq!(key.get(..2), Some(&b"k\0"[..]));
    }

    #[test]
    fn number_prefixes_are_read_as_c_describes_them() {
        let mut state = FloatState::default();
        let token = b"0x1p-3";
        let accepted = token
            .iter()
            .enumerate()
            .all(|(len, &byte)| state.accepts(token, len, byte));
        assert!(accepted);
        let mut state = FloatState::default();
        assert!(state.accepts(b"", 0, b'1'));
        assert!(!state.accepts(b"1", 1, b'k'));
    }
}
