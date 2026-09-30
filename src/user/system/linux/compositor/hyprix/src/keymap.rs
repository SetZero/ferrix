//! The keymap file every `wl_keyboard` is handed.
//!
//! `wl_keyboard.keymap` carries a descriptor and a length; the client maps it
//! read-only and compiles the text with libxkbcommon. So the compositor needs
//! a file holding the keymap `input:kb_layout` asked for and a descriptor
//! onto it, and the same one goes to every client -- which is what
//! libwayland's own compositors do, since the file is read-only and each
//! client maps its own copy.
//!
//! The file is a `memfd`, sealed against every change, as Smithay's
//! `SealedFile` makes one (`utils/sealed_file.rs`). A client that maps a file
//! the compositor could still shrink has to guard against `SIGBUS`; a sealed
//! one cannot be shrunk, and the seal is the compositor's promise of that.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// The keymap file.
#[derive(Debug)]
pub struct Keymap {
    fd: OwnedFd,
    size: u32,
}

impl Keymap {
    /// Write the keymap into a sealed `memfd`.
    ///
    /// # Errors
    ///
    /// Whatever the call said. Sealing that fails is not an error: the client
    /// then guards against a shrink it will never see, which costs it a
    /// signal handler and nothing else.
    pub fn new(keymap: &str) -> io::Result<Self> {
        let text = keymap.as_bytes();
        // The length a client is told counts the terminating NUL, which the
        // text does not carry: `xkb_keymap_new_from_string` reads a C string.
        let size = u32::try_from(text.len() + 1).map_err(|_| io::Error::other("too large"))?;

        let name = c"hyprix-keymap";
        #[expect(
            unsafe_code,
            reason = "AUDIT: memfd_create is not in std; the name is a literal with its NUL and the flags are constants"
        )]
        // SAFETY: `name` is a NUL-terminated literal that outlives the call.
        let raw = unsafe {
            libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING)
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        #[expect(
            unsafe_code,
            reason = "AUDIT: memfd_create just returned this descriptor and nothing else in this process holds it"
        )]
        // SAFETY: as the comment says.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };

        write_all(&fd, text)?;
        write_all(&fd, &[0])?;

        // Seal it: no shrinking, no growing, no writing, and no further
        // seals. A failure leaves a working keymap that is merely not
        // promised to stay put.
        let seals =
            libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
        #[expect(
            unsafe_code,
            reason = "AUDIT: fcntl is not in std for a raw descriptor; F_ADD_SEALS takes an integer and changes nothing else"
        )]
        // SAFETY: `fd` is this process's own memfd.
        let _ = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, seals) };

        Ok(Self { fd, size })
    }

    /// The descriptor to send, and the length to send with it.
    #[must_use]
    pub fn handed(&self) -> (compositor_wire::Fd, u32) {
        (compositor_wire::Fd(self.fd.as_raw_fd()), self.size)
    }
}

/// Write the whole of `bytes`, which a `memfd` never refuses in part but
/// which `write` is allowed to do.
fn write_all(fd: &OwnedFd, bytes: &[u8]) -> io::Result<()> {
    let mut at = 0;
    while at < bytes.len() {
        let rest = bytes.get(at..).unwrap_or_default();
        #[expect(
            unsafe_code,
            reason = "AUDIT: write is not in std for a raw descriptor; the pointer and length are one slice's"
        )]
        // SAFETY: `rest` is a live slice of exactly the length passed.
        let written = unsafe { libc::write(fd.as_raw_fd(), rest.as_ptr().cast(), rest.len()) };
        if written < 0 {
            return Err(io::Error::last_os_error());
        }
        if written == 0 {
            return Err(io::Error::other("the keymap file would take no more"));
        }
        at = at.saturating_add(usize::try_from(written).unwrap_or(0));
    }
    Ok(())
}
