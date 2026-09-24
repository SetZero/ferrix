//! `iconv.h`: converting text between character sets. GLib converts with
//! it.
//!
//! The sets are the Unicode encodings and the three single-byte ones a
//! Linux program meets without a locale: UTF-8; ASCII; ISO-8859-1; CP1252;
//! UTF-16 and UCS-2; UTF-32 and UCS-4, and `WCHAR_T`; each under glibc's
//! names for it, compared without case, `-` or `_`. The empty name is the
//! locale's code set. Another name fails `iconv_open` with `EINVAL`,
//! which GLib takes as "not supported".
//!
//! What glibc does and this matches, compared with glibc 2.43 on the host:
//!
//! * `UTF-16` and `UTF-32` read a byte-order mark if there is one and are
//!   little-endian otherwise, and write one before the first character.
//!   `UCS-4` is big-endian, `UCS-2` and `WCHAR_T` little-endian, neither
//!   with a mark; the `LE` and `BE` names are what they say.
//! * An invalid sequence stops with `EILSEQ` and an incomplete one at the
//!   end of the input with `EINVAL`, each leaving the input at it, and a
//!   full output buffer with `E2BIG`. A character the target cannot hold is
//!   `EILSEQ` too.
//! * `//IGNORE` skips both and goes on, and then answers `EILSEQ` once the
//!   input is used up. `//TRANSLIT` writes `?` for a character the target
//!   cannot hold, and counts it as irreversible. glibc's tables would write
//!   some better, `EUR` for `€` in ISO-8859-1; those tables are not here.

use core::ffi::{CStr, c_char, c_int, c_void};
use core::mem::size_of;

use crate::errno;
use crate::malloc::{free, malloc};

/// The byte order of a multi-byte encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    /// Most significant byte first.
    Big,
    /// Least significant byte first: every architecture here.
    Little,
}

/// A character set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Set {
    /// UTF-8.
    Utf8,
    /// ASCII.
    Ascii,
    /// ISO-8859-1, whose bytes are the first 256 code points.
    Latin1,
    /// Windows' Western European code page.
    Cp1252,
    /// UTF-16, with surrogate pairs; `bom` for the name without an order.
    Utf16 { order: Order, bom: bool },
    /// UCS-2: UTF-16 without surrogates.
    Ucs2(Order),
    /// UTF-32; `bom` for the name without an order.
    Utf32 { order: Order, bom: bool },
}

/// CP1252's characters from 0x80 to 0x9F, 0 where it has none.
const CP1252_HIGH: [u16; 32] = [
    0x20ac, 0, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021, 0x02c6, 0x2030, 0x0160, 0x2039,
    0x0152, 0, 0x017d, 0, 0, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014, 0x02dc,
    0x2122, 0x0161, 0x203a, 0x0153, 0, 0x017e, 0x0178,
];

/// Each name, without case, `-` or `_`, and its set.
const NAMES: [(&[u8], Set); 29] = [
    (b"UTF8", Set::Utf8),
    (b"ASCII", Set::Ascii),
    (b"USASCII", Set::Ascii),
    (b"ANSIX3.41968", Set::Ascii),
    (b"646", Set::Ascii),
    (b"ISO646US", Set::Ascii),
    (b"US", Set::Ascii),
    (b"ISO88591", Set::Latin1),
    (b"ISO885911987", Set::Latin1),
    (b"LATIN1", Set::Latin1),
    (b"L1", Set::Latin1),
    (b"CP819", Set::Latin1),
    (b"IBM819", Set::Latin1),
    (b"CP1252", Set::Cp1252),
    (b"WINDOWS1252", Set::Cp1252),
    (
        b"UTF16",
        Set::Utf16 {
            order: Order::Little,
            bom: true,
        },
    ),
    (
        b"UTF16LE",
        Set::Utf16 {
            order: Order::Little,
            bom: false,
        },
    ),
    (
        b"UTF16BE",
        Set::Utf16 {
            order: Order::Big,
            bom: false,
        },
    ),
    (b"UCS2", Set::Ucs2(Order::Little)),
    (b"UCS2LE", Set::Ucs2(Order::Little)),
    (b"UCS2BE", Set::Ucs2(Order::Big)),
    (
        b"UTF32",
        Set::Utf32 {
            order: Order::Little,
            bom: true,
        },
    ),
    (
        b"UTF32LE",
        Set::Utf32 {
            order: Order::Little,
            bom: false,
        },
    ),
    (
        b"UTF32BE",
        Set::Utf32 {
            order: Order::Big,
            bom: false,
        },
    ),
    (
        b"UCS4",
        Set::Utf32 {
            order: Order::Big,
            bom: false,
        },
    ),
    (
        b"UCS4BE",
        Set::Utf32 {
            order: Order::Big,
            bom: false,
        },
    ),
    (
        b"UCS4LE",
        Set::Utf32 {
            order: Order::Little,
            bom: false,
        },
    ),
    (
        b"ISO10646UCS4",
        Set::Utf32 {
            order: Order::Big,
            bom: false,
        },
    ),
    (
        b"WCHART",
        Set::Utf32 {
            order: Order::Little,
            bom: false,
        },
    ),
];

/// A conversion descriptor.
#[derive(Debug)]
struct Descriptor {
    /// What is read.
    from: Set,
    /// What is written.
    to: Set,
    /// `//TRANSLIT`: write `?` for what the target cannot hold.
    translit: bool,
    /// `//IGNORE`: skip what cannot be read or written.
    ignore: bool,
    /// The byte order a byte-order mark set for the input, once read.
    input_order: Option<Order>,
    /// Whether the output's byte-order mark is still to be written.
    mark_pending: bool,
}

/// The set a name names, and its `//` flags: `(set, translit, ignore)`.
fn parse_name(name: &[u8]) -> Option<(Set, bool, bool)> {
    let mut parts = name.split(|&b| b == b'/').filter(|p| !p.is_empty());
    let base = parts.next().unwrap_or_default();
    let (mut translit, mut ignore) = (false, false);
    for flag in parts {
        if flag.eq_ignore_ascii_case(b"TRANSLIT") {
            translit = true;
        } else if flag.eq_ignore_ascii_case(b"IGNORE") {
            ignore = true;
        }
    }
    let mut key = [0u8; 32];
    let mut len = 0;
    for &byte in base.iter().filter(|&&b| b != b'-' && b != b'_') {
        *key.get_mut(len)? = byte.to_ascii_uppercase();
        len += 1;
    }
    let key = key.get(..len)?;
    if key.is_empty() {
        return Some((locale_set(), translit, ignore));
    }
    NAMES
        .iter()
        .find(|(n, _)| *n == key)
        .map(|&(_, set)| (set, translit, ignore))
}

/// The locale's code set: UTF-8 in a UTF-8 locale, ASCII in C.
fn locale_set() -> Set {
    /// `CODESET`.
    const CODESET: c_int = 14;
    let name = crate::locale::nl_langinfo(CODESET);
    // SAFETY: `nl_langinfo` answers a static C string.
    if unsafe { CStr::from_ptr(name) }.to_bytes() == b"UTF-8" {
        Set::Utf8
    } else {
        Set::Ascii
    }
}

/// Opens a conversion from `fromcode` to `tocode`, or fails with `EINVAL`
/// for a set there is not.
///
/// # Safety
///
/// Both must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn iconv_open(tocode: *const c_char, fromcode: *const c_char) -> *mut c_void {
    let invalid = core::ptr::without_provenance_mut(usize::MAX);
    // SAFETY: the caller passes NUL-terminated strings.
    let to = unsafe { CStr::from_ptr(tocode) };
    // SAFETY: as above.
    let from = unsafe { CStr::from_ptr(fromcode) };
    let (Some((to, translit, ignore)), Some((from, _, _))) =
        (parse_name(to.to_bytes()), parse_name(from.to_bytes()))
    else {
        errno::set(errno::EINVAL);
        return invalid;
    };
    let cd = malloc(size_of::<Descriptor>()).cast::<Descriptor>();
    if cd.is_null() {
        return invalid;
    }
    let descriptor = Descriptor {
        from,
        to,
        translit,
        ignore,
        input_order: None,
        mark_pending: has_mark(to),
    };
    // SAFETY: fresh memory for one descriptor.
    unsafe { cd.write(descriptor) };
    cd.cast()
}

/// Closes a conversion.
///
/// # Safety
///
/// `cd` must be from `iconv_open` and not used again.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn iconv_close(cd: *mut c_void) -> c_int {
    // SAFETY: the caller's descriptor, from `malloc`.
    unsafe { free(cd) };
    0
}

/// Whether writing `set` starts with a byte-order mark.
fn has_mark(set: Set) -> bool {
    matches!(
        set,
        Set::Utf16 { bom: true, .. } | Set::Utf32 { bom: true, .. }
    )
}

/// Why a character could not be read.
enum ReadError {
    /// The input ends inside a character.
    Incomplete,
    /// The bytes are not a character.
    Invalid,
}

/// `n` bytes of `input` as a number in `order`.
fn number(input: &[u8], n: usize, order: Order) -> Option<u32> {
    let bytes = input.get(..n)?;
    let fold = |acc: u32, &b: &u8| (acc << 8) | u32::from(b);
    Some(match order {
        Order::Big => bytes.iter().fold(0, fold),
        Order::Little => bytes.iter().rev().fold(0, fold),
    })
}

/// Reads one character from `input`: its code point and how many bytes it
/// took.
fn decode(set: Set, order: Order, input: &[u8]) -> Result<(u32, usize), ReadError> {
    let first = u32::from(*input.first().ok_or(ReadError::Incomplete)?);
    match set {
        Set::Ascii if first < 0x80 => Ok((first, 1)),
        Set::Ascii => Err(ReadError::Invalid),
        Set::Latin1 => Ok((first, 1)),
        Set::Cp1252 => match first {
            0x80..=0x9f => match CP1252_HIGH.get(first as usize - 0x80) {
                Some(&c) if c != 0 => Ok((u32::from(c), 1)),
                _ => Err(ReadError::Invalid),
            },
            _ => Ok((first, 1)),
        },
        Set::Utf8 => decode_utf8(input),
        Set::Utf16 { .. } | Set::Ucs2(_) => {
            let unit = number(input, 2, order).ok_or(ReadError::Incomplete)?;
            if !(0xd800..0xe000).contains(&unit) {
                return Ok((unit, 2));
            }
            if matches!(set, Set::Ucs2(_)) || unit >= 0xdc00 {
                return Err(ReadError::Invalid);
            }
            let low = number(input.get(2..).unwrap_or_default(), 2, order)
                .ok_or(ReadError::Incomplete)?;
            if !(0xdc00..0xe000).contains(&low) {
                return Err(ReadError::Invalid);
            }
            Ok((0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00), 4))
        }
        Set::Utf32 { .. } => {
            let c = number(input, 4, order).ok_or(ReadError::Incomplete)?;
            if c > 0x10_ffff || (0xd800..0xe000).contains(&c) {
                return Err(ReadError::Invalid);
            }
            Ok((c, 4))
        }
    }
}

/// Reads one UTF-8 character, refusing overlong forms, surrogates and code
/// points past U+10FFFF.
fn decode_utf8(input: &[u8]) -> Result<(u32, usize), ReadError> {
    let first = *input.first().ok_or(ReadError::Incomplete)?;
    let (len, mut c, min) = match first {
        0x00..=0x7f => return Ok((u32::from(first), 1)),
        0xc2..=0xdf => (2, u32::from(first & 0x1f), 0x80),
        0xe0..=0xef => (3, u32::from(first & 0x0f), 0x800),
        0xf0..=0xf4 => (4, u32::from(first & 0x07), 0x1_0000),
        _ => return Err(ReadError::Invalid),
    };
    for i in 1..len {
        let Some(&byte) = input.get(i) else {
            return Err(ReadError::Incomplete);
        };
        if byte & 0xc0 != 0x80 {
            return Err(ReadError::Invalid);
        }
        c = (c << 6) | u32::from(byte & 0x3f);
        // Refuse as early as glibc: an overlong or out-of-range prefix is
        // invalid even before the rest of it arrives.
        if i == 1 && ((len == 3 && c < 0x20) || (len == 4 && !(0x10..0x110).contains(&c))) {
            return Err(ReadError::Invalid);
        }
    }
    if c < min || (0xd800..0xe000).contains(&c) || c > 0x10_ffff {
        return Err(ReadError::Invalid);
    }
    Ok((c, len))
}

/// Writes `c` in `set` into `out`, returning how many bytes, or `None` if
/// the set cannot hold it.
fn encode(set: Set, c: u32, out: &mut [u8; 4]) -> Option<usize> {
    let put = |out: &mut [u8; 4], value: u32, n: usize, order: Order| {
        for (i, slot) in out.iter_mut().take(n).enumerate() {
            let shift = match order {
                Order::Big => 8 * (n - 1 - i),
                Order::Little => 8 * i,
            };
            *slot = (value >> shift) as u8;
        }
        n
    };
    match set {
        Set::Ascii if c < 0x80 => Some(put(out, c, 1, Order::Big)),
        Set::Latin1 if c < 0x100 => Some(put(out, c, 1, Order::Big)),
        Set::Cp1252 if c < 0x80 || (0xa0..0x100).contains(&c) => Some(put(out, c, 1, Order::Big)),
        Set::Cp1252 => {
            let index = CP1252_HIGH
                .iter()
                .position(|&h| h != 0 && u32::from(h) == c)?;
            Some(put(out, 0x80 + index as u32, 1, Order::Big))
        }
        Set::Ascii | Set::Latin1 => None,
        Set::Utf8 => {
            let n = match c {
                0..0x80 => return Some(put(out, c, 1, Order::Big)),
                0x80..0x800 => 2,
                0x800..0x1_0000 => 3,
                _ => 4,
            };
            let lead: u32 = [0, 0, 0xc0, 0xe0, 0xf0].get(n).copied().unwrap_or(0);
            for (i, slot) in out.iter_mut().take(n).enumerate() {
                let shift = 6 * (n - 1 - i);
                let bits = (c >> shift) & 0x3f;
                *slot = if i == 0 {
                    (lead | (c >> shift)) as u8
                } else {
                    (0x80 | bits) as u8
                };
            }
            Some(n)
        }
        Set::Ucs2(order) if c < 0x1_0000 => Some(put(out, c, 2, order)),
        Set::Ucs2(_) => None,
        Set::Utf16 { order, .. } if c < 0x1_0000 => Some(put(out, c, 2, order)),
        Set::Utf16 { order, .. } => {
            let v = c - 0x1_0000;
            let mut high = [0u8; 4];
            let _ = put(&mut high, 0xd800 + (v >> 10), 2, order);
            let mut low = [0u8; 4];
            let _ = put(&mut low, 0xdc00 + (v & 0x3ff), 2, order);
            *out = [high[0], high[1], low[0], low[1]];
            Some(4)
        }
        Set::Utf32 { order, .. } => Some(put(out, c, 4, order)),
    }
}

/// The byte-order mark of `set`, as it writes one.
fn mark(set: Set) -> &'static [u8] {
    match set {
        Set::Utf16 { .. } => &[0xff, 0xfe],
        Set::Utf32 { .. } => &[0xff, 0xfe, 0, 0],
        _ => &[],
    }
}

/// Reads a byte-order mark at the start of `input` for `set`, which reads
/// one: the order it sets and its length, or the default and 0.
fn read_mark(set: Set, input: &[u8]) -> Result<(Order, usize), ReadError> {
    match set {
        Set::Utf16 { bom: true, .. } => match input {
            [0xfe, 0xff, ..] => Ok((Order::Big, 2)),
            [0xff, 0xfe, ..] => Ok((Order::Little, 2)),
            [_] | [] => Err(ReadError::Incomplete),
            _ => Ok((Order::Little, 0)),
        },
        Set::Utf32 { bom: true, .. } => match input {
            [0, 0, 0xfe, 0xff, ..] => Ok((Order::Big, 4)),
            [0xff, 0xfe, 0, 0, ..] => Ok((Order::Little, 4)),
            _ if input.len() < 4 => Err(ReadError::Incomplete),
            _ => Ok((Order::Little, 0)),
        },
        Set::Utf16 { order, .. } | Set::Utf32 { order, .. } | Set::Ucs2(order) => Ok((order, 0)),
        _ => Ok((Order::Little, 0)),
    }
}

/// Converts from `*inbuf`, `*inleft` bytes, into `*outbuf`, `*outleft`
/// bytes, advancing all four. Returns the characters converted
/// irreversibly, or -1 with `errno` set. With a null `inbuf` or `*inbuf`
/// it starts the conversion over.
///
/// # Safety
///
/// `cd` must be from `iconv_open`, and the buffers valid for their lengths.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn iconv(
    cd: *mut c_void,
    inbuf: *mut *mut c_char,
    inleft: *mut usize,
    outbuf: *mut *mut c_char,
    outleft: *mut usize,
) -> usize {
    if cd.is_null() || cd.addr() == usize::MAX {
        errno::set(errno::EBADF);
        return usize::MAX;
    }
    // SAFETY: the caller's descriptor.
    let d = unsafe { &mut *cd.cast::<Descriptor>() };
    // SAFETY: the caller passes null or a valid pointer.
    if inbuf.is_null() || unsafe { inbuf.read() }.is_null() {
        d.input_order = None;
        d.mark_pending = has_mark(d.to);
        return 0;
    }
    // SAFETY: the caller's pointers are valid.
    let in_start = unsafe { inbuf.read() }.cast::<u8>();
    // SAFETY: as above.
    let in_len = unsafe { inleft.read() };
    // A null output buffer holds nothing.
    let out_start = if outbuf.is_null() {
        core::ptr::null_mut()
    } else {
        // SAFETY: as above.
        unsafe { outbuf.read() }
    }
    .cast::<u8>();
    let out_len = if out_start.is_null() || outleft.is_null() {
        0
    } else {
        // SAFETY: as above.
        unsafe { outleft.read() }
    };
    // SAFETY: the input holds `in_len` bytes.
    let mut input = unsafe { core::slice::from_raw_parts(in_start, in_len) };
    let mut output: &mut [u8] = if out_start.is_null() {
        &mut []
    } else {
        // SAFETY: the output holds `out_len` bytes, which nothing else uses.
        unsafe { core::slice::from_raw_parts_mut(out_start, out_len) }
    };
    let mut irreversible = 0_usize;
    let mut ignored = false;
    let result = loop {
        if input.is_empty() {
            break Ok(());
        }
        let order = match d.input_order {
            Some(order) => order,
            None => match read_mark(d.from, input) {
                Ok((order, skip)) => {
                    d.input_order = Some(order);
                    input = input.get(skip..).unwrap_or_default();
                    continue;
                }
                Err(_) => break Err(errno::EINVAL),
            },
        };
        let (c, used) = match decode(d.from, order, input) {
            Ok(read) => read,
            Err(ReadError::Incomplete) => break Err(errno::EINVAL),
            Err(ReadError::Invalid) if d.ignore => {
                ignored = true;
                input = input.get(1..).unwrap_or_default();
                continue;
            }
            Err(ReadError::Invalid) => break Err(errno::EILSEQ),
        };
        let mut bytes = [0u8; 4];
        let written = match encode(d.to, c, &mut bytes) {
            Some(n) => n,
            None if d.translit => {
                irreversible += 1;
                encode(d.to, u32::from(b'?'), &mut bytes).unwrap_or(0)
            }
            None if d.ignore => {
                ignored = true;
                input = input.get(used..).unwrap_or_default();
                continue;
            }
            None => break Err(errno::EILSEQ),
        };
        let prefix = if d.mark_pending { mark(d.to) } else { &[] };
        if output.len() < prefix.len() + written {
            break Err(errno::E2BIG);
        }
        let (head, rest) = core::mem::take(&mut output).split_at_mut(prefix.len() + written);
        let (mark_part, char_part) = head.split_at_mut(prefix.len());
        mark_part.copy_from_slice(prefix);
        char_part.copy_from_slice(bytes.get(..written).unwrap_or_default());
        output = rest;
        d.mark_pending = false;
        input = input.get(used..).unwrap_or_default();
    };
    let consumed = in_len - input.len();
    // SAFETY: the caller's pointers; each moves by what was used.
    unsafe { inbuf.write(in_start.wrapping_add(consumed).cast()) };
    // SAFETY: as above.
    unsafe { inleft.write(input.len()) };
    if !out_start.is_null() {
        let produced = out_len - output.len();
        // SAFETY: as above.
        unsafe { outbuf.write(out_start.wrapping_add(produced).cast()) };
        // SAFETY: as above.
        unsafe { outleft.write(output.len()) };
    }
    match result {
        Ok(()) if ignored => {
            errno::set(errno::EILSEQ);
            usize::MAX
        }
        Ok(()) => irreversible,
        Err(error) => {
            errno::set(error);
            usize::MAX
        }
    }
}
