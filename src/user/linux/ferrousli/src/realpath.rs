//! `stdlib.h`'s `realpath`: the absolute name of a file, with every symbolic
//! link, `.` and `..` resolved.
//!
//! This is musl's `misc/realpath.c`, which resolves the name in user space
//! rather than through `/proc/self/fd`, so it works without `/proc`. The name
//! still to resolve is kept at the end of a stack buffer, one component is
//! moved at a time to the output, and a component that is a link has the
//! link's contents pushed back onto the stack. A `..` whose parent is already
//! known to be a directory is resolved without asking the kernel, and leading
//! `..`s of a relative name are applied to the working directory at the end.
//!
//! A name of `PATH_MAX` bytes or more fails with `ENAMETOOLONG`, and more than
//! `SYMLOOP_MAX` links with `ELOOP`.

use core::ffi::{c_char, c_int};
use core::ptr::null_mut;

use crate::errno;
use crate::string::{strdup, strnlen};
use crate::unistd::{getcwd, readlink};

/// `PATH_MAX`, from `include/limits.h`.
const PATH_MAX: usize = 4096;
/// `SYMLOOP_MAX`, from `include/limits.h`.
const SYMLOOP_MAX: usize = 40;

/// The byte at `index`, or NUL past the end.
fn at(buffer: &[u8], index: usize) -> u8 {
    buffer.get(index).copied().unwrap_or(0)
}

/// Stores `byte` at `index`, if it is inside the buffer.
fn set(buffer: &mut [u8], index: usize, byte: u8) {
    if let Some(slot) = buffer.get_mut(index) {
        *slot = byte;
    }
}

/// How many slashes start at `index`.
fn slash_len(buffer: &[u8], index: usize) -> usize {
    buffer.get(index..).map_or(0, |rest| {
        rest.iter().take_while(|&&byte| byte == b'/').count()
    })
}

/// The length of the output's first `q` bytes once their last component is
/// dropped for a `..`: back to the slash before it, and past that slash unless
/// it is the root's.
fn parent(output: &[u8], mut q: usize) -> usize {
    while q > 0 && at(output, q - 1) != b'/' {
        q -= 1;
    }
    if q > 1 && (q > 2 || at(output, 0) != b'/') {
        q -= 1;
    }
    q
}

/// The calling thread's `errno`.
fn last_errno() -> c_int {
    // SAFETY: the pointer is this thread's errno.
    unsafe { errno::__errno_location().read() }
}

/// Fails with `error`.
fn fail(error: c_int) -> *mut c_char {
    errno::set(error);
    null_mut()
}

/// The absolute name of `filename`, with links, `.` and `..` resolved, stored
/// in `resolved`, which must hold `PATH_MAX` bytes, or in a string from
/// `malloc` if `resolved` is null. Returns null with `errno` set on failure.
///
/// # Safety
///
/// `filename` must be null or a NUL-terminated string, and `resolved` null or
/// valid for writes of `PATH_MAX` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn realpath(filename: *const c_char, resolved: *mut c_char) -> *mut c_char {
    if filename.is_null() {
        return fail(errno::EINVAL);
    }
    let mut stack = [0u8; PATH_MAX + 1];
    let mut output = [0u8; PATH_MAX];
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { strnlen(filename, stack.len()) };
    if len == 0 {
        return fail(errno::ENOENT);
    }
    if len >= PATH_MAX {
        return fail(errno::ENAMETOOLONG);
    }
    let mut p = stack.len() - len - 1;
    for offset in 0..len {
        // SAFETY: `strnlen` found `len` readable bytes.
        let byte = unsafe { filename.wrapping_add(offset).read() };
        set(&mut stack, p + offset, byte as u8);
    }
    let mut q = 0;
    let mut links = 0;
    let mut ups = 0;
    let mut check_dir = false;

    'restart: loop {
        loop {
            // A component starting with a slash is the root: start again from
            // it. An initial `//` that is not `///` is kept, as POSIX allows.
            if at(&stack, p) == b'/' {
                check_dir = false;
                ups = 0;
                q = 0;
                set(&mut output, q, b'/');
                q += 1;
                p += 1;
                if at(&stack, p) == b'/' && at(&stack, p + 1) != b'/' {
                    set(&mut output, q, b'/');
                    q += 1;
                }
                p += slash_len(&stack, p);
                continue;
            }
            let component = stack.get(p..).map_or(0, |rest| {
                rest.iter()
                    .position(|&byte| byte == b'/' || byte == 0)
                    .unwrap_or(rest.len())
            });
            let original = component;
            let mut l = component;
            if l == 0 && !check_dir {
                break 'restart;
            }
            if l == 1 && at(&stack, p) == b'.' {
                p += l;
                p += slash_len(&stack, p);
                continue;
            }
            // Copy the component to the output, with a slash before it, to ask
            // the kernel about it, but do not count it until it is known not to
            // be a link.
            if q > 0 && at(&output, q - 1) != b'/' {
                if p == 0 {
                    return fail(errno::ENAMETOOLONG);
                }
                p -= 1;
                set(&mut stack, p, b'/');
                l += 1;
            }
            if q + l >= PATH_MAX {
                return fail(errno::ENAMETOOLONG);
            }
            for offset in 0..l {
                let byte = at(&stack, p + offset);
                set(&mut output, q + offset, byte);
            }
            set(&mut output, q + l, 0);
            p += l;

            let mut up = false;
            let mut known_dir = false;
            if original == 2 && at(&stack, p - 2) == b'.' && at(&stack, p - 1) == b'.' {
                up = true;
                // `..`s with nothing before them to cancel wait for the
                // working directory.
                if q <= 3 * ups {
                    ups += 1;
                    q += l;
                    p += slash_len(&stack, p);
                    continue;
                }
                known_dir = !check_dir;
            }
            let read = if known_dir {
                None
            } else {
                // SAFETY: the output is NUL-terminated, and the kernel writes at
                // most `p` bytes to the start of the stack, below what is left
                // of the name.
                let got = unsafe { readlink(output.as_ptr().cast(), stack.as_mut_ptr().cast(), p) };
                match usize::try_from(got) {
                    Ok(got) if got == p => return fail(errno::ENAMETOOLONG),
                    Ok(0) => return fail(errno::ENOENT),
                    Ok(got) => Some(got),
                    Err(_) if last_errno() != errno::EINVAL => return null_mut(),
                    Err(_) => None,
                }
            };
            let Some(got) = read else {
                // Not a link.
                check_dir = false;
                if up {
                    q = parent(&output, q);
                    p += slash_len(&stack, p);
                    continue;
                }
                if original != 0 {
                    q += l;
                }
                check_dir = at(&stack, p) != 0;
                p += slash_len(&stack, p);
                continue;
            };
            links += 1;
            if links == SYMLOOP_MAX {
                return fail(errno::ELOOP);
            }
            // A link ending in a slash drops the slashes already on the stack.
            if at(&stack, got - 1) == b'/' {
                while at(&stack, p) == b'/' {
                    p += 1;
                }
            }
            p -= got;
            stack.copy_within(0..got, p);
            continue 'restart;
        }
    }

    set(&mut output, q, 0);
    if at(&output, 0) != b'/' {
        // SAFETY: the stack is a live local of the size given.
        if unsafe { getcwd(stack.as_mut_ptr().cast(), stack.len()) }.is_null() {
            return null_mut();
        }
        let mut l = stack
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(stack.len());
        let mut skip = 0;
        while ups > 0 {
            ups -= 1;
            while l > 1 && at(&stack, l - 1) != b'/' {
                l -= 1;
            }
            if l > 1 {
                l -= 1;
            }
            skip += 2;
            if skip < q {
                skip += 1;
            }
        }
        let rest = q.saturating_sub(skip);
        if rest > 0 && at(&stack, l - 1) != b'/' {
            set(&mut stack, l, b'/');
            l += 1;
        }
        if l + rest + 1 >= PATH_MAX {
            return fail(errno::ENAMETOOLONG);
        }
        output.copy_within(skip..skip + rest + 1, l);
        for offset in 0..l {
            let byte = at(&stack, offset);
            set(&mut output, offset, byte);
        }
        q = l + rest;
    }

    if resolved.is_null() {
        // SAFETY: the output is NUL-terminated.
        return unsafe { strdup(output.as_ptr().cast()) };
    }
    for offset in 0..=q {
        // SAFETY: `q` is below `PATH_MAX`, which the caller vouches for.
        unsafe {
            resolved
                .wrapping_add(offset)
                .write(at(&output, offset) as c_char)
        };
    }
    resolved
}
