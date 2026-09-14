//! `assert.h`: the function a failed `assert` calls.
//!
//! The macro is in `include/assert.h`. When its expression is false it calls
//! `__assert_fail` with the expression's text, the file, the line and the
//! function, which writes musl's message and aborts:
//!
//! ```text
//! Assertion failed: expr (file: func: line)
//! ```
//!
//! That is musl 1.2.5's `src/exit/assert.c` (MIT). glibc's message says the
//! same in another order, after the program's name; the message is not part
//! of either library's ABI, and musl's is kept.
//!
//! musl writes it with `fprintf` to `stderr`. This writes it to file
//! descriptor 2 itself, in one `writev`, because a failed assertion says the
//! program's state is not what it believed, and that may include the stdio
//! state or the lock of `stderr`. Nothing buffered in other streams is
//! flushed, as with musl, since `abort` flushes nothing.

use core::ffi::{c_char, c_int};
use core::ptr::null_mut;

use crate::signal::abort;
use crate::string::strlen;
use crate::syscall::{self, nr};
use crate::uio::Iovec;

/// How many pieces the message is written in.
const PARTS: usize = 9;

/// The longest `int` in decimal, with its sign.
const DECIMAL_LEN: usize = 11;

/// The bytes of `text`, or `(null)` for a null pointer, as musl's `printf`
/// writes one.
///
/// # Safety
///
/// `text` must be null or a NUL-terminated string that stays put for `'a`.
unsafe fn bytes<'a>(text: *const c_char) -> &'a [u8] {
    if text.is_null() {
        return b"(null)";
    }
    // SAFETY: the caller promises a NUL-terminated string.
    let len = unsafe { strlen(text) };
    // SAFETY: `strlen` found a NUL at `len`, so that many bytes are readable.
    unsafe { core::slice::from_raw_parts(text.cast::<u8>(), len) }
}

/// `value` in decimal, as `%d` writes it, in the end of `buffer`.
fn decimal(value: c_int, buffer: &mut [u8; DECIMAL_LEN]) -> &[u8] {
    let mut rest = value.unsigned_abs();
    let mut start = DECIMAL_LEN;
    loop {
        start -= 1;
        if let Some(slot) = buffer.get_mut(start) {
            *slot = b'0'.wrapping_add((rest % 10) as u8);
        }
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    if value < 0 {
        start -= 1;
        if let Some(slot) = buffer.get_mut(start) {
            *slot = b'-';
        }
    }
    buffer.get(start..).unwrap_or(&[])
}

/// Writes `parts` to standard error, one after the other.
///
/// One `writev` writes the whole message unless the kernel stops short, in
/// which case the rest follows in further calls. A failure gives up: there is
/// nobody left to tell.
fn write_parts(parts: &mut [&[u8]; PARTS]) {
    loop {
        let mut vector = [Iovec {
            iov_base: null_mut(),
            iov_len: 0,
        }; PARTS];
        for (slot, part) in vector.iter_mut().zip(parts.iter()) {
            slot.iov_base = part.as_ptr().cast_mut().cast();
            slot.iov_len = part.len();
        }
        // SAFETY: each entry names a live slice, which the kernel only reads.
        let ret = unsafe { syscall::syscall3(nr::WRITEV, 2, vector.as_ptr().addr(), PARTS) };
        let mut written = match usize::try_from(ret) {
            Ok(written) if written > 0 => written,
            _ => return,
        };
        let mut left = false;
        for part in parts.iter_mut() {
            let skip = written.min(part.len());
            *part = part.get(skip..).unwrap_or(&[]);
            written -= skip;
            left |= !part.is_empty();
        }
        if !left {
            return;
        }
    }
}

/// Called by `assert` when its expression is false: writes
/// `Assertion failed: expr (file: func: line)` to standard error and aborts.
///
/// musl's header declares `line` as `int` and glibc's as `unsigned int`. Both
/// arrive as the same 32 bits, and this prints them as musl does, signed.
///
/// # Safety
///
/// `expr`, `file` and `func` must each be null or a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __assert_fail(
    expr: *const c_char,
    file: *const c_char,
    line: c_int,
    func: *const c_char,
) -> ! {
    let mut buffer = [0; DECIMAL_LEN];
    let mut parts: [&[u8]; PARTS] = [
        b"Assertion failed: ",
        // SAFETY: the caller promises a string or null.
        unsafe { bytes(expr) },
        b" (",
        // SAFETY: the caller promises a string or null.
        unsafe { bytes(file) },
        b": ",
        // SAFETY: the caller promises a string or null.
        unsafe { bytes(func) },
        b": ",
        decimal(line, &mut buffer),
        b")\n",
    ];
    write_parts(&mut parts);
    abort()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(value: c_int) -> String {
        let mut buffer = [0; DECIMAL_LEN];
        String::from_utf8_lossy(decimal(value, &mut buffer)).into_owned()
    }

    #[test]
    fn a_line_is_written_as_percent_d_writes_it() {
        assert_eq!(written(0), "0");
        assert_eq!(written(7), "7");
        assert_eq!(written(100), "100");
        assert_eq!(written(c_int::MAX), "2147483647");
        // glibc's `unsigned int` past `INT_MAX` reads as musl's `int` does.
        assert_eq!(written(-1), "-1");
        assert_eq!(written(c_int::MIN), "-2147483648");
    }

    #[test]
    fn a_null_string_is_written_as_printf_writes_it() {
        // SAFETY: a null pointer is allowed.
        assert_eq!(unsafe { bytes(core::ptr::null()) }, b"(null)");
        // SAFETY: a string literal is NUL-terminated.
        assert_eq!(unsafe { bytes(c"x == y".as_ptr()) }, b"x == y");
    }
}
