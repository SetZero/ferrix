//! `/dev/dri/renderD<N>` as a program opens it: the virtgpu ioctls
//! `docs/GPU.md` step 2 answers.
//!
//! The node is the *device-independent* layer of §3.3's seam, as
//! `drm_gem.c` is on Linux: the inode, the generic ioctls, and -- when it is
//! written -- the handle table and an object's lifetime. Nothing here knows
//! virgl or virtio. What a resource's format words mean is the renderer's
//! own language, and it passes through as bytes; what userspace learns from
//! this node is the driver's *name*, which is how it picks a back end, on
//! Ferrix exactly as on Linux.
//!
//! # Many opens, unlike a card
//!
//! [`crate::display::drm::CardFile`] allows one open at a time, a written
//! deviation from Linux. A render node does not: Linux's render nodes exist
//! precisely so that every GL client opens one of its own without being the
//! display's master, and §3.3 puts the handle table at the open for that
//! reason. So this refuses nobody, and what an open owns it owns alone.
//!
//! # What is answered so far
//!
//! `DRM_IOCTL_VERSION`, which names the driver, and `VIRTGPU_GETPARAM`,
//! which says what the device can do. Both are answered from what the
//! driver said in its HELLO, so neither costs a message. `RESOURCE_CREATE`
//! and the rest need an object of the core's and come next; `GET_CAPS`
//! needs the capability set's bytes, which the core does not hold -- it
//! knows the set's number, and the driver read the bytes.

use alloc::sync::Arc;
use alloc::vec;
use core::any::Any;

use ferrix_linux_abi::drm::{self, Version};
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::socket::Width;
use ferrix_linux_abi::virtgpu::{self, Field, GetParam, Layout};
use ferrix_vfs::{Inode, Metadata, Result as VfsResult};

use super::Renderer;
use crate::syscall::process::Process;
use crate::syscall::uaccess;

/// The width this kernel's programs use, which is the width its structures
/// are read and written at.
const NATIVE: Width = if size_of::<usize>() == 8 {
    Width::Bits64
} else {
    Width::Bits32
};

/// What `DRM_IOCTL_VERSION` reports beside the driver's own name.
const VERSION_MAJOR: i32 = 0;
const VERSION_MINOR: i32 = 1;
const VERSION_PATCH: i32 = 0;

/// One open of a render node.
///
/// It holds the renderer rather than the device: an open outlives nothing,
/// and a renderer whose driver has gone answers `ENODEV`.
pub(crate) struct RenderFile {
    renderer: Arc<Renderer>,
}

impl core::fmt::Debug for RenderFile {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RenderFile")
            .field("renderer", &self.renderer.index)
            .finish_non_exhaustive()
    }
}

impl RenderFile {
    /// Open `renderer`. Any number of opens may hold one.
    pub(crate) fn open(renderer: Arc<Renderer>) -> Result<Arc<RenderFile>, Errno> {
        if renderer.is_gone() {
            return Err(Errno::ENXIO);
        }
        Ok(Arc::new(RenderFile { renderer }))
    }
}

impl Inode for RenderFile {
    fn metadata(&self) -> Metadata {
        self.renderer.metadata()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn is_stream(&self) -> bool {
        true
    }

    /// A render node carries no byte stream: everything it does is an
    /// ioctl, and Linux answers a read of one with `EINVAL`.
    fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> VfsResult<usize> {
        Err(Errno::EINVAL)
    }
}

/// The open render node `file` reads and writes go to, if it is one.
pub(crate) fn of(io: &Arc<dyn Inode>) -> Option<Arc<RenderFile>> {
    Arc::clone(io).into_any().downcast::<RenderFile>().ok()
}

/// The number `renderD<N>` names, with no leading zero.
///
/// Linux numbers render nodes from 128, and so does the core; a name below
/// that is not one, which keeps `renderD0` from meaning anything.
pub(crate) fn render_number(name: &[u8]) -> Option<u32> {
    let digits = name.strip_prefix(b"renderD")?;
    if digits.is_empty() || digits.first() == Some(&b'0') {
        return None;
    }
    let number: u32 = core::str::from_utf8(digits).ok()?.parse().ok()?;
    (number >= 128).then_some(number)
}

/// Answer `request` on the open render node `file`, or `ENOTTY` for one this
/// subset does not have.
pub(crate) fn ioctl(
    process: &Process,
    file: &RenderFile,
    request: u32,
    arg: u64,
) -> Result<usize, Errno> {
    if file.renderer.is_gone() {
        return Err(Errno::ENODEV);
    }
    match request {
        request if request == drm::ioctl_version(NATIVE) => version(process, file, arg),
        virtgpu::IOCTL_GETPARAM => get_param(process, file, arg),
        _ => Err(Errno::ENOTTY),
    }
}

/// `DRM_IOCTL_VERSION`: who is driving this node.
///
/// The name is the driver's own, from its HELLO, because that is what
/// userspace picks a back end by -- `virtio_gpu` here, something else for
/// the card §4 describes. The three lengths are answered whether or not
/// there was room for the text, which is how a caller asks how much room to
/// make.
fn version(process: &Process, file: &RenderFile, arg: u64) -> Result<usize, Errno> {
    let mut bytes = vec![0u8; Version::size(NATIVE)];
    uaccess::copy_from_user(process.space(), arg, &mut bytes).map_err(|_| Errno::EFAULT)?;
    let mut version = Version::read(NATIVE, &bytes).ok_or(Errno::EFAULT)?;
    let give = |at: u64, capacity: u64, text: &[u8]| -> Result<u64, Errno> {
        let len = text.len() as u64;
        if at != 0 && capacity > 0 {
            let copied = text.get(..capacity.min(len) as usize).unwrap_or(text);
            uaccess::copy_to_user(process.space(), at, copied).map_err(|_| Errno::EFAULT)?;
        }
        Ok(len)
    };
    version.version_major = VERSION_MAJOR;
    version.version_minor = VERSION_MINOR;
    version.version_patchlevel = VERSION_PATCH;
    version.name_len = give(
        version.name,
        version.name_len,
        file.renderer.name().as_bytes(),
    )?;
    version.date_len = give(version.date, version.date_len, b"0")?;
    version.desc_len = give(version.desc, version.desc_len, b"virtio GPU")?;
    version.write(NATIVE, &mut bytes).ok_or(Errno::EFAULT)?;
    uaccess::copy_to_user(process.space(), arg, &bytes).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}

/// `VIRTGPU_GETPARAM`: what this device can do.
///
/// Answered from the HELLO rather than by asking the driver: these are
/// properties of the device the core was told about when it accepted the
/// conversation, and a question the driver would have to be woken for is a
/// question answered slowly for no reason.
///
/// A parameter this version does not know is `EINVAL`, as Linux answers one.
fn get_param(process: &Process, file: &RenderFile, arg: u64) -> Result<usize, Errno> {
    let mut bytes = vec![0u8; GetParam::SIZE];
    uaccess::copy_from_user(process.space(), arg, &mut bytes).map_err(|_| Errno::EFAULT)?;
    let param = GetParam::read(&bytes).ok_or(Errno::EFAULT)?;
    let value: u64 = match param.param {
        // There is a renderer at all only because the device has 3D behind
        // it: the driver offers no render conversation for a plain scanout.
        virtgpu::PARAM_3D_FEATURES => 1,
        // The core asks for a capability set by its number, which is the
        // fixed query this parameter stands for.
        virtgpu::PARAM_CAPSET_QUERY_FIX => 1,
        // A bitmask, one bit per set. The core knows the one set the
        // driver named in its HELLO and claims no more than that; the
        // device may well have others, which the driver read and the core
        // was never told about.
        virtgpu::PARAM_SUPPORTED_CAPSET_IDS => 1_u64 << file.renderer.capset(),
        // Blob resources, host-visible memory, sharing across devices and
        // the rest are not offered yet. They are the protocol's to carry
        // before they are the node's to answer.
        virtgpu::PARAM_RESOURCE_BLOB
        | virtgpu::PARAM_HOST_VISIBLE
        | virtgpu::PARAM_CROSS_DEVICE
        | virtgpu::PARAM_CONTEXT_INIT
        | virtgpu::PARAM_EXPLICIT_DEBUG_NAME => 0,
        _ => return Err(Errno::EINVAL),
    };
    // The answer goes where the caller's pointer says, not into the
    // structure: `value` is a user address.
    if param.value == 0 {
        return Err(Errno::EFAULT);
    }
    uaccess::copy_to_user(process.space(), param.value, &value.to_le_bytes())
        .map_err(|_| Errno::EFAULT)?;
    Ok(0)
}
