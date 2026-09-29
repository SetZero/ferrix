//! `stdlib.h`'s `_Float128` conversions on x86-64: `strtof128` and
//! `strfromf128`, which GCC's C++ runtime imports for `std::from_chars` and
//! `std::to_chars` of `__float128`, and so everything linking `libstdc++`,
//! the Steam client's browser helper among them.
//!
//! Both are exact: the parse is [`crate::strtod`]'s, for IEEE binary128, and
//! the text [`crate::stdio::float`]'s, which works on a binary128's bits on
//! every architecture. Rust has no `f128` on stable, and the SysV ABI passes
//! and returns `_Float128` in `xmm0`, which no Rust type reaches, so each C
//! name is a short shim moving the sixteen bytes between `xmm0` and memory,
//! as `strtold` moves an x87 value.
//!
//! On AArch64 `_Float128` is `long double`, which `strtold` and `printf`'s
//! `%L` conversions already handle, and ARMv7-A has no `_Float128`; neither
//! exports these names yet.

use core::ffi::{c_char, c_int};

use crate::errno;
use crate::stdio::float::{Float, format};
use crate::stdio::printf::{Spec, StringSink};

/// Parses `s` as a binary128 and writes its sixteen bytes, little-endian, to
/// `out`. [`strtof128`] calls it and loads the result into `xmm0`.
///
/// # Safety
///
/// As `strtod`, and `out` must be valid to write sixteen bytes.
unsafe extern "C" fn strtof128_bits(s: *const c_char, endptr: *mut *mut c_char, out: *mut u8) {
    // SAFETY: the caller's contract is `convert`'s.
    let bits = unsafe { crate::strtod::convert(s, endptr, &crate::float::BINARY128) };
    let bytes = bits.to_le_bytes();
    let mut k = 0;
    while let Some(&byte) = bytes.get(k) {
        // SAFETY: the caller vouches for sixteen bytes at `out`.
        unsafe { out.wrapping_add(k).write(byte) };
        k += 1;
    }
}

/// Parses a `_Float128`, returned in `xmm0`. C declares it `_Float128
/// strtof128(const char *, char **)`; the Rust signature shows no return
/// value because the value is in no register Rust knows.
///
/// # Safety
///
/// As `strtod`.
#[unsafe(naked)]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strtof128(s: *const c_char, endptr: *mut *mut c_char) {
    // On entry the stack is 8 below a multiple of 16. Taking 24 aligns it for
    // the call and leaves 16 bytes for the result. `rdi` and `rsi` pass
    // through unchanged.
    core::arch::naked_asm!(
        "sub rsp, 24",
        "mov rdx, rsp",
        "call {convert}",
        "movdqu xmm0, xmmword ptr [rsp]",
        "add rsp, 24",
        "ret",
        convert = sym strtof128_bits,
    )
}

/// The precision and conversion of a `strfromf128` format, `%[.precision]c`
/// with `c` one of `aAeEfFgG`, or `None` for any other. An empty precision
/// is zero, as `printf`'s is.
fn directive(fmt: &[u8]) -> Option<(Option<usize>, u8)> {
    let rest = fmt.strip_prefix(b"%")?;
    let (&conversion, body) = rest.split_last()?;
    if !b"aAeEfFgG".contains(&conversion) {
        return None;
    }
    let precision = match body.strip_prefix(b".") {
        None if body.is_empty() => None,
        None => return None,
        Some(digits) => {
            let mut value = 0usize;
            for &digit in digits {
                if !digit.is_ascii_digit() {
                    return None;
                }
                value = value
                    .checked_mul(10)?
                    .checked_add(usize::from(digit - b'0'))?;
            }
            Some(value)
        }
    };
    Some((precision, conversion))
}

/// [`strfromf128`]'s work, with the value's bits at `value`.
///
/// # Safety
///
/// As [`strfromf128`], and `value` must be valid to read sixteen bytes.
unsafe extern "C" fn strfromf128_bits(
    str: *mut c_char,
    n: usize,
    fmt: *const c_char,
    value: *const u8,
) -> c_int {
    let mut bytes = [0u8; 16];
    let mut k = 0;
    while let Some(slot) = bytes.get_mut(k) {
        // SAFETY: the caller vouches for sixteen bytes at `value`.
        *slot = unsafe { value.wrapping_add(k).read() };
        k += 1;
    }
    // SAFETY: the caller passes a NUL-terminated format.
    let text = unsafe { core::ffi::CStr::from_ptr(fmt) }.to_bytes();
    let Some((precision, conversion)) = directive(text) else {
        errno::set(errno::EINVAL);
        return -1;
    };
    let spec = Spec {
        flags: 0,
        width: 0,
        precision,
    };
    // SAFETY: the caller vouches for `n` bytes, of which `n - 1` are for text.
    let mut sink = unsafe { StringSink::new(str.cast(), n.saturating_sub(1)) };
    let written = format(
        &mut sink,
        &spec,
        conversion,
        Float::Binary128(u128::from_le_bytes(bytes)),
        0,
    );
    if n != 0 {
        // SAFETY: `used` is at most `n - 1`.
        unsafe { str.wrapping_add(sink.used()).write(0) };
    }
    match written.map(c_int::try_from) {
        Ok(Ok(count)) => count,
        Ok(Err(_)) => {
            errno::set(errno::EOVERFLOW);
            -1
        }
        Err(error) => {
            errno::set(error);
            -1
        }
    }
}

/// Writes the `_Float128` in `xmm0` as the format `fmt` says, `%[.p]c` with
/// `c` one of `aAeEfFgG`, into the `n` bytes at `str`, truncated and
/// terminated as `snprintf`'s output is, and returns the length the whole text
/// takes. Any other format is -1 with `EINVAL`, where C leaves it undefined.
/// C declares it `int strfromf128(char *, size_t, const char *, _Float128)`;
/// the Rust signature leaves the value out because it is in `xmm0`.
///
/// # Safety
///
/// `str` must be valid for writes of `n` bytes and `fmt` a NUL-terminated
/// string.
#[unsafe(naked)]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strfromf128(str: *mut c_char, n: usize, fmt: *const c_char) -> c_int {
    // The value goes to the 16 bytes the aligning 24 leave, and its address
    // in `rcx`; `rdi`, `rsi` and `rdx` pass through unchanged.
    core::arch::naked_asm!(
        "sub rsp, 24",
        "movdqu xmmword ptr [rsp], xmm0",
        "mov rcx, rsp",
        "call {write}",
        "add rsp, 24",
        "ret",
        write = sym strfromf128_bits,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directive_is_one_conversion_with_an_optional_precision() {
        assert_eq!(directive(b"%g"), Some((None, b'g')));
        assert_eq!(directive(b"%.36e"), Some((Some(36), b'e')));
        assert_eq!(directive(b"%.a"), Some((Some(0), b'a')));
        assert_eq!(directive(b"%d"), None);
        assert_eq!(directive(b"%5g"), None);
        assert_eq!(directive(b"g"), None);
        assert_eq!(directive(b"%"), None);
    }

    #[test]
    fn a_binary128_parses_and_prints_back_exactly() {
        let mut bits = [0u8; 16];
        let text = c"0.1";
        // SAFETY: a literal, a null `endptr` and a 16-byte local.
        unsafe { strtof128_bits(text.as_ptr(), core::ptr::null_mut(), bits.as_mut_ptr()) };
        // 0.1 rounded to binary128: 0x3ffb999999999999999999999999999a.
        assert_eq!(
            u128::from_le_bytes(bits),
            0x3ffb_9999_9999_9999_9999_9999_9999_999a
        );
        let mut out = [0 as c_char; 64];
        // SAFETY: a 64-byte buffer, a literal format and the parsed bytes.
        let len =
            unsafe { strfromf128_bits(out.as_mut_ptr(), 64, c"%.36g".as_ptr(), bits.as_ptr()) };
        // SAFETY: the buffer was terminated.
        let printed = unsafe { core::ffi::CStr::from_ptr(out.as_ptr()) }.to_bytes();
        assert_eq!(printed, b"0.100000000000000000000000000000000005");
        assert_eq!(len, 38);
        // Truncated, still terminated, and the length of the whole.
        // SAFETY: as above, with a 5-byte room.
        let len =
            unsafe { strfromf128_bits(out.as_mut_ptr(), 5, c"%.36g".as_ptr(), bits.as_ptr()) };
        // SAFETY: as above.
        let printed = unsafe { core::ffi::CStr::from_ptr(out.as_ptr()) }.to_bytes();
        assert_eq!(printed, b"0.10");
        assert_eq!(len, 38);
    }
}
