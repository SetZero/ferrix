//! The render node: `/dev/dri/renderD128`, as a client of the GPU opens it.
//!
//! The card next door is the screen; this is where the GPU is
//! (`docs/GPU.md` §3.3). A compositor that draws on the GPU opens both: the
//! card to show a frame, the render node to make one. What is here is only
//! as much as says the node is real -- who is driving it and what it can do
//! -- because the calls that make an object need a handle table the kernel
//! does not have yet.
//!
//! # Why this reports rather than fails
//!
//! A card with no GPU behind it has no render node, and that is not an
//! error: it is the ordinary 2D case, and the display test boots it that
//! way on purpose. So the probe answers a line either way, and whoever is
//! judging decides which line it should have been. That is what makes the
//! two boots a pair -- with the 3D device the node must be there, without it
//! must not -- rather than a check that passes whatever happens.

use std::ffi::CStr;
use std::io;

use ferrix_linux_abi::drm::{self, Version};
use ferrix_linux_abi::socket::Width;
use ferrix_linux_abi::virtgpu::{self, GetParam, Layout};

/// What the display test reads this program's render line by. Deliberately
/// not the compositor's own prefix: that one is what the boot is watched
/// for, and a second line carrying it would be taken for the scanout's.
const MARKER: &str = "render:";

/// The first render node. Linux numbers them from 128, and so does Ferrix.
const NODE: &CStr = c"/dev/dri/renderD128";

/// The longest driver name read back from `DRM_IOCTL_VERSION`.
const NAME_BYTES: usize = 64;

/// An open render node.
pub struct Render {
    fd: libc::c_int,
}

impl core::fmt::Debug for Render {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Render")
            .field("fd", &self.fd)
            .finish()
    }
}

impl Drop for Render {
    fn drop(&mut self) {
        // SAFETY: the descriptor is this object's own and is open.
        let _ = unsafe { libc::close(self.fd) };
    }
}

impl Render {
    /// Open the render node.
    ///
    /// # Errors
    ///
    /// Whatever `open` said; `ENOENT` when the card has no GPU behind it,
    /// which is the ordinary answer for a 2D device.
    pub fn open() -> io::Result<Self> {
        // SAFETY: NODE is a NUL-terminated path; the flags are constants.
        let fd = unsafe { libc::open(NODE.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd })
    }

    /// The width this program's structures are laid out at.
    fn width() -> Width {
        if size_of::<usize>() == 4 {
            Width::Bits32
        } else {
            Width::Bits64
        }
    }

    /// Run `request` with `value` as its argument, and read the answer back
    /// into it. The same shape as the card's, for the structures that are
    /// one size at every width.
    fn ioctl<L: Layout>(&self, request: u32, value: &mut L) -> io::Result<()> {
        let mut bytes = vec![0u8; L::SIZE];
        value
            .write(&mut bytes)
            .ok_or_else(|| io::Error::other("a structure larger than its buffer"))?;
        // SAFETY: `bytes` is a live buffer of exactly the size the request's
        // number encodes, which the kernel reads and writes within.
        let result = unsafe { libc::ioctl(self.fd, request as _, bytes.as_mut_ptr()) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        *value = L::read(&bytes).ok_or_else(|| io::Error::other("a short answer"))?;
        Ok(())
    }

    /// Who is driving this node, from `DRM_IOCTL_VERSION`.
    ///
    /// This is what userspace picks a back end by, exactly as on Linux:
    /// `virtio_gpu` speaks virgl, and another name would speak something
    /// else. `struct drm_version` carries pointers and lengths, so it is one
    /// size on a 64-bit program and another on a 32-bit one, and it is
    /// written and read at the width this program was built for.
    ///
    /// # Errors
    ///
    /// Whatever the node said.
    pub fn driver(&self) -> io::Result<String> {
        let width = Self::width();
        let mut name = [0u8; NAME_BYTES];
        let version = Version {
            version_major: 0,
            version_minor: 0,
            version_patchlevel: 0,
            name_len: name.len() as u64,
            name: name.as_mut_ptr() as usize as u64,
            date_len: 0,
            date: 0,
            desc_len: 0,
            desc: 0,
        };
        let mut bytes = vec![0u8; Version::size(width)];
        version
            .write(width, &mut bytes)
            .ok_or_else(|| io::Error::other("a structure larger than its buffer"))?;
        // SAFETY: `bytes` is a live buffer of the size the request's number
        // encodes, and the one pointer in it is to `name`, which outlives
        // the call and whose length is beside it.
        let result =
            unsafe { libc::ioctl(self.fd, drm::ioctl_version(width) as _, bytes.as_mut_ptr()) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        let answered =
            Version::read(width, &bytes).ok_or_else(|| io::Error::other("a short answer"))?;
        let len = usize::try_from(answered.name_len)
            .unwrap_or(usize::MAX)
            .min(name.len());
        Ok(String::from_utf8_lossy(name.get(..len).unwrap_or(&[])).into_owned())
    }

    /// What `VIRTGPU_GETPARAM` says about `param`.
    ///
    /// The answer goes to an address the request carries, not back into the
    /// structure, so the `u64` it is written into outlives the call here.
    ///
    /// # Errors
    ///
    /// Whatever the node said; `EINVAL` for a parameter it does not know.
    pub fn param(&self, param: u64) -> io::Result<u64> {
        let mut answer: u64 = 0;
        let mut request = GetParam {
            param,
            value: (&raw mut answer) as usize as u64,
        };
        self.ioctl(virtgpu::IOCTL_GETPARAM, &mut request)?;
        Ok(answer)
    }
}

/// One line saying what the render node is, or that there is none.
///
/// Never an error: see the module's note. The line is
/// `render: renderD128 <driver> 3d <n> capsets 0x<mask>`, or
/// `render: none <why>`.
#[must_use]
pub fn probe() -> String {
    let node = match Render::open() {
        Ok(node) => node,
        Err(error) => return format!("{MARKER} none {}", reason(&error)),
    };
    let driver = match node.driver() {
        Ok(driver) if !driver.is_empty() => driver,
        Ok(_) => return format!("{MARKER} none the node named no driver"),
        Err(error) => return format!("{MARKER} none version failed: {}", reason(&error)),
    };
    // What the node says it can do. A parameter it does not know is an
    // error rather than a zero, so each is reported as it answered.
    let three_d = node.param(virtgpu::PARAM_3D_FEATURES).unwrap_or(0);
    let capsets = node.param(virtgpu::PARAM_SUPPORTED_CAPSET_IDS).unwrap_or(0);
    format!("{MARKER} renderD128 {driver} 3d {three_d} capsets 0x{capsets:x}")
}

/// An error as one word and its message, with no newline in it.
fn reason(error: &io::Error) -> String {
    error.to_string().replace('\n', " ")
}
