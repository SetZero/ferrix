//! `memfd_create`: an anonymous file, on a tmpfs of its own that nothing
//! mounts.
//!
//! The file is a tmpfs regular file that no directory names, opened for
//! reading and writing. Its pages are VMO pages like any tmpfs file's, so it
//! reads, writes, truncates and maps as one does -- which is the point: a
//! Wayland client hands `wl_shm` a memfd, both sides map it shared, and the
//! compositor seals it so the client cannot shrink it under the mapping.
//!
//! # Seals
//!
//! With `MFD_ALLOW_SEALING` the file starts with no seals and takes them
//! through `fcntl(F_ADD_SEALS)`; without it, it carries `F_SEAL_SEAL` from the
//! start, as every other tmpfs file does. What each seal refuses is tmpfs's to
//! enforce, and a write seal's refusal while a shared mapping may write the
//! file is mm's, through the file object's may-write count.
//!
//! # What is refused
//!
//! Flags other than `MFD_CLOEXEC` and `MFD_ALLOW_SEALING` are `EINVAL`.
//! That includes `MFD_HUGETLB`, since there is no hugetlbfs, and
//! `MFD_NOEXEC_SEAL`/`MFD_EXEC`, which Linux 6.3 added with an exec seal this
//! kernel does not have yet: `EINVAL` is what a kernel from before them
//! answers. A name longer than 249 bytes is `EINVAL`, and one that cannot be
//! read is `EFAULT`.

use alloc::format;
use alloc::sync::Arc;
use alloc::vec::Vec;

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{MFD_ALLOW_SEALING, MFD_CLOEXEC};
use ferrix_sync::Once;
use ferrix_vfs::tmpfs::Tmpfs;
use ferrix_vfs::{FileSystem, Location, OpenFile, OpenFlags};

use crate::fs;
use crate::syscall::process::Process;
use crate::syscall::uaccess::{self, UserError};

/// The longest name: `NAME_MAX` less the `memfd:` every name is shown with.
const NAME_MAX_LEN: usize = 249;

/// The mode a memfd's inode has: `S_IFREG | 0777`, as on Linux.
const PERMISSIONS: u32 = 0o777;

/// The tmpfs every memfd lives on, made on first use.
static MEMFD_FS: Once<Arc<Tmpfs>> = Once::new();

/// The one memfd tmpfs.
fn memfd_fs() -> &'static Arc<Tmpfs> {
    MEMFD_FS.call_once(fs::new_tmpfs)
}

/// `memfd_create(name, flags)`.
///
/// # Errors
///
/// `EINVAL` for an unknown flag or a name longer than 249 bytes, `EFAULT` for
/// a name that cannot be read, `EMFILE` for a full descriptor table, and
/// whatever tmpfs's storage refuses.
pub(crate) fn sys_memfd_create(process: &Process, name: u64, flags: u32) -> Result<usize, Errno> {
    if flags & !(MFD_CLOEXEC | MFD_ALLOW_SEALING) != 0 {
        return Err(Errno::EINVAL);
    }
    let mut bytes = Vec::new();
    match uaccess::copy_cstr_from_user(process.space(), name, NAME_MAX_LEN + 1, &mut bytes) {
        Ok(()) => {}
        // Unterminated within the limit: the name is too long, not unreadable.
        Err(UserError::Fault) if bytes.len() > NAME_MAX_LEN => return Err(Errno::EINVAL),
        Err(_) => return Err(Errno::EFAULT),
    }
    if bytes.len() > NAME_MAX_LEN {
        return Err(Errno::EINVAL);
    }

    let tmpfs = memfd_fs();
    let inode = tmpfs.new_unlinked_file(PERMISSIONS, flags & MFD_ALLOW_SEALING != 0)?;
    let shown = format!("memfd:{}", alloc::string::String::from_utf8_lossy(&bytes));
    let location = Location::detached(
        Arc::clone(tmpfs) as Arc<dyn FileSystem>,
        inode,
        shown.as_bytes(),
        Arc::new(crate::sync::SchedParker),
    );
    let open = OpenFlags {
        read: true,
        write: true,
        ..OpenFlags::default()
    };
    let file: Arc<OpenFile> = OpenFile::new(location, &open)?;
    let descriptor = process
        .files()
        .lock()
        .insert(file, flags & MFD_CLOEXEC != 0)?;
    usize::try_from(descriptor).map_err(|_| Errno::EMFILE)
}
