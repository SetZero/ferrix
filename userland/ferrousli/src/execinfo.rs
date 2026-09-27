//! glibc's `execinfo.h`: `backtrace`, which answers the return addresses on
//! the calling thread's stack, `backtrace_symbols_fd`, which writes one line
//! naming each, and `backtrace_symbols`, which answers those lines as
//! strings. LLVM and `rustc`'s driver import the first two, for the stack a
//! crash report prints; PulseAudio's `libpulsecommon` imports the first and
//! the last, for the stack its log may show, and is not loaded without them.
//!
//! As glibc's, the walk is libgcc's unwinder's, `_Unwind_Backtrace`, found in
//! `libgcc_s.so.1` -- which a C++ program such as `rustc` has loaded already
//! -- rather than frame pointers, which optimised code does not keep. A
//! static program has no loader to find it with, and its `backtrace` answers
//! no frames.

use core::ffi::{c_char, c_int, c_void};
use core::ptr::null_mut;

use crate::link::{DlInfo, dladdr, dlopen, dlsym_caller};

/// `_Unwind_Backtrace(trace, argument)`.
type Backtrace = unsafe extern "C" fn(Trace, *mut c_void) -> c_int;
/// The callback `_Unwind_Backtrace` calls for each frame.
type Trace = extern "C" fn(*mut c_void, *mut c_void) -> c_int;
/// `_Unwind_GetIP(context)`: the frame's return address.
type GetIp = unsafe extern "C" fn(*mut c_void) -> usize;

/// `_URC_NO_REASON`: go on to the next frame.
const NO_REASON: c_int = 0;
/// `_URC_END_OF_STACK`: stop.
const END_OF_STACK: c_int = 5;
/// `RTLD_NOW`.
const RTLD_NOW: c_int = 2;

/// Where a walk writes, and how far it has got.
struct Walk {
    buffer: *mut *mut c_void,
    size: usize,
    count: usize,
    get_ip: GetIp,
    /// Whether the frame to come is `backtrace`'s own, which glibc leaves
    /// out.
    own: bool,
}

/// libgcc's unwinder, from the objects loaded or from `libgcc_s.so.1`.
fn unwinder() -> Option<(Backtrace, GetIp)> {
    let find = |handle: *mut c_void| {
        // SAFETY: the names are NUL-terminated.
        let backtrace = unsafe { dlsym_caller(handle, c"_Unwind_Backtrace".as_ptr(), null_mut()) };
        // SAFETY: as above.
        let get_ip = unsafe { dlsym_caller(handle, c"_Unwind_GetIP".as_ptr(), null_mut()) };
        if backtrace.is_null() || get_ip.is_null() {
            return None;
        }
        // SAFETY: libgcc's `_Unwind_Backtrace` has this signature.
        let backtrace = unsafe { core::mem::transmute::<*mut c_void, Backtrace>(backtrace) };
        // SAFETY: and `_Unwind_GetIP` this one.
        let get_ip = unsafe { core::mem::transmute::<*mut c_void, GetIp>(get_ip) };
        Some((backtrace, get_ip))
    };
    // `RTLD_DEFAULT` first, then the library itself.
    find(null_mut()).or_else(|| {
        // SAFETY: the name is NUL-terminated.
        let handle = unsafe { dlopen(c"libgcc_s.so.1".as_ptr(), RTLD_NOW) };
        if handle.is_null() { None } else { find(handle) }
    })
}

/// Records one frame of a [`Walk`].
extern "C" fn record(context: *mut c_void, argument: *mut c_void) -> c_int {
    // SAFETY: `backtrace` passes its live `Walk`, and nothing else holds it
    // during the walk.
    let walk = unsafe { &mut *argument.cast::<Walk>() };
    if walk.own {
        walk.own = false;
        return NO_REASON;
    }
    if walk.count >= walk.size {
        return END_OF_STACK;
    }
    // SAFETY: `context` is the unwinder's, for this frame.
    let ip = unsafe { (walk.get_ip)(context) };
    // SAFETY: `count < size`, and the caller vouches for `size` slots.
    unsafe {
        walk.buffer
            .wrapping_add(walk.count)
            .write(core::ptr::with_exposed_provenance_mut(ip));
    }
    walk.count += 1;
    NO_REASON
}

/// Writes up to `size` return addresses of the calling thread's stack into
/// `buffer`, innermost first, and answers how many.
///
/// # Safety
///
/// `buffer` must be valid for writing `size` pointers.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn backtrace(buffer: *mut *mut c_void, size: c_int) -> c_int {
    let Ok(size) = usize::try_from(size) else {
        return 0;
    };
    if size == 0 {
        return 0;
    }
    let Some((walk_stack, get_ip)) = unwinder() else {
        return 0;
    };
    let mut walk = Walk {
        buffer,
        size,
        count: 0,
        get_ip,
        own: true,
    };
    // SAFETY: `record` reads the `Walk` passed, which outlives the call.
    let _ = unsafe { walk_stack(record, (&raw mut walk).cast()) };
    // The outermost frame's address is sometimes zero, which glibc drops.
    if walk.count > 0
        // SAFETY: slot `count - 1` was written.
        && unsafe { walk.buffer.wrapping_add(walk.count - 1).read() }.is_null()
    {
        walk.count -= 1;
    }
    c_int::try_from(walk.count).unwrap_or(c_int::MAX)
}

/// A line being built for [`backtrace_symbols_fd`], cut short if it would
/// not fit.
struct Line {
    bytes: [u8; 1024],
    len: usize,
}

impl Line {
    fn push(&mut self, byte: u8) {
        if let Some(slot) = self.bytes.get_mut(self.len) {
            *slot = byte;
            self.len += 1;
        }
    }

    /// A NUL-terminated string, without its NUL.
    ///
    /// # Safety
    ///
    /// `text` must be a NUL-terminated string.
    unsafe fn string(&mut self, text: *const c_char) {
        let mut at = text;
        // SAFETY: the caller vouches for the terminator, and this stops there.
        while unsafe { at.read() } != 0 {
            // SAFETY: as above.
            self.push(unsafe { at.read() } as u8);
            at = at.wrapping_add(1);
        }
    }

    /// `value` as `0x` and lower-case hexadecimal digits, as `%#tx` and `%p`
    /// write it.
    fn hex(&mut self, value: usize) {
        self.push(b'0');
        self.push(b'x');
        let digits = usize::BITS / 4;
        let mut started = false;
        for index in (0..digits).rev() {
            let digit = (value >> (index * 4)) & 0xf;
            if digit != 0 || started || index == 0 {
                started = true;
                self.push(b"0123456789abcdef".get(digit).copied().unwrap_or(b'?'));
            }
        }
    }
}

/// The line naming `address`, as glibc writes it: `object(symbol+0xoffset)
/// [0xaddress]`, `object(+0xoffset) [0xaddress]` when no symbol is near, or
/// `[0xaddress]` when no object holds it. Without a newline.
fn describe(address: *mut c_void) -> Line {
    let mut line = Line {
        bytes: [0; 1024],
        len: 0,
    };
    let mut info = DlInfo {
        dli_fname: core::ptr::null(),
        dli_fbase: null_mut(),
        dli_sname: core::ptr::null(),
        dli_saddr: null_mut(),
    };
    // SAFETY: `info` is live.
    let found = unsafe { dladdr(address, &raw mut info) } != 0;
    if found && !info.dli_fname.is_null() {
        // SAFETY: `dladdr` answers NUL-terminated names.
        unsafe { line.string(info.dli_fname) };
        line.push(b'(');
        let base = if info.dli_sname.is_null() {
            info.dli_fbase.addr()
        } else {
            // SAFETY: as above.
            unsafe { line.string(info.dli_sname) };
            info.dli_saddr.addr()
        };
        let (sign, offset) = if address.addr() >= base {
            (b'+', address.addr() - base)
        } else {
            (b'-', base - address.addr())
        };
        line.push(sign);
        line.hex(offset);
        line.push(b')');
        line.push(b' ');
    }
    line.push(b'[');
    line.hex(address.addr());
    line.push(b']');
    line
}

/// Answers a string for each of the `size` addresses at `buffer`, the line
/// [`backtrace_symbols_fd`] writes without its newline, as glibc does: in
/// one block from `malloc`, which the caller frees whole, holding the `size`
/// pointers and after them the strings they point to. Null when there is no
/// memory for the block.
///
/// # Safety
///
/// `buffer` must be valid for reading `size` pointers.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn backtrace_symbols(
    buffer: *const *mut c_void,
    size: c_int,
) -> *mut *mut c_char {
    let count = usize::try_from(size).unwrap_or(0);
    let address = |index: usize| {
        // SAFETY: the caller vouches for `size` pointers.
        unsafe { buffer.wrapping_add(index).read() }
    };
    let pointers = count.saturating_mul(size_of::<*mut c_char>());
    let total = (0..count).fold(pointers, |total, index| {
        total.saturating_add(describe(address(index)).len + 1)
    });
    // `malloc`'s alignment is a pointer's, so the slots at the start are
    // aligned; the strings after them are bytes.
    let block = crate::malloc::malloc(total);
    if block.is_null() {
        return null_mut();
    }
    let slots = block.cast::<*mut c_char>();
    let mut text = block.cast::<u8>().wrapping_add(pointers);
    for index in 0..count {
        let line = describe(address(index));
        // SAFETY: the block holds the pointers and every line with its NUL,
        // each measured above as it is written here, and `text` stays in it.
        unsafe { core::ptr::copy_nonoverlapping(line.bytes.as_ptr(), text, line.len) };
        // SAFETY: as above: the line's NUL is inside the block.
        unsafe { text.wrapping_add(line.len).write(0) };
        // SAFETY: slot `index` is one of the `count` at the block's start.
        unsafe { slots.wrapping_add(index).write(text.cast()) };
        text = text.wrapping_add(line.len + 1);
    }
    slots
}

/// Writes one line for each of the `size` addresses at `buffer` to `fd`, as
/// glibc does: [`describe`]'s, and a newline.
///
/// # Safety
///
/// `buffer` must be valid for reading `size` pointers.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn backtrace_symbols_fd(buffer: *const *mut c_void, size: c_int, fd: c_int) {
    let count = usize::try_from(size).unwrap_or(0);
    for index in 0..count {
        // SAFETY: the caller vouches for `size` pointers.
        let address = unsafe { buffer.wrapping_add(index).read() };
        let mut line = describe(address);
        line.push(b'\n');
        let bytes = line.bytes.get(..line.len).unwrap_or_default();
        // SAFETY: `bytes` is live; a short or failed write is not reported,
        // as glibc does not.
        let _ = unsafe { crate::unistd::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_written_as_percent_p_writes_it() {
        let mut line = Line {
            bytes: [0; 1024],
            len: 0,
        };
        line.hex(0);
        line.push(b' ');
        line.hex(0x7f00_dead_beef);
        assert_eq!(line.bytes.get(..line.len), Some(&b"0x0 0x7f00deadbeef"[..]));
    }

    #[test]
    fn symbols_are_one_block_of_pointers_then_their_strings() {
        // Addresses no object holds, so each line is its address alone.
        let addresses = [
            core::ptr::with_exposed_provenance_mut::<c_void>(0x10),
            core::ptr::with_exposed_provenance_mut::<c_void>(0xabc0),
        ];
        let size = c_int::try_from(addresses.len()).unwrap_or(0);
        // SAFETY: `addresses` holds `size` pointers.
        let block = unsafe { backtrace_symbols(addresses.as_ptr(), size) };
        assert!(!block.is_null());
        let start = block.addr();
        let pointers = addresses.len() * size_of::<*mut c_char>();
        let mut expected = start + pointers;
        for (index, want) in [&b"[0x10]"[..], &b"[0xabc0]"[..]].into_iter().enumerate() {
            // SAFETY: the block holds `size` pointers.
            let text = unsafe { block.wrapping_add(index).read() };
            assert_eq!(
                text.addr(),
                expected,
                "string {index} follows the one before"
            );
            // SAFETY: each string is NUL-terminated inside the block.
            let got = unsafe { core::ffi::CStr::from_ptr(text) }.to_bytes();
            assert_eq!(got, want);
            expected += want.len() + 1;
        }
        // SAFETY: the block is `malloc`'s, and freed once.
        unsafe { crate::malloc::free(block.cast()) };
    }
}
