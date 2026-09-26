//! `nl_types.h`: message catalogues, `catopen`, `catgets` and `catclose`.
//!
//! A catalogue is the file `gencat` writes, mapped whole and read in place,
//! as musl reads it; this is musl's `catopen.c`, `catgets.c` and `catclose.c`
//! (MIT) in Rust. The file is big-endian throughout:
//!
//! | Offset | Field |
//! |---|---|
//! | 0 | magic, `0xff88ff89` |
//! | 4 | number of sets |
//! | 8 | file size less the 20-byte header |
//! | 12 | offset of the message table, from the end of the header |
//! | 16 | offset of the strings, from the end of the header |
//! | 20 | the sets, 12 bytes each: id, number of messages, index of the first |
//!
//! Messages are 12 bytes too: id, length, offset into the strings. Sets and
//! messages are sorted by id, so both lookups are binary searches.
//!
//! A catalogue named without a `/` is looked for along `NLSPATH`, which is not
//! read in a set-user-id program. libc++'s `std::messages` is the caller that
//! brought these in.

use core::ffi::{c_char, c_int, c_void};
use core::mem::MaybeUninit;
use core::ptr::{null_mut, with_exposed_provenance_mut};

use crate::errno::{self, ENOENT, ENOMSG};
use crate::locale::{LC_MESSAGES, nl_langinfo};
use crate::mman::{MAP_FAILED, mmap, munmap};
use crate::stat::{Stat, fstat};
use crate::stdlib::secure_getenv;
use crate::string::strlen;
use crate::unistd::close;

/// What `catopen` returns on failure, `(nl_catd)-1`.
const FAILED: *mut c_void = with_exposed_provenance_mut(usize::MAX);

/// A catalogue's first four bytes.
const MAGIC: u32 = 0xff88_ff89;

/// The header's size, which the offsets in it do not count.
const HEADER: usize = 20;

/// The size of a set and of a message record.
const RECORD: usize = 12;

/// The longest path built from `NLSPATH`, as musl's `PATH_MAX` buffer.
const PATH_MAX: usize = 4096;

/// `O_RDONLY | O_CLOEXEC`.
const O_RDONLY_CLOEXEC: c_int = 0o2_000_000;
/// `PROT_READ`.
const PROT_READ: c_int = 1;
/// `MAP_SHARED`.
const MAP_SHARED: c_int = 1;

/// The big-endian word at `at` in a mapped catalogue.
///
/// # Safety
///
/// Four bytes at `at` must be readable.
unsafe fn word(at: *const u8) -> u32 {
    // SAFETY: the caller vouches for four bytes.
    u32::from_be(unsafe { at.cast::<u32>().read_unaligned() })
}

/// Maps the file at `path` and returns it as a catalogue, or [`FAILED`] with
/// `ENOENT` when it cannot be opened or is not one.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
unsafe fn open_catalogue(path: *const c_char) -> *mut c_void {
    // SAFETY: the caller's contract is `open`'s.
    let fd = unsafe { crate::fcntl::open(path, O_RDONLY_CLOEXEC, 0) };
    if fd < 0 {
        errno::set(ENOENT);
        return FAILED;
    }
    let mut st = MaybeUninit::<Stat>::zeroed();
    // SAFETY: `st` is a `Stat` to write.
    let size = if unsafe { fstat(fd, st.as_mut_ptr()) } == 0 {
        // SAFETY: `fstat` succeeded and filled it.
        usize::try_from(unsafe { st.assume_init_ref() }.st_size).unwrap_or(0)
    } else {
        0
    };
    let map = if size >= HEADER {
        // SAFETY: a new shared read-only mapping of an open file.
        unsafe { mmap(null_mut(), size, PROT_READ, MAP_SHARED, fd, 0) }
    } else {
        MAP_FAILED
    };
    let _ = close(fd);
    if map == MAP_FAILED {
        errno::set(ENOENT);
        return FAILED;
    }
    let bytes = map.cast::<u8>().cast_const();
    // SAFETY: the mapping is at least `HEADER` bytes.
    let magic = unsafe { word(bytes) };
    // SAFETY: as above.
    let recorded = unsafe { word(bytes.wrapping_add(8)) };
    // The size recorded must be the file's, or `catclose` would unmap the
    // wrong length.
    if magic != MAGIC || (recorded as usize).checked_add(HEADER) != Some(size) {
        // SAFETY: `map` is the mapping of `size` bytes made above.
        let _ = unsafe { munmap(map, size) };
        errno::set(ENOENT);
        return FAILED;
    }
    map
}

/// Opens the message catalogue `name`: that file if the name has a `/`, and
/// otherwise the first file along `NLSPATH` that is one. `%N` in a template
/// is the name, `%L` the locale, `%l`, `%t` and `%c` its language, territory
/// and codeset, and `%%` a percent sign. The locale is `LC_MESSAGES`'s with
/// `NL_CAT_LOCALE` (any flag but zero, as in musl), and `LANG` otherwise.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn catopen(name: *const c_char, flag: c_int) -> *mut c_void {
    // SAFETY: the caller passes a string.
    let name_len = unsafe { strlen(name) };
    // SAFETY: `name_len` bytes of it are readable.
    let name_bytes = unsafe { core::slice::from_raw_parts(name.cast::<u8>(), name_len) };
    if name_bytes.contains(&b'/') {
        // SAFETY: the caller passes a string.
        return unsafe { open_catalogue(name) };
    }
    // SAFETY: the variable's name is a string.
    let path = unsafe { secure_getenv(c"NLSPATH".as_ptr()) };
    if path.is_null() {
        errno::set(ENOENT);
        return FAILED;
    }
    let lang = if flag != 0 {
        nl_langinfo((LC_MESSAGES << 16) | 0xffff).cast_const()
    } else {
        // SAFETY: the variable's name is a string.
        unsafe { secure_getenv(c"LANG".as_ptr()) }.cast_const()
    };
    let lang: &[u8] = if lang.is_null() {
        &[]
    } else {
        // SAFETY: `nl_langinfo` and `getenv` return strings.
        let len = unsafe { strlen(lang) };
        // SAFETY: `len` bytes of the string are readable.
        unsafe { core::slice::from_raw_parts(lang.cast::<u8>(), len) }
    };
    // SAFETY: `getenv` returns a string.
    let path_len = unsafe { strlen(path) };
    // SAFETY: `path_len` bytes of it are readable.
    let path = unsafe { core::slice::from_raw_parts(path.cast::<u8>(), path_len) };

    let mut buf = [0_u8; PATH_MAX];
    for template in path.split(|&b| b == b':') {
        let Some(len) = expand(template, name_bytes, lang, &mut buf) else {
            continue;
        };
        // An empty template, as a leading or doubled `:` gives, is `%N`.
        let catalogue = if len == 0 {
            // SAFETY: the caller passes a string.
            unsafe { open_catalogue(name) }
        } else {
            // SAFETY: `expand` left room for the NUL and wrote it.
            unsafe { open_catalogue(buf.as_ptr().cast()) }
        };
        if catalogue != FAILED {
            return catalogue;
        }
    }
    errno::set(ENOENT);
    FAILED
}

/// Writes `template` with its `%` substitutions and a NUL into `buf`, and
/// returns the length before the NUL; `None` for an unknown substitution or
/// a path too long for `buf`, which musl skips.
fn expand(template: &[u8], name: &[u8], lang: &[u8], buf: &mut [u8]) -> Option<usize> {
    let mut len = 0;
    let mut at = 0;
    while let Some(&byte) = template.get(at) {
        let piece: &[u8] = if byte != b'%' {
            template.get(at..=at)?
        } else {
            at += 1;
            match template.get(at)? {
                b'N' => name,
                b'L' => lang,
                b'l' => lang.split(|b| b"_.@".contains(b)).next().unwrap_or(&[]),
                b't' => match lang.iter().position(|&b| b == b'_') {
                    Some(underscore) => lang
                        .get(underscore + 1..)?
                        .split(|b| b".@".contains(b))
                        .next()
                        .unwrap_or(&[]),
                    None => &[],
                },
                b'c' => b"UTF-8",
                b'%' => b"%",
                _ => return None,
            }
        };
        // Room for the piece and, at the end, the NUL.
        if piece.len() >= buf.len().checked_sub(len)? {
            return None;
        }
        buf.get_mut(len..len + piece.len())?.copy_from_slice(piece);
        len += piece.len();
        at += 1;
    }
    *buf.get_mut(len)? = 0;
    Some(len)
}

/// The record with id `id` among `count` sorted 12-byte records at `table`.
///
/// # Safety
///
/// `count * 12` bytes at `table` must be readable.
unsafe fn search(table: *const u8, id: u32, count: u32) -> Option<*const u8> {
    let (mut low, mut high) = (0_usize, count as usize);
    while low < high {
        let middle = low + (high - low) / 2;
        let record = table.wrapping_add(middle * RECORD);
        // SAFETY: `middle` is below `count`.
        let found = unsafe { word(record) };
        match found.cmp(&id) {
            core::cmp::Ordering::Less => low = middle + 1,
            core::cmp::Ordering::Greater => high = middle,
            core::cmp::Ordering::Equal => return Some(record),
        }
    }
    None
}

/// The message `msg_id` of set `set_id` in `catd`, or `s` with `ENOMSG` when
/// the catalogue has no such message.
///
/// As in musl, the tables are trusted to lie where the header says: a
/// catalogue is a file `gencat` wrote, and `catopen` checked only its magic
/// number and its size.
///
/// # Safety
///
/// `catd` must be a catalogue `catopen` returned and `catclose` has not
/// closed.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn catgets(
    catd: *mut c_void,
    set_id: c_int,
    msg_id: c_int,
    s: *const c_char,
) -> *mut c_char {
    let map = catd.cast::<u8>().cast_const();
    let sets = map.wrapping_add(HEADER);
    // SAFETY: a catalogue begins with its 20-byte header.
    let set_count = unsafe { word(map.wrapping_add(4)) };
    // SAFETY: as above.
    let messages = sets.wrapping_add(unsafe { word(map.wrapping_add(12)) } as usize);
    // SAFETY: as above.
    let strings = sets.wrapping_add(unsafe { word(map.wrapping_add(16)) } as usize);
    // SAFETY: the header counts the sets that follow it.
    let Some(set) = (unsafe { search(sets, set_id_word(set_id), set_count) }) else {
        errno::set(ENOMSG);
        return s.cast_mut();
    };
    // SAFETY: a set record is 12 bytes.
    let count = unsafe { word(set.wrapping_add(4)) };
    // SAFETY: as above.
    let first = messages.wrapping_add(RECORD * unsafe { word(set.wrapping_add(8)) } as usize);
    // SAFETY: the set counts its messages from its first.
    let Some(message) = (unsafe { search(first, set_id_word(msg_id), count) }) else {
        errno::set(ENOMSG);
        return s.cast_mut();
    };
    // SAFETY: a message record is 12 bytes.
    let offset = unsafe { word(message.wrapping_add(8)) } as usize;
    strings.wrapping_add(offset).cast_mut().cast()
}

/// An id as the unsigned word a catalogue stores, as musl's `htonl` gives.
fn set_id_word(id: c_int) -> u32 {
    u32::from_ne_bytes(id.to_ne_bytes())
}

/// Closes `catd`, unmapping it.
///
/// # Safety
///
/// As [`catgets`]; `catd` must not be used afterwards.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn catclose(catd: *mut c_void) -> c_int {
    // SAFETY: a catalogue records its size less the header.
    let size = unsafe { word(catd.cast::<u8>().wrapping_add(8)) } as usize + HEADER;
    // SAFETY: the mapping `catopen` made was `size` bytes.
    let _ = unsafe { munmap(catd, size) };
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expanded(template: &[u8], name: &[u8], lang: &[u8]) -> Option<Vec<u8>> {
        let mut buf = [0_u8; 64];
        let len = expand(template, name, lang, &mut buf)?;
        buf.get(..len).map(<[u8]>::to_vec)
    }

    #[test]
    fn nlspath_templates_substitute_as_musl_does() {
        let lang = b"de_AT.UTF-8@euro";
        assert_eq!(
            expanded(b"/usr/share/%L/%N.cat", b"app", lang).unwrap(),
            b"/usr/share/de_AT.UTF-8@euro/app.cat"
        );
        assert_eq!(
            expanded(b"%l-%t-%c-%%", b"app", lang).unwrap(),
            b"de-AT-UTF-8-%"
        );
        assert_eq!(expanded(b"%l/%t", b"app", b"C").unwrap(), b"C/");
        assert_eq!(expanded(b"", b"app", lang).unwrap(), b"");
        assert_eq!(
            expanded(b"%q", b"app", lang),
            None,
            "an unknown substitution"
        );
        assert_eq!(expanded(b"50%", b"app", lang), None, "a trailing percent");
        assert_eq!(
            expanded(&[b'x'; 64], b"app", lang),
            None,
            "too long for the buffer"
        );
    }
}
