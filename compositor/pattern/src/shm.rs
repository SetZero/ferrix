//! The client's side of shared memory: a `memfd` it draws into and the
//! compositor reads.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// Memory shared with the compositor.
#[derive(Debug)]
pub struct Shared {
    fd: OwnedFd,
    address: *mut core::ffi::c_void,
    len: usize,
}

impl Shared {
    /// Make `len` bytes of anonymous memory and map it.
    ///
    /// # Errors
    ///
    /// Whatever the call said.
    pub fn new(len: usize) -> io::Result<Self> {
        let name = c"pattern";
        #[expect(
            unsafe_code,
            reason = "AUDIT: memfd_create is not in std; the name is a literal with its NUL and the flags are constants"
        )]
        // SAFETY: `name` is a NUL-terminated literal that outlives the call.
        let raw = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        #[expect(
            unsafe_code,
            reason = "AUDIT: memfd_create just returned this descriptor and nothing else in this process holds it"
        )]
        // SAFETY: as the comment says.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let size = i64::try_from(len).map_err(|_| io::Error::other("too large"))?;
        #[expect(
            unsafe_code,
            reason = "AUDIT: ftruncate is not in std for a raw descriptor; it sets the length of a memfd this process just made"
        )]
        // SAFETY: `fd` is this process's own memfd.
        let sized = unsafe { libc::ftruncate(fd.as_raw_fd(), size) };
        if sized < 0 {
            return Err(io::Error::last_os_error());
        }
        #[expect(
            unsafe_code,
            reason = "AUDIT: mmap is not in std; it maps a descriptor this process owns at a length it chose, and the result is checked against MAP_FAILED"
        )]
        // SAFETY: a null hint lets the kernel choose the address.
        let address = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, address, len })
    }

    /// The descriptor to send the compositor.
    #[must_use]
    pub fn as_raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }

    /// The memory, to draw into.
    #[must_use]
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        #[expect(
            unsafe_code,
            reason = "AUDIT: the mapping is this value's own and lives exactly as long as it, so the slice cannot outlive it"
        )]
        // SAFETY: `address` is a mapping of `len` bytes made by `Self::new`
        // and unmapped only by `Drop`.
        unsafe {
            core::slice::from_raw_parts_mut(self.address.cast::<u8>(), self.len)
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        #[expect(
            unsafe_code,
            reason = "AUDIT: munmap of exactly the address and length this value mapped, once, from Drop"
        )]
        // SAFETY: `address` and `len` are what `Self::new` mapped.
        unsafe {
            let _ = libc::munmap(self.address, self.len);
        }
    }
}
