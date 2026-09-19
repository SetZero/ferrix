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
//! # What is answered
//!
//! `DRM_IOCTL_VERSION`, which names the driver, `VIRTGPU_GETPARAM`, which
//! says what the device can do, and `VIRTGPU_GET_CAPS`, the capability set
//! itself. All three are answered from what the core learned before it
//! published the renderer, so none costs a message.
//!
//! `RESOURCE_CREATE` makes an object on the device, with a backing of its
//! own, and puts it in this open's handle table; `RESOURCE_INFO` reads that
//! table back and `MAP` names the backing for `mmap`. `TRANSFER_TO_HOST` and
//! `TRANSFER_FROM_HOST` move bytes between the backing and the device's copy,
//! and `EXECBUFFER` runs a command stream, whose bytes are the renderer's
//! own language and are never read here.
//!
//! An open has one context, made the first time it is needed, as Linux makes
//! one for a device that has no `CONTEXT_INIT`: what one program draws and
//! the resources it may name are apart from every other's.
//!
//! `DRM_IOCTL_GEM_CLOSE` lets a handle go, and an open that closes lets go
//! of the rest.
//!
//! What is not answered, and what is in the way of each:
//!
//! * **`WAIT` that waits.** The driver does not offer fences, so nothing
//!   says when the GPU has *finished* a stream rather than taken it. A
//!   transfer from the device is ordered after every stream before it, which
//!   is the one place this path needs to know, so `WAIT` answers at once.
//! * **`CONTEXT_INIT` and blob resources**, which `GETPARAM` says are not
//!   offered.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;

use ferrix_linux_abi::drm::{self, GemClose, PrimeHandle, Version};
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::socket::Width;
use ferrix_linux_abi::types;
use ferrix_linux_abi::virtgpu::{
    self, ExecBuffer, Field, GetCaps, GetParam, Layout, Map, ResourceCreate, ResourceInfo,
    TransferToHost, Wait,
};
use ferrix_renderctl::message::{Direction, Region, Status, Transfer, flags};
use ferrix_renderctl::session::RequestError;
use ferrix_vfs::{Inode, Metadata, Result as VfsResult};

use super::{COMMAND_BYTES, RenderError, Renderer};
use crate::sync::SpinLock;
use crate::syscall::process::Process;
use crate::syscall::uaccess;
use crate::user::vmo::Vmo;

/// How far up a `VIRTGPU_MAP` offset the handle is: the low half is a place
/// in the object, which no object is too big for, and the high half is
/// which object. An offset is a name here, not a place in a file, as it is
/// on Linux, where the numbers come out of a fake-offset allocator instead.
const MAP_SHIFT: u32 = 32;

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

/// An object of the renderer, and everything about it that outlives the
/// handle a program names it by.
///
/// Held by whatever names it -- an open's handle table, a descriptor
/// [`export`] made -- and let go of when the last of them goes. That is what
/// lets a program hand its drawn frame to the card and then close the
/// handle, without the resource going while the screen is showing it.
pub(crate) struct Object {
    renderer: Arc<Renderer>,
    /// What the core and the device call it. It is also the `res_handle`
    /// answered to a program: the driver names the device's resource by the
    /// core's object id, so the two are one number and not two.
    id: u32,
    /// The shape it was made with, which is what a card needs to show it.
    width: u32,
    height: u32,
    stride: u32,
    /// How many bytes of backing it was made with.
    bytes: u32,
    /// That backing, which `mmap` maps and the driver pinned for the device.
    backing: Option<Arc<Vmo>>,
}

impl core::fmt::Debug for Object {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Object")
            .field("id", &self.id)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl Object {
    /// What the device calls it, and its shape: what a card is told to show
    /// it by.
    pub(crate) const fn shown(&self) -> (u32, u32, u32, u32) {
        (self.id, self.width, self.height, self.stride)
    }
}

impl Drop for Object {
    /// Give it back to the device, without waiting: the reply goes to the
    /// renderer's task, which drops it. The same bargain a close makes.
    fn drop(&mut self) {
        self.renderer.release(&[self.id], None);
    }
}

/// One object this open has a handle for.
#[derive(Clone, Debug)]
struct Handle {
    /// What a program calls it: `bo_handle`, small and this open's alone.
    handle: u32,
    /// The object itself, which outlives this handle if anything else names
    /// it.
    object: Arc<Object>,
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
    /// This open's context on the device, once something has needed one.
    context: SpinLock<Option<u32>>,
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
            context: SpinLock::new(None),
        }))
    }

    /// This open's context, made now if it has none.
    ///
    /// Two threads of one program may both find none and both make one; the
    /// second is given straight back. Making one sleeps, so the lock cannot
    /// be held across it.
    fn context(&self) -> Result<u32, Errno> {
        if let Some(context) = *self.context.lock() {
            return Ok(context);
        }
        let made = self.renderer.make_context().map_err(errno_of)?;
        let mut held = self.context.lock();
        match *held {
            Some(first) => {
                drop(held);
                self.renderer.release(&[], Some(made));
                Ok(first)
            }
            None => {
                *held = Some(made);
                Ok(made)
            }
        }
    }

    /// What is behind `handle`, if this open has it.
    fn held(&self, handle: u32) -> Result<Handle, Errno> {
        self.handles
            .lock()
            .live
            .iter()
            .find(|held| held.handle == handle)
            .cloned()
            .ok_or(Errno::ENOENT)
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
            .map(|held| held.object.id)
            .collect();
        // The objects go with the handles, which is what dropping them does;
        // what is left is the context, which the core takes away once they
        // have. Nothing is said about the objects here, so a handle another
        // descriptor still names keeps its object.
        let _ = objects;
        self.renderer.release(&[], *self.context.get_mut());
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

    /// The backing of the object a `VIRTGPU_MAP` offset names.
    fn mapping_at(&self, offset: u64) -> Option<(Arc<dyn Any + Send + Sync>, u64)> {
        let handle = u32::try_from(offset >> MAP_SHIFT).ok()?;
        let backing = self.held(handle).ok()?.object.backing.clone()?;
        let object: Arc<dyn Any + Send + Sync> = backing;
        Some((object, offset & ((1 << MAP_SHIFT) - 1)))
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
        virtgpu::IOCTL_MAP => map(process, file, arg),
        virtgpu::IOCTL_TRANSFER_TO_HOST => transfer(process, file, arg, Direction::ToDevice),
        virtgpu::IOCTL_TRANSFER_FROM_HOST => transfer(process, file, arg, Direction::FromDevice),
        virtgpu::IOCTL_EXECBUFFER => exec_buffer(process, file, arg),
        virtgpu::IOCTL_WAIT => wait(process, file, arg),
        virtgpu::IOCTL_GET_CAPS => get_caps(process, file, arg),
        drm::IOCTL_GEM_CLOSE => gem_close(process, file, arg),
        drm::IOCTL_PRIME_HANDLE_TO_FD => export(process, file, arg),
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
/// The caller's words -- target, format, bind and the resource's shape --
/// are virgl's and are passed down as bytes; what they say is the driver's
/// business and this side never reads them (`docs/GPU.md` §3.3). What this
/// side decides is how many bytes of backing the object has, which is
/// `size`, as it is on Linux: a caller that will never move bytes to or from
/// a resource asks for a page and the device's copy is as big as its shape
/// says regardless.
///
/// `bo_handle` must be zero: attaching a resource to an object that already
/// exists is what a second resource on one buffer needs, and nothing here
/// makes one.
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
    let context = file.context()?;
    let (object, backing) = file
        .renderer
        .make_object(
            context,
            u64::from(create.size),
            flags::MAPPABLE | flags::TO_DEVICE | flags::FROM_DEVICE,
            [
                create.target,
                create.format,
                create.bind,
                create.width,
                create.height,
                create.depth,
                create.array_size,
                create.last_level,
                create.nr_samples,
                create.flags,
            ],
        )
        .map_err(errno_of)?;
    let held = Arc::new(Object {
        renderer: Arc::clone(&file.renderer),
        id: object,
        width: create.width,
        height: create.height,
        stride: create.stride,
        bytes: create.size,
        backing,
    });
    let handle = {
        let mut handles = file.handles.lock();
        let handle = handles.next;
        handles.next = handles.next.saturating_add(1);
        handles.live.push(Handle {
            handle,
            object: held,
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

/// `VIRTGPU_MAP`: the offset to `mmap` this node at to reach an object's
/// backing.
fn map(process: &Process, file: &RenderFile, arg: u64) -> Result<usize, Errno> {
    let mut bytes = vec![0u8; Map::SIZE];
    uaccess::copy_from_user(process.space(), arg, &mut bytes).map_err(|_| Errno::EFAULT)?;
    let mut map = Map::read(&bytes).ok_or(Errno::EFAULT)?;
    if file.held(map.handle)?.object.backing.is_none() {
        return Err(Errno::EINVAL);
    }
    map.offset = u64::from(map.handle) << MAP_SHIFT;
    map.write(&mut bytes).ok_or(Errno::EFAULT)?;
    uaccess::copy_to_user(process.space(), arg, &bytes).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}

/// `VIRTGPU_TRANSFER_TO_HOST` and `VIRTGPU_TRANSFER_FROM_HOST`: move bytes
/// between an object's backing and the device's copy, and wait until they
/// have moved.
///
/// The two structures are one layout, which a test in `libs/linux-abi`
/// holds them to, so one reader serves both.
fn transfer(
    process: &Process,
    file: &RenderFile,
    arg: u64,
    direction: Direction,
) -> Result<usize, Errno> {
    let mut bytes = vec![0u8; TransferToHost::SIZE];
    uaccess::copy_from_user(process.space(), arg, &mut bytes).map_err(|_| Errno::EFAULT)?;
    let asked = TransferToHost::read(&bytes).ok_or(Errno::EFAULT)?;
    let held = file.held(asked.bo_handle)?;
    let context = file.context()?;
    file.renderer
        .transfer(Transfer {
            object: held.object.id,
            context,
            direction,
            level: asked.level,
            offset: u64::from(asked.offset),
            region: Region {
                x: asked.r#box.x,
                y: asked.r#box.y,
                z: asked.r#box.z,
                width: asked.r#box.w,
                height: asked.r#box.h,
                depth: asked.r#box.d,
            },
            stride: asked.stride,
            layer_stride: asked.layer_stride,
        })
        .map_err(errno_of)?;
    Ok(0)
}

/// `VIRTGPU_EXECBUFFER`: run a command stream in this open's context.
///
/// The stream is copied once, into the core, and from there into the work
/// VMO: a program's memory is not something a driver in another process can
/// be pointed at. No fence comes in or goes out, and no ring is named: the
/// flags that ask for those are refused, as `GETPARAM` said they would be.
/// `bo_handles` is a hint on Linux -- which objects the stream touches, for
/// fencing them -- and with no fences to hang on them it is not read.
fn exec_buffer(process: &Process, file: &RenderFile, arg: u64) -> Result<usize, Errno> {
    let mut bytes = vec![0u8; ExecBuffer::SIZE];
    uaccess::copy_from_user(process.space(), arg, &mut bytes).map_err(|_| Errno::EFAULT)?;
    let exec = ExecBuffer::read(&bytes).ok_or(Errno::EFAULT)?;
    if exec.flags != 0 || exec.num_in_syncobjs != 0 || exec.num_out_syncobjs != 0 {
        return Err(Errno::EINVAL);
    }
    // A stream is words, and one longer than a slot is not split: only its
    // writer knows where a command ends.
    if exec.size == 0 || !exec.size.is_multiple_of(4) || u64::from(exec.size) > COMMAND_BYTES {
        return Err(Errno::EINVAL);
    }
    let mut commands = vec![0u8; exec.size as usize];
    uaccess::copy_from_user(process.space(), exec.command, &mut commands)
        .map_err(|_| Errno::EFAULT)?;
    let context = file.context()?;
    file.renderer.submit(context, &commands).map_err(errno_of)?;
    Ok(0)
}

/// `VIRTGPU_WAIT`: wait until an object is idle.
///
/// Answered at once for an object this open has; see the module's note on
/// why that is honest here and what would make it wait.
fn wait(process: &Process, file: &RenderFile, arg: u64) -> Result<usize, Errno> {
    let mut bytes = vec![0u8; Wait::SIZE];
    uaccess::copy_from_user(process.space(), arg, &mut bytes).map_err(|_| Errno::EFAULT)?;
    let wait = Wait::read(&bytes).ok_or(Errno::EFAULT)?;
    let _ = file.held(wait.handle)?;
    Ok(0)
}

/// `VIRTGPU_GET_CAPS`: the capability set, as the device gave it.
///
/// As many bytes as the caller has room for, which is how Linux answers it:
/// a renderer built against an older, shorter set reads the front of a
/// newer one. A set the driver's streams are not in is `EINVAL`.
fn get_caps(process: &Process, file: &RenderFile, arg: u64) -> Result<usize, Errno> {
    let mut bytes = vec![0u8; GetCaps::SIZE];
    uaccess::copy_from_user(process.space(), arg, &mut bytes).map_err(|_| Errno::EFAULT)?;
    let asked = GetCaps::read(&bytes).ok_or(Errno::EFAULT)?;
    let (capset, caps) = file.renderer.caps();
    if asked.cap_set_id != capset || caps.is_empty() {
        return Err(Errno::EINVAL);
    }
    let given = caps
        .get(..caps.len().min(asked.size as usize))
        .unwrap_or(&[]);
    if asked.addr == 0 {
        return Err(Errno::EFAULT);
    }
    uaccess::copy_to_user(process.space(), asked.addr, given).map_err(|_| Errno::EFAULT)?;
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
    let held = file.held(info.bo_handle)?;
    info.res_handle = held.object.id;
    info.size = held.object.bytes;
    // Not a blob resource: `PARAM_RESOURCE_BLOB` says the device offers
    // none, so nothing here can be one.
    info.blob_mem = 0;
    info.write(&mut bytes).ok_or(Errno::EFAULT)?;
    uaccess::copy_to_user(process.space(), arg, &bytes).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}

/// What `/proc/self/fd` calls an exported buffer object.
const EXPORTED_NAME: &[u8] = b"anon_inode:[dmabuf]";

/// A buffer object as a descriptor: what `DRM_IOCTL_PRIME_HANDLE_TO_FD`
/// answers, and what the card's `FD_TO_HANDLE` takes.
///
/// A dmabuf on Linux, and the same job here: a name for the object that
/// another node of the card can be given, which holds the object alive for
/// as long as it is held. What it is *not* is a buffer another process can
/// map or another device can read -- there is one device, and the whole of
/// what this carries between the two nodes is which resource the device
/// already holds.
#[derive(Debug)]
pub(crate) struct Exported {
    object: Arc<Object>,
}

impl Exported {
    /// The object it names.
    pub(crate) fn object(&self) -> Arc<Object> {
        Arc::clone(&self.object)
    }
}

impl Inode for Exported {
    fn metadata(&self) -> Metadata {
        crate::fs::anon::metadata()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn is_stream(&self) -> bool {
        true
    }

    /// Nothing is read from it: it is a name for an object and not a stream
    /// of bytes, which is what Linux's dmabuf answers `read` for too.
    fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> VfsResult<usize> {
        Err(Errno::EINVAL)
    }
}

/// The open render node an exported object came from, if `file` is one.
pub(crate) fn exported(io: &Arc<dyn Inode>) -> Option<Arc<Exported>> {
    Arc::clone(io).into_any().downcast::<Exported>().ok()
}

/// `DRM_IOCTL_PRIME_HANDLE_TO_FD`: a buffer object as a descriptor.
///
/// What a compositor does with it is give it to the card, which shows what
/// was drawn without the pixels ever leaving the device (`docs/GPU.md` §3.5
/// piece 6). The descriptor holds the object alive, so the handle may be
/// closed afterwards, as it may on Linux.
///
/// `DRM_RDWR` is taken and ignored: there is nothing to read or write
/// through it. `DRM_CLOEXEC` is the only flag that means anything here.
fn export(process: &Process, file: &RenderFile, arg: u64) -> Result<usize, Errno> {
    let mut bytes = vec![0u8; PrimeHandle::SIZE];
    uaccess::copy_from_user(process.space(), arg, &mut bytes).map_err(|_| Errno::EFAULT)?;
    let mut prime = PrimeHandle::read(&bytes).ok_or(Errno::EFAULT)?;
    if prime.flags & !(types::O_CLOEXEC | types::O_RDWR) != 0 {
        return Err(Errno::EINVAL);
    }
    let held = file.held(prime.handle)?;
    let exported = Arc::new(Exported {
        object: held.object,
    });
    let open = crate::fs::anon::open(exported, EXPORTED_NAME, false)?;
    let descriptor = process
        .files()
        .lock()
        .insert(open, prime.flags & types::O_CLOEXEC != 0)?;
    prime.fd = descriptor;
    prime.write(&mut bytes).ok_or(Errno::EFAULT)?;
    uaccess::copy_to_user(process.space(), arg, &bytes).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}

/// `DRM_IOCTL_GEM_CLOSE`: let a handle go.
///
/// The object goes with it, without waiting for the device: the same bargain
/// a close makes, and Linux's `drm_gem_close_ioctl` does not wait either. A
/// handle this open has not got is `EINVAL`, as Linux answers one.
///
/// Whatever the device does with the resource, the *handle* is gone here, so
/// a program that closes what it no longer draws with can go on making
/// objects for as long as it runs. Without this a compositor's textures
/// accumulated for the length of a session.
fn gem_close(process: &Process, file: &RenderFile, arg: u64) -> Result<usize, Errno> {
    let mut bytes = vec![0u8; GemClose::SIZE];
    uaccess::copy_from_user(process.space(), arg, &mut bytes).map_err(|_| Errno::EFAULT)?;
    let close = GemClose::read(&bytes).ok_or(Errno::EFAULT)?;
    let object = {
        let mut handles = file.handles.lock();
        let at = handles
            .live
            .iter()
            .position(|held| held.handle == close.handle)
            .ok_or(Errno::EINVAL)?;
        handles.live.swap_remove(at)
    };
    // The object goes when the last thing naming it does, which is here
    // unless a descriptor [`export`] made still holds it.
    drop(object);
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
