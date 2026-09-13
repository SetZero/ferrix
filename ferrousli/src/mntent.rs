//! `mntent.h`: reading a table of file systems, such as `/etc/fstab` or
//! `/proc/mounts`, an entry a line.
//!
//! This follows musl's `misc/mntent.c`. A line holds four fields separated by
//! spaces or tabs, then optionally the dump frequency and the pass number. A
//! line starting with `#`, or holding no field, is skipped, and so is a last
//! line without a newline. A line longer than the caller's buffer fails with
//! `ERANGE`, and the next call reads the line after it. In each field an octal
//! escape such as `\040` stands for its byte and `\\` for a backslash, which is
//! how the kernel writes a space in a mount point.
//!
//! It differs from musl 1.2.5 in three small ways:
//!
//! * musl scans each line with `sscanf`, which this library does not have yet.
//!   The scan here finds the same fields by hand.
//! * A field ends at a newline too. musl 1.2.5's `%[^ \t]` leaves the newline
//!   on the last field of a line with no frequency or pass number; libc-test's
//!   `mntent.c` expects it gone, as glibc does.
//! * The frequency and pass number are those of the line returned. musl reads
//!   them while scanning every line, so a skipped comment holding numbers can
//!   set them for the next entry.
//!
//! `hasmntopt` is `strstr` over the options, as in musl, so `"ro"` is also
//! found in `"errors=remount-ro"`. `addmntent` is not here yet.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::errno;
use crate::stdio::EOF;
use crate::stdio::file::File;
use crate::stdio::io::{feof, ferror, fgetc, fgets, getline};
use crate::stdio::open::{fclose, fopen};
use crate::string::{strlen, strstr};

/// C's `struct mntent`, from musl's `mntent.h`. glibc's is the same.
#[repr(C)]
#[derive(Debug)]
pub struct Mntent {
    /// The device or server mounted.
    pub mnt_fsname: *mut c_char,
    /// Where it is mounted.
    pub mnt_dir: *mut c_char,
    /// The file system type.
    pub mnt_type: *mut c_char,
    /// The mount options, separated by commas.
    pub mnt_opts: *mut c_char,
    /// How often `dump` backs it up.
    pub mnt_freq: c_int,
    /// The order `fsck` checks it in, or 0 not to.
    pub mnt_passno: c_int,
}

const _: () = assert!(offset_of!(Mntent, mnt_freq) == 32);
const _: () = assert!(offset_of!(Mntent, mnt_passno) == 36);
const _: () = assert!(size_of::<Mntent>() == 40);

/// The entry `getmntent` returns.
#[derive(Debug)]
struct Shared(UnsafeCell<Mntent>);

// SAFETY: C documents `getmntent` as returning static storage that the next
// call overwrites, and as unsafe to call from two threads at once. Only
// `getmntent` writes it, as in musl.
unsafe impl Sync for Shared {}

/// What `getmntent` returns.
static ENTRY: Shared = Shared(UnsafeCell::new(Mntent {
    mnt_fsname: null_mut(),
    mnt_dir: null_mut(),
    mnt_type: null_mut(),
    mnt_opts: null_mut(),
    mnt_freq: 0,
    mnt_passno: 0,
}));

/// The line `getmntent` reads with `getline`. Its strings point into it, so it
/// is kept between calls, and never freed, as in musl.
static LINE: AtomicPtr<c_char> = AtomicPtr::new(null_mut());
/// The size of [`LINE`]'s allocation.
static LINE_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Where the fields of one line are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Scan {
    /// Field `k` starts at `marks[2 * k]` and ends at `marks[2 * k + 1]`, as
    /// `sscanf`'s `%n` records them. A field that is not there has its start
    /// after any white space, and its end at the end of the line, as do the
    /// fields after it.
    marks: [usize; 8],
    /// The dump frequency, if the line has one.
    freq: Option<c_int>,
    /// The pass number, if the line has one after the frequency.
    passno: Option<c_int>,
}

/// Whether `byte` is white space to `sscanf`'s ` ` directive: `isspace` in
/// the C locale.
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Whether `byte` ends a field.
fn ends_field(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n')
}

/// The first position at or after `at` that is not white space.
fn skip_space(line: &[u8], mut at: usize) -> usize {
    while line.get(at).copied().is_some_and(is_space) {
        at += 1;
    }
    at
}

/// Reads a decimal `int` after any white space from `at`, as `%d` does, and
/// returns it with the position after it. A value that does not fit is
/// truncated, where C leaves it undefined.
fn number(line: &[u8], at: usize) -> Option<(c_int, usize)> {
    let mut at = skip_space(line, at);
    let negative = match line.get(at) {
        Some(b'-') => {
            at += 1;
            true
        }
        Some(b'+') => {
            at += 1;
            false
        }
        _ => false,
    };
    let start = at;
    let mut value: i64 = 0;
    while let Some(&digit @ b'0'..=b'9') = line.get(at) {
        value = value
            .saturating_mul(10)
            .saturating_add(i64::from(digit - b'0'));
        at += 1;
    }
    if at == start {
        return None;
    }
    let value = if negative {
        value.wrapping_neg()
    } else {
        value
    };
    Some((value as c_int, at))
}

/// Finds the fields of `line`, the bytes before its NUL.
fn scan(line: &[u8]) -> Scan {
    let mut found = Scan {
        marks: [line.len(); 8],
        freq: None,
        passno: None,
    };
    let mut at = 0;
    for field in 0..4 {
        at = skip_space(line, at);
        let start = at;
        if let Some(mark) = found.marks.get_mut(2 * field) {
            *mark = start;
        }
        while line.get(at).is_some_and(|&byte| !ends_field(byte)) {
            at += 1;
        }
        if at == start {
            return found;
        }
        if let Some(mark) = found.marks.get_mut(2 * field + 1) {
            *mark = at;
        }
    }
    if let Some((freq, after)) = number(line, at) {
        found.freq = Some(freq);
        found.passno = number(line, after).map(|(passno, _)| passno);
    }
    found
}

/// Rewrites the escapes in `field` in place, as musl's `unescape_ent` does,
/// and returns the field's new length.
///
/// `\\` is a backslash. A backslash and up to three octal digits is the byte
/// they spell, truncated to eight bits, unless that is zero. Then, as before
/// anything else, the backslash stands for itself.
fn unescape(field: &mut [u8]) -> usize {
    let mut src = 0;
    let mut dest = 0;
    while let Some(&byte) = field.get(src) {
        let (out, next) = if byte != b'\\' {
            (byte, src + 1)
        } else if field.get(src + 1) == Some(&b'\\') {
            (b'\\', src + 2)
        } else {
            let mut value: u8 = 0;
            let mut at = src + 1;
            while at < src + 4 {
                match field.get(at) {
                    Some(&digit @ b'0'..=b'7') => {
                        value = (value << 3).wrapping_add(digit - b'0');
                        at += 1;
                    }
                    _ => break,
                }
            }
            if value != 0 {
                (value, at)
            } else {
                (b'\\', src + 1)
            }
        };
        if let Some(slot) = field.get_mut(dest) {
            *slot = out;
        }
        dest += 1;
        src = next;
    }
    dest
}

/// Discards the rest of the line `stream` is in, through its newline.
///
/// # Safety
///
/// `stream` must be a live stream.
unsafe fn discard_line(stream: *mut File) {
    loop {
        // SAFETY: the caller passes a live stream.
        let c = unsafe { fgetc(stream) };
        if c == EOF || c == c_int::from(b'\n') {
            return;
        }
    }
}

/// Reads the next entry of `stream` into `*mnt`, with its strings in the
/// `size` bytes at `buf`, or in [`LINE`] if there is no `buf`.
///
/// # Safety
///
/// `stream` must be a live stream, `mnt` valid for a write of a `struct
/// mntent`, and a buffer given valid for writes of `size` bytes.
unsafe fn read_entry(
    stream: *mut File,
    mnt: *mut Mntent,
    buf: Option<(*mut c_char, c_int)>,
) -> *mut Mntent {
    loop {
        let line = match buf {
            Some((line, size)) => {
                // SAFETY: the caller vouches for the stream and the buffer.
                if unsafe { fgets(line, size, stream) }.is_null() {
                    return null_mut();
                }
                line
            }
            None => {
                let mut line = LINE.load(Ordering::Relaxed);
                let mut size = LINE_SIZE.load(Ordering::Relaxed);
                // SAFETY: the buffer is null or an earlier `getline`'s, of
                // `size` bytes, and the caller passes a live stream.
                let read = unsafe { getline(&raw mut line, &raw mut size, stream) };
                LINE.store(line, Ordering::Relaxed);
                LINE_SIZE.store(size, Ordering::Relaxed);
                if read < 0 {
                    return null_mut();
                }
                line
            }
        };
        // SAFETY: the caller passes a live stream.
        let at_end = unsafe { feof(stream) } != 0;
        // SAFETY: as above.
        if at_end || unsafe { ferror(stream) } != 0 {
            return null_mut();
        }
        // SAFETY: `fgets` or `getline` left a NUL-terminated string there.
        let len = unsafe { strlen(line) };
        // SAFETY: the string and its NUL are the buffer just filled, which
        // nothing else refers to while this runs.
        let text = unsafe { core::slice::from_raw_parts_mut(line.cast::<u8>(), len + 1) };
        let Some(body) = text.get(..len) else {
            return null_mut();
        };
        if !body.contains(&b'\n') {
            // SAFETY: the caller passes a live stream.
            unsafe { discard_line(stream) };
            errno::set(errno::ERANGE);
            return null_mut();
        }
        if len > c_int::MAX as usize {
            continue;
        }
        let found = scan(body);
        let [s0, e0, s1, e1, s2, e2, s3, e3] = found.marks;
        // The line holds a newline, which ends a field, so a first field that
        // is there never ends at the end of the line.
        if body.get(s0) == Some(&b'#') || e0 == len {
            continue;
        }
        let fields = [(s0, e0), (s1, e1), (s2, e2), (s3, e3)];
        for (_, end) in fields {
            if let Some(nul) = text.get_mut(end) {
                *nul = 0;
            }
        }
        for (start, end) in fields {
            let kept = match text.get_mut(start..end) {
                Some(field) => unescape(field),
                None => continue,
            };
            if let Some(nul) = text.get_mut(start + kept) {
                *nul = 0;
            }
        }
        let entry = Mntent {
            mnt_fsname: line.wrapping_add(s0),
            mnt_dir: line.wrapping_add(s1),
            mnt_type: line.wrapping_add(s2),
            mnt_opts: line.wrapping_add(s3),
            mnt_freq: found.freq.unwrap_or(0),
            mnt_passno: found.passno.unwrap_or(0),
        };
        // SAFETY: the caller vouches for `mnt`.
        unsafe { mnt.write(entry) };
        return mnt;
    }
}

/// Opens the table `path` as a stream, with `fopen`'s `mode`.
///
/// # Safety
///
/// Both strings must be NUL-terminated.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn setmntent(path: *const c_char, mode: *const c_char) -> *mut File {
    // SAFETY: the caller's contract is `fopen`'s.
    unsafe { fopen(path, mode) }
}

/// Closes a table `setmntent` opened, if `stream` is not null. Returns 1.
///
/// # Safety
///
/// `stream` must be null or a live stream, which is not used again.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn endmntent(stream: *mut File) -> c_int {
    if !stream.is_null() {
        // SAFETY: the caller passes a live stream.
        let _ = unsafe { fclose(stream) };
    }
    1
}

/// Reads the next entry of `stream` into `*mnt`, with its strings in the
/// `size` bytes at `buf`. Returns `mnt`, or null at the end of the table, on
/// an error, or with `errno` set to `ERANGE` for a line too long for `buf`.
///
/// # Safety
///
/// `stream` must be a live stream, `mnt` valid for a write of a `struct
/// mntent`, and `buf` valid for writes of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getmntent_r(
    stream: *mut File,
    mnt: *mut Mntent,
    buf: *mut c_char,
    size: c_int,
) -> *mut Mntent {
    // SAFETY: the caller's contract is `read_entry`'s.
    unsafe { read_entry(stream, mnt, Some((buf, size))) }
}

/// Reads the next entry of `stream` into static storage, which the next call
/// overwrites, and returns it, or null as `getmntent_r` does.
///
/// # Safety
///
/// `stream` must be a live stream, and no other thread may be in `getmntent`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getmntent(stream: *mut File) -> *mut Mntent {
    // SAFETY: the static is valid, and see `Shared`.
    unsafe { read_entry(stream, ENTRY.0.get(), None) }
}

/// Finds `opt` in the entry's options, and returns where it starts, or null.
///
/// # Safety
///
/// `mnt` must be valid for a read of a `struct mntent` whose `mnt_opts` is a
/// NUL-terminated string, and `opt` must be one too.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn hasmntopt(mnt: *const Mntent, opt: *const c_char) -> *mut c_char {
    // SAFETY: the caller vouches for `mnt`.
    let opts = unsafe { (*mnt).mnt_opts };
    // SAFETY: both strings are NUL-terminated, as the caller vouches.
    unsafe { strstr(opts, opt) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_where_sscanf_s_n_would_record_them() {
        let found = scan(b"/dev/sda\t/\text4\trw,nosuid\t2\t1\n");
        assert_eq!(found.marks, [0, 8, 9, 10, 11, 15, 16, 25]);
        assert_eq!((found.freq, found.passno), (Some(2), Some(1)));
        let found = scan(b"  none /proc proc defaults\n");
        assert_eq!(found.marks, [2, 6, 7, 12, 13, 17, 18, 26]);
        assert_eq!((found.freq, found.passno), (None, None));
        assert_eq!(scan(b"\n").marks, [1; 8]);
        assert_eq!(scan(b"a b\n").marks, [0, 1, 2, 3, 4, 4, 4, 4]);
        let found = scan(b"a b c d 7\n");
        assert_eq!((found.freq, found.passno), (Some(7), None));
        let found = scan(b"a b c d -3 +4\n");
        assert_eq!((found.freq, found.passno), (Some(-3), Some(4)));
        let found = scan(b"a b c d x 4\n");
        assert_eq!((found.freq, found.passno), (None, None));
    }

    fn unescaped(text: &[u8]) -> Vec<u8> {
        let mut bytes = text.to_vec();
        let kept = unescape(&mut bytes);
        bytes.truncate(kept);
        bytes
    }

    #[test]
    fn escapes_are_octal_bytes_or_doubled_backslashes() {
        assert_eq!(unescaped(b"a\\040b"), b"a b");
        assert_eq!(unescaped(b"a\\\\b"), b"a\\b");
        assert_eq!(unescaped(b"\\11\\012"), b"\t\n");
        assert_eq!(unescaped(b"\\1234"), b"S4");
        assert_eq!(unescaped(b"\\777"), [255]);
        // A zero byte, a non-octal digit and a trailing backslash are not
        // escapes.
        assert_eq!(unescaped(b"\\0x"), b"\\0x");
        assert_eq!(unescaped(b"\\9"), b"\\9");
        assert_eq!(unescaped(b"end\\"), b"end\\");
    }
}
