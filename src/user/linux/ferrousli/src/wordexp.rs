//! `wordexp.h`: a string expanded into words as the shell expands them.
//!
//! Adapted from musl 1.2.5's `misc/wordexp.c` (MIT). The shell does the work:
//! `/bin/sh -c 'eval "printf %s\\\\0 x $1 $2"' sh <words> 2>/dev/null` prints
//! each word of the string after a dummy `x`, each ending in a NUL, and the
//! words are read back from a pipe. Output with no NUL, which is what a
//! shell that found a syntax error prints, is `WRDE_SYNTAX`. As in musl:
//!
//! * `WRDE_NOCMD` is checked before the shell runs: an unquoted `` ` `` or
//!   `$(` is `WRDE_CMDSUB`, and an unquoted `|`, `&`, `;`, `<`, `>`, `(`,
//!   `)`, `{`, `}` or newline is `WRDE_BADCHAR`; `$((` is arithmetic, and
//!   allowed;
//! * `WRDE_SHOWERR` leaves the shell's standard error alone, which otherwise
//!   goes to `/dev/null`;
//! * `WRDE_UNDEF` is not honoured: the shell's `set -u` is not asked for;
//! * cancellation is disabled while the shell runs.
//!
//! musl forks; this spawns the shell with [`crate::posix_spawn`], whose
//! `dup2` action puts the pipe's write end on standard output. The Steam
//! Runtime's scout libasound expands its configuration paths with it.

use core::ffi::{CStr, c_char, c_int};
use core::mem::size_of;
use core::ptr::null_mut;

use crate::errno;
use crate::growable::Growable;
use crate::malloc::{calloc, free, malloc, realloc};
use crate::posix_spawn::{
    FileActions, posix_spawn, posix_spawn_file_actions_adddup2, posix_spawn_file_actions_destroy,
    posix_spawn_file_actions_init,
};
use crate::syscall::{self, nr};

/// `WRDE_DOOFFS`: leave `we_offs` null slots before the words.
const WRDE_DOOFFS: c_int = 1;
/// `WRDE_APPEND`: add to the words of an earlier call.
const WRDE_APPEND: c_int = 2;
/// `WRDE_NOCMD`: refuse command substitution.
const WRDE_NOCMD: c_int = 4;
/// `WRDE_REUSE`: free an earlier call's words first.
const WRDE_REUSE: c_int = 8;
/// `WRDE_SHOWERR`: let the shell's errors through.
const WRDE_SHOWERR: c_int = 16;

/// `WRDE_NOSPACE`: out of memory, or the shell could not be run.
const WRDE_NOSPACE: c_int = 1;
/// `WRDE_BADCHAR`: an unquoted character that is not allowed.
const WRDE_BADCHAR: c_int = 2;
/// `WRDE_CMDSUB`: command substitution, with `WRDE_NOCMD`.
const WRDE_CMDSUB: c_int = 4;
/// `WRDE_SYNTAX`: the shell found a syntax error.
const WRDE_SYNTAX: c_int = 5;

/// `O_CLOEXEC`, which every architecture here numbers alike.
const O_CLOEXEC: c_int = 0o2_000_000;

/// C's `wordexp_t`, from `include/wordexp.h`. glibc's is the same.
#[repr(C)]
#[derive(Debug)]
pub struct Wordexp {
    /// How many words were found.
    pub we_wordc: usize,
    /// The words, after `we_offs` null slots, ending with a null.
    pub we_wordv: *mut *mut c_char,
    /// How many null slots `WRDE_DOOFFS` reserves.
    pub we_offs: usize,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Wordexp>() == 24);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<Wordexp>() == 12);

/// The byte at `i`, or NUL past the end.
fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

/// `WRDE_NOCMD`'s check of `s`: `None` if the shell may see it, or the
/// error.
fn refuse_commands(s: &[u8]) -> Option<c_int> {
    let (mut sq, mut dq, mut np) = (false, false, 0usize);
    let mut i = 0;
    while i < s.len() {
        match at(s, i) {
            b'\\' => {
                if !sq {
                    i += 1;
                    if at(s, i) == 0 {
                        return Some(WRDE_SYNTAX);
                    }
                }
            }
            b'\'' => sq ^= !dq,
            b'"' => dq ^= !sq,
            b'(' if np > 0 => np += 1,
            b')' if np > 0 => np -= 1,
            b'(' | b')' | b'\n' | b'|' | b'&' | b';' | b'<' | b'>' | b'{' | b'}' => {
                if !(sq || dq || np > 0) {
                    return Some(WRDE_BADCHAR);
                }
            }
            b'$' if sq => {}
            b'$' if at(s, i + 1) == b'(' && at(s, i + 2) == b'(' => {
                i += 2;
                np += 2;
            }
            b'$' if at(s, i + 1) != b'(' => {}
            b'$' | b'`' if !sq => return Some(WRDE_CMDSUB),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Runs the shell on `s` and reads everything it prints into `out`. Returns
/// `false` if it could not be run or read.
fn run_shell(s: *const c_char, show_errors: bool, out: &mut Growable<u8>) -> bool {
    let mut pipe = [0 as c_int; 2];
    // SAFETY: `pipe` holds two descriptors.
    if unsafe { crate::unistd::pipe2(pipe.as_mut_ptr(), O_CLOEXEC) } < 0 {
        return false;
    }
    let [read_end, write_end] = pipe;
    let redirect = if show_errors { c"" } else { c"2>/dev/null" };
    let argv: [*const c_char; 7] = [
        c"sh".as_ptr(),
        c"-c".as_ptr(),
        c"eval \"printf %s\\\\\\\\0 x $1 $2\"".as_ptr(),
        c"sh".as_ptr(),
        s,
        redirect.as_ptr(),
        core::ptr::null(),
    ];
    let mut uninit = core::mem::MaybeUninit::<FileActions>::uninit();
    let actions = uninit.as_mut_ptr();
    // SAFETY: `actions` is a live local, which this initialises.
    let _ = unsafe { posix_spawn_file_actions_init(actions) };
    // SAFETY: as above, and now initialised.
    let mut failed = unsafe { posix_spawn_file_actions_adddup2(actions, write_end, 1) } != 0;
    let mut pid: c_int = 0;
    if !failed {
        let envp = crate::stdlib::environ()
            .load(core::sync::atomic::Ordering::Relaxed)
            .cast_const()
            .cast::<*const c_char>();
        // SAFETY: the path and arguments are NUL-terminated strings in a
        // null-terminated array, the actions were made above, and the
        // environment is the program's.
        failed = unsafe {
            posix_spawn(
                &raw mut pid,
                c"/bin/sh".as_ptr(),
                actions.cast_const(),
                core::ptr::null(),
                argv.as_ptr(),
                envp,
            )
        } != 0;
    }
    // SAFETY: the actions were made above and are not used again.
    let _ = unsafe { posix_spawn_file_actions_destroy(actions) };
    let _ = crate::unistd::close(write_end);
    let mut read_all = !failed;
    let mut chunk = [0u8; 512];
    while read_all {
        // SAFETY: the kernel writes at most the chunk's length into it.
        let ret = unsafe {
            syscall::syscall3(
                nr::READ,
                read_end as usize,
                chunk.as_mut_ptr().addr(),
                chunk.len(),
            )
        };
        match errno::decode(ret) {
            Ok(0) => break,
            Ok(n) => {
                if !out.reserve(n) {
                    read_all = false;
                    break;
                }
                for &b in chunk.get(..n).unwrap_or_default() {
                    let _ = out.push(b);
                }
            }
            Err(errno::EINTR) => {}
            Err(_) => read_all = false,
        }
    }
    let _ = crate::unistd::close(read_end);
    if !failed {
        let mut status = 0;
        // SAFETY: `status` is a live local.
        while unsafe { crate::wait::waitpid(pid, &raw mut status, 0) } < 0
            && errno::get() == errno::EINTR
        {}
    }
    read_all
}

/// A copy of `word` in memory from `malloc`, NUL-terminated, or null.
fn copy_word(word: &[u8]) -> *mut c_char {
    let Some(size) = word.len().checked_add(1) else {
        return null_mut();
    };
    let copy = malloc(size).cast::<u8>();
    if copy.is_null() {
        return copy.cast();
    }
    for (i, &b) in word.iter().enumerate() {
        // SAFETY: `i` is below the copy's `size`.
        unsafe { copy.wrapping_add(i).write(b) };
    }
    // SAFETY: the last byte of the copy.
    unsafe { copy.wrapping_add(word.len()).write(0) };
    copy.cast()
}

/// `wordexp` with cancellation already disabled.
///
/// # Safety
///
/// As [`wordexp`].
unsafe fn expand(s: *const c_char, we: &mut Wordexp, flags: c_int) -> c_int {
    if flags & WRDE_REUSE != 0 {
        // SAFETY: the caller vouches that `we` holds an earlier result.
        unsafe { wordfree(we) };
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let text = unsafe { CStr::from_ptr(s) }.to_bytes();
    if flags & WRDE_NOCMD != 0
        && let Some(error) = refuse_commands(text)
    {
        return error;
    }
    let (existing, mut wv) = if flags & WRDE_APPEND != 0 {
        (we.we_wordc, we.we_wordv)
    } else {
        (0, null_mut())
    };
    let nospace = |we: &mut Wordexp| {
        if flags & WRDE_APPEND == 0 {
            we.we_wordc = 0;
            we.we_wordv = null_mut();
        }
        WRDE_NOSPACE
    };
    let mut i = existing;
    if flags & WRDE_DOOFFS != 0 {
        if we.we_offs > usize::MAX / size_of::<*mut c_char>() / 4 {
            return nospace(we);
        }
        i += we.we_offs;
    } else {
        we.we_offs = 0;
    }

    let mut out = Growable::new();
    let ran = run_shell(s, flags & WRDE_SHOWERR != 0, &mut out);
    if !ran && out.len() == 0 {
        return nospace(we);
    }
    let output = out.as_slice();
    // The dummy `x` first; output without its NUL is a syntax error.
    let Some(first) = output.iter().position(|&b| b == 0) else {
        return WRDE_SYNTAX;
    };
    let mut error = if ran { 0 } else { WRDE_NOSPACE };
    let mut room = if wv.is_null() { 0 } else { i + 1 };
    // Each word runs to its NUL, or to the end of the output, as `getdelim`
    // reads them.
    let mut rest = output.get(first + 1..).unwrap_or_default();
    while !rest.is_empty() {
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        let word = rest.get(..end).unwrap_or_default();
        rest = rest.get(end + 1..).unwrap_or_default();
        if i + 1 >= room {
            room += room / 2 + 10;
            let Some(bytes) = room.checked_mul(size_of::<*mut c_char>()) else {
                error = WRDE_NOSPACE;
                break;
            };
            // SAFETY: `wv` is null or a vector from `malloc`.
            let grown = unsafe { realloc(wv.cast(), bytes) }.cast::<*mut c_char>();
            if grown.is_null() {
                error = WRDE_NOSPACE;
                break;
            }
            wv = grown;
        }
        let copy = copy_word(word);
        if copy.is_null() {
            error = WRDE_NOSPACE;
            break;
        }
        // SAFETY: `i + 1 < room`, the slots the vector holds.
        unsafe { wv.wrapping_add(i).write(copy) };
        i += 1;
        // SAFETY: as above.
        unsafe { wv.wrapping_add(i).write(null_mut()) };
    }

    if wv.is_null() {
        wv = calloc(i + 1, size_of::<*mut c_char>()).cast();
    }
    we.we_wordv = wv;
    we.we_wordc = i;
    if flags & WRDE_DOOFFS != 0 {
        if !wv.is_null() {
            let mut slot = we.we_offs;
            while slot > 0 {
                // SAFETY: the vector holds the reserved slots before the words.
                unsafe { wv.wrapping_add(slot - 1).write(null_mut()) };
                slot -= 1;
            }
        }
        we.we_wordc -= we.we_offs;
    }
    error
}

/// Expands `s` as the shell would into the words of `*we`. Returns 0 or a
/// `WRDE_` error; see the module's documentation for the flags.
///
/// # Safety
///
/// `s` must be a NUL-terminated string and `we` a writable `wordexp_t`,
/// which with `WRDE_APPEND` or `WRDE_REUSE` must hold an earlier result.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wordexp(s: *const c_char, we: *mut Wordexp, flags: c_int) -> c_int {
    let old = crate::cancel::set_state(crate::cancel::DISABLE);
    // SAFETY: the caller passes a writable `wordexp_t`.
    let we = unsafe { &mut *we };
    // SAFETY: the caller passes a string, and `we` as `expand` needs it.
    let ret = unsafe { expand(s, we, flags) };
    let _ = crate::cancel::set_state(old);
    ret
}

/// Frees the words `wordexp` stored in `*we`.
///
/// # Safety
///
/// `we` must hold a result from `wordexp`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wordfree(we: *mut Wordexp) {
    // SAFETY: the caller passes a valid `wordexp_t`.
    let we = unsafe { &mut *we };
    if we.we_wordv.is_null() {
        return;
    }
    let mut i = 0;
    while i < we.we_wordc {
        // SAFETY: the vector holds `we_offs + we_wordc` words.
        let word = unsafe { we.we_wordv.wrapping_add(we.we_offs + i).read() };
        // SAFETY: each word came from `malloc`.
        unsafe { free(word.cast()) };
        i += 1;
    }
    // SAFETY: the vector came from `malloc`.
    unsafe { free(we.we_wordv.cast()) };
    we.we_wordv = null_mut();
    we.we_wordc = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nocmd_refuses_substitution_and_unquoted_operators() {
        assert_eq!(refuse_commands(b"a $HOME 'b c'"), None);
        assert_eq!(refuse_commands(b"$((1+2))"), None);
        assert_eq!(refuse_commands(b"'$(x)' \"a|b\""), None);
        assert_eq!(refuse_commands(b"$(ls)"), Some(WRDE_CMDSUB));
        assert_eq!(refuse_commands(b"`ls`"), Some(WRDE_CMDSUB));
        assert_eq!(refuse_commands(b"a;b"), Some(WRDE_BADCHAR));
        assert_eq!(refuse_commands(b"(a)"), Some(WRDE_BADCHAR));
        assert_eq!(refuse_commands(b"a\\"), Some(WRDE_SYNTAX));
    }
}
