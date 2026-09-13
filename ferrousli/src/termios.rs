//! `termios.h`, and `unistd.h`'s terminal calls: `tcgetpgrp`, `tcsetpgrp`,
//! `ttyname` and `ttyname_r`.
//!
//! Each follows musl. The attribute calls are the kernel's `TCGETS` family of
//! requests, which read and write the leading 36 bytes of C's 60-byte `struct
//! termios`: the kernel's structure has 19 control characters and no speed
//! fields. The speeds are `c_cflag`'s `CBAUD` bits, so the `cf*speed` functions
//! change only those. `tcgetwinsize` and `tcsetwinsize` are POSIX.1-2024's.
//! `ttyname_r` reads the terminal's name from the link `/proc/self/fd/<fd>`,
//! then checks that the name still leads to the same file, as musl does.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int, c_uint, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;

use crate::errno;
use crate::stat::{Stat, fstat, stat};
use crate::syscall::{self, nr};
use crate::unistd::{isatty, readlink};

/// `TCGETS`, from `include/bits/ioctl.h`. `TCSETSW` and `TCSETSF` follow
/// `TCSETS` in the order of `TCSANOW`, `TCSADRAIN` and `TCSAFLUSH`.
const TCGETS: usize = 0x5401;
/// `TCSETS`, from `include/bits/ioctl.h`.
const TCSETS: usize = 0x5402;
/// `TCSBRK`, from `include/bits/ioctl.h`.
const TCSBRK: usize = 0x5409;
/// `TCXONC`, from `include/bits/ioctl.h`.
const TCXONC: usize = 0x540a;
/// `TCFLSH`, from `include/bits/ioctl.h`.
const TCFLSH: usize = 0x540b;
/// `TIOCGPGRP`, from `include/bits/ioctl.h`.
const TIOCGPGRP: usize = 0x540f;
/// `TIOCSPGRP`, from `include/bits/ioctl.h`.
const TIOCSPGRP: usize = 0x5410;
/// `TIOCGWINSZ`, from `include/bits/ioctl.h`.
const TIOCGWINSZ: usize = 0x5413;
/// `TIOCSWINSZ`, from `include/bits/ioctl.h`.
const TIOCSWINSZ: usize = 0x5414;
/// `TIOCGSID`, from `include/bits/ioctl.h`.
const TIOCGSID: usize = 0x5429;

/// `CBAUD`, from `include/bits/termios.h`: the speed bits of `c_cflag`.
const CBAUD: c_uint = 0o010_017;

/// The input modes `cfmakeraw` clears, from `include/bits/termios.h`:
/// `IGNBRK`, `BRKINT`, `PARMRK`, `ISTRIP`, `INLCR`, `IGNCR`, `ICRNL` and
/// `IXON`.
const RAW_IFLAG: c_uint = 0o1 | 0o2 | 0o10 | 0o40 | 0o100 | 0o200 | 0o400 | 0o2000;
/// `OPOST`, from `include/bits/termios.h`.
const OPOST: c_uint = 0o1;
/// The local modes `cfmakeraw` clears, from `include/bits/termios.h`: `ISIG`,
/// `ICANON`, `ECHO`, `ECHONL` and `IEXTEN`.
const RAW_LFLAG: c_uint = 0o1 | 0o2 | 0o10 | 0o100 | 0o100_000;
/// `CSIZE`, from `include/bits/termios.h`. `CS8` is the same bits.
const CSIZE: c_uint = 0o60;
/// `PARENB`, from `include/bits/termios.h`.
const PARENB: c_uint = 0o400;
/// `VTIME`, from `include/bits/termios.h`.
const VTIME: usize = 5;
/// `VMIN`, from `include/bits/termios.h`.
const VMIN: usize = 6;

/// `TTY_NAME_MAX`, from `include/limits.h`: the size of `ttyname`'s buffer.
const TTY_NAME_MAX: usize = 32;

/// C's `struct termios`, from `include/bits/termios.h`. glibc's is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Termios {
    /// The input modes.
    pub c_iflag: c_uint,
    /// The output modes.
    pub c_oflag: c_uint,
    /// The control modes, with the speed in the `CBAUD` bits.
    pub c_cflag: c_uint,
    /// The local modes.
    pub c_lflag: c_uint,
    /// The line discipline.
    pub c_line: u8,
    /// The control characters, indexed by `VINTR` and the rest.
    pub c_cc: [u8; 32],
    /// The input speed, which Linux keeps in `c_cflag` instead.
    pub __c_ispeed: c_uint,
    /// The output speed, which Linux keeps in `c_cflag` instead.
    pub __c_ospeed: c_uint,
}

const _: () = assert!(size_of::<Termios>() == 60);
const _: () = assert!(offset_of!(Termios, c_cc) == 17);
const _: () = assert!(offset_of!(Termios, __c_ispeed) == 52);

/// Makes terminal request `request` on `fd` with `arg`.
fn ioctl(fd: c_int, request: usize, arg: usize) -> c_int {
    // SAFETY: each caller passes an `arg` its request may read or write.
    let ret = unsafe { syscall::syscall3(nr::IOCTL, fd as usize, request, arg) };
    errno::from_syscall(ret) as c_int
}

/// The calling thread's `errno`.
fn last_errno() -> c_int {
    // SAFETY: the pointer is this thread's errno.
    unsafe { errno::__errno_location().read() }
}

/// Stores the attributes of terminal `fd` in `*tio`.
///
/// # Safety
///
/// `tio` must be valid for a write of a `struct termios`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn tcgetattr(fd: c_int, tio: *mut Termios) -> c_int {
    ioctl(fd, TCGETS, tio.addr())
}

/// Sets the attributes of terminal `fd` to `*tio`: now for `TCSANOW`, once
/// queued output is written for `TCSADRAIN`, and then discarding unread input
/// too for `TCSAFLUSH`. Any other `act` fails with `EINVAL`.
///
/// # Safety
///
/// `tio` must be valid for a read of a `struct termios`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn tcsetattr(fd: c_int, act: c_int, tio: *const Termios) -> c_int {
    let Ok(act @ 0..=2) = usize::try_from(act) else {
        errno::set(errno::EINVAL);
        return -1;
    };
    ioctl(fd, TCSETS + act, tio.addr())
}

/// The output speed in `*tio`, a `B*` constant.
///
/// # Safety
///
/// `tio` must be valid for a read of a `struct termios`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn cfgetospeed(tio: *const Termios) -> c_uint {
    // SAFETY: the caller passes a valid `struct termios`.
    let flags = unsafe { (*tio).c_cflag };
    flags & CBAUD
}

/// The input speed in `*tio`, which on Linux is the output speed.
///
/// # Safety
///
/// As `cfgetospeed`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn cfgetispeed(tio: *const Termios) -> c_uint {
    // SAFETY: the caller's promises are `cfgetospeed`'s.
    unsafe { cfgetospeed(tio) }
}

/// Sets the speed in `*tio` to `speed`, a `B*` constant. Any other value
/// fails with `EINVAL`.
///
/// # Safety
///
/// `tio` must be valid for a read and a write of a `struct termios`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn cfsetospeed(tio: *mut Termios, speed: c_uint) -> c_int {
    if speed & !CBAUD != 0 {
        errno::set(errno::EINVAL);
        return -1;
    }
    // SAFETY: the caller passes a valid `struct termios`.
    let flags = unsafe { &mut (*tio).c_cflag };
    *flags = (*flags & !CBAUD) | speed;
    0
}

/// Sets the speed in `*tio`, as `cfsetospeed` does, unless `speed` is 0,
/// which POSIX says leaves the input speed matching the output speed.
///
/// # Safety
///
/// As `cfsetospeed`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn cfsetispeed(tio: *mut Termios, speed: c_uint) -> c_int {
    if speed == 0 {
        return 0;
    }
    // SAFETY: the caller's promises are `cfsetospeed`'s.
    unsafe { cfsetospeed(tio, speed) }
}

/// `cfsetospeed`, under its BSD name.
///
/// # Safety
///
/// As `cfsetospeed`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn cfsetspeed(tio: *mut Termios, speed: c_uint) -> c_int {
    // SAFETY: the caller's promises are `cfsetospeed`'s.
    unsafe { cfsetospeed(tio, speed) }
}

/// Changes `*tio` to raw mode: no input or output processing, no echo, no
/// signals from control characters, eight bits without parity, and each read
/// returning as soon as one byte is there.
///
/// # Safety
///
/// `tio` must be valid for a read and a write of a `struct termios`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn cfmakeraw(tio: *mut Termios) {
    // SAFETY: the caller passes a valid `struct termios`, which nothing else
    // refers to while this runs.
    let tio = unsafe { &mut *tio };
    tio.c_iflag &= !RAW_IFLAG;
    tio.c_oflag &= !OPOST;
    tio.c_lflag &= !RAW_LFLAG;
    tio.c_cflag = (tio.c_cflag & !(CSIZE | PARENB)) | CSIZE;
    if let Some(min) = tio.c_cc.get_mut(VMIN) {
        *min = 1;
    }
    if let Some(time) = tio.c_cc.get_mut(VTIME) {
        *time = 0;
    }
}

/// Waits until the output queued on terminal `fd` has been written.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tcdrain(fd: c_int) -> c_int {
    ioctl(fd, TCSBRK, 1)
}

/// Suspends or restarts output or input on terminal `fd`, as `action` says.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tcflow(fd: c_int, action: c_int) -> c_int {
    ioctl(fd, TCXONC, action as usize)
}

/// Discards the unread input, the unwritten output or both of terminal `fd`,
/// as `queue` says.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tcflush(fd: c_int, queue: c_int) -> c_int {
    ioctl(fd, TCFLSH, queue as usize)
}

/// Sends a break on terminal `fd`. A nonzero duration's meaning is left to
/// the implementation, and like musl this ignores it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tcsendbreak(fd: c_int, _duration: c_int) -> c_int {
    ioctl(fd, TCSBRK, 0)
}

/// The id of the session terminal `fd` controls.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tcgetsid(fd: c_int) -> c_int {
    let mut sid: c_int = 0;
    if ioctl(fd, TIOCGSID, (&raw mut sid).addr()) < 0 {
        return -1;
    }
    sid
}

/// The id of terminal `fd`'s foreground process group.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tcgetpgrp(fd: c_int) -> c_int {
    let mut pgrp: c_int = 0;
    if ioctl(fd, TIOCGPGRP, (&raw mut pgrp).addr()) < 0 {
        return -1;
    }
    pgrp
}

/// Makes process group `pgrp` terminal `fd`'s foreground group.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tcsetpgrp(fd: c_int, pgrp: c_int) -> c_int {
    ioctl(fd, TIOCSPGRP, (&raw const pgrp).addr())
}

/// Stores terminal `fd`'s window size in `*size`.
///
/// # Safety
///
/// `size` must be valid for a write of a `struct winsize`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn tcgetwinsize(fd: c_int, size: *mut c_void) -> c_int {
    ioctl(fd, TIOCGWINSZ, size.addr())
}

/// Sets terminal `fd`'s window size to `*size`.
///
/// # Safety
///
/// `size` must be valid for a read of a `struct winsize`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn tcsetwinsize(fd: c_int, size: *const c_void) -> c_int {
    ioctl(fd, TIOCSWINSZ, size.addr())
}

/// Writes `/proc/self/fd/<fd>` and a NUL into `out`, which is zeroed.
fn fd_link(out: &mut [u8; 32], fd: c_uint) {
    const PREFIX: &[u8] = b"/proc/self/fd/";
    let mut digits = [0u8; 10];
    let mut count = 0;
    let mut value = fd;
    loop {
        if let Some(slot) = digits.get_mut(count) {
            *slot = b'0' + (value % 10) as u8;
        }
        count += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let reversed = digits.iter().take(count).rev();
    for (slot, &byte) in out.iter_mut().zip(PREFIX.iter().chain(reversed)) {
        *slot = byte;
    }
}

/// Stores the name of terminal `fd` in the `size` bytes at `name`, and
/// returns 0, or the error: `ENOTTY` if `fd` is not a terminal, `ERANGE` if
/// the name and its NUL do not fit, and `ENODEV` if the name no longer leads
/// to the terminal. `errno` is not the result.
///
/// # Safety
///
/// `name` must be valid for writes of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ttyname_r(fd: c_int, name: *mut c_char, size: usize) -> c_int {
    if isatty(fd) == 0 {
        return last_errno();
    }
    let mut link = [0u8; 32];
    fd_link(&mut link, fd.cast_unsigned());
    // SAFETY: `link` is NUL-terminated, and the caller passes `size` writable
    // bytes.
    let got = unsafe { readlink(link.as_ptr().cast(), name, size) };
    let Ok(got) = usize::try_from(got) else {
        return last_errno();
    };
    if got == size {
        return errno::ERANGE;
    }
    // SAFETY: `got` is below `size`.
    unsafe { name.wrapping_add(got).write(0) };
    let mut by_name = Stat::default();
    let mut by_fd = Stat::default();
    // SAFETY: `name` is NUL-terminated now, and `by_name` a live local.
    if unsafe { stat(name, &raw mut by_name) } != 0 {
        return last_errno();
    }
    // SAFETY: `by_fd` is a live local.
    if unsafe { fstat(fd, &raw mut by_fd) } != 0 {
        return last_errno();
    }
    if (by_name.st_dev, by_name.st_ino) != (by_fd.st_dev, by_fd.st_ino) {
        return errno::ENODEV;
    }
    0
}

/// The buffer `ttyname` returns.
#[derive(Debug)]
struct NameBuffer(UnsafeCell<[u8; TTY_NAME_MAX]>);

// SAFETY: C documents `ttyname`'s result as static storage that the next call
// overwrites, and the function as unsafe to call from two threads at once, as
// musl's is. Only `ttyname` writes it.
unsafe impl Sync for NameBuffer {}

/// What `ttyname` returns.
static NAME: NameBuffer = NameBuffer(UnsafeCell::new([0; TTY_NAME_MAX]));

/// The name of terminal `fd`, in static storage the next call overwrites, or
/// null with `errno` set.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ttyname(fd: c_int) -> *mut c_char {
    let buffer = NAME.0.get().cast::<c_char>();
    // SAFETY: see `NameBuffer`; the buffer holds `TTY_NAME_MAX` bytes.
    let result = unsafe { ttyname_r(fd, buffer, TTY_NAME_MAX) };
    if result != 0 {
        errno::set(result);
        return null_mut();
    }
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_speed_is_the_cbaud_bits_of_the_control_modes() {
        // B9600 and B115200, from include/bits/termios.h.
        let mut tio = Termios {
            c_cflag: 0o7_700_000,
            ..Termios::default()
        };
        // SAFETY: `tio` is a live local.
        assert_eq!(unsafe { cfsetospeed(&raw mut tio, 0o15) }, 0);
        // SAFETY: as above.
        assert_eq!(unsafe { cfgetispeed(&raw const tio) }, 0o15);
        assert_eq!(tio.c_cflag, 0o7_700_015);
        // SAFETY: as above.
        assert_eq!(unsafe { cfsetispeed(&raw mut tio, 0) }, 0);
        // SAFETY: as above.
        assert_eq!(unsafe { cfsetspeed(&raw mut tio, 0o010_002) }, 0);
        assert_eq!(tio.c_cflag, 0o7_710_002);
        // SAFETY: as above.
        assert_eq!(unsafe { cfsetospeed(&raw mut tio, 0o20) }, -1);
        assert_eq!(last_errno(), errno::EINVAL);
    }

    #[test]
    fn raw_mode_clears_processing_and_sets_eight_bits() {
        let mut tio = Termios {
            c_iflag: c_uint::MAX,
            c_oflag: c_uint::MAX,
            c_cflag: c_uint::MAX,
            c_lflag: c_uint::MAX,
            c_cc: [0xff; 32],
            ..Termios::default()
        };
        // SAFETY: `tio` is a live local.
        unsafe { cfmakeraw(&raw mut tio) };
        assert_eq!(tio.c_iflag, !RAW_IFLAG);
        assert_eq!(tio.c_oflag, !OPOST);
        assert_eq!(tio.c_lflag, !RAW_LFLAG);
        assert_eq!(tio.c_cflag & (CSIZE | PARENB), CSIZE);
        assert_eq!(
            (tio.c_cc.get(VMIN), tio.c_cc.get(VTIME)),
            (Some(&1), Some(&0))
        );
    }

    #[test]
    fn the_link_names_the_descriptor() {
        let mut link = [0u8; 32];
        fd_link(&mut link, 4_294_967_295);
        assert_eq!(link.get(..25), Some(&b"/proc/self/fd/4294967295\0"[..]));
        let mut link = [0u8; 32];
        fd_link(&mut link, 0);
        assert_eq!(link.get(..16), Some(&b"/proc/self/fd/0\0"[..]));
    }

    #[test]
    fn a_file_that_is_not_a_terminal_is_refused() {
        // SAFETY: the path is NUL-terminated. O_RDONLY | O_CLOEXEC.
        let fd =
            unsafe { syscall::syscall3(nr::OPEN, c"/dev/null".as_ptr().addr(), 0o2_000_000, 0) }
                as c_int;
        assert!(fd >= 0);
        let mut name = [0 as c_char; 64];
        // SAFETY: the buffer is a live local of the size given.
        let result = unsafe { ttyname_r(fd, name.as_mut_ptr(), name.len()) };
        assert_eq!(result, errno::ENOTTY);
        assert!(ttyname(fd).is_null());
        assert_eq!(last_errno(), errno::ENOTTY);
        let mut tio = Termios::default();
        // SAFETY: `tio` is a live local.
        assert_eq!(unsafe { tcsetattr(fd, 3, &raw const tio) }, -1);
        assert_eq!(last_errno(), errno::EINVAL);
        // SAFETY: as above.
        assert_eq!(unsafe { tcgetattr(fd, &raw mut tio) }, -1);
        assert_eq!(last_errno(), errno::ENOTTY);
        assert_eq!(tcgetpgrp(fd), -1);
        // SAFETY: `close` reads no memory.
        let _ = unsafe { syscall::syscall2(nr::CLOSE, fd as usize, 0) };
    }
}
