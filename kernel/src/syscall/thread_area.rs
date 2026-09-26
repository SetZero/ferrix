//! `set_thread_area` and `get_thread_area`: an i386 program's thread
//! pointer, which is a segment.
//!
//! musl and glibc on i386 ask for a flat data segment based at their thread
//! block and load its selector into `%gs` (`docs/I386.md` §2). The calling
//! thread has three such descriptors, GDT entries 12 to 14; the rules for
//! what may go in one, and the encoding each way, are
//! `ferrix_linux_abi::user_desc`'s, and where the three live while the thread
//! runs is the architecture's (`arch::set_thread_area`). Only i386's table
//! maps either call.
//!
//! The order is Linux's `do_set_thread_area`: the descriptor is refused before
//! anything else, a free entry is found and its number written back to the
//! program before anything is installed, and a number outside the three is
//! `EINVAL`.

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::user_desc::{ANY_ENTRY, TLS_ENTRIES, TLS_FIRST_ENTRY, UserDesc, tls_index};

use crate::arch;
use crate::syscall::process::Process;
use crate::syscall::uaccess;

/// `set_thread_area(u_info)`: install the descriptor `u_info` describes, in
/// the entry it names or, for entry -1, in the first empty one, whose number
/// is written back to `u_info->entry_number`.
///
/// # Errors
///
/// `EFAULT` for an unreadable `u_info` or an unwritable number, `EINVAL` for
/// a descriptor `tls_desc_okay` refuses or an entry outside the three, and
/// `ESRCH` when entry -1 finds all three taken.
pub(crate) fn sys_set_thread_area(process: &Process, u_info: u64) -> Result<usize, Errno> {
    let mut bytes = [0_u8; UserDesc::SIZE];
    uaccess::copy_from_user(process.space(), u_info, &mut bytes).map_err(|_| Errno::EFAULT)?;
    let desc = UserDesc::from_bytes(&bytes).ok_or(Errno::EFAULT)?;
    let descriptor = desc.to_descriptor().map_err(|_| Errno::EINVAL)?;
    let index = if desc.entry_number == ANY_ENTRY {
        let free = (0..TLS_ENTRIES)
            .find(|&index| arch::thread_area(index) == Some(0))
            .ok_or(Errno::ESRCH)?;
        let number = TLS_FIRST_ENTRY + free as u32;
        uaccess::copy_to_user(process.space(), u_info, &number.to_le_bytes())
            .map_err(|_| Errno::EFAULT)?;
        free
    } else {
        tls_index(desc.entry_number).ok_or(Errno::EINVAL)?
    };
    let _ = arch::set_thread_area(Some(index), descriptor).ok_or(Errno::EINVAL)?;
    Ok(0)
}

/// `get_thread_area(u_info)`: describe the entry `u_info->entry_number` names
/// back into `u_info`; an empty entry reads as the shape that empties one.
///
/// # Errors
///
/// `EFAULT` for an unreadable number or an unwritable `u_info`, and `EINVAL`
/// for an entry outside the three.
pub(crate) fn sys_get_thread_area(process: &Process, u_info: u64) -> Result<usize, Errno> {
    let mut number = [0_u8; 4];
    uaccess::copy_from_user(process.space(), u_info, &mut number).map_err(|_| Errno::EFAULT)?;
    let entry_number = u32::from_le_bytes(number);
    let index = tls_index(entry_number).ok_or(Errno::EINVAL)?;
    let descriptor = arch::thread_area(index).ok_or(Errno::EINVAL)?;
    let desc = UserDesc::from_descriptor(entry_number, descriptor);
    uaccess::copy_to_user(process.space(), u_info, &desc.to_bytes()).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}
