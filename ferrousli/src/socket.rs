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
//!   messages. On a 64-bit target the old and new numbers are the same, so
//!   `getsockopt`, `setsockopt` and `recvmsg` pass everything through. On
//!   ARMv7-A the headers give the new timeout numbers, and a kernel without
//!   them is asked again with the old ones and a 32-bit `timeval`, as musl
//!   does; the timestamp options and messages are passed through, so a
//!   kernel without the new timestamps refuses them.
//! * On a 64-bit target C's `struct msghdr` and `struct cmsghdr` have
//!   `int`-sized lengths followed by padding, where the kernel has `size_t`
//!   lengths. `sendmsg` and `recvmsg` give the kernel a copy with the padding
//!   zeroed, as musl does, so a program's stray bytes are never read as the
//!   high half of a length. A 32-bit target's lengths are the kernel's.
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
    #[cfg(target_pointer_width = "64")]
    pub __pad1: c_int,
    /// The control messages, or null.
    pub msg_control: *mut c_void,
    /// The size of `msg_control`.
    pub msg_controllen: c_uint,
    /// Padding, which the kernel reads as the high half of `msg_controllen`.
    #[cfg(target_pointer_width = "64")]
    pub __pad2: c_int,
    /// The flags `recvmsg` reports.
    pub msg_flags: c_int,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Msghdr>() == 56);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<Msghdr>() == 28);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(Msghdr, msg_iov) == 8);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(Msghdr, msg_control) == 16);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(Msghdr, msg_flags) == 24);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(Msghdr, msg_iov) == 16);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(Msghdr, __pad1) == 28);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(Msghdr, msg_control) == 32);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(Msghdr, __pad2) == 44);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(Msghdr, msg_flags) == 48);

/// The size of C's `struct cmsghdr`: `cmsg_len`, its padding on a 64-bit
/// target, `cmsg_level` and `cmsg_type`, four bytes each.
const CMSG_HEADER: usize = if cfg!(target_pointer_width = "64") {
    16
} else {
    12
};

/// The most control data `sendmsg` copies: musl's buffer of 66 headers' worth
/// of bytes, room for an `SCM_RIGHTS` message carrying 255 descriptors.
#[cfg(target_pointer_width = "64")]
const CONTROL_MAX: usize = 66 * CMSG_HEADER;

/// Zeroes the padding after each control message's `cmsg_len` in `control`,
/// visiting the messages as `CMSG_FIRSTHDR` and `CMSG_NXTHDR` do.
#[cfg(target_pointer_width = "64")]
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
    #[cfg(target_arch = "arm")]
    if let Some(old) = old_timeout(level, name) {
        // SAFETY: the caller vouches for `len`.
        if !len.is_null() && unsafe { len.read() } as usize >= size_of::<crate::time::Timeval>() {
            let mut narrow: [core::ffi::c_long; 2] = [0; 2];
            let mut narrow_len = size_of_val(&narrow) as c_uint;
            // SAFETY: the kernel writes the caller's buffers, which it vouches for.
            let ret = unsafe {
                syscall::syscall6(
                    nr::GETSOCKOPT,
                    fd as usize,
                    level as usize,
                    name as usize,
                    value.addr(),
                    len.addr(),
                    0,
                )
            };
            if ret != -(errno::ENOPROTOOPT as isize) {
                return errno::from_syscall(ret) as c_int;
            }
            let ret = call(
                nr::GETSOCKOPT,
                [
                    fd as usize,
                    level as usize,
                    old as usize,
                    (&raw mut narrow).addr(),
                    (&raw mut narrow_len).addr(),
                    0,
                ],
            );
            if ret == 0 {
                let [sec, usec] = narrow;
                // SAFETY: the caller's buffer holds a `struct timeval`, which
                // `len` said.
                unsafe {
                    value
                        .cast::<crate::time::Timeval>()
                        .write_unaligned(crate::time::Timeval {
                            tv_sec: i64::from(sec),
                            tv_usec: i64::from(usec),
                        });
                }
                // SAFETY: as above, for `len`.
                unsafe { len.write(size_of::<crate::time::Timeval>() as c_uint) };
            }
            return ret as c_int;
        }
    }
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

/// The old number of `SO_RCVTIMEO` or `SO_SNDTIMEO` at `SOL_SOCKET`, which
/// takes a `struct timeval` of two 32-bit `long`s, for the new one ARMv7-A's
/// headers give, which takes C's 64-bit one; `None` for any other option.
#[cfg(target_arch = "arm")]
fn old_timeout(level: c_int, name: c_int) -> Option<c_int> {
    /// `SOL_SOCKET`.
    const SOL_SOCKET: c_int = 1;
    match (level, name) {
        // `SO_RCVTIMEO_NEW` and `SO_SNDTIMEO_NEW`, to `_OLD`.
        (SOL_SOCKET, 66) => Some(20),
        (SOL_SOCKET, 67) => Some(21),
        _ => None,
    }
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
    #[cfg(target_arch = "arm")]
    if let Some(old) = old_timeout(level, name)
        && len as usize >= size_of::<crate::time::Timeval>()
    {
        // SAFETY: the kernel reads the caller's buffer, which it vouches for.
        let ret = unsafe {
            syscall::syscall6(
                nr::SETSOCKOPT,
                fd as usize,
                level as usize,
                name as usize,
                value.addr(),
                len as usize,
                0,
            )
        };
        if ret != -(errno::ENOPROTOOPT as isize) {
            return errno::from_syscall(ret) as c_int;
        }
        // SAFETY: the caller's buffer holds a `struct timeval`, which `len`
        // says.
        let wide = unsafe { value.cast::<crate::time::Timeval>().read_unaligned() };
        let (Ok(sec), Ok(usec)) = (
            core::ffi::c_long::try_from(wide.tv_sec),
            core::ffi::c_long::try_from(wide.tv_usec),
        ) else {
            errno::set(errno::ENOTSUP);
            return -1;
        };
        let narrow: [core::ffi::c_long; 2] = [sec, usec];
        return call(
            nr::SETSOCKOPT,
            [
                fd as usize,
                level as usize,
                old as usize,
                (&raw const narrow).addr(),
                size_of_val(&narrow),
                0,
            ],
        ) as c_int;
    }
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
    #[cfg_attr(
        target_pointer_width = "32",
        expect(unused_mut, reason = "only a 64-bit header is rewritten")
    )]
    // SAFETY: the caller passes a valid header.
    let mut header = unsafe { msg.read() };
    #[cfg(target_pointer_width = "64")]
    {
        header.__pad1 = 0;
        header.__pad2 = 0;
    }
    #[cfg(target_pointer_width = "64")]
    let mut control = [0u8; CONTROL_MAX];
    #[cfg(target_pointer_width = "64")]
    if header.msg_controllen != 0 {
        let len = header.msg_controllen as usize;
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
    #[cfg(target_pointer_width = "64")]
    {
        header.__pad1 = 0;
        header.__pad2 = 0;
    }
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

/// C's `struct mmsghdr`: a message header, and the bytes `sendmmsg` sent or
/// `recvmmsg` received with it.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Mmsghdr {
    /// The message.
    pub msg_hdr: Msghdr,
    /// Its length, written by the call.
    pub msg_len: c_uint,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Mmsghdr>() == 64);

/// `IOV_MAX`, the most messages the kernel takes in one `sendmmsg`.
#[cfg(target_pointer_width = "64")]
const IOV_MAX: c_uint = 1024;

/// Sends up to `vlen` messages on socket `fd`, and returns how many went,
/// with each one's length in its `msg_len`; -1 only if the first fails.
///
/// As musl 1.2.5's `sendmmsg.c` (MIT; see [`crate::math`] for the notice).
/// On a 64-bit architecture each message goes through [`sendmsg`], which
/// clears the padding the kernel would read as the high half of a length;
/// the kernel's own `sendmmsg` would read the caller's headers as they are.
///
/// # Safety
///
/// `msgvec` must hold `vlen` headers, each valid as `sendmsg` requires.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sendmmsg(
    fd: c_int,
    msgvec: *mut Mmsghdr,
    vlen: c_uint,
    flags: c_uint,
) -> c_int {
    #[cfg(target_pointer_width = "64")]
    {
        let vlen = vlen.min(IOV_MAX) as usize;
        let mut sent = 0;
        while sent < vlen {
            let message = msgvec.wrapping_add(sent);
            // SAFETY: the caller vouches for `vlen` headers, and each begins
            // with its `struct msghdr`.
            let ret = unsafe { sendmsg(fd, message.cast_const().cast(), flags as c_int) };
            if ret < 0 {
                break;
            }
            // SAFETY: as above; Linux sends at most `INT_MAX` bytes at once.
            unsafe { (*message).msg_len = ret as c_uint };
            sent += 1;
        }
        if sent == 0 && vlen != 0 {
            -1
        } else {
            sent as c_int
        }
    }
    #[cfg(target_pointer_width = "32")]
    {
        call_cp(
            nr::SENDMMSG,
            [
                fd as usize,
                msgvec.addr(),
                vlen as usize,
                flags as usize,
                0,
                0,
            ],
        ) as c_int
    }
}

/// Receives up to `vlen` messages from socket `fd`, waiting at most
/// `*timeout` if it is not null, and returns how many came.
///
/// As musl 1.2.5's `recvmmsg.c`: the padding in each header is cleared
/// first, and the kernel's call does the rest. This library's `struct
/// timespec` is the kernel's on every architecture, so ARMv7-A calls
/// `recvmmsg_time64` under the plain name.
///
/// # Safety
///
/// `msgvec` must hold `vlen` writable headers, each valid as `recvmsg`
/// requires, and `timeout` must be null or valid for reading and writing.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn recvmmsg(
    fd: c_int,
    msgvec: *mut Mmsghdr,
    vlen: c_uint,
    flags: c_uint,
    timeout: *mut crate::time::Timespec,
) -> c_int {
    #[cfg(target_pointer_width = "64")]
    for i in 0..vlen as usize {
        let message = msgvec.wrapping_add(i);
        // SAFETY: the caller vouches for `vlen` writable headers.
        unsafe { (*message).msg_hdr.__pad1 = 0 };
        // SAFETY: as above.
        unsafe { (*message).msg_hdr.__pad2 = 0 };
    }
    call_cp(
        nr::RECVMMSG,
        [
            fd as usize,
            msgvec.addr(),
            vlen as usize,
            flags as usize,
            timeout.addr(),
            0,
        ],
    ) as c_int
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

    #[cfg(target_pointer_width = "64")]
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

    #[cfg(target_pointer_width = "64")]
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
