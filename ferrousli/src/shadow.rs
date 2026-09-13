//! `shadow.h`'s `getspnam_r`: a user's entry in the shadow password file.
//!
//! This follows musl's `passwd/getspnam_r.c`. An Openwall-style
//! `/etc/tcb/<name>/shadow` is read if it exists, opened without following a
//! link or blocking, and only if it is a regular file; otherwise
//! `/etc/shadow`. Nothing is allocated: each line is read into the caller's
//! buffer, and a matching line that does not fit fails with `ERANGE`. A name
//! starting with `.` or holding `/` is refused, so it cannot reach another
//! file, and so is a buffer too small for the name and a hundred bytes more.
//!
//! The numeric fields are decimal, and an empty one is -1. `getspnam`,
//! `getspent` and the rest of `shadow.h` are not here yet.

use core::ffi::{c_char, c_int, c_long, c_uint, c_ulong};
use core::mem::size_of;
use core::ptr::null_mut;

use crate::errno;
use crate::fcntl::open;
use crate::pwd::{colon, cut, last_errno};
use crate::stat::{Stat, fstat};
use crate::stdio::file::File;
use crate::stdio::io::fgets;
use crate::stdio::open::{fclose, fdopen, fopen};
use crate::string::strlen;
use crate::unistd::close;

/// `O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`, from
/// `asm-generic/fcntl.h`.
const TCB_FLAGS: c_int = 0o400_000 | 0o4000 | 0o2_000_000;
/// `S_IFMT`, from `include/sys/stat.h`.
const S_IFMT: c_uint = 0o170_000;
/// `S_IFREG`, from `include/sys/stat.h`.
const S_IFREG: c_uint = 0o100_000;
/// `NAME_MAX`, from `include/limits.h`.
const NAME_MAX: usize = 255;

/// C's `struct spwd`, from `include/shadow.h`. glibc's is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Spwd {
    /// The user name.
    pub sp_namp: *mut c_char,
    /// The encrypted password.
    pub sp_pwdp: *mut c_char,
    /// The day of the last change, counted from 1970-01-01.
    pub sp_lstchg: c_long,
    /// The days before the password may change.
    pub sp_min: c_long,
    /// The days before it must change.
    pub sp_max: c_long,
    /// The days of warning before it expires.
    pub sp_warn: c_long,
    /// The days after it expires that the account is disabled.
    pub sp_inact: c_long,
    /// The day the account expires.
    pub sp_expire: c_long,
    /// Reserved.
    pub sp_flag: c_ulong,
}

const _: () = assert!(size_of::<Spwd>() == 72);

/// Reads a number at `at` as musl's `xatol` does: -1 for an empty field, else
/// the decimal digits there. Returns it and where the digits end.
fn number(line: &[u8], at: usize) -> (c_long, usize) {
    if matches!(line.get(at), Some(b':' | b'\n')) {
        return (-1, at);
    }
    let mut value: c_long = 0;
    let mut end = at;
    while let Some(&digit @ b'0'..=b'9') = line.get(end) {
        value = value
            .wrapping_mul(10)
            .wrapping_add(c_long::from(digit - b'0'));
        end += 1;
    }
    (value, end)
}

/// Where a shadow line's password is, and its seven numbers.
#[derive(Debug, PartialEq, Eq)]
struct Fields {
    /// Where the password field starts.
    pwdp: usize,
    /// `sp_lstchg` to `sp_flag`.
    numbers: [c_long; 7],
}

/// Splits `line`, which ends in a newline and a NUL, as musl's `__parsespent`
/// does, or `None` if it is not a shadow entry.
fn parse(line: &mut [u8]) -> Option<Fields> {
    let end_name = colon(line, 0)?;
    cut(line, end_name);
    let pwdp = end_name + 1;
    let end_pwdp = colon(line, pwdp)?;
    cut(line, end_pwdp);
    let mut at = end_pwdp;
    let mut numbers = [0; 7];
    for (index, slot) in numbers.iter_mut().enumerate() {
        let (value, end) = number(line, at + 1);
        *slot = value;
        let separator = if index == 6 { b'\n' } else { b':' };
        if line.get(end) != Some(&separator) {
            return None;
        }
        at = end;
    }
    Some(Fields { pwdp, numbers })
}

/// Opens the shadow file for `name`: its TCB file if there is one, else
/// `/etc/shadow`. `Ok(None)` if neither exists.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
unsafe fn open_shadow(path: *const c_char) -> Result<Option<*mut File>, c_int> {
    // SAFETY: the caller passes a NUL-terminated path.
    let fd = unsafe { open(path, TCB_FLAGS, 0) };
    if fd >= 0 {
        let mut info = Stat::default();
        errno::set(errno::EINVAL);
        // SAFETY: `info` is a live local.
        let regular = unsafe { fstat(fd, &raw mut info) } == 0 && info.st_mode & S_IFMT == S_IFREG;
        let stream = if regular {
            // SAFETY: the mode is NUL-terminated.
            unsafe { fdopen(fd, c"rb".as_ptr()) }
        } else {
            null_mut()
        };
        if stream.is_null() {
            let error = last_errno();
            let _ = close(fd);
            return Err(error);
        }
        return Ok(Some(stream));
    }
    match last_errno() {
        errno::ENOENT | errno::ENOTDIR => {}
        error => return Err(error),
    }
    // SAFETY: both strings are NUL-terminated.
    let stream = unsafe { fopen(c"/etc/shadow".as_ptr(), c"rbe".as_ptr()) };
    if !stream.is_null() {
        return Ok(Some(stream));
    }
    match last_errno() {
        errno::ENOENT | errno::ENOTDIR => Ok(None),
        error => Err(error),
    }
}

/// Stores user `name`'s shadow entry in `*sp`, its strings in the `size`
/// bytes at `buf`, and sets `*result` to `sp`, or to null if there is none.
/// Returns 0 or the error, which is also left in `errno`.
///
/// # Safety
///
/// `name` must be a NUL-terminated string, `sp` and `result` valid for
/// writes, and `buf` for writes of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getspnam_r(
    name: *const c_char,
    sp: *mut Spwd,
    buf: *mut c_char,
    size: usize,
    result: *mut *mut Spwd,
) -> c_int {
    let original_errno = last_errno();
    // SAFETY: the caller passes a writable result.
    unsafe { result.write(null_mut()) };
    // SAFETY: the caller passes a NUL-terminated name.
    let len = unsafe { strlen(name) };
    // SAFETY: `strlen` found `len` readable bytes.
    let name_bytes = unsafe { core::slice::from_raw_parts(name.cast::<u8>(), len) };
    if len == 0 || name_bytes.first() == Some(&b'.') || name_bytes.contains(&b'/') {
        errno::set(errno::EINVAL);
        return errno::EINVAL;
    }
    if size < len + 100 {
        errno::set(errno::ERANGE);
        return errno::ERANGE;
    }
    let mut path = [0u8; 20 + NAME_MAX];
    let parts: [&[u8]; 3] = [b"/etc/tcb/", name_bytes, b"/shadow"];
    let path_len: usize = parts.iter().map(|part| part.len()).sum();
    if path_len >= path.len() {
        errno::set(errno::EINVAL);
        return errno::EINVAL;
    }
    for (slot, &byte) in path.iter_mut().zip(parts.into_iter().flatten()) {
        *slot = byte;
    }

    // SAFETY: the path is NUL-terminated.
    let stream = match unsafe { open_shadow(path.as_ptr().cast()) } {
        Ok(Some(stream)) => stream,
        Ok(None) => {
            errno::set(original_errno);
            return 0;
        }
        Err(error) => {
            errno::set(error);
            return error;
        }
    };
    let limit = c_int::try_from(size).unwrap_or(c_int::MAX);
    let mut error = 0;
    let mut skip = false;
    // SAFETY: the caller passes `size` writable bytes, and the stream is live.
    while !unsafe { fgets(buf, limit, stream) }.is_null() {
        // SAFETY: `fgets` left a NUL-terminated string.
        let k = unsafe { strlen(buf) };
        if k == 0 {
            break;
        }
        // SAFETY: the string and its NUL are the caller's buffer, which nothing
        // else refers to here.
        let line = unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), k + 1) };
        let ends_line = line.get(k - 1) == Some(&b'\n');
        let matches = !skip && line.get(..len) == Some(name_bytes) && line.get(len) == Some(&b':');
        if !matches {
            skip = !ends_line;
            continue;
        }
        if !ends_line {
            error = errno::ERANGE;
            break;
        }
        let Some(fields) = parse(line) else {
            continue;
        };
        let [lstchg, min, max, warn, inact, expire, flag] = fields.numbers;
        let entry = Spwd {
            sp_namp: buf,
            sp_pwdp: buf.wrapping_add(fields.pwdp),
            sp_lstchg: lstchg,
            sp_min: min,
            sp_max: max,
            sp_warn: warn,
            sp_inact: inact,
            sp_expire: expire,
            sp_flag: flag as c_ulong,
        };
        // SAFETY: the caller passes a writable entry.
        unsafe { sp.write(entry) };
        // SAFETY: the caller passes a writable result.
        unsafe { result.write(sp) };
        break;
    }
    // SAFETY: the stream is live, and not used again.
    let _ = unsafe { fclose(stream) };
    errno::set(if error != 0 { error } else { original_errno });
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(text: &str) -> Option<Fields> {
        let mut line: Vec<u8> = text.bytes().chain([0]).collect();
        parse(&mut line)
    }

    #[test]
    fn numbers_are_decimal_and_empty_ones_are_minus_one() {
        let found = fields("root:$6$salt$hash:19000:0:99999:7:::\n");
        assert_eq!(
            found.as_ref().map(|found| found.numbers),
            Some([19000, 0, 99999, 7, -1, -1, -1])
        );
        assert_eq!(found.map(|found| found.pwdp), Some(5));
        assert_eq!(
            fields("root:*:1:2:3:4:5:6:7\n").map(|found| found.numbers),
            Some([1, 2, 3, 4, 5, 6, 7])
        );
    }

    #[test]
    fn a_line_with_too_few_fields_or_no_newline_is_not_an_entry() {
        assert_eq!(fields("root:*:1:2:3:4:5:6\n"), None);
        assert_eq!(fields("root:*:1:2:3:4:5:6:7"), None);
        assert_eq!(fields("root\n"), None);
        assert_eq!(fields("root:*:x:2:3:4:5:6:7\n"), None);
    }
}
