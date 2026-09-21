//! `sys/socket.h`: sockets.
//!
//! Each function is its system call, as musl's are on x86-64, with three
//! things musl does that are either left out or need no code here:
//!
//! * musl's `socket` and `socketpair` retry without `SOCK_CLOEXEC` and
//!   `SOCK_NONBLOCK` for kernels before 2.6.27, and set the flags with `fcntl`
//!   instead. No such retry is made here.
//! * Where `time_t` outgrew the kernel's old socket options, musl rewrites
//!   `SO_RCVTIMEO`, `SO_SNDTIMEO`, the timestamp options and their control
//!   messages. On x86-64 the old and new numbers are the same, so
//!   `getsockopt`, `setsockopt` and `recvmsg` pass everything through.
//! * C's `struct msghdr` and `struct cmsghdr` have `int`-sized lengths
//!   followed by padding, where the kernel has `size_t` lengths. `sendmsg` and
//!   `recvmsg` give the kernel a copy with the padding zeroed, as musl does, so
//!   a program's stray bytes are never read as the high half of a length.
//!
//! `send` and `recv` are `sendto` and `recvfrom` without an address. None of
//! these is a cancellation point yet.

use core::ffi::{c_int, c_uint, c_ulong, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;

use crate::errno;
use crate::syscall::{self, nr};

/// C's `struct msghdr`, from `include/sys/socket.h` on a little-endian 64-bit
/// target. glibc's has `size_t` lengths in the same places, which on such a
/// target are the same bytes as long as the padding is zero.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Msghdr {
    /// The address to send to or received from, or null.
    pub msg_name: *mut c_void,
    /// The size of `msg_name`.
    pub msg_namelen: c_uint,
    /// The buffers, an array of `struct iovec`.
    pub msg_iov: *mut c_void,
    /// How many buffers there are.
    pub msg_iovlen: c_int,
    /// Padding, which the kernel reads as the high half of `msg_iovlen`.
    pub __pad1: c_int,
    /// The control messages, or null.
    pub msg_control: *mut c_void,
    /// The size of `msg_control`.
    pub msg_controllen: c_uint,
    /// Padding, which the kernel reads as the high half of `msg_controllen`.
    pub __pad2: c_int,
    /// The flags `recvmsg` reports.
    pub msg_flags: c_int,
}

const _: () = assert!(size_of::<Msghdr>() == 56);
const _: () = assert!(offset_of!(Msghdr, msg_iov) == 16);
const _: () = assert!(offset_of!(Msghdr, __pad1) == 28);
const _: () = assert!(offset_of!(Msghdr, msg_control) == 32);
const _: () = assert!(offset_of!(Msghdr, __pad2) == 44);
const _: () = assert!(offset_of!(Msghdr, msg_flags) == 48);

/// The size of C's `struct cmsghdr`: `cmsg_len`, its padding, `cmsg_level`
/// and `cmsg_type`, four bytes each.
const CMSG_HEADER: usize = 16;

/// The most control data `sendmsg` copies: musl's buffer of 66 headers' worth
/// of bytes, room for an `SCM_RIGHTS` message carrying 255 descriptors.
const CONTROL_MAX: usize = 66 * CMSG_HEADER;

/// Zeroes the padding after each control message's `cmsg_len` in `control`,
/// visiting the messages as `CMSG_FIRSTHDR` and `CMSG_NXTHDR` do.
fn zero_control_padding(control: &mut [u8]) {
    let end = control.len();
    let mut at = 0;
    if end < CMSG_HEADER {
        return;
    }
    loop {
        let Some(len) = control
            .get(at..at + 4)
            .and_then(|len| <[u8; 4]>::try_from(len).ok())
            .map(u32::from_ne_bytes)
        else {
            return;
        };
        if let Some(padding) = control.get_mut(at + 4..at + 8) {
            for byte in padding {
                *byte = 0;
            }
        }
        let len = len as usize;
        let aligned = (len + 7) & !7;
        if len < CMSG_HEADER || aligned + CMSG_HEADER >= end - at {
            return;
        }
        at += aligned;
    }
}

/// Makes the socket system call `number` with up to six arguments.
fn call(number: usize, args: [usize; 6]) -> isize {
    let [a0, a1, a2, a3, a4, a5] = args;
    // SAFETY: each caller vouches for the memory its arguments point to.
    let ret = unsafe { syscall::syscall6(number, a0, a1, a2, a3, a4, a5) };
    errno::from_syscall(ret)
}

/// [`call`], as a cancellation point.
fn call_cp(number: usize, args: [usize; 6]) -> isize {
    let [a0, a1, a2, a3, a4, a5] = args;
    // SAFETY: each caller vouches for the memory its arguments point to.
    let ret = unsafe { crate::cancel::syscall_cp(number, a0, a1, a2, a3, a4, a5) };
    errno::from_syscall(ret)
}

/// Creates a socket of `domain`, `type` and `protocol`, and returns its
/// descriptor. `type` may carry `SOCK_CLOEXEC` and `SOCK_NONBLOCK`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn socket(domain: c_int, r#type: c_int, protocol: c_int) -> c_int {
    call(
        nr::SOCKET,
        [domain as usize, r#type as usize, protocol as usize, 0, 0, 0],
    ) as c_int
}

/// Creates a pair of connected sockets, and stores their descriptors in
/// `fds`.
///
/// # Safety
///
/// `fds` must be valid for writes of two `int`s.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn socketpair(
    domain: c_int,
    r#type: c_int,
    protocol: c_int,
    fds: *mut c_int,
) -> c_int {
    call(
        nr::SOCKETPAIR,
        [
            domain as usize,
            r#type as usize,
            protocol as usize,
            fds.addr(),
            0,
            0,
        ],
    ) as c_int
}

/// Gives socket `fd` the address `addr`, `len` bytes long.
///
/// # Safety
///
/// `addr` must be valid for reads of `len` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn bind(fd: c_int, addr: *const c_void, len: c_uint) -> c_int {
    call(nr::BIND, [fd as usize, addr.addr(), len as usize, 0, 0, 0]) as c_int
}

/// Connects socket `fd` to the address `addr`, `len` bytes long.
///
/// # Safety
///
/// `addr` must be valid for reads of `len` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn connect(fd: c_int, addr: *const c_void, len: c_uint) -> c_int {
    call_cp(
        nr::CONNECT,
        [fd as usize, addr.addr(), len as usize, 0, 0, 0],
    ) as c_int
}

/// Makes socket `fd` accept connections, with up to `backlog` waiting.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn listen(fd: c_int, backlog: c_int) -> c_int {
    call(nr::LISTEN, [fd as usize, backlog as usize, 0, 0, 0, 0]) as c_int
}

/// Accepts a connection on socket `fd`, returning the new socket's
/// descriptor, with the peer's address stored in `addr` and its length in
/// `*len` if `addr` is not null.
///
/// # Safety
///
/// `addr` must be null, or valid for writes of `*len` bytes with `len` valid
/// for a read and a write.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn accept(fd: c_int, addr: *mut c_void, len: *mut c_uint) -> c_int {
    call_cp(nr::ACCEPT, [fd as usize, addr.addr(), len.addr(), 0, 0, 0]) as c_int
}

/// `accept`, with `SOCK_CLOEXEC` and `SOCK_NONBLOCK` in `flags` set on the
/// new socket.
///
/// # Safety
///
/// As `accept`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn accept4(
    fd: c_int,
    addr: *mut c_void,
    len: *mut c_uint,
    flags: c_int,
) -> c_int {
    call_cp(
        nr::ACCEPT4,
        [fd as usize, addr.addr(), len.addr(), flags as usize, 0, 0],
    ) as c_int
}

/// Stores socket `fd`'s own address in `addr` and its length in `*len`.
///
/// # Safety
///
/// `addr` must be valid for writes of `*len` bytes, and `len` for a read and
/// a write.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getsockname(fd: c_int, addr: *mut c_void, len: *mut c_uint) -> c_int {
    call(
        nr::GETSOCKNAME,
        [fd as usize, addr.addr(), len.addr(), 0, 0, 0],
    ) as c_int
}

/// Stores the address of socket `fd`'s peer in `addr` and its length in
/// `*len`.
///
/// # Safety
///
/// As `getsockname`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getpeername(fd: c_int, addr: *mut c_void, len: *mut c_uint) -> c_int {
    call(
        nr::GETPEERNAME,
        [fd as usize, addr.addr(), len.addr(), 0, 0, 0],
    ) as c_int
}

/// Stores socket option `name` at `level` in `value` and its length in
/// `*len`.
///
/// # Safety
///
/// `value` must be valid for writes of `*len` bytes, and `len` for a read and
/// a write.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getsockopt(
    fd: c_int,
    level: c_int,
    name: c_int,
    value: *mut c_void,
    len: *mut c_uint,
) -> c_int {
    call(
        nr::GETSOCKOPT,
        [
            fd as usize,
            level as usize,
            name as usize,
            value.addr(),
            len.addr(),
            0,
        ],
    ) as c_int
}

/// Sets socket option `name` at `level` to the `len` bytes at `value`.
///
/// # Safety
///
/// `value` must be valid for reads of `len` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn setsockopt(
    fd: c_int,
    level: c_int,
    name: c_int,
    value: *const c_void,
    len: c_uint,
) -> c_int {
    call(
        nr::SETSOCKOPT,
        [
            fd as usize,
            level as usize,
            name as usize,
            value.addr(),
            len as usize,
            0,
        ],
    ) as c_int
}

/// Shuts down reading, writing or both on socket `fd`, as `how` says.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn shutdown(fd: c_int, how: c_int) -> c_int {
    call(nr::SHUTDOWN, [fd as usize, how as usize, 0, 0, 0, 0]) as c_int
}

/// Sends the `len` bytes at `buf` to the address `addr`, `alen` bytes long,
/// or on the connected socket `fd` if `addr` is null, and returns how many
/// were sent.
///
/// # Safety
///
/// `buf` must be valid for reads of `len` bytes, and `addr` null or valid for
/// reads of `alen` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sendto(
    fd: c_int,
    buf: *const c_void,
    len: usize,
    flags: c_int,
    addr: *const c_void,
    alen: c_uint,
) -> isize {
    call_cp(
        nr::SENDTO,
        [
            fd as usize,
            buf.addr(),
            len,
            flags as usize,
            addr.addr(),
            alen as usize,
        ],
    )
}

/// Sends the `len` bytes at `buf` on the connected socket `fd`.
///
/// # Safety
///
/// `buf` must be valid for reads of `len` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn send(fd: c_int, buf: *const c_void, len: usize, flags: c_int) -> isize {
    // SAFETY: the caller vouches for `buf`, and there is no address.
    unsafe { sendto(fd, buf, len, flags, core::ptr::null(), 0) }
}

/// Receives up to `len` bytes into `buf` from socket `fd`, and returns how
/// many arrived. If `addr` is not null, the sender's address is stored there
/// and its length in `*alen`.
///
/// # Safety
///
/// `buf` must be valid for writes of `len` bytes, and `addr` null or valid for
/// writes of `*alen` bytes with `alen` valid for a read and a write.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn recvfrom(
    fd: c_int,
    buf: *mut c_void,
    len: usize,
    flags: c_int,
    addr: *mut c_void,
    alen: *mut c_uint,
) -> isize {
    call_cp(
        nr::RECVFROM,
        [
            fd as usize,
            buf.addr(),
            len,
            flags as usize,
            addr.addr(),
            alen.addr(),
        ],
    )
}

/// Receives up to `len` bytes into `buf` from socket `fd`.
///
/// # Safety
///
/// `buf` must be valid for writes of `len` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn recv(fd: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize {
    // SAFETY: the caller vouches for `buf`, and no address is asked for.
    unsafe { recvfrom(fd, buf, len, flags, null_mut(), null_mut()) }
}

/// Sends the buffers and control messages `*msg` describes on socket `fd`.
/// Fails with `ENOMEM` if the control messages are longer than musl's
/// buffer for them.
///
/// # Safety
///
/// `msg` must be null or point to a `struct msghdr` whose pointers are valid
/// for reads of the lengths it gives.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sendmsg(fd: c_int, msg: *const Msghdr, flags: c_int) -> isize {
    if msg.is_null() {
        return call_cp(nr::SENDMSG, [fd as usize, 0, flags as usize, 0, 0, 0]);
    }
    // SAFETY: the caller passes a valid header.
    let mut header = unsafe { msg.read() };
    header.__pad1 = 0;
    header.__pad2 = 0;
    let mut control = [0u8; CONTROL_MAX];
    let len = header.msg_controllen as usize;
    if len != 0 {
        let Some(copy) = control.get_mut(..len) else {
            errno::set(errno::ENOMEM);
            return -1;
        };
        for (offset, slot) in copy.iter_mut().enumerate() {
            // SAFETY: the header says `msg_control` holds `len` bytes.
            *slot = unsafe { header.msg_control.cast::<u8>().wrapping_add(offset).read() };
        }
        zero_control_padding(copy);
        header.msg_control = control.as_mut_ptr().cast();
    }
    call_cp(
        nr::SENDMSG,
        [
            fd as usize,
            (&raw const header).addr(),
            flags as usize,
            0,
            0,
            0,
        ],
    )
}

/// Receives into the buffers `*msg` describes from socket `fd`, with the
/// sender's address and control messages if it has room for them.
///
/// # Safety
///
/// `msg` must be null or point to a writable `struct msghdr` whose pointers
/// are valid for writes of the lengths it gives.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn recvmsg(fd: c_int, msg: *mut Msghdr, flags: c_int) -> isize {
    if msg.is_null() {
        return call_cp(nr::RECVMSG, [fd as usize, 0, flags as usize, 0, 0, 0]);
    }
    // SAFETY: the caller passes a valid header.
    let mut header = unsafe { msg.read() };
    header.__pad1 = 0;
    header.__pad2 = 0;
    let ret = call_cp(
        nr::RECVMSG,
        [
            fd as usize,
            (&raw mut header).addr(),
            flags as usize,
            0,
            0,
            0,
        ],
    );
    // SAFETY: the caller's header is writable. The kernel updated the copy's
    // lengths and flags, and musl writes it back whether the call failed or not.
    unsafe { msg.write(header) };
    ret
}

/// `SIOCATMARK`, from `include/bits/ioctl.h`.
const SIOCATMARK: c_int = 0x8905;

/// Whether the socket `fd` is at its out-of-band mark: 1 if it is, 0 if it is
/// not, or -1 with `errno` set. This is musl's `sockatmark.c`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sockatmark(fd: c_int) -> c_int {
    let mut mark: c_int = 0;
    // SAFETY: `SIOCATMARK` writes one `int`, and `mark` is a live local.
    let ret = unsafe { crate::ioctl::ioctl(fd, SIOCATMARK, (&raw mut mark).addr() as c_ulong) };
    if ret < 0 { -1 } else { mark }
}

/// The control message after `cmsg` in `mhdr`'s buffer, or null at the end:
/// what glibc's `CMSG_NXTHDR` macro calls, and a program compiled against
/// glibc's header imports.
///
/// Each length is read as its low 32 bits. glibc's structures have `size_t`
/// lengths and this library's header `int` lengths with padding after, and
/// on a little-endian target the low half is the same bytes in both.
///
/// # Safety
///
/// `mhdr` must be a `struct msghdr` and `cmsg` a control message inside its
/// buffer.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __cmsg_nxthdr(mhdr: *mut Msghdr, cmsg: *mut c_void) -> *mut c_void {
    /// A length rounded up to the alignment of a `size_t`, as `CMSG_ALIGN`.
    const fn align(len: usize) -> usize {
        len.next_multiple_of(size_of::<usize>())
    }
    // SAFETY: the caller passes a message header.
    let header = unsafe { &*mhdr };
    let (control, controllen) = (
        header.msg_control.cast::<u8>(),
        header.msg_controllen as usize,
    );
    // SAFETY: the caller passes a control message, whose first field is its
    // length.
    let len = unsafe { cmsg.cast::<c_uint>().read() } as usize;
    if len < CMSG_HEADER {
        return null_mut();
    }
    let end = control.wrapping_add(controllen);
    let next = cmsg.cast::<u8>().wrapping_add(align(len));
    if next.wrapping_add(CMSG_HEADER) > end {
        return null_mut();
    }
    // SAFETY: the next header lies inside the buffer, checked just above.
    let next_len = u32::from_ne_bytes(unsafe { next.cast::<[u8; 4]>().read() }) as usize;
    if next.wrapping_add(align(next_len)) > end {
        return null_mut();
    }
    next.cast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sockatmark_reports_the_out_of_band_mark_or_fails() {
        use std::os::fd::AsRawFd;

        // A connected stream socket that has had no urgent data is not at a
        // mark, and says so rather than failing.
        let listener = std::net::TcpListener::bind("127.0.0.1:0");
        let address = listener
            .as_ref()
            .ok()
            .and_then(|listener| listener.local_addr().ok());
        let (Ok(listener), Some(address)) = (listener, address) else {
            panic!("no loopback listener");
        };
        let Ok(stream) = std::net::TcpStream::connect(address) else {
            panic!("no loopback connection");
        };
        assert_eq!(sockatmark(stream.as_raw_fd()), 0);
        drop(stream);
        drop(listener);
        // A descriptor that is not open fails.
        assert_eq!(sockatmark(-1), -1);
        // SAFETY: the pointer is this thread's errno.
        let error = unsafe { errno::__errno_location().read() };
        assert_eq!(error, errno::EBADF);
    }

    fn message(len: u32, level: u32, r#type: u32) -> [u8; 16] {
        let mut header = [0xffu8; 16];
        for (slot, byte) in header.iter_mut().zip(
            len.to_ne_bytes()
                .into_iter()
                .chain([0xff; 4])
                .chain(level.to_ne_bytes())
                .chain(r#type.to_ne_bytes()),
        ) {
            *slot = byte;
        }
        header
    }

    #[test]
    fn padding_is_zeroed_in_each_control_message_and_nothing_else() {
        // Two messages of 20 bytes each, aligned to 24, in 48 bytes.
        let mut control = [0xeeu8; 48];
        for (slot, byte) in control.iter_mut().zip(message(20, 1, 1)) {
            *slot = byte;
        }
        for (slot, byte) in control.iter_mut().skip(24).zip(message(20, 1, 2)) {
            *slot = byte;
        }
        zero_control_padding(&mut control);
        assert_eq!(control.get(4..8), Some(&[0u8; 4][..]));
        assert_eq!(control.get(28..32), Some(&[0u8; 4][..]));
        assert_eq!(control.get(16..24), Some(&[0xeeu8; 8][..]));
        assert_eq!(control.get(0..4), Some(&20u32.to_ne_bytes()[..]));

        // A length below a header's stops the walk after that message, and
        // so does a message the buffer has no room after.
        let mut short = [0xffu8; 40];
        for (slot, byte) in short.iter_mut().zip(message(8, 1, 1)) {
            *slot = byte;
        }
        zero_control_padding(&mut short);
        assert_eq!(short.get(4..8), Some(&[0u8; 4][..]));
        assert_eq!(short.get(16..40), Some(&[0xffu8; 24][..]));

        // Less than a header is not walked at all.
        let mut tiny = [0xffu8; 12];
        zero_control_padding(&mut tiny);
        assert_eq!(tiny, [0xff; 12]);
    }

    #[test]
    fn a_pair_carries_bytes_both_ways() {
        let mut fds = [-1; 2];
        // AF_UNIX and SOCK_STREAM, from include/sys/socket.h.
        // SAFETY: `fds` holds two ints.
        assert_eq!(unsafe { socketpair(1, 1, 0, fds.as_mut_ptr()) }, 0);
        let [a, b] = fds;
        // SAFETY: the buffers are live locals of the lengths given.
        assert_eq!(unsafe { send(a, b"ping".as_ptr().cast(), 4, 0) }, 4);
        let mut buf = [0u8; 8];
        // SAFETY: as above.
        assert_eq!(unsafe { recv(b, buf.as_mut_ptr().cast(), buf.len(), 0) }, 4);
        assert_eq!(buf.get(..4), Some(&b"ping"[..]));
        assert_eq!(shutdown(a, 1), 0);
        // SAFETY: as above.
        assert_eq!(unsafe { recv(b, buf.as_mut_ptr().cast(), buf.len(), 0) }, 0);
        assert_eq!(listen(-1, 1), -1);
        for fd in fds {
            // SAFETY: `close` reads no memory.
            let _ = unsafe { syscall::syscall2(nr::CLOSE, fd as usize, 0) };
        }
    }
}
