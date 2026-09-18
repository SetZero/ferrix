//! The render node: `/dev/dri/renderD128`, as a client of the GPU opens it.
//!
//! The card next door is the screen; this is where the GPU is
//! (`docs/GPU.md` §3.3). A compositor that draws on the GPU opens both: the
//! card to show a frame, the render node to make one. What is here says the
//! node is real -- who is driving it and what it can do -- and then makes one
//! resource and asks about it, which is the first thing that costs the
//! device a message rather than being answered from the driver's HELLO.
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
use ferrix_linux_abi::virtgpu::{self, GetParam, Layout, ResourceCreate, ResourceInfo};

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

    /// Make a resource of `size` bytes, and answer its object handle and the
    /// resource behind it.
    ///
    /// The target, format and bind words are virgl's and go to the driver
    /// untouched; a plain buffer is what they say here, because a buffer is
    /// the one shape `MAKE_OBJ` carries today. The shape fields are left at
    /// what a buffer means -- one row, one layer, no mip levels.
    ///
    /// # Errors
    ///
    /// Whatever the node said; `ENODEV` when the driver has gone.
    pub fn create_resource(&self, size: u32) -> io::Result<(u32, u32)> {
        let mut request = ResourceCreate {
            target: PIPE_BUFFER,
            format: VIRGL_FORMAT_R8_UNORM,
            bind: VIRGL_BIND_VERTEX_BUFFER,
            width: size,
            height: 1,
            depth: 1,
            array_size: 1,
            last_level: 0,
            nr_samples: 0,
            flags: 0,
            bo_handle: 0,
            res_handle: 0,
            size,
            stride: size,
        };
        self.ioctl(virtgpu::IOCTL_RESOURCE_CREATE, &mut request)?;
        Ok((request.bo_handle, request.res_handle))
    }

    /// What `VIRTGPU_RESOURCE_INFO` says is behind object `handle`: its
    /// resource and its size.
    ///
    /// # Errors
    ///
    /// Whatever the node said; `ENOENT` for a handle this open has not got.
    pub fn resource_info(&self, handle: u32) -> io::Result<(u32, u32)> {
        let mut request = ResourceInfo {
            bo_handle: handle,
            res_handle: 0,
            size: 0,
            blob_mem: 0,
        };
        self.ioctl(virtgpu::IOCTL_RESOURCE_INFO, &mut request)?;
        Ok((request.res_handle, request.size))
    }
}

/// virgl's `PIPE_BUFFER`: a resource with no shape, which is what bytes on
/// their way to a shader are. From Mesa's `p_defines.h`, as `user/gpu` takes
/// it.
const PIPE_BUFFER: u32 = 0;

/// virgl's `VIRGL_FORMAT_R8_UNORM`: one byte a pixel, which is how a
/// buffer's bytes are counted.
const VIRGL_FORMAT_R8_UNORM: u32 = 64;

/// virgl's `VIRGL_BIND_VERTEX_BUFFER`. virgl numbers some of its bind bits
/// differently from Mesa's `PIPE_BIND_*`, so it is taken from virgl's header.
const VIRGL_BIND_VERTEX_BUFFER: u32 = 1 << 4;

/// How big the resource the probe asks for is: one page, which is enough to
/// be a real resource on the device and small enough to cost nothing. The
/// same size the kernel's own proof uses.
const PROBE_BYTES: u32 = 4096;

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
    format!(
        "{MARKER} renderD128 {driver} 3d {three_d} capsets 0x{capsets:x} {}",
        object(&node)
    )
}

/// What making one resource and asking about it said: `object <handle>/<res>
/// of <size> bytes`, or `object none <why>`.
///
/// This is the half of the line that costs the device a message. Everything
/// before it is answered from the driver's HELLO, so a node that names a
/// driver proves only that the core was told about one; a resource proves
/// the whole path -- the node's handle table, the core's session, the driver
/// and the device.
///
/// The resource is let go of when the node closes, which is the only way an
/// open has to let go of one today.
fn object(node: &Render) -> String {
    let (handle, resource) = match node.create_resource(PROBE_BYTES) {
        Ok(made) => made,
        Err(error) => return format!("object none create failed: {}", reason(&error)),
    };
    match node.resource_info(handle) {
        // The node answers its own table, so a disagreement here is the
        // table being wrong rather than the device saying something else.
        Ok((told, size)) if told == resource && size == PROBE_BYTES => {
            format!("object {handle}/{resource} of {size} bytes")
        }
        Ok((told, size)) => {
            format!("object none info said {told}/{size}, not {resource}/{PROBE_BYTES}")
        }
        Err(error) => format!("object none info failed: {}", reason(&error)),
    }
}

/// An error as one word and its message, with no newline in it.
fn reason(error: &io::Error) -> String {
    error.to_string().replace('\n', " ")
}
