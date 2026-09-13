//! `crypt.h`: one-way password hashing.
//!
//! `crypt` reads the hash to use from the setting string it is given, as every
//! implementation since the traditional two-character salt has:
//!
//! | Setting | Hash |
//! |---|---|
//! | `$1$salt$` | MD5, in [`md5`] |
//! | `$5$[rounds=n$]salt$` | SHA-256, in [`sha2`] |
//! | `$6$[rounds=n$]salt$` | SHA-512, in [`sha2`] |
//!
//! A setting this does not understand — `$2a$` and the rest of the blowfish
//! family, and the traditional DES hash — returns `"*"`, which matches no
//! password, rather than a hash from the wrong algorithm. musl's `crypt` falls
//! through to DES; until that is here, a DES entry in `/etc/shadow` cannot be
//! checked, and `"*"` refuses the login rather than accepting it.
//!
//! The result is written into storage the caller can read until the next call:
//! a static buffer for `crypt`, and the caller's `struct crypt_data` for
//! `crypt_r`. Neither ever fails with a null pointer; a setting that cannot be
//! used gives `"*"`, as musl does.

pub(crate) mod md5;
pub(crate) mod sha2;

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int};

use crate::string::strlen;

/// The alphabet password hashes have always been written in: `.`, `/`, the
/// digits, then the letters. It is not RFC 4648's, and the characters of each
/// group come out least significant first.
const B64: [u8; 64] = *b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// What every hash here fits in, with its NUL.
const BUFFER_LEN: usize = 128;

/// The longest hash: `$6$`, `rounds=9999999$`, a 16-byte salt, `$`, and the
/// 86 characters SHA-512's digest is written in.
const LONGEST: usize = 3 + 15 + 16 + 1 + 86;

const _: () = assert!(LONGEST < BUFFER_LEN, "a hash would not fit `crypt`");

/// The buffer `crypt` returns.
#[derive(Debug)]
struct Buffer(UnsafeCell<[u8; BUFFER_LEN]>);

// SAFETY: C documents `crypt`'s result as static storage that the next call
// overwrites, and the function as unsafe to call from two threads at once, as
// musl's is. `crypt_r` exists for a program that needs otherwise, and only
// `crypt` writes this.
unsafe impl Sync for Buffer {}

/// What `crypt` returns.
static RESULT: Buffer = Buffer(UnsafeCell::new([0; BUFFER_LEN]));

/// C's `struct crypt_data`, from `include/crypt.h`.
///
/// musl treats the whole structure as a character buffer and writes the result
/// from its first byte, over `initialized`. This writes into `__buf` and
/// leaves `initialized` alone, which is what the field is for; either way the
/// caller reads the returned pointer. glibc's structure is far larger and its
/// layout is not this one, so a program that compiled against glibc's header
/// must not be given this.
#[repr(C)]
#[derive(Debug)]
pub struct CryptData {
    /// Set by a caller that wants a first call distinguished; unused here.
    pub initialized: c_int,
    /// Where the result is written.
    pub __buf: [c_char; 256],
}

const _: () = assert!(size_of::<CryptData>() == 260);
const _: () = assert!(align_of::<CryptData>() == 4);

/// The byte at `at` of `digest`, or zero past its end.
///
/// The permutation tables are written as the hashes' specifications write
/// them, and this keeps a mistake in one from indexing out of bounds.
fn byte(digest: &[u8], at: u8) -> u8 {
    digest.get(usize::from(at)).copied().unwrap_or(0)
}

/// Writes a hash into a caller's buffer, refusing to run past its end.
#[derive(Debug)]
struct Writer<'a> {
    /// Where the hash is written.
    out: &'a mut [u8],
    /// How much has been written.
    at: usize,
}

impl<'a> Writer<'a> {
    /// A writer over `out`.
    fn new(out: &'a mut [u8]) -> Self {
        Writer { out, at: 0 }
    }

    /// Appends `bytes`, or `None` if they do not fit.
    fn push(&mut self, bytes: &[u8]) -> Option<()> {
        let end = self.at.checked_add(bytes.len())?;
        let slot = self.out.get_mut(self.at..end)?;
        slot.copy_from_slice(bytes);
        self.at = end;
        Some(())
    }

    /// Appends `value` in decimal.
    fn push_uint(&mut self, value: u32) -> Option<()> {
        let mut digits = [0u8; 10];
        let mut written = 0;
        let mut rest = value;
        loop {
            let digit = b'0'.wrapping_add((rest % 10) as u8);
            *digits.get_mut(written)? = digit;
            written += 1;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        for index in (0..written).rev() {
            let digit = *digits.get(index)?;
            self.push(&[digit])?;
        }
        Some(())
    }

    /// Appends `count` characters of `value`, least significant first, in the
    /// alphabet [`B64`].
    fn push_base64(&mut self, value: u32, count: usize) -> Option<()> {
        let mut rest = value;
        for _ in 0..count {
            let character = *B64.get((rest % 64) as usize)?;
            self.push(&[character])?;
            rest /= 64;
        }
        Some(())
    }

    /// Terminates the hash and returns its length without the NUL.
    fn finish(self) -> Option<usize> {
        *self.out.get_mut(self.at)? = 0;
        Some(self.at)
    }
}

/// The hash of `key` under `setting`, written to `out` with a NUL, or `None`
/// if no hash here understands the setting.
fn hash(key: &[u8], setting: &[u8], out: &mut [u8]) -> Option<usize> {
    match setting {
        [b'$', b'1', b'$', ..] => md5::md5crypt(key, setting, out),
        [b'$', b'5', b'$', ..] => sha2::sha256crypt(key, setting, out),
        [b'$', b'6', b'$', ..] => sha2::sha512crypt(key, setting, out),
        _ => None,
    }
}

/// Writes the refusal that matches no password.
fn refuse(out: &mut [u8]) {
    let mut writer = Writer::new(out);
    if writer.push(b"*").is_some() {
        let _ = writer.finish();
    }
}

/// The bytes of a C string.
///
/// # Safety
///
/// `text` must be a NUL-terminated string that stays put for `'a`.
unsafe fn bytes<'a>(text: *const c_char) -> &'a [u8] {
    // SAFETY: the caller promises a NUL-terminated string.
    let len = unsafe { strlen(text) };
    // SAFETY: `strlen` found a NUL at `len`, so that many bytes are readable.
    unsafe { core::slice::from_raw_parts(text.cast::<u8>(), len) }
}

/// Hashes into `out` and returns it, or `"*"` if the setting is not one of
/// this library's.
///
/// # Safety
///
/// `key` and `setting` must be NUL-terminated strings.
unsafe fn hash_into(key: *const c_char, setting: *const c_char, out: &mut [u8]) -> *mut c_char {
    // SAFETY: the caller promises a NUL-terminated string.
    let key = unsafe { bytes(key) };
    // SAFETY: the caller promises a NUL-terminated string.
    let setting = unsafe { bytes(setting) };
    if hash(key, setting, out).is_none() {
        refuse(out);
    }
    out.as_mut_ptr().cast()
}

/// The hash of `key` under `setting`, in static storage the next call
/// overwrites.
///
/// # Safety
///
/// `key` and `setting` must be NUL-terminated strings, and no other thread may
/// be in `crypt` at the same time.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn crypt(key: *const c_char, setting: *const c_char) -> *mut c_char {
    // SAFETY: see `Buffer`: this is the only writer, and C's contract for
    // `crypt` is that the result stands until the next call.
    let out = unsafe { &mut *RESULT.0.get() };
    // SAFETY: the caller promises NUL-terminated strings.
    unsafe { hash_into(key, setting, out) }
}

/// The hash of `key` under `setting`, written into `data`.
///
/// # Safety
///
/// `key` and `setting` must be NUL-terminated strings, and `data` must point
/// to a `struct crypt_data` this thread alone is using.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn crypt_r(
    key: *const c_char,
    setting: *const c_char,
    data: *mut CryptData,
) -> *mut c_char {
    // SAFETY: the caller promises a structure valid for writes.
    let buffer = unsafe { &raw mut (*data).__buf };
    // SAFETY: `__buf` is 256 bytes, which every hash here fits in, and a
    // `c_char` and a `u8` have the same size and alignment.
    let out = unsafe { core::slice::from_raw_parts_mut(buffer.cast::<u8>(), 256) };
    // SAFETY: the caller promises NUL-terminated strings.
    unsafe { hash_into(key, setting, out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashed(key: &[u8], setting: &[u8]) -> String {
        let mut out = [0u8; BUFFER_LEN];
        match hash(key, setting, &mut out) {
            Some(len) => String::from_utf8_lossy(out.get(..len).unwrap_or(&[])).into_owned(),
            None => {
                refuse(&mut out);
                "*".to_owned()
            }
        }
    }

    #[test]
    fn each_prefix_reaches_its_hash() {
        assert!(hashed(b"password", b"$1$salt$").starts_with("$1$salt$"));
        assert!(hashed(b"password", b"$5$salt$").starts_with("$5$salt$"));
        assert!(hashed(b"password", b"$6$salt$").starts_with("$6$salt$"));
    }

    #[test]
    fn a_hash_that_is_not_here_refuses_rather_than_guesses() {
        // Blowfish and the traditional DES hash, which this does not have.
        assert_eq!(hashed(b"password", b"$2a$10$abcdefghijklmnopqrstuv"), "*");
        assert_eq!(hashed(b"password", b"ab"), "*");
        assert_eq!(hashed(b"password", b""), "*");
        // A prefix that names no hash at all.
        assert_eq!(hashed(b"password", b"$9$salt$"), "*");
    }

    #[test]
    fn the_same_key_and_setting_give_the_same_hash() {
        assert_eq!(
            hashed(b"password", b"$6$salt$"),
            hashed(b"password", b"$6$salt$")
        );
        assert_ne!(
            hashed(b"password", b"$6$salt$"),
            hashed(b"passworD", b"$6$salt$")
        );
        assert_ne!(
            hashed(b"password", b"$6$salt$"),
            hashed(b"password", b"$6$salu$")
        );
    }

    #[test]
    fn a_hash_can_be_checked_against_itself() {
        // What a login does: hash the typed password under the stored entry,
        // and compare with it.
        let stored = hashed(b"password", b"$6$rounds=1000$abcdefgh$");
        assert_eq!(hashed(b"password", stored.as_bytes()), stored);
        assert_ne!(hashed(b"wrong", stored.as_bytes()), stored);
    }

    #[test]
    fn the_longest_hash_is_counted_rather_than_run() {
        // Ten million rounds take a minute and show nothing this does not:
        // only the width of the printed count changes with it.
        let hash = hashed(b"password", b"$6$rounds=1000$0123456789abcdef$");
        let digest = hash.rsplit('$').next().unwrap_or_default();
        assert_eq!(digest.len(), 86);
        assert_eq!(hash.len(), LONGEST - "9999999".len() + "1000".len());
        assert_eq!(
            LONGEST,
            "$6$".len() + "rounds=9999999$".len() + 16 + "$".len() + digest.len()
        );
    }

    #[test]
    fn a_writer_refuses_to_run_past_its_buffer() {
        let mut small = [0u8; 4];
        let mut writer = Writer::new(&mut small);
        assert_eq!(writer.push(b"abcde"), None, "more than the buffer holds");
        assert_eq!(writer.push(b"abcd"), Some(()), "exactly the buffer");
        // Four bytes written leave nowhere for the NUL, so there is no hash.
        assert_eq!(writer.finish(), None);

        let mut room = [0u8; 4];
        let mut writer = Writer::new(&mut room);
        assert_eq!(writer.push(b"abc"), Some(()));
        assert_eq!(writer.finish(), Some(3));
        assert_eq!(room, *b"abc\0");
    }

    #[test]
    fn a_writer_writes_decimals_and_base64() {
        let mut buffer = [0u8; 16];
        let mut writer = Writer::new(&mut buffer);
        assert_eq!(writer.push_uint(0), Some(()));
        assert_eq!(writer.push_uint(9_999_999), Some(()));
        assert_eq!(writer.push_base64(0, 1), Some(()));
        let len = writer.finish().unwrap();
        assert_eq!(&buffer[..len], b"09999999.");
    }
}
