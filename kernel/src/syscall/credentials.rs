//! Who a process is, and what it is allowed to do.
//!
//! # Everything is root, and nothing may pretend otherwise
//!
//! Credentials are stage 12's. Until then every process runs as uid 0 and gid
//! 0 with every capability, and the calls here answer for that system and no
//! other. The reads report it. The writes accept exactly the changes that
//! change nothing -- to 0, or `-1` for "leave this one" -- and refuse every
//! other id with `EPERM`.
//!
//! Refusing is the honest answer, and accepting would not be. A process that
//! called `setuid(1000)` and was told it succeeded would go on to act as a
//! process that had dropped root: a daemon would open its sockets believing
//! a compromise could not reach the files root owns, `su` would believe it had
//! handed a shell to an unprivileged user. None of that would be true, because
//! nothing here can make it true, and the one party that knows it -- the
//! kernel -- would have said otherwise. `EPERM` is what an unprivileged
//! process is told, and every program that calls these checks for it.
//!
//! Linux itself would let root make these changes; this is the one place the
//! answers deliberately differ from what root gets there, and it is the
//! direction that fails safe.

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::nr::Syscall;

use crate::syscall::attributes::{self, int, subject};
use crate::syscall::process::Process;
use crate::syscall::uaccess;

/// `(uid_t)-1` and `(gid_t)-1`: "unchanged", to the calls that take several.
const UNCHANGED: u32 = u32::MAX;

/// The most supplementary groups a process may have: Linux's `NGROUPS_MAX`.
const NGROUPS_MAX: u32 = 65_536;

/// The highest capability number Linux 6.x defines, `CAP_CHECKPOINT_RESTORE`.
/// `CAP_LAST_CAP` in `linux/capability.h`, 40, for both builds.
pub(crate) const CAP_LAST_CAP: u32 = 40;

/// `_LINUX_CAPABILITY_VERSION_1`: one 32-bit set per field.
const CAPABILITY_VERSION_1: u32 = 0x1998_0330;
/// `_LINUX_CAPABILITY_VERSION_2`, deprecated for version 3 and the same shape.
const CAPABILITY_VERSION_2: u32 = 0x2007_1026;
/// `_LINUX_CAPABILITY_VERSION_3`: two 32-bit words per set.
const CAPABILITY_VERSION_3: u32 = 0x2008_0522;

/// Bytes in one `struct __user_cap_data_struct`: three `__u32`s. Twelve on
/// both builds, and the header eight; checked with `sizeof` against
/// `linux/capability.h` compiled for x86-64 and `arm-linux-gnueabihf`.
const CAP_DATA: usize = 12;

/// Answer `call` if it is one of this module's.
pub(crate) fn dispatch(
    call: Syscall,
    a: &[u64; 6],
    process: &Process,
) -> Option<Result<usize, Errno>> {
    let id = |slot: usize| a.get(slot).map_or(0, |&value| value as u32);
    let answer = match call {
        Syscall::Setuid | Syscall::Setgid => sys_setid(id(0)),
        Syscall::Setreuid | Syscall::Setregid => become_ids(&[id(0), id(1)]),
        Syscall::Setresuid | Syscall::Setresgid => become_ids(&[id(0), id(1), id(2)]),
        // `setfsuid` has no error return: it answers the previous id whether
        // or not the change was made, and a refused change is detected by
        // calling it again. The previous id is always 0.
        Syscall::Setfsuid | Syscall::Setfsgid => Ok(0),
        Syscall::Getresuid | Syscall::Getresgid => sys_getresid(process, [a[0], a[1], a[2]]),
        Syscall::Getgroups => sys_getgroups(process, int(a[0]), a[1]),
        Syscall::Setgroups => sys_setgroups(process, id(0), a[1]),
        Syscall::Capget => sys_capget(process, a[0], a[1]),
        Syscall::Capset => sys_capset(process, a[0], a[1]),
        _ => return None,
    };
    Some(answer)
}

/// One id a `set*id` call asks for: nothing to do for 0 or "unchanged",
/// `EPERM` for anything else. See the module documentation.
fn become_id(id: u32) -> Result<(), Errno> {
    match id {
        0 | UNCHANGED => Ok(()),
        _ => Err(Errno::EPERM),
    }
}

/// `setreuid`, `setresuid` and their group forms: every id must be one
/// [`become_id`] accepts, or none of them is applied.
fn become_ids(ids: &[u32]) -> Result<usize, Errno> {
    ids.iter().try_for_each(|&id| become_id(id)).map(|()| 0)
}

/// `setuid` and `setgid`, which take one id and have no "unchanged": `-1` is
/// not a valid uid (`uid_valid` fails for it), so Linux answers `EINVAL`.
fn sys_setid(id: u32) -> Result<usize, Errno> {
    if id == UNCHANGED {
        return Err(Errno::EINVAL);
    }
    become_id(id).map(|()| 0)
}

/// `getresuid` and `getresgid`: three `uid_t`s, 32 bits on every architecture
/// this kernel has (the 16-bit calls are not in its tables), written one after
/// another as `kernel/sys.c` writes them. All zero.
fn sys_getresid(process: &Process, at: [u64; 3]) -> Result<usize, Errno> {
    for address in at {
        uaccess::put_u32(process.space(), address, 0)?;
    }
    Ok(0)
}

/// `getgroups`: the supplementary groups, which are group 0 alone -- what
/// root's are on Linux -- unless `setgroups` emptied them.
///
/// A size of zero asks only for the count, which is how `id` sizes its
/// buffer; a size smaller than the count is `EINVAL`, as `kernel/groups.c`
/// answers. Each `gid_t` is 32 bits on all three architectures.
pub(crate) fn sys_getgroups(process: &Process, size: i32, list: u64) -> Result<usize, Errno> {
    let size = usize::try_from(size).map_err(|_| Errno::EINVAL)?;
    let count = usize::from(attributes::get(process).in_root_group);
    if size == 0 {
        return Ok(count);
    }
    if size < count {
        return Err(Errno::EINVAL);
    }
    if count == 1 {
        uaccess::put_u32(process.space(), list, 0)?;
    }
    Ok(count)
}

/// `setgroups`: accepted when it changes nothing a process could rely on.
///
/// The list is read, so a bad pointer is `EFAULT` as on Linux. An empty list,
/// or one of nothing but group 0 -- root keeping root's group -- is accepted
/// and read back by `getgroups`, and any other group is `EPERM`, for the
/// reason the module gives: a process told it had joined a group would
/// believe it had that group's access, which nothing here grants.
fn sys_setgroups(process: &Process, size: u32, list: u64) -> Result<usize, Errno> {
    if size > NGROUPS_MAX {
        return Err(Errno::EINVAL);
    }
    check_only_root_group(process, size, list)?;
    attributes::update(process, |a| a.in_root_group = size != 0);
    Ok(0)
}

/// Read a `setgroups` list and refuse it if any group in it is not 0.
fn check_only_root_group(process: &Process, size: u32, list: u64) -> Result<(), Errno> {
    let mut chunk = [0_u8; 256];
    let mut remaining = usize::try_from(size).map_err(|_| Errno::EINVAL)? * 4;
    let mut at = list;
    while remaining > 0 {
        let len = remaining.min(chunk.len());
        let bytes = chunk.get_mut(..len).ok_or(Errno::EFAULT)?;
        uaccess::copy_from_user(process.space(), at, bytes).map_err(|_| Errno::EFAULT)?;
        if bytes.iter().any(|&byte| byte != 0) {
            return Err(Errno::EPERM);
        }
        remaining -= len;
        at = at.checked_add(len as u64).ok_or(Errno::EFAULT)?;
    }
    Ok(())
}

/// How many data structures a capability version reads or writes, or `None`
/// for a version the kernel does not speak.
fn data_count(version: u32) -> Option<usize> {
    match version {
        CAPABILITY_VERSION_1 => Some(1),
        CAPABILITY_VERSION_2 | CAPABILITY_VERSION_3 => Some(2),
        _ => None,
    }
}

/// Validate the header's version as `cap_validate_magic` does.
///
/// An unknown version is how a program asks which one the kernel prefers:
/// the kernel writes version 3 into the header and answers `EINVAL`. libcap
/// probes exactly this way, with a zeroed header.
fn validate_version(process: &Process, header: u64) -> Result<usize, Errno> {
    let version = uaccess::get_u32(process.space(), header)?;
    match data_count(version) {
        Some(count) => Ok(count),
        None => {
            uaccess::put_u32(process.space(), header, CAPABILITY_VERSION_3)?;
            Err(Errno::EINVAL)
        }
    }
}

/// `capget`: every capability effective and permitted, none inheritable.
///
/// That is what a root process on Linux reports, and it is true here: nothing
/// checks a capability, so a process can do everything any capability would
/// let it. The order of refusals is `kernel/capability.c`'s -- in particular a
/// version probe with a null data pointer succeeds, having written the
/// preferred version, because that is the form libcap's probe takes.
pub(crate) fn sys_capget(process: &Process, header: u64, data: u64) -> Result<usize, Errno> {
    let validated = validate_version(process, header);
    if data == 0 {
        return match validated {
            Ok(_) | Err(Errno::EINVAL) => Ok(0),
            Err(errno) => Err(errno),
        };
    }
    let count = validated?;
    let pid = int(u64::from(uaccess::get_u32(
        process.space(),
        header.wrapping_add(4),
    )?));
    if pid < 0 {
        return Err(Errno::EINVAL);
    }
    let _ = subject(process, pid)?;

    let full = [u32::MAX, (1_u32 << (CAP_LAST_CAP - 31)) - 1];
    let mut bytes = [0_u8; CAP_DATA * 2];
    for (index, slot) in bytes.chunks_mut(CAP_DATA).enumerate() {
        let word = full.get(index).copied().unwrap_or(0).to_le_bytes();
        // effective, permitted, inheritable: the first two full, the last empty.
        for (byte, value) in slot.iter_mut().zip(word.iter().chain(word.iter())) {
            *byte = *value;
        }
    }
    let used = bytes.get(..CAP_DATA * count).ok_or(Errno::EINVAL)?;
    uaccess::copy_to_user(process.space(), data, used).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}

/// `capset`: accepted for the caller, within the capabilities that exist.
///
/// Another process is `EPERM`, as it has been on Linux since 2.6.24. Bits
/// above [`CAP_LAST_CAP`] are `EPERM` too, because Linux requires the new
/// permitted set to be within the old one and those bits are in no set. A
/// narrower set is accepted and not enforced: nothing checks capabilities
/// until stage 12, so there is nothing to narrow, and `capget` goes on
/// reporting what a process can actually do.
pub(crate) fn sys_capset(process: &Process, header: u64, data: u64) -> Result<usize, Errno> {
    let count = validate_version(process, header)?;
    let pid = int(u64::from(uaccess::get_u32(
        process.space(),
        header.wrapping_add(4),
    )?));
    if !matches!(subject(process, pid), Ok(attributes::Subject::Caller(_))) {
        return Err(Errno::EPERM);
    }
    let mut bytes = [0_u8; CAP_DATA * 2];
    let used = bytes.get_mut(..CAP_DATA * count).ok_or(Errno::EINVAL)?;
    uaccess::copy_from_user(process.space(), data, used).map_err(|_| Errno::EFAULT)?;
    let high_mask = !((1_u32 << (CAP_LAST_CAP - 31)) - 1);
    if let Some(high) = bytes.get(CAP_DATA..)
        && count == 2
        && high
            .chunks(4)
            .any(|word| matches!(*word, [a, b, c, d] if u32::from_le_bytes([a, b, c, d]) & high_mask != 0))
    {
        return Err(Errno::EPERM);
    }
    Ok(0)
}
