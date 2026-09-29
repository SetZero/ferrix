//! `syslog.h`: messages to the system logger at `/dev/log`.
//!
//! This follows musl's `misc/syslog.c`. A message is formatted as
//! `<priority>Mmm dd hh:mm:ss ident[pid]: text`, the time in UTC, and sent as
//! one datagram on a Unix socket connected to `/dev/log`. After a lost
//! connection it reconnects and sends once more. When there is no logger,
//! because nothing listens at `/dev/log` or the kernel has no Unix sockets, as
//! Ferrix does not until they land, the message is dropped without a word,
//! unless `LOG_CONS` asked for it to go to `/dev/console` instead.
//! `LOG_PERROR` also writes it, from the identity on, to standard error.
//! Messages longer than 1023 bytes are cut there.
//!
//! One lock guards the state, so threads may log at once. The time is
//! formatted with `strftime` in the program's locale, where musl uses the C
//! locale; they differ only once `setlocale` has chosen other month names.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int};
use core::mem::MaybeUninit;
use core::ptr::null_mut;

use crate::errno;
use crate::fcntl::open;
use crate::lock::SpinLock;
use crate::process::getpid;
use crate::socket::{connect, send, socket};
use crate::strftime::strftime;
use crate::string::strnlen;
use crate::syscall::{self, nr};
use crate::time::time;
use crate::tm::{Tm, gmtime_r};
use crate::unistd::close;
use crate::va::{self, VaListArg};

/// `LOG_PID`, from `include/syslog.h`.
const LOG_PID: c_int = 0x01;
/// `LOG_CONS`, from `include/syslog.h`.
const LOG_CONS: c_int = 0x02;
/// `LOG_NDELAY`, from `include/syslog.h`.
const LOG_NDELAY: c_int = 0x08;
/// `LOG_PERROR`, from `include/syslog.h`.
const LOG_PERROR: c_int = 0x20;
/// `LOG_USER`, from `include/syslog.h`.
const LOG_USER: c_int = 1 << 3;
/// `LOG_FACMASK`, from `include/syslog.h`.
const LOG_FACMASK: c_int = 0x3f8;
/// `AF_UNIX`, from `include/sys/socket.h`.
const AF_UNIX: c_int = 1;
/// `SOCK_DGRAM | SOCK_CLOEXEC`, from `include/sys/socket.h`.
const DGRAM_CLOEXEC: c_int = 2 | 0o2_000_000;
/// `O_WRONLY | O_NOCTTY | O_CLOEXEC`, from `asm-generic/fcntl.h`.
const CONSOLE_FLAGS: c_int = 0o1 | 0o400 | 0o2_000_000;
/// The largest message sent, with its newline.
const MESSAGE_MAX: usize = 1024;

/// musl's address for `/dev/log`: a `short` family and nine bytes of path,
/// which with padding is 12 bytes.
const LOG_ADDRESS: [u8; 12] = [1, 0, b'/', b'd', b'e', b'v', b'/', b'l', b'o', b'g', 0, 0];

/// What `openlog` and `setlogmask` set.
#[derive(Debug)]
struct State {
    /// The identity, NUL-terminated, at most 31 bytes.
    ident: [u8; 32],
    /// The `LOG_*` options.
    options: c_int,
    /// The facility for messages that name none.
    facility: c_int,
    /// Which priorities are logged, a bit each.
    mask: c_int,
    /// The socket, or -1.
    fd: c_int,
}

/// The logger's state and the lock that guards it.
#[derive(Debug)]
struct Logger {
    /// Held while the state is used.
    lock: SpinLock,
    /// The state.
    state: UnsafeCell<State>,
}

// SAFETY: the state is only reached through `with_state`, with the lock held.
unsafe impl Sync for Logger {}

/// The one logger.
static LOGGER: Logger = Logger {
    lock: SpinLock::new(),
    state: UnsafeCell::new(State {
        ident: [0; 32],
        options: 0,
        facility: LOG_USER,
        mask: 0xff,
        fd: -1,
    }),
};

/// Runs `op` on the state with the lock held.
fn with_state<R>(op: impl FnOnce(&mut State) -> R) -> R {
    let _guard = LOGGER.lock.lock();
    // SAFETY: the lock is held, so nothing else reaches the state.
    let state = unsafe { &mut *LOGGER.state.get() };
    op(state)
}

/// The calling thread's `errno`.
fn last_errno() -> c_int {
    // SAFETY: the pointer is this thread's errno.
    unsafe { errno::__errno_location().read() }
}

/// Opens the socket and connects it to `/dev/log`, which may fail quietly.
fn open_socket(state: &mut State) {
    state.fd = socket(AF_UNIX, DGRAM_CLOEXEC, 0);
    if state.fd >= 0 {
        // SAFETY: the address is a live constant of the length given.
        let _ = unsafe { connect(state.fd, LOG_ADDRESS.as_ptr().cast(), 12) };
    }
}

/// Sets which priorities are logged, if `mask` is not zero, and returns the
/// mask before.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setlogmask(mask: c_int) -> c_int {
    with_state(|state| {
        let old = state.mask;
        if mask != 0 {
            state.mask = mask;
        }
        old
    })
}

/// Sets the identity, options and default facility of later messages, and
/// with `LOG_NDELAY` connects to the logger now.
///
/// # Safety
///
/// `ident` must be null or a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn openlog(ident: *const c_char, options: c_int, facility: c_int) {
    with_state(|state| {
        state.ident = [0; 32];
        if !ident.is_null() {
            // SAFETY: the caller passes a NUL-terminated string.
            let len = unsafe { strnlen(ident, state.ident.len() - 1) };
            for (offset, slot) in state.ident.iter_mut().take(len).enumerate() {
                // SAFETY: `strnlen` found `len` readable bytes.
                *slot = unsafe { ident.wrapping_add(offset).read() } as u8;
            }
        }
        state.options = options;
        state.facility = facility;
        if options & LOG_NDELAY != 0 && state.fd < 0 {
            open_socket(state);
        }
    });
}

/// Closes the connection to the logger.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn closelog() {
    // Cancellation waits until this is finished, as in musl.
    let _cancel = crate::cancel::disable();
    with_state(|state| {
        let _ = close(state.fd);
        state.fd = -1;
    });
}

/// Stores `byte` at `*len` in `out`, if it fits, and counts it.
fn put(out: &mut [u8], len: &mut usize, byte: u8) {
    if let Some(slot) = out.get_mut(*len) {
        *slot = byte;
    }
    *len += 1;
}

/// Writes `value` in decimal.
fn decimal(out: &mut [u8], len: &mut usize, value: u32) {
    if value >= 10 {
        decimal(out, len, value / 10);
    }
    put(out, len, b'0' + (value % 10) as u8);
}

/// Writes a message's header into `out`: `<priority>time ` and then
/// `ident[pid]: `, without the brackets when `pid` is 0. Returns the header's
/// length and where the identity starts.
fn header(
    out: &mut [u8],
    priority: c_int,
    time: &[u8],
    ident: &[u8],
    pid: c_int,
) -> (usize, usize) {
    let mut len = 0;
    put(out, &mut len, b'<');
    decimal(out, &mut len, priority.cast_unsigned());
    put(out, &mut len, b'>');
    for &byte in time {
        put(out, &mut len, byte);
    }
    put(out, &mut len, b' ');
    let ident_at = len;
    for &byte in ident {
        put(out, &mut len, byte);
    }
    if pid != 0 {
        put(out, &mut len, b'[');
        decimal(out, &mut len, pid.cast_unsigned());
        put(out, &mut len, b']');
    }
    put(out, &mut len, b':');
    put(out, &mut len, b' ');
    (len.min(out.len()), ident_at)
}

/// Writes `bytes` to `fd`, ignoring the result.
fn write_all(fd: c_int, bytes: &[u8]) {
    // SAFETY: the kernel only reads the bytes.
    let _ =
        unsafe { syscall::syscall3(nr::WRITE, fd as usize, bytes.as_ptr().addr(), bytes.len()) };
}

/// Sends `entry` on the logger's socket, reconnecting once if the connection
/// was lost. False if it was not sent.
fn send_entry(fd: c_int, entry: &[u8]) -> bool {
    // SAFETY: the entry is live, of the length given.
    if unsafe { send(fd, entry.as_ptr().cast(), entry.len(), 0) } >= 0 {
        return true;
    }
    let lost = matches!(
        last_errno(),
        errno::ECONNREFUSED | errno::ECONNRESET | errno::ENOTCONN | errno::EPIPE
    );
    // SAFETY: the address is a live constant of the length given.
    lost && unsafe { connect(fd, LOG_ADDRESS.as_ptr().cast(), 12) } >= 0
        // SAFETY: as for the first send.
        && unsafe { send(fd, entry.as_ptr().cast(), entry.len(), 0) } >= 0
}

/// Formats and sends one message, with the lock held.
///
/// # Safety
///
/// `message` must be a format string and `ap` its arguments.
unsafe fn log(state: &mut State, priority: c_int, message: *const c_char, ap: VaListArg) {
    // Cancellation waits until this is finished, as in musl.
    let _cancel = crate::cancel::disable();
    let saved_errno = last_errno();
    if state.fd < 0 {
        open_socket(state);
    }
    let priority = if priority & LOG_FACMASK == 0 {
        priority | state.facility
    } else {
        priority
    };
    // SAFETY: a null argument asks only for the result.
    let now = unsafe { time(null_mut()) };
    let mut tm = MaybeUninit::<Tm>::uninit();
    let mut clock = [0u8; 16];
    // SAFETY: `now` is a live local, and `gmtime_r` fills the whole of `tm`.
    let filled = unsafe { gmtime_r(&raw const now, tm.as_mut_ptr()) };
    let clock_len = if filled.is_null() {
        0
    } else {
        // SAFETY: the buffer holds 16 bytes, the format is NUL-terminated, and
        // `gmtime_r` filled `tm`.
        unsafe {
            strftime(
                clock.as_mut_ptr().cast(),
                clock.len(),
                c"%b %e %T".as_ptr(),
                tm.as_ptr(),
            )
        }
    };
    let pid = if state.options & LOG_PID != 0 {
        getpid()
    } else {
        0
    };
    let ident_len = state.ident.iter().position(|&byte| byte == 0).unwrap_or(0);

    let mut buf = [0u8; MESSAGE_MAX];
    let (mut len, ident_at) = header(
        &mut buf,
        priority,
        clock.get(..clock_len).unwrap_or_default(),
        state.ident.get(..ident_len).unwrap_or_default(),
        pid,
    );
    errno::set(saved_errno);
    let room = buf.len() - len;
    // SAFETY: `room` bytes are writable after the header, and the caller
    // passes a format and its arguments.
    let written = unsafe {
        crate::stdio::printf::vsnprintf(
            buf.as_mut_ptr().wrapping_add(len).cast(),
            room,
            message,
            ap,
        )
    };
    let Ok(written) = usize::try_from(written) else {
        return;
    };
    len = if written >= room {
        buf.len() - 1
    } else {
        len + written
    };
    if buf.get(len - 1) != Some(&b'\n') {
        put(&mut buf, &mut len, b'\n');
    }
    let entry = buf.get(..len).unwrap_or_default();
    let text = entry.get(ident_at..).unwrap_or_default();

    if !send_entry(state.fd, entry) && state.options & LOG_CONS != 0 {
        // SAFETY: the path is NUL-terminated.
        let console = unsafe { open(c"/dev/console".as_ptr(), CONSOLE_FLAGS, 0) };
        if console >= 0 {
            write_all(console, text);
            let _ = close(console);
        }
    }
    if state.options & LOG_PERROR != 0 {
        write_all(2, text);
    }
}

/// Logs `message`, formatted with the arguments in `ap`, at `priority`, if
/// the mask lets that priority through and it names no bits beyond a facility
/// and a level.
///
/// # Safety
///
/// `message` must be a format string and `ap` a `va_list` of its arguments.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vsyslog(priority: c_int, message: *const c_char, ap: VaListArg) {
    if priority & !0x3ff != 0 {
        return;
    }
    with_state(|state| {
        if state.mask & (1 << (priority & 7)) == 0 {
            return;
        }
        // SAFETY: the caller passes a format and its arguments.
        unsafe { log(state, priority, message, ap) };
    });
}

va::variadic!(syslog, 2, vsyslog);

/// [`vsyslog`] as `_FORTIFY_SOURCE` rewrites it, with glibc's flag before the
/// format. The flag asks glibc to refuse `%n` in a writable format; as with
/// `printf`'s checked forms here, that check is not made.
///
/// # Safety
///
/// As [`vsyslog`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __vsyslog_chk(
    priority: c_int,
    _flag: c_int,
    message: *const c_char,
    ap: VaListArg,
) {
    // SAFETY: the caller's contract is `vsyslog`'s.
    unsafe { vsyslog(priority, message, ap) }
}

va::variadic!(__syslog_chk, 3, __vsyslog_chk);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_has_the_priority_time_identity_and_pid() {
        let mut out = [0u8; 64];
        let (len, ident_at) = header(&mut out, 134, b"Sep 13 20:04:05", b"busybox", 42);
        assert_eq!(
            out.get(..len),
            Some(&b"<134>Sep 13 20:04:05 busybox[42]: "[..])
        );
        assert_eq!(out.get(ident_at..len), Some(&b"busybox[42]: "[..]));
        let (len, _) = header(&mut out, 13, b"Jan  1 00:00:00", b"", 0);
        assert_eq!(out.get(..len), Some(&b"<13>Jan  1 00:00:00 : "[..]));
    }

    #[test]
    fn an_entry_goes_out_as_one_datagram() {
        let mut fds = [-1; 2];
        // SAFETY: `fds` holds two ints. AF_UNIX and SOCK_DGRAM.
        let made = unsafe { crate::socket::socketpair(1, 2, 0, fds.as_mut_ptr()) };
        assert_eq!(made, 0);
        let [a, b] = fds;
        assert!(send_entry(a, b"<14>Sep 13 20:04:05 test: hello\n"));
        let mut got = [0u8; 64];
        // SAFETY: the buffer is a live local of the length given.
        let n = unsafe { crate::socket::recv(b, got.as_mut_ptr().cast(), got.len(), 0) };
        assert_eq!(
            got.get(..usize::try_from(n).unwrap_or(0)),
            Some(&b"<14>Sep 13 20:04:05 test: hello\n"[..])
        );
        let _ = close(a);
        let _ = close(b);
        assert!(!send_entry(-1, b"x"));
    }

    #[test]
    fn the_mask_is_returned_and_zero_leaves_it() {
        let old = setlogmask(0);
        assert_eq!(setlogmask(0x1), old);
        assert_eq!(setlogmask(0), 0x1);
        assert_eq!(setlogmask(old), 0x1);
    }
}
