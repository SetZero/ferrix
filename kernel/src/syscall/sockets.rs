//! The socket calls, answered honestly until stage 10 brings a network stack.
//!
//! # What "honestly" means here
//!
//! There are no socket families, so `socket` and `socketpair` are refused with
//! `EAFNOSUPPORT` -- the answer Linux gives for a family it was built without,
//! and the one every program already handles: `ping` says the family is not
//! supported, a libc resolver skips IPv6, `syslog()` gives up on `/dev/log`
//! quietly. And since nothing can create a socket, no descriptor is one, so
//! every call that takes a socket descriptor is `EBADF` for a closed one and
//! `ENOTSOCK` for an open one, exactly as Linux answers when handed a file.
//!
//! The checks Linux makes *before* it looks at the descriptor are made here
//! too, in its order -- a bad flag is `EINVAL`, a buffer outside the user half
//! `EFAULT` -- so a program sees the same first error it would on Linux.
//!
//! When stage 10 lands, the family check stops refusing and the descriptor
//! calls start finding sockets; nothing that calls this has to change.

use ferrix_bootinfo::is_user_address;
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::nr::Syscall;
use ferrix_linux_abi::types::{O_CLOEXEC, O_NONBLOCK};

use crate::syscall::attributes::int;
use crate::syscall::fd;
use crate::syscall::process::Process;

/// The bits of a socket type that are the type: `SOCK_TYPE_MASK`.
const SOCK_TYPE_MASK: u32 = 0xF;
/// One past the last socket type: `SOCK_MAX` in `linux/net.h`.
const SOCK_MAX: u32 = 11;
/// `SOCK_NONBLOCK`, which is `O_NONBLOCK` on all three architectures (it
/// differs only on Alpha, MIPS, PA-RISC and SPARC).
const SOCK_NONBLOCK: u32 = O_NONBLOCK;
/// `SOCK_CLOEXEC`, which is `O_CLOEXEC` everywhere.
const SOCK_CLOEXEC: u32 = O_CLOEXEC;
/// The number of address families, `AF_MAX` (`NPROTO`), in `linux/socket.h`.
const AF_MAX: i32 = 46;
/// A flag only the kernel's compat layer may set on `sendmsg`/`recvmsg`.
const MSG_CMSG_COMPAT: u32 = 0x8000_0000;

/// Answer `call` if it is one of this module's.
pub(crate) fn dispatch(
    call: Syscall,
    a: &[u64; 6],
    process: &Process,
) -> Option<Result<usize, Errno>> {
    let descriptor = fd::arg(a[0]);
    let answer = match call {
        Syscall::Socket => sys_socket(int(a[0]), a[1] as u32),
        Syscall::Socketpair => sys_socketpair(int(a[0]), a[1] as u32, a[3]),
        Syscall::Bind
        | Syscall::Listen
        | Syscall::Accept
        | Syscall::Connect
        | Syscall::Getsockname
        | Syscall::Getpeername
        | Syscall::Shutdown
        | Syscall::Getsockopt => not_a_socket(process, descriptor),
        Syscall::Accept4 => {
            known_flags(a[3] as u32).and_then(|()| not_a_socket(process, descriptor))
        }
        Syscall::Sendto | Syscall::Recvfrom => {
            user_buffer(a[1], a[2]).and_then(|()| not_a_socket(process, descriptor))
        }
        Syscall::Sendmsg | Syscall::Recvmsg => {
            if a[2] as u32 & MSG_CMSG_COMPAT != 0 {
                Err(Errno::EINVAL)
            } else {
                not_a_socket(process, descriptor)
            }
        }
        Syscall::Setsockopt => {
            if int(a[4]) < 0 {
                Err(Errno::EINVAL)
            } else {
                not_a_socket(process, descriptor)
            }
        }
        _ => return None,
    };
    Some(answer)
}

/// Only `SOCK_NONBLOCK` and `SOCK_CLOEXEC` may accompany a type, or be given
/// to `accept4`.
fn known_flags(flags: u32) -> Result<(), Errno> {
    if flags & !(SOCK_CLOEXEC | SOCK_NONBLOCK) != 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

/// `socket`: `__sys_socket` and `__sock_create`'s checks in their order, and
/// then the refusal every family gets.
pub(crate) fn sys_socket(family: i32, kind: u32) -> Result<usize, Errno> {
    known_flags(kind & !SOCK_TYPE_MASK)?;
    if !(0..AF_MAX).contains(&family) {
        return Err(Errno::EAFNOSUPPORT);
    }
    if kind & SOCK_TYPE_MASK >= SOCK_MAX {
        return Err(Errno::EINVAL);
    }
    Err(Errno::EAFNOSUPPORT)
}

/// `socketpair`: as [`sys_socket`], after refusing a result pointer outside
/// the user half.
///
/// Linux reserves the two descriptors and writes their numbers before it
/// creates the sockets, so an unwritable pointer is `EFAULT` ahead of the
/// family's refusal. The pointer's range is checked here; a mapped-but-
/// unwritable page inside it is the one case answered differently.
pub(crate) fn sys_socketpair(family: i32, kind: u32, pair: u64) -> Result<usize, Errno> {
    known_flags(kind & !SOCK_TYPE_MASK)?;
    user_buffer(pair, 8)?;
    sys_socket(family, kind)
}

/// `access_ok`: a buffer must lie in the user half. Only the range is checked,
/// not whether it is mapped, which is all `import_ubuf` checks before the
/// descriptor is looked up.
fn user_buffer(at: u64, len: u64) -> Result<(), Errno> {
    let len = len as usize as u64;
    if len == 0 {
        return Ok(());
    }
    let last = at.checked_add(len - 1).ok_or(Errno::EFAULT)?;
    if is_user_address(at) && is_user_address(last) {
        Ok(())
    } else {
        Err(Errno::EFAULT)
    }
}

/// A call on a socket descriptor: `EBADF` if nothing is open there, and
/// `ENOTSOCK` if something is, because nothing can be a socket yet.
pub(crate) fn not_a_socket(process: &Process, descriptor: i32) -> Result<usize, Errno> {
    let _file = fd::file(process, descriptor)?;
    Err(Errno::ENOTSOCK)
}
