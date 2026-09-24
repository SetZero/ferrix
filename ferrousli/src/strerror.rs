//! `strerror`, `strerror_r` and `strsignal`: the text describing an error
//! number or a signal.
//!
//! The wording is musl's (MIT), from `src/errno/__strerror.h` and
//! `src/string/strsignal.c` in musl 1.2.5. musl describes only the errors a
//! program is likely to meet and gives the rest "No error information". Here
//! every error number the kernel's headers define has a message: those musl
//! lacks are worded afresh from what the kernel's header says each means.
//! glibc's table is LGPL and is not a source.

use core::ffi::{CStr, c_char, c_int};

use crate::errno as e;
use crate::locale::Locale;

/// The message for 0 and for any number without one of its own.
const UNKNOWN_ERROR: &CStr = c"No error information";

/// One more than the highest error number the kernel defines.
const ERROR_COUNT: usize = e::EHWPOISON as usize + 1;

/// Each error number and its message.
const ERROR_TEXT: &[(c_int, &CStr)] = &[
    // musl's messages.
    (e::EILSEQ, c"Illegal byte sequence"),
    (e::EDOM, c"Domain error"),
    (e::ERANGE, c"Result not representable"),
    (e::ENOTTY, c"Not a tty"),
    (e::EACCES, c"Permission denied"),
    (e::EPERM, c"Operation not permitted"),
    (e::ENOENT, c"No such file or directory"),
    (e::ESRCH, c"No such process"),
    (e::EEXIST, c"File exists"),
    (e::EOVERFLOW, c"Value too large for data type"),
    (e::ENOSPC, c"No space left on device"),
    (e::ENOMEM, c"Out of memory"),
    (e::EBUSY, c"Resource busy"),
    (e::EINTR, c"Interrupted system call"),
    (e::EAGAIN, c"Resource temporarily unavailable"),
    (e::ESPIPE, c"Invalid seek"),
    (e::EXDEV, c"Cross-device link"),
    (e::EROFS, c"Read-only file system"),
    (e::ENOTEMPTY, c"Directory not empty"),
    (e::ECONNRESET, c"Connection reset by peer"),
    (e::ETIMEDOUT, c"Operation timed out"),
    (e::ECONNREFUSED, c"Connection refused"),
    (e::EHOSTDOWN, c"Host is down"),
    (e::EHOSTUNREACH, c"Host is unreachable"),
    (e::EADDRINUSE, c"Address in use"),
    (e::EPIPE, c"Broken pipe"),
    (e::EIO, c"I/O error"),
    (e::ENXIO, c"No such device or address"),
    (e::ENOTBLK, c"Block device required"),
    (e::ENODEV, c"No such device"),
    (e::ENOTDIR, c"Not a directory"),
    (e::EISDIR, c"Is a directory"),
    (e::ETXTBSY, c"Text file busy"),
    (e::ENOEXEC, c"Exec format error"),
    (e::EINVAL, c"Invalid argument"),
    (e::E2BIG, c"Argument list too long"),
    (e::ELOOP, c"Symbolic link loop"),
    (e::ENAMETOOLONG, c"Filename too long"),
    (e::ENFILE, c"Too many open files in system"),
    (e::EMFILE, c"No file descriptors available"),
    (e::EBADF, c"Bad file descriptor"),
    (e::ECHILD, c"No child process"),
    (e::EFAULT, c"Bad address"),
    (e::EFBIG, c"File too large"),
    (e::EMLINK, c"Too many links"),
    (e::ENOLCK, c"No locks available"),
    (e::EDEADLK, c"Resource deadlock would occur"),
    (e::ENOTRECOVERABLE, c"State not recoverable"),
    (e::EOWNERDEAD, c"Previous owner died"),
    (e::ECANCELED, c"Operation canceled"),
    (e::ENOSYS, c"Function not implemented"),
    (e::ENOMSG, c"No message of desired type"),
    (e::EIDRM, c"Identifier removed"),
    (e::ENOSTR, c"Device not a stream"),
    (e::ENODATA, c"No data available"),
    (e::ETIME, c"Device timeout"),
    (e::ENOSR, c"Out of streams resources"),
    (e::ENOLINK, c"Link has been severed"),
    (e::EPROTO, c"Protocol error"),
    (e::EBADMSG, c"Bad message"),
    (e::EBADFD, c"File descriptor in bad state"),
    (e::ENOTSOCK, c"Not a socket"),
    (e::EDESTADDRREQ, c"Destination address required"),
    (e::EMSGSIZE, c"Message too large"),
    (e::EPROTOTYPE, c"Protocol wrong type for socket"),
    (e::ENOPROTOOPT, c"Protocol not available"),
    (e::EPROTONOSUPPORT, c"Protocol not supported"),
    (e::ESOCKTNOSUPPORT, c"Socket type not supported"),
    (e::ENOTSUP, c"Not supported"),
    (e::EPFNOSUPPORT, c"Protocol family not supported"),
    (e::EAFNOSUPPORT, c"Address family not supported by protocol"),
    (e::EADDRNOTAVAIL, c"Address not available"),
    (e::ENETDOWN, c"Network is down"),
    (e::ENETUNREACH, c"Network unreachable"),
    (e::ENETRESET, c"Connection reset by network"),
    (e::ECONNABORTED, c"Connection aborted"),
    (e::ENOBUFS, c"No buffer space available"),
    (e::EISCONN, c"Socket is connected"),
    (e::ENOTCONN, c"Socket not connected"),
    (e::ESHUTDOWN, c"Cannot send after socket shutdown"),
    (e::EALREADY, c"Operation already in progress"),
    (e::EINPROGRESS, c"Operation in progress"),
    (e::ESTALE, c"Stale file handle"),
    (e::EREMOTEIO, c"Remote I/O error"),
    (e::EDQUOT, c"Quota exceeded"),
    (e::ENOMEDIUM, c"No medium found"),
    (e::EMEDIUMTYPE, c"Wrong medium type"),
    (e::EMULTIHOP, c"Multihop attempted"),
    (e::ENOKEY, c"Required key not available"),
    (e::EKEYEXPIRED, c"Key has expired"),
    (e::EKEYREVOKED, c"Key has been revoked"),
    (e::EKEYREJECTED, c"Key was rejected by service"),
    // The numbers musl gives no message of their own.
    (e::ECHRNG, c"Channel number out of range"),
    (e::EL2NSYNC, c"Level 2 not synchronized"),
    (e::EL3HLT, c"Level 3 halted"),
    (e::EL3RST, c"Level 3 reset"),
    (e::ELNRNG, c"Link number out of range"),
    (e::EUNATCH, c"Protocol driver not attached"),
    (e::ENOCSI, c"No CSI structure available"),
    (e::EL2HLT, c"Level 2 halted"),
    (e::EBADE, c"Invalid exchange"),
    (e::EBADR, c"Invalid request descriptor"),
    (e::EXFULL, c"Exchange full"),
    (e::ENOANO, c"No anode"),
    (e::EBADRQC, c"Invalid request code"),
    (e::EBADSLT, c"Invalid slot"),
    (e::EBFONT, c"Bad font file format"),
    (e::ENONET, c"Machine not on the network"),
    (e::ENOPKG, c"Package not installed"),
    (e::EREMOTE, c"Object is remote"),
    (e::EADV, c"Advertise error"),
    (e::ESRMNT, c"Srmount error"),
    (e::ECOMM, c"Communication error on send"),
    (e::EDOTDOT, c"RFS specific error"),
    (e::ENOTUNIQ, c"Name not unique on network"),
    (e::EREMCHG, c"Remote address changed"),
    (e::ELIBACC, c"Shared library not accessible"),
    (e::ELIBBAD, c"Shared library corrupted"),
    (e::ELIBSCN, c"Library section corrupted"),
    (e::ELIBMAX, c"Too many shared libraries"),
    (e::ELIBEXEC, c"Shared library cannot be executed"),
    (e::ERESTART, c"System call should be restarted"),
    (e::ESTRPIPE, c"Streams pipe error"),
    (e::EUSERS, c"Too many users"),
    (e::ETOOMANYREFS, c"Too many references"),
    (e::EUCLEAN, c"Structure needs cleaning"),
    (e::ENOTNAM, c"Not a XENIX named type file"),
    (e::ENAVAIL, c"No XENIX semaphores available"),
    (e::EISNAM, c"Is a named type file"),
    (e::ERFKILL, c"Operation not possible due to RF-kill"),
    (e::EHWPOISON, c"Memory page has hardware error"),
];

/// The messages, indexed by error number.
#[allow(
    clippy::indexing_slicing,
    reason = "evaluated at compile time, where an index out of range fails the build"
)]
static ERROR_MESSAGES: [&CStr; ERROR_COUNT] = {
    let mut table = [UNKNOWN_ERROR; ERROR_COUNT];
    let mut i = 0;
    while i < ERROR_TEXT.len() {
        let (number, text) = ERROR_TEXT[i];
        table[number as usize] = text;
        i += 1;
    }
    table
};

/// The message for error number `error`.
fn error_message(error: c_int) -> &'static CStr {
    usize::try_from(error)
        .ok()
        .and_then(|index| ERROR_MESSAGES.get(index))
        .copied()
        .unwrap_or(UNKNOWN_ERROR)
}

/// The message describing error number `error`. A number without one gets
/// "No error information", as in musl.
///
/// The string is static and must not be written.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn strerror(error: c_int) -> *mut c_char {
    error_message(error).as_ptr().cast_mut()
}

/// [`strerror`] in the locale `locale`. There are no message catalogues, so
/// every locale's messages are the C locale's.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn strerror_l(error: c_int, locale: *mut Locale) -> *mut c_char {
    let _ = locale;
    strerror(error)
}

/// Each error number's name, as `errno.h` spells it. `EWOULDBLOCK`,
/// `EDEADLOCK` and `ENOTSUP` are other names for `EAGAIN`, `EDEADLK` and
/// `EOPNOTSUPP`, and are not here: glibc answers those numbers with the
/// first names too.
macro_rules! error_names {
    ($($name:ident),* $(,)?) => {
        [$((e::$name, concat!(stringify!($name), "\0").as_bytes())),*]
    };
}

/// See [`error_names`]: each number, and its name with a NUL after it.
static ERROR_NAMES: [(c_int, &[u8]); 131] = error_names![
    EPERM,
    ENOENT,
    ESRCH,
    EINTR,
    EIO,
    ENXIO,
    E2BIG,
    ENOEXEC,
    EBADF,
    ECHILD,
    EAGAIN,
    ENOMEM,
    EACCES,
    EFAULT,
    ENOTBLK,
    EBUSY,
    EEXIST,
    EXDEV,
    ENODEV,
    ENOTDIR,
    EISDIR,
    EINVAL,
    ENFILE,
    EMFILE,
    ENOTTY,
    ETXTBSY,
    EFBIG,
    ENOSPC,
    ESPIPE,
    EROFS,
    EMLINK,
    EPIPE,
    EDOM,
    ERANGE,
    EDEADLK,
    ENAMETOOLONG,
    ENOLCK,
    ENOSYS,
    ENOTEMPTY,
    ELOOP,
    ENOMSG,
    EIDRM,
    ECHRNG,
    EL2NSYNC,
    EL3HLT,
    EL3RST,
    ELNRNG,
    EUNATCH,
    ENOCSI,
    EL2HLT,
    EBADE,
    EBADR,
    EXFULL,
    ENOANO,
    EBADRQC,
    EBADSLT,
    EBFONT,
    ENOSTR,
    ENODATA,
    ETIME,
    ENOSR,
    ENONET,
    ENOPKG,
    EREMOTE,
    ENOLINK,
    EADV,
    ESRMNT,
    ECOMM,
    EPROTO,
    EMULTIHOP,
    EDOTDOT,
    EBADMSG,
    EOVERFLOW,
    ENOTUNIQ,
    EBADFD,
    EREMCHG,
    ELIBACC,
    ELIBBAD,
    ELIBSCN,
    ELIBMAX,
    ELIBEXEC,
    EILSEQ,
    ERESTART,
    ESTRPIPE,
    EUSERS,
    ENOTSOCK,
    EDESTADDRREQ,
    EMSGSIZE,
    EPROTOTYPE,
    ENOPROTOOPT,
    EPROTONOSUPPORT,
    ESOCKTNOSUPPORT,
    EOPNOTSUPP,
    EPFNOSUPPORT,
    EAFNOSUPPORT,
    EADDRINUSE,
    EADDRNOTAVAIL,
    ENETDOWN,
    ENETUNREACH,
    ENETRESET,
    ECONNABORTED,
    ECONNRESET,
    ENOBUFS,
    EISCONN,
    ENOTCONN,
    ESHUTDOWN,
    ETOOMANYREFS,
    ETIMEDOUT,
    ECONNREFUSED,
    EHOSTDOWN,
    EHOSTUNREACH,
    EALREADY,
    EINPROGRESS,
    ESTALE,
    EUCLEAN,
    ENOTNAM,
    ENAVAIL,
    EISNAM,
    EREMOTEIO,
    EDQUOT,
    ENOMEDIUM,
    EMEDIUMTYPE,
    ECANCELED,
    ENOKEY,
    EKEYEXPIRED,
    EKEYREVOKED,
    EKEYREJECTED,
    EOWNERDEAD,
    ENOTRECOVERABLE,
    ERFKILL,
    EHWPOISON,
];

/// The name of error number `error`, such as `"EINVAL"`, or null for a
/// number that has none: GNU's `strerrorname_np`, which systemd prints.
/// As glibc's, 0 is named `"0"`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn strerrorname_np(error: c_int) -> *const c_char {
    if error == 0 {
        return c"0".as_ptr();
    }
    ERROR_NAMES
        .iter()
        .find(|(number, _)| *number == error)
        .map_or(core::ptr::null(), |(_, name)| name.as_ptr().cast())
}

/// The message for error number `error`, or null for a number that has
/// none: GNU's `strerrordesc_np`, [`strerror`] without its fallback.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn strerrordesc_np(error: c_int) -> *const c_char {
    if strerrorname_np(error).is_null() {
        return core::ptr::null();
    }
    error_message(error).as_ptr()
}

/// Copies the message for `error` into `buf`, which holds `len` bytes: the
/// XSI `strerror_r`, which returns an `int`.
///
/// Returns 0, or `ERANGE` if the message and its NUL do not fit. Then, as in
/// musl, as much as fits is still copied and terminated, unless `len` is 0.
/// It does not set `errno`. An unknown number is not an error here, as in
/// musl; glibc returns `EINVAL` for one.
///
/// glibc's own `strerror_r` symbol is the GNU version, which returns a
/// `char *`. The header this library ships declares the XSI one, so that is
/// what the name exports.
///
/// # Safety
///
/// `buf` must be valid for writing `len` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strerror_r(error: c_int, buf: *mut c_char, len: usize) -> c_int {
    let text = error_message(error).to_bytes();
    let Some(room) = len.checked_sub(1) else {
        return e::ERANGE;
    };
    let mut i = 0;
    while i < room {
        let Some(&byte) = text.get(i) else {
            break;
        };
        // SAFETY: `i < len - 1`, and the caller vouches for `len` bytes.
        unsafe { buf.wrapping_add(i).write(byte as c_char) };
        i += 1;
    }
    // SAFETY: `i <= len - 1`.
    unsafe { buf.wrapping_add(i).write(0) };
    if text.len() > room { e::ERANGE } else { 0 }
}

/// glibc's exported name for the XSI [`strerror_r`], which its header
/// selects when a program asks for POSIX rather than GNU.
///
/// # Safety
///
/// As [`strerror_r`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __xpg_strerror_r(error: c_int, buf: *mut c_char, len: usize) -> c_int {
    // SAFETY: the caller meets `strerror_r`'s contract.
    unsafe { strerror_r(error, buf, len) }
}

/// The description of any signal number without one.
const UNKNOWN_SIGNAL: &CStr = c"Unknown signal";

/// The signals Linux numbers 1 to 31 on x86-64, in order, and their
/// descriptions. AArch64 and 32-bit Arm number them the same; MIPS, SPARC and
/// Alpha do not, and would need their own order. The names are checked
/// against the kernel's header in the unit tests.
const SIGNAL_TEXT: [(&str, &CStr); 31] = [
    ("SIGHUP", c"Hangup"),
    ("SIGINT", c"Interrupt"),
    ("SIGQUIT", c"Quit"),
    ("SIGILL", c"Illegal instruction"),
    ("SIGTRAP", c"Trace/breakpoint trap"),
    ("SIGABRT", c"Aborted"),
    ("SIGBUS", c"Bus error"),
    ("SIGFPE", c"Arithmetic exception"),
    ("SIGKILL", c"Killed"),
    ("SIGUSR1", c"User defined signal 1"),
    ("SIGSEGV", c"Segmentation fault"),
    ("SIGUSR2", c"User defined signal 2"),
    ("SIGPIPE", c"Broken pipe"),
    ("SIGALRM", c"Alarm clock"),
    ("SIGTERM", c"Terminated"),
    ("SIGSTKFLT", c"Stack fault"),
    ("SIGCHLD", c"Child process status"),
    ("SIGCONT", c"Continued"),
    ("SIGSTOP", c"Stopped (signal)"),
    ("SIGTSTP", c"Stopped"),
    ("SIGTTIN", c"Stopped (tty input)"),
    ("SIGTTOU", c"Stopped (tty output)"),
    ("SIGURG", c"Urgent I/O condition"),
    ("SIGXCPU", c"CPU time limit exceeded"),
    ("SIGXFSZ", c"File size limit exceeded"),
    ("SIGVTALRM", c"Virtual timer expired"),
    ("SIGPROF", c"Profiling timer expired"),
    ("SIGWINCH", c"Window changed"),
    ("SIGIO", c"I/O possible"),
    ("SIGPWR", c"Power failure"),
    ("SIGSYS", c"Bad system call"),
];

/// The descriptions of the real-time signals, 32 to 64.
const REALTIME_TEXT: [&CStr; 33] = [
    c"RT32", c"RT33", c"RT34", c"RT35", c"RT36", c"RT37", c"RT38", c"RT39", c"RT40", c"RT41",
    c"RT42", c"RT43", c"RT44", c"RT45", c"RT46", c"RT47", c"RT48", c"RT49", c"RT50", c"RT51",
    c"RT52", c"RT53", c"RT54", c"RT55", c"RT56", c"RT57", c"RT58", c"RT59", c"RT60", c"RT61",
    c"RT62", c"RT63", c"RT64",
];

/// One more than the highest signal number, `_NSIG` on x86-64.
const SIGNAL_COUNT: usize = 1 + SIGNAL_TEXT.len() + REALTIME_TEXT.len();

/// The descriptions, indexed by signal number.
#[allow(
    clippy::indexing_slicing,
    reason = "evaluated at compile time, where an index out of range fails the build"
)]
static SIGNAL_MESSAGES: [&CStr; SIGNAL_COUNT] = {
    let mut table = [UNKNOWN_SIGNAL; SIGNAL_COUNT];
    let mut i = 0;
    while i < SIGNAL_TEXT.len() {
        table[1 + i] = SIGNAL_TEXT[i].1;
        i += 1;
    }
    let mut i = 0;
    while i < REALTIME_TEXT.len() {
        table[1 + SIGNAL_TEXT.len() + i] = REALTIME_TEXT[i];
        i += 1;
    }
    table
};

/// The description of signal `signal`, or "Unknown signal".
///
/// The string is static and must not be written.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn strsignal(signal: c_int) -> *mut c_char {
    let text = match usize::try_from(signal) {
        Ok(0) | Err(_) => UNKNOWN_SIGNAL,
        Ok(index) => SIGNAL_MESSAGES
            .get(index)
            .copied()
            .unwrap_or(UNKNOWN_SIGNAL),
    };
    text.as_ptr().cast_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The string a returned pointer points at.
    fn text(p: *const c_char) -> &'static str {
        // SAFETY: every pointer tested is to one of this module's static
        // strings.
        unsafe { CStr::from_ptr(p) }
            .to_str()
            .unwrap_or("<not UTF-8>")
    }

    /// Every `(name, number)` the given header `#define`s as a plain number.
    fn defines(header: &str) -> Vec<(String, i64)> {
        header
            .lines()
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                if words.next() != Some("#define") {
                    return None;
                }
                let name = words.next()?;
                let value = words.next()?.parse().ok()?;
                Some((name.to_owned(), value))
            })
            .collect()
    }

    #[test]
    fn every_error_number_the_kernel_defines_has_its_own_message() {
        // The generated constants, read back from their source.
        let generated = include_str!("generated/errno.rs");
        let mut seen = 0;
        for line in generated.lines() {
            let Some(rest) = line.strip_prefix("pub const ") else {
                continue;
            };
            let Some((name, value)) = rest.split_once(": c_int = ") else {
                continue;
            };
            let Ok(number) = value.trim_end_matches(';').parse::<c_int>() else {
                // An alias of another name, which has its own line.
                continue;
            };
            assert!(
                usize::try_from(number).is_ok_and(|n| n < ERROR_COUNT),
                "{name} is beyond the table"
            );
            assert_ne!(text(strerror(number)), "No error information", "{name}");
            seen += 1;
        }
        assert!(seen > 130, "only {seen} error numbers were found");
    }

    #[test]
    fn no_two_entries_claim_one_number() {
        for (i, (number, _)) in ERROR_TEXT.iter().enumerate() {
            for (other, _) in ERROR_TEXT.iter().skip(i + 1) {
                assert_ne!(number, other);
            }
        }
    }

    #[test]
    fn unknown_error_numbers_get_musls_text() {
        for error in [0, -1, c_int::MIN, 134, 4095, c_int::MAX] {
            assert_eq!(text(strerror(error)), "No error information", "{error}");
        }
        assert_eq!(text(strerror(e::ENOENT)), "No such file or directory");
        assert_eq!(text(strerror(e::EWOULDBLOCK)), text(strerror(e::EAGAIN)));
    }

    #[test]
    fn strerror_r_copies_whole_messages_and_reports_short_buffers() {
        let mut buf = [b'x' as c_char; 32];
        let p = buf.as_mut_ptr();
        // SAFETY: `buf` holds 32 bytes.
        assert_eq!(unsafe { strerror_r(e::EDOM, p, 32) }, 0);
        assert_eq!(text(p), "Domain error");

        // "Domain error" is 12 bytes and needs 13 with its NUL.

        // SAFETY: as above.
        assert_eq!(unsafe { strerror_r(e::EDOM, p, 13) }, 0);
        assert_eq!(text(p), "Domain error");
        // SAFETY: as above.
        assert_eq!(unsafe { strerror_r(e::EDOM, p, 12) }, e::ERANGE);
        assert_eq!(text(p), "Domain erro");
        // SAFETY: as above.
        assert_eq!(unsafe { __xpg_strerror_r(e::EDOM, p, 1) }, e::ERANGE);
        assert_eq!(text(p), "");

        buf = [b'x' as c_char; 32];
        // Nothing is written through a zero length, not even a NUL.
        // SAFETY: a zero length writes nothing.
        assert_eq!(unsafe { strerror_r(e::EDOM, p, 0) }, e::ERANGE);
        assert_eq!(buf[0], b'x' as c_char);
    }

    #[test]
    fn signal_descriptions_follow_the_kernels_numbering() {
        let header = std::fs::read_to_string("/usr/include/x86_64-linux-gnu/asm/signal.h")
            .unwrap_or_else(|error| panic!("read the kernel's signal.h: {error}"));
        let numbers = defines(&header);
        for (index, (name, description)) in SIGNAL_TEXT.iter().enumerate() {
            let number = numbers
                .iter()
                .find(|(defined, _)| defined == name)
                .map(|(_, number)| *number);
            assert_eq!(number, Some(index as i64 + 1), "{name}");
            let signal = index as c_int + 1;
            assert_eq!(Some(text(strsignal(signal))), description.to_str().ok());
        }
        let first_realtime = numbers
            .iter()
            .find(|(defined, _)| defined == "SIGRTMIN")
            .map(|(_, number)| *number);
        assert_eq!(first_realtime, Some(32));
        assert_eq!(text(strsignal(32)), "RT32");
        assert_eq!(text(strsignal(64)), "RT64");
        for signal in [0, -1, 65, 128, c_int::MIN, c_int::MAX] {
            assert_eq!(text(strsignal(signal)), "Unknown signal", "{signal}");
        }
    }

    #[test]
    fn strerrorname_np_names_what_the_host_glibc_names() {
        unsafe extern "C" {
            #[link_name = "strerrorname_np"]
            safe fn host_strerrorname_np(error: c_int) -> *const c_char;
        }
        for error in -2..200 {
            let host = host_strerrorname_np(error);
            let ours = strerrorname_np(error);
            if host.is_null() {
                assert!(ours.is_null(), "{error}");
                assert!(strerrordesc_np(error).is_null(), "{error}");
            } else {
                assert!(!ours.is_null(), "{error}");
                // SAFETY: glibc's names are static C strings.
                let host = unsafe { CStr::from_ptr(host) };
                // SAFETY: as are this library's.
                let ours = unsafe { CStr::from_ptr(ours) };
                assert_eq!(ours, host, "{error}");
                assert!(!strerrordesc_np(error).is_null(), "{error}");
            }
        }
    }
}
