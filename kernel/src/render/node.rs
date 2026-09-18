//! `/dev/dri/renderD<N>` as a program opens it: the virtgpu ioctls
//! `docs/GPU.md` step 2 answers.
//!
//! The node is the *device-independent* layer of §3.3's seam, as
//! `drm_gem.c` is on Linux: the inode, the generic ioctls, the handle table
//! and an object's lifetime. Nothing here knows virgl or virtio. What a resource's format words mean is the renderer's
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
//! driver said in its HELLO, so neither costs a message.
//!
//! `RESOURCE_CREATE` makes an object on the device and puts it in this
//! open's handle table; `RESOURCE_INFO` reads that table back. They are the
//! first two calls that cost a message, and the first that leave anything
//! behind.
//!
//! What is not answered yet, and what is in the way of each:
//!
//! * **A handle released on purpose.** `DRM_IOCTL_GEM_CLOSE` is the call for
//!   it and its number is not in [`ferrix_linux_abi::drm`], which takes
//!   every number from a committed probe. Adding it means running
//!   `probe/drm.sh` on a Linux host. Until then an open's objects go when
//!   the open does, which [`RenderFile::drop`] does do.
//! * **`MAP`.** Blocked on the protocol, not on this: `MAKE_OBJ` carries a
//!   size and no backing, so an object has no guest pages to map.
//! * **`GET_CAPS`.** The core knows a capability set's number, not its
//!   bytes; the driver read those.
//! * **`CONTEXT_INIT`, `EXECBUFFER`, the transfers and `WAIT`**, which is
//!   why an object made here is in no context yet.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;

use ferrix_linux_abi::drm::{self, Version};
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::socket::Width;
use ferrix_linux_abi::virtgpu::{self, Field, GetParam, Layout, ResourceCreate, ResourceInfo};
use ferrix_renderctl::message::{Status, flags};
use ferrix_renderctl::session::RequestError;
use ferrix_vfs::{Inode, Metadata, Result as VfsResult};

use super::{RenderError, Renderer};
use crate::sync::SpinLock;
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

/// One object this open has a handle for.
#[derive(Clone, Copy, Debug)]
struct Handle {
    /// What a program calls it: `bo_handle`, small and this open's alone.
    handle: u32,
    /// What the core and the device call it. It is also the `res_handle`
    /// answered to a program: the driver names the device's resource by the
    /// core's object id, so the two are one number and not two.
    object: u32,
    /// How many bytes it was made with.
    bytes: u32,
}

/// One open of a render node.
///
/// It holds the renderer rather than the device: an open outlives nothing,
/// and a renderer whose driver has gone answers `ENODEV`.
///
/// The handle table is the open's, which is why a render node takes any
/// number of opens: two programs' `bo_handle` 1 are different objects, and
/// neither can name the other's (`docs/GPU.md` §3.3).
pub(crate) struct RenderFile {
    renderer: Arc<Renderer>,
    handles: SpinLock<Handles>,
}

/// An open's handle table.
struct Handles {
    live: Vec<Handle>,
    /// The next `bo_handle` to hand out. Handles count from 1: zero is "no
    /// object" in every call that takes one.
    next: u32,
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
        Ok(Arc::new(RenderFile {
            renderer,
            handles: SpinLock::new(Handles {
                live: Vec::new(),
                next: 1,
            }),
        }))
    }
}

impl Drop for RenderFile {
    /// Let go of every object this open made, without waiting: the replies go
    /// to the renderer's task, which drops them. The same bargain a card's
    /// open makes, and for the same reason -- a close does not wait on a
    /// device.
    fn drop(&mut self) {
        let objects: Vec<u32> = self
            .handles
            .get_mut()
            .live
            .iter()
            .map(|held| held.object)
            .collect();
        self.renderer.release(&objects);
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
        virtgpu::IOCTL_RESOURCE_CREATE => resource_create(process, file, arg),
        virtgpu::IOCTL_RESOURCE_INFO => resource_info(process, file, arg),
        _ => Err(Errno::ENOTTY),
    }
}

/// What a failed request answers a program.
fn errno_of(error: RenderError) -> Errno {
    match error {
        // No room for another object, which is the one request failure that
        // is about this device being full rather than about the call.
        RenderError::Request(RequestError::Full) => Errno::ENOSPC,
        RenderError::Request(_) => Errno::EINVAL,
        // The device was asked and said no. `ENOMEM` when it said so, and
        // `EINVAL` for a resource it would not make, which is what Linux's
        // virtio-gpu answers for each.
        RenderError::Refused(Status::OutOfMemory) => Errno::ENOMEM,
        RenderError::Refused(_) => Errno::EINVAL,
        RenderError::Busy => Errno::EBUSY,
        RenderError::Gone => Errno::ENODEV,
        RenderError::TimedOut => Errno::ETIMEDOUT,
    }
}

/// `VIRTGPU_RESOURCE_CREATE`: make a resource, and a handle for it.
///
/// The caller's `target`, `format` and `bind` are virgl's words and are
/// passed down as bytes; what they say is the driver's business and this
/// side never reads them (`docs/GPU.md` §3.3). What this side decides is how
/// many bytes the object is, which is `size`.
///
/// `bo_handle` must be zero: attaching a resource to an object that already
/// exists is what a second resource on one buffer needs, and nothing here
/// makes one yet.
///
/// **Only the three words cross the seam.** `MAKE_OBJ` carries a size and a
/// description, and the description `user/gpu` reads is target, format and
/// bind -- so `width`, `height`, `depth`, `array_size`, `last_level`,
/// `nr_samples` and `stride` are *not* carried, and the driver shapes every
/// resource as `size` by 1 by 1. That is a buffer. A caller asking for a
/// texture gets an object of the right size and the wrong shape, which is
/// why nothing asks for one yet: carrying the shape is a change to the
/// description both sides read, and belongs with the transfers that would
/// first need it.
fn resource_create(process: &Process, file: &RenderFile, arg: u64) -> Result<usize, Errno> {
    let mut bytes = vec![0u8; ResourceCreate::SIZE];
    uaccess::copy_from_user(process.space(), arg, &mut bytes).map_err(|_| Errno::EFAULT)?;
    let mut create = ResourceCreate::read(&bytes).ok_or(Errno::EFAULT)?;
    if create.bo_handle != 0 {
        return Err(Errno::EINVAL);
    }
    if create.size == 0 {
        return Err(Errno::EINVAL);
    }
    // An object is made outside any context: a context is `CONTEXT_INIT`'s to
    // set up, and that call is not answered yet, so there is none to put it
    // in. The driver attaches a resource to a context when it is told one.
    let object = file
        .renderer
        .make_object(
            0,
            u64::from(create.size),
            flags::TO_DEVICE | flags::FROM_DEVICE,
            [create.target, create.format, create.bind],
        )
        .map_err(errno_of)?;
    let handle = {
        let mut handles = file.handles.lock();
        let handle = handles.next;
        handles.next = handles.next.saturating_add(1);
        handles.live.push(Handle {
            handle,
            object,
            bytes: create.size,
        });
        handle
    };
    create.bo_handle = handle;
    create.res_handle = object;
    create.write(&mut bytes).ok_or(Errno::EFAULT)?;
    // The object is made and the handle is this open's; a program that
    // cannot be told its number still has both, and `EFAULT` here would
    // leave it no way to name them. The write above is the only failure
    // this can have, and it is the caller's own pointer that caused it.
    uaccess::copy_to_user(process.space(), arg, &bytes).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}

/// `VIRTGPU_RESOURCE_INFO`: what is behind a handle.
///
/// Answered from the open's own table rather than by asking the driver: the
/// three things a program asks for here were all settled when the object was
/// made, and a question the driver would have to be woken for is a question
/// answered slowly for no reason.
fn resource_info(process: &Process, file: &RenderFile, arg: u64) -> Result<usize, Errno> {
    let mut bytes = vec![0u8; ResourceInfo::SIZE];
    uaccess::copy_from_user(process.space(), arg, &mut bytes).map_err(|_| Errno::EFAULT)?;
    let mut info = ResourceInfo::read(&bytes).ok_or(Errno::EFAULT)?;
    let held = file
        .handles
        .lock()
        .live
        .iter()
        .find(|held| held.handle == info.bo_handle)
        .copied()
        .ok_or(Errno::ENOENT)?;
    info.res_handle = held.object;
    info.size = held.bytes;
    // Not a blob resource: `PARAM_RESOURCE_BLOB` says the device offers
    // none, so nothing here can be one.
    info.blob_mem = 0;
    info.write(&mut bytes).ok_or(Errno::EFAULT)?;
    uaccess::copy_to_user(process.space(), arg, &bytes).map_err(|_| Errno::EFAULT)?;
    Ok(0)
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
