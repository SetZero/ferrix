//! A 32-bit program's system call arguments, rewritten into the layout the
//! handlers read (`docs/I386.md` §3.2, I3).
//!
//! The entry zero-extends each of an i386 call's six registers, which is
//! right for every `int`, `unsigned` and pointer argument: the handlers
//! narrow an `int` themselves. Two kinds of argument are not right that way,
//! and both are the register layout rather than a structure in memory:
//!
//! * **A 64-bit value** (`loff_t`, a `u64`) arrives in two registers, low
//!   word first, with no alignment to an even register -- Linux's
//!   `compat_arg_u64` on x86 and its `ia32_*` wrappers in
//!   `arch/x86/kernel/sys_ia32.c`, unlike ARMv7-A's EABI. It is joined into
//!   one argument and the ones after it move up, so the handler sees the
//!   64-bit prototype.
//! * **A signed `long`** (`off_t`) is 32 bits and has to be sign-extended, or
//!   `lseek(fd, -1, SEEK_CUR)` would seek four gigabytes forward.
//!
//! After this, `linux::wide` and `linux::native_signed` read the arguments as
//! they would a 64-bit program's. A call that passes a structure in memory
//! instead asks its handler to read it at the call's word width, and
//! [`ioctl_passes`] keeps back the `ioctl` requests no handler reads that way
//! yet.

use ferrix_linux_abi::nr::Syscall;
use ferrix_linux_abi::types::{
    FIOCLEX, FIONBIO, FIONCLEX, FIONREAD, TCFLSH, TCGETS, TCGETS2, TCSETS, TCSETS2, TCSETSF,
    TCSETSF2, TCSETSW, TCSETSW2, TCXONC, TIOCGPGRP, TIOCGPTN, TIOCGSID, TIOCGWINSZ, TIOCNOTTY,
    TIOCOUTQ, TIOCSCTTY, TIOCSPGRP, TIOCSPTLCK, TIOCSWINSZ,
};

/// `call`'s arguments as a 64-bit program would have passed them.
///
/// Only the calls i386's table maps reach here, and only those whose
/// registers need rewriting are rewritten; everything else is returned as it
/// came.
pub(crate) fn normalize(call: Syscall, a: [u64; 6]) -> [u64; 6] {
    let pair = |low: u64, high: u64| (high & 0xFFFF_FFFF) << 32 | (low & 0xFFFF_FFFF);
    let signed = |value: u64| value as u32 as i32 as i64 as u64;
    let [a0, a1, a2, a3, a4, a5] = a;
    match call {
        // `pread64(fd, buf, count, pos)`: the position is registers 3 and 4.
        Syscall::Pread64 | Syscall::Pwrite64 => [a0, a1, a2, pair(a3, a4), 0, 0],
        // `ftruncate64(fd, length)`, `truncate64(path, length)`.
        Syscall::Ftruncate64 | Syscall::Truncate64 => [a0, pair(a1, a2), 0, 0, 0, 0],
        // `fallocate(fd, mode, offset, len)`, both 64-bit.
        Syscall::Fallocate => [a0, a1, pair(a2, a3), pair(a4, a5), 0, 0],
        // `readahead(fd, offset, count)`.
        Syscall::Readahead => [a0, pair(a1, a2), a3, 0, 0, 0],
        // `lseek(fd, offset, whence)`, `ftruncate(fd, length)` and
        // `truncate(path, length)` take a 32-bit `off_t`.
        Syscall::Lseek => [a0, signed(a1), a2, a3, a4, a5],
        Syscall::Ftruncate | Syscall::Truncate => [a0, signed(a1), a2, a3, a4, a5],
        _ => a,
    }
}

/// Whether an i386 program's `ioctl` `request` may reach the handlers, which
/// read their argument as a 64-bit program lays it out.
///
/// Linux answers a 32-bit program's `ioctl` through `compat_ioctl`, which
/// either passes a request through -- its argument is the same at both widths
/// -- or translates it, and refuses anything it knows neither way with
/// `ENOTTY`. These are the requests passed through: the terminal's, whose
/// `termios`, `termios2` and `winsize` hold only 32-bit and narrower fields,
/// and the ones whose argument is an `int` or nothing. Everything else is
/// refused until it is translated, rather than read at the wrong width.
pub(crate) fn ioctl_passes(request: u32) -> bool {
    matches!(
        request,
        TCGETS
            | TCSETS
            | TCSETSW
            | TCSETSF
            | TCGETS2
            | TCSETS2
            | TCSETSW2
            | TCSETSF2
            | TCXONC
            | TCFLSH
            | TIOCSCTTY
            | TIOCNOTTY
            | TIOCGPGRP
            | TIOCSPGRP
            | TIOCGSID
            | TIOCOUTQ
            | TIOCGWINSZ
            | TIOCSWINSZ
            | TIOCGPTN
            | TIOCSPTLCK
            | FIONREAD
            | FIONBIO
            | FIOCLEX
            | FIONCLEX
    )
}
