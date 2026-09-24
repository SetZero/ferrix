//! virtio-gpu: the ioctls, constants and structures a 3D client uses on
//! `/dev/dri/renderD<N>`.
//!
//! This is the render node's side of `docs/GPU.md`'s seam. [`crate::drm`] is
//! the display: modes, framebuffers and page flips on `/dev/dri/card0`, which
//! every DRM driver answers the same way. These are one driver's own ioctls,
//! numbered from `DRM_COMMAND_BASE`, and a card never answers them.
//!
//! # Where the numbers come from
//!
//! `include/uapi/drm/virtgpu_drm.h`, which every architecture takes
//! unchanged. `probe/virtgpu.c` prints every number and layout below from
//! that header, natively for 64-bit and under `qemu-arm` for ARMv7-A, into
//! `probe/virtgpu-64.txt` and `probe/virtgpu-32.txt`; the tests read both
//! files and require this module to agree with every line.
//!
//! # No structure here has a width
//!
//! Unlike [`crate::drm::Version`], which holds `size_t` lengths and `char *`
//! pointers and so differs between the widths, every structure below carries
//! its user pointers as `__u64` and its sizes as `__u32`. All of them are the
//! same size with the same offsets on x86-64 and on ARMv7-A, which is why
//! none of them takes a [`crate::socket::Width`], and the tests assert that
//! the two probe files are identical rather than listing exceptions.
//!
//! # Reading and writing
//!
//! Every structure is read from and written into the bytes the ioctl's
//! argument points at, little-endian, returning `None` rather than panicking
//! when the buffer is short. What the fields mean is the device's business;
//! nothing here validates a value.

use crate::layout::layout;
pub use crate::layout::{Field, Layout};

// ---------------------------------------------------------------------------
// ioctls
// ---------------------------------------------------------------------------

/// `DRM_IOCTL_VIRTGPU_MAP`: ask for the offset to `mmap` an object at.
pub const IOCTL_MAP: u32 = 0xC010_6441;
/// `DRM_IOCTL_VIRTGPU_EXECBUFFER`: submit a command buffer to a context.
pub const IOCTL_EXECBUFFER: u32 = 0xC040_6442;
/// `DRM_IOCTL_VIRTGPU_GETPARAM`: ask what the device supports.
pub const IOCTL_GETPARAM: u32 = 0xC010_6443;
/// `DRM_IOCTL_VIRTGPU_RESOURCE_CREATE`: create a 3D resource and its object.
pub const IOCTL_RESOURCE_CREATE: u32 = 0xC038_6444;
/// `DRM_IOCTL_VIRTGPU_RESOURCE_INFO`: the resource behind an object.
pub const IOCTL_RESOURCE_INFO: u32 = 0xC010_6445;
/// `DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST`: read a resource back.
pub const IOCTL_TRANSFER_FROM_HOST: u32 = 0xC02C_6446;
/// `DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST`: send a resource's contents over.
pub const IOCTL_TRANSFER_TO_HOST: u32 = 0xC02C_6447;
/// `DRM_IOCTL_VIRTGPU_WAIT`: wait until an object is idle.
pub const IOCTL_WAIT: u32 = 0xC008_6448;
/// `DRM_IOCTL_VIRTGPU_GET_CAPS`: read a capability set.
pub const IOCTL_GET_CAPS: u32 = 0xC018_6449;
/// `DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB`: create a blob resource. The
/// number is written down so the node can tell it apart from a call it has
/// not heard of; blob resources are later.
pub const IOCTL_RESOURCE_CREATE_BLOB: u32 = 0xC030_644A;
/// `DRM_IOCTL_VIRTGPU_CONTEXT_INIT`: set a context's parameters.
pub const IOCTL_CONTEXT_INIT: u32 = 0xC010_644B;

// ---------------------------------------------------------------------------
// What GETPARAM answers about
// ---------------------------------------------------------------------------

/// `VIRTGPU_PARAM_3D_FEATURES`: the device does 3D at all.
pub const PARAM_3D_FEATURES: u64 = 1;
/// `VIRTGPU_PARAM_CAPSET_QUERY_FIX`.
pub const PARAM_CAPSET_QUERY_FIX: u64 = 2;
/// `VIRTGPU_PARAM_RESOURCE_BLOB`.
pub const PARAM_RESOURCE_BLOB: u64 = 3;
/// `VIRTGPU_PARAM_HOST_VISIBLE`.
pub const PARAM_HOST_VISIBLE: u64 = 4;
/// `VIRTGPU_PARAM_CROSS_DEVICE`.
pub const PARAM_CROSS_DEVICE: u64 = 5;
/// `VIRTGPU_PARAM_CONTEXT_INIT`: contexts take parameters.
pub const PARAM_CONTEXT_INIT: u64 = 6;
/// `VIRTGPU_PARAM_SUPPORTED_CAPSET_IDs`, spelled with that lowercase `s` in
/// the header.
pub const PARAM_SUPPORTED_CAPSET_IDS: u64 = 7;
/// `VIRTGPU_PARAM_EXPLICIT_DEBUG_NAME`.
pub const PARAM_EXPLICIT_DEBUG_NAME: u64 = 8;

// ---------------------------------------------------------------------------
// Capability sets
// ---------------------------------------------------------------------------

/// `VIRTGPU_DRM_CAPSET_VIRGL`: virgl's first capability set.
pub const CAPSET_VIRGL: u32 = 1;
/// `VIRTGPU_DRM_CAPSET_VIRGL2`: virgl's second.
pub const CAPSET_VIRGL2: u32 = 2;

// ---------------------------------------------------------------------------
// Context parameters
// ---------------------------------------------------------------------------

/// `VIRTGPU_CONTEXT_PARAM_CAPSET_ID`: which capability set the context is.
pub const CONTEXT_PARAM_CAPSET_ID: u64 = 1;
/// `VIRTGPU_CONTEXT_PARAM_NUM_RINGS`: how many command rings it has.
pub const CONTEXT_PARAM_NUM_RINGS: u64 = 2;
/// `VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK`: which rings a poll reports.
pub const CONTEXT_PARAM_POLL_RINGS_MASK: u64 = 3;
/// `VIRTGPU_CONTEXT_PARAM_DEBUG_NAME`: a name for the host's logs.
pub const CONTEXT_PARAM_DEBUG_NAME: u64 = 4;

// ---------------------------------------------------------------------------
// What a submission asks for
// ---------------------------------------------------------------------------

/// `VIRTGPU_WAIT_NOWAIT`: answer whether the object is idle rather than
/// wait for it, `EBUSY` for not yet.
pub const WAIT_NOWAIT: u32 = 1;

/// `VIRTGPU_EXECBUF_FENCE_FD_IN`: wait on `fence_fd` before running.
pub const EXECBUF_FENCE_FD_IN: u32 = 1;
/// `VIRTGPU_EXECBUF_FENCE_FD_OUT`: return a fence in `fence_fd`.
pub const EXECBUF_FENCE_FD_OUT: u32 = 2;
/// `VIRTGPU_EXECBUF_RING_IDX`: `ring_idx` says which ring to run on.
pub const EXECBUF_RING_IDX: u32 = 4;

// ---------------------------------------------------------------------------
// Layouts
// ---------------------------------------------------------------------------

layout! {
    /// `struct drm_virtgpu_getparam`: `VIRTGPU_GETPARAM`'s argument.
    GetParam = "drm_virtgpu_getparam", 16 {
        /// Which parameter, a `PARAM_*`.
        param: u64 = 0 / "param",
        /// The user address of the `u64` the device writes the answer into.
        value: u64 = 8 / "value",
    }
}

layout! {
    /// `struct drm_virtgpu_context_init`: `VIRTGPU_CONTEXT_INIT`'s argument.
    ContextInit = "drm_virtgpu_context_init", 16 {
        /// How many parameters `ctx_set_params` points at.
        num_params: u32 = 0 / "num_params",
        /// Padding, zero.
        pad: u32 = 4 / "pad",
        /// The user address of that many [`ContextSetParam`].
        ctx_set_params: u64 = 8 / "ctx_set_params",
    }
}

layout! {
    /// `struct drm_virtgpu_context_set_param`: one of [`ContextInit`]'s.
    ContextSetParam = "drm_virtgpu_context_set_param", 16 {
        /// Which parameter, a `CONTEXT_PARAM_*`.
        param: u64 = 0 / "param",
        /// What to set it to.
        value: u64 = 8 / "value",
    }
}

layout! {
    /// `struct drm_virtgpu_resource_create`: `VIRTGPU_RESOURCE_CREATE`'s
    /// argument. The device fills in `bo_handle` and `res_handle`.
    ResourceCreate = "drm_virtgpu_resource_create", 56 {
        /// The virgl target, such as a buffer or a 2D texture.
        target: u32 = 0 / "target",
        /// The virgl format.
        format: u32 = 4 / "format",
        /// What the resource may be bound as.
        bind: u32 = 8 / "bind",
        /// Width in pixels, or bytes for a buffer.
        width: u32 = 12 / "width",
        /// Height in pixels.
        height: u32 = 16 / "height",
        /// Depth.
        depth: u32 = 20 / "depth",
        /// How many array layers.
        array_size: u32 = 24 / "array_size",
        /// The last mip level.
        last_level: u32 = 28 / "last_level",
        /// How many samples.
        nr_samples: u32 = 32 / "nr_samples",
        /// Resource flags.
        flags: u32 = 36 / "flags",
        /// An existing object to attach to, or zero for a new one.
        bo_handle: u32 = 40 / "bo_handle",
        /// The resource, filled in by the device.
        res_handle: u32 = 44 / "res_handle",
        /// How many bytes of memory to back it with.
        size: u32 = 48 / "size",
        /// Bytes per row, which the host validates transfers against.
        stride: u32 = 52 / "stride",
    }
}

layout! {
    /// `struct drm_virtgpu_resource_info`: `VIRTGPU_RESOURCE_INFO`'s
    /// argument.
    ResourceInfo = "drm_virtgpu_resource_info", 16 {
        /// The object asked about.
        bo_handle: u32 = 0 / "bo_handle",
        /// Its resource, filled in by the device.
        res_handle: u32 = 4 / "res_handle",
        /// Its size in bytes.
        size: u32 = 8 / "size",
        /// Which kind of blob memory it is, or zero.
        blob_mem: u32 = 12 / "blob_mem",
    }
}

layout! {
    /// `struct drm_virtgpu_map`: `VIRTGPU_MAP`'s argument. The offset comes
    /// back to be passed to `mmap`, and is not an address.
    Map = "drm_virtgpu_map", 16 {
        /// The offset to `mmap` at, filled in by the device.
        offset: u64 = 0 / "offset",
        /// The object to map.
        handle: u32 = 8 / "handle",
        /// Padding, zero.
        pad: u32 = 12 / "pad",
    }
}

layout! {
    /// `struct drm_virtgpu_get_caps`: `VIRTGPU_GET_CAPS`'s argument.
    GetCaps = "drm_virtgpu_get_caps", 24 {
        /// Which capability set, a `CAPSET_*`.
        cap_set_id: u32 = 0 / "cap_set_id",
        /// Which version of it.
        cap_set_ver: u32 = 4 / "cap_set_ver",
        /// The user address to write it to.
        addr: u64 = 8 / "addr",
        /// How many bytes are there to write into.
        size: u32 = 16 / "size",
        /// Padding, zero.
        pad: u32 = 20 / "pad",
    }
}

layout! {
    /// `struct drm_virtgpu_execbuffer`: `VIRTGPU_EXECBUFFER`'s argument.
    ExecBuffer = "drm_virtgpu_execbuffer", 64 {
        /// `EXECBUF_*`.
        flags: u32 = 0 / "flags",
        /// How many bytes of commands.
        size: u32 = 4 / "size",
        /// The user address of the command buffer.
        command: u64 = 8 / "command",
        /// The user address of an array of object handles.
        bo_handles: u64 = 16 / "bo_handles",
        /// How many handles are in it.
        num_bo_handles: u32 = 24 / "num_bo_handles",
        /// A fence to wait on or return, by `flags`. Signed: `-1` is none.
        fence_fd: i32 = 28 / "fence_fd",
        /// Which command ring, when `EXECBUF_RING_IDX` is set.
        ring_idx: u32 = 32 / "ring_idx",
        /// The size of one sync object entry.
        syncobj_stride: u32 = 36 / "syncobj_stride",
        /// How many sync objects to wait on.
        num_in_syncobjs: u32 = 40 / "num_in_syncobjs",
        /// How many to signal.
        num_out_syncobjs: u32 = 44 / "num_out_syncobjs",
        /// The user address of the ones to wait on.
        in_syncobjs: u64 = 48 / "in_syncobjs",
        /// The user address of the ones to signal.
        out_syncobjs: u64 = 56 / "out_syncobjs",
    }
}

layout! {
    /// `struct drm_virtgpu_3d_box`: the region a transfer covers.
    Box3d = "drm_virtgpu_3d_box", 24 {
        /// Left.
        x: u32 = 0 / "x",
        /// Top.
        y: u32 = 4 / "y",
        /// Front.
        z: u32 = 8 / "z",
        /// Width.
        w: u32 = 12 / "w",
        /// Height.
        h: u32 = 16 / "h",
        /// Depth.
        d: u32 = 20 / "d",
    }
}

layout! {
    /// `struct drm_virtgpu_3d_transfer_to_host`: `VIRTGPU_TRANSFER_TO_HOST`'s
    /// argument.
    TransferToHost = "drm_virtgpu_3d_transfer_to_host", 44 {
        /// The object to send.
        bo_handle: u32 = 0 / "bo_handle",
        /// Which region of it.
        r#box: Box3d = 4 / "box",
        /// Which mip level.
        level: u32 = 28 / "level",
        /// Where in the object to start.
        offset: u32 = 32 / "offset",
        /// Bytes per row.
        stride: u32 = 36 / "stride",
        /// Bytes per layer.
        layer_stride: u32 = 40 / "layer_stride",
    }
}

layout! {
    /// `struct drm_virtgpu_3d_transfer_from_host`:
    /// `VIRTGPU_TRANSFER_FROM_HOST`'s argument.
    TransferFromHost = "drm_virtgpu_3d_transfer_from_host", 44 {
        /// The object to read back into.
        bo_handle: u32 = 0 / "bo_handle",
        /// Which region of it.
        r#box: Box3d = 4 / "box",
        /// Which mip level.
        level: u32 = 28 / "level",
        /// Where in the object to start.
        offset: u32 = 32 / "offset",
        /// Bytes per row.
        stride: u32 = 36 / "stride",
        /// Bytes per layer.
        layer_stride: u32 = 40 / "layer_stride",
    }
}

layout! {
    /// `struct drm_virtgpu_3d_wait`: `VIRTGPU_WAIT`'s argument.
    Wait = "drm_virtgpu_3d_wait", 8 {
        /// The object to wait on. Zero is not a handle.
        handle: u32 = 0 / "handle",
        /// Wait flags.
        flags: u32 = 4 / "flags",
    }
}
