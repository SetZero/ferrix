//! virtio-gpu's device protocol, 2D half: its configuration space, its feature
//! bits, the control commands a scanout needs, and the device's responses.
//!
//! Virtio 1.2 §5.7 defines the GPU device. What iteration 1 of the display
//! (`docs/DISPLAY.md`) needs of it is the 2D subset: ask which scanouts exist,
//! create a resource, give it guest pages as backing, point a scanout at it,
//! and tell the device which rectangle changed. Each of those is a command
//! here, encoded into bytes, and each response is parsed back. What drives a
//! device — the queue, the order of commands, what to do when it misbehaves —
//! is `ferrix-virtio-gpu`'s. The 3D commands and capability sets came with
//! `docs/GPU.md`'s Path A and the cursor queue's two commands ([`Cursor`])
//! with its §3.10, and blob resources, which Venus makes every host-visible
//! Vulkan allocation as, with §6.1. EDID is later.
//!
//! # A command is two buffers
//!
//! Every control command is a chain of one device-readable buffer holding the
//! request and one device-writable buffer the device writes its response
//! into. [`Command::len`] says how long the first must be and
//! [`Command::response_len`] how long the second; a response buffer shorter
//! than that makes QEMU fail the command.
//!
//! # Backing is pages, not a buffer
//!
//! A resource's backing is a list of `(device address, length)` entries. The
//! addresses are what `VMO_PIN_ADDRESSES` returned, one per page, with no
//! promise that page `i + 1` follows page `i`; [`backing_entries`] joins
//! pages into one entry only where their device addresses are consecutive, as
//! `blk::plan` does for a request.
//!
//! # Trust
//!
//! A response is the device's word. [`Response::parse`] refuses one shorter
//! than its header or than its type's body, one longer than the buffer it was
//! written into, a type virtio does not define, and a success of a kind the
//! command cannot have produced. Error responses are the device's to give and
//! are returned as [`GpuError::Device`]. Scanout rectangles are checked for
//! sizes a mode can have.

use core::fmt;

use crate::pci::{FEATURE_ACCESS_PLATFORM, FEATURE_VERSION_1};

/// The configuration-block reader, which every device class shares.
pub use crate::DeviceConfig;
/// The pin granularity a device address is given per, which every device
/// class shares.
pub use crate::PAGE_SIZE;

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Feature bits, virtio 1.2 §5.7.3, checked against QEMU 9.2.4's
// `include/standard-headers/linux/virtio_gpu.h`.
// ---------------------------------------------------------------------------

/// `VIRTIO_GPU_F_VIRGL`: 3D commands.
pub const FEATURE_VIRGL: u64 = 1 << 0;
/// `VIRTIO_GPU_F_EDID`: `GET_EDID`.
pub const FEATURE_EDID: u64 = 1 << 1;
/// `VIRTIO_GPU_F_RESOURCE_UUID`: `RESOURCE_ASSIGN_UUID`.
pub const FEATURE_RESOURCE_UUID: u64 = 1 << 2;
/// `VIRTIO_GPU_F_RESOURCE_BLOB`: blob resources.
pub const FEATURE_RESOURCE_BLOB: u64 = 1 << 3;
/// `VIRTIO_GPU_F_CONTEXT_INIT`: contexts with capability sets and timelines.
pub const FEATURE_CONTEXT_INIT: u64 = 1 << 4;

/// The features a 2D scanout driver accepts: the transport's
/// [`FEATURE_VERSION_1`], required, and [`FEATURE_ACCESS_PLATFORM`], without
/// which a device behind an IOMMU refuses `FEATURES_OK`. Every GPU feature is
/// declined, since each only matters to a driver that sends its commands.
pub const DRIVER_FEATURES: u64 = FEATURE_VERSION_1 | FEATURE_ACCESS_PLATFORM;

/// The features a driver that means to send 3D commands accepts:
/// [`DRIVER_FEATURES`] and the two that 3D needs.
///
/// Accepted rather than required. A device that offers neither is a 2D
/// card, and the driver that asked for 3D still drives it -- virtio's
/// negotiation is the driver naming what it *can* take -- so what came back
/// is what says whether there is a GPU behind this one. [`FEATURE_VIRGL`]
/// in the negotiated set is that answer.
///
/// Separate from [`DRIVER_FEATURES`] because accepting a feature changes
/// what the device does: QEMU brings virglrenderer up for a driver that
/// took [`FEATURE_VIRGL`], and a boot that is judged on the 2D path has no
/// business asking for that.
///
/// [`FEATURE_RESOURCE_BLOB`] is accepted with them, for Venus: a device
/// offers it only with `blob=on`, and granting it changes nothing a virgl
/// renderer does -- it only lets the driver send the blob commands.
pub const DRIVER_FEATURES_3D: u64 =
    DRIVER_FEATURES | FEATURE_VIRGL | FEATURE_CONTEXT_INIT | FEATURE_RESOURCE_BLOB;

/// The features without which the driver gives up.
pub const REQUIRED_FEATURES: u64 = FEATURE_VERSION_1;

/// The control queue's index.
pub const CONTROL_QUEUE: u16 = 0;
/// The cursor queue's index.
pub const CURSOR_QUEUE: u16 = 1;

// ---------------------------------------------------------------------------
// Configuration space, virtio 1.2 §5.7.4.
// ---------------------------------------------------------------------------

/// Offset of `events_read`.
pub const CONFIG_EVENTS_READ: u32 = 0;
/// Offset of `events_clear`, which the driver writes to acknowledge.
pub const CONFIG_EVENTS_CLEAR: u32 = 4;
/// Offset of `num_scanouts`.
pub const CONFIG_NUM_SCANOUTS: u32 = 8;
/// Offset of `num_capsets`.
pub const CONFIG_NUM_CAPSETS: u32 = 12;
/// Bytes of `struct virtio_gpu_config`.
pub const CONFIG_LEN: u32 = 16;

/// `VIRTIO_GPU_EVENT_DISPLAY`: the displays changed; send `GET_DISPLAY_INFO`
/// again.
pub const EVENT_DISPLAY: u32 = 1 << 0;

/// `VIRTIO_GPU_MAX_SCANOUTS`.
pub const MAX_SCANOUTS: usize = 16;

/// `struct virtio_gpu_config`, as read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    /// Pending events, [`EVENT_DISPLAY`].
    pub events_read: u32,
    /// How many scanouts the device has, 1 to [`MAX_SCANOUTS`].
    pub num_scanouts: u32,
    /// How many capability sets it has.
    pub num_capsets: u32,
}

impl Config {
    /// Read the block, refusing one too short or with a scanout count virtio
    /// does not allow.
    pub fn read<S: DeviceConfig + ?Sized>(source: &S) -> Result<Self, GpuError> {
        if source.config_len() < CONFIG_LEN {
            return Err(GpuError::ConfigTooShort(source.config_len()));
        }
        let config = Self {
            events_read: source.config_read32(CONFIG_EVENTS_READ),
            num_scanouts: source.config_read32(CONFIG_NUM_SCANOUTS),
            num_capsets: source.config_read32(CONFIG_NUM_CAPSETS),
        };
        if config.num_scanouts == 0 || config.num_scanouts as usize > MAX_SCANOUTS {
            return Err(GpuError::Scanouts(config.num_scanouts));
        }
        Ok(config)
    }
}

// ---------------------------------------------------------------------------
// Formats
// ---------------------------------------------------------------------------

/// `enum virtio_gpu_formats`: the byte order of a 2D resource's pixels, named
/// as they lie in memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Format {
    /// `B8G8R8A8_UNORM`.
    B8G8R8A8 = 1,
    /// `B8G8R8X8_UNORM`: DRM's `XRGB8888` on a little-endian machine, the
    /// one format the display creates.
    B8G8R8X8 = 2,
    /// `A8R8G8B8_UNORM`.
    A8R8G8B8 = 3,
    /// `X8R8G8B8_UNORM`.
    X8R8G8B8 = 4,
    /// `R8G8B8A8_UNORM`.
    R8G8B8A8 = 67,
    /// `X8B8G8R8_UNORM`.
    X8B8G8R8 = 68,
    /// `A8B8G8R8_UNORM`.
    A8B8G8R8 = 121,
    /// `R8G8B8X8_UNORM`.
    R8G8B8X8 = 134,
}

// ---------------------------------------------------------------------------
// The control header and commands, virtio 1.2 §5.7.6.
// ---------------------------------------------------------------------------

/// `VIRTIO_GPU_CMD_GET_DISPLAY_INFO`.
pub const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
/// `VIRTIO_GPU_CMD_RESOURCE_CREATE_2D`.
pub const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
/// `VIRTIO_GPU_CMD_RESOURCE_UNREF`.
pub const CMD_RESOURCE_UNREF: u32 = 0x0102;
/// `VIRTIO_GPU_CMD_SET_SCANOUT`.
pub const CMD_SET_SCANOUT: u32 = 0x0103;
/// `VIRTIO_GPU_CMD_RESOURCE_FLUSH`.
pub const CMD_RESOURCE_FLUSH: u32 = 0x0104;
/// `VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D`.
pub const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
/// `VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING`.
pub const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
/// `VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING`.
pub const CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;
/// `VIRTIO_GPU_CMD_GET_CAPSET_INFO`.
pub const CMD_GET_CAPSET_INFO: u32 = 0x0108;
/// `VIRTIO_GPU_CMD_GET_CAPSET`.
pub const CMD_GET_CAPSET: u32 = 0x0109;
/// `VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB`, which follows `GET_EDID` and
/// `RESOURCE_ASSIGN_UUID` in the 2D commands' numbering.
pub const CMD_RESOURCE_CREATE_BLOB: u32 = 0x010c;
/// `VIRTIO_GPU_CMD_CTX_CREATE`, the first of the 3D commands.
pub const CMD_CTX_CREATE: u32 = 0x0200;
/// `VIRTIO_GPU_CMD_CTX_DESTROY`.
pub const CMD_CTX_DESTROY: u32 = 0x0201;
/// `VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE`.
pub const CMD_CTX_ATTACH_RESOURCE: u32 = 0x0202;
/// `VIRTIO_GPU_CMD_CTX_DETACH_RESOURCE`.
pub const CMD_CTX_DETACH_RESOURCE: u32 = 0x0203;
/// `VIRTIO_GPU_CMD_RESOURCE_CREATE_3D`.
pub const CMD_RESOURCE_CREATE_3D: u32 = 0x0204;
/// `VIRTIO_GPU_CMD_TRANSFER_TO_HOST_3D`.
pub const CMD_TRANSFER_TO_HOST_3D: u32 = 0x0205;
/// `VIRTIO_GPU_CMD_TRANSFER_FROM_HOST_3D`.
pub const CMD_TRANSFER_FROM_HOST_3D: u32 = 0x0206;
/// `VIRTIO_GPU_CMD_SUBMIT_3D`.
pub const CMD_SUBMIT_3D: u32 = 0x0207;
/// `VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB`.
pub const CMD_RESOURCE_MAP_BLOB: u32 = 0x0208;
/// `VIRTIO_GPU_CMD_RESOURCE_UNMAP_BLOB`.
pub const CMD_RESOURCE_UNMAP_BLOB: u32 = 0x0209;

/// `VIRTIO_GPU_RESP_OK_NODATA`.
pub const RESP_OK_NODATA: u32 = 0x1100;
/// `VIRTIO_GPU_RESP_OK_DISPLAY_INFO`.
pub const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
/// `VIRTIO_GPU_RESP_OK_CAPSET_INFO`.
pub const RESP_OK_CAPSET_INFO: u32 = 0x1102;
/// `VIRTIO_GPU_RESP_OK_CAPSET`.
pub const RESP_OK_CAPSET: u32 = 0x1103;
/// `VIRTIO_GPU_RESP_OK_MAP_INFO`, after `OK_EDID` and `OK_RESOURCE_UUID`.
pub const RESP_OK_MAP_INFO: u32 = 0x1106;
/// `VIRTIO_GPU_RESP_ERR_UNSPEC`, the first error response.
pub const RESP_ERR_UNSPEC: u32 = 0x1200;
/// `VIRTIO_GPU_RESP_ERR_OUT_OF_MEMORY`.
pub const RESP_ERR_OUT_OF_MEMORY: u32 = 0x1201;
/// `VIRTIO_GPU_RESP_ERR_INVALID_SCANOUT_ID`.
pub const RESP_ERR_INVALID_SCANOUT_ID: u32 = 0x1202;
/// `VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID`.
pub const RESP_ERR_INVALID_RESOURCE_ID: u32 = 0x1203;
/// `VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID`.
pub const RESP_ERR_INVALID_CONTEXT_ID: u32 = 0x1204;
/// `VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER`.
pub const RESP_ERR_INVALID_PARAMETER: u32 = 0x1205;

/// Bytes of `struct virtio_gpu_ctrl_hdr`.
pub const HEADER_LEN: usize = 24;

/// `VIRTIO_GPU_FLAG_FENCE`: the device answers only once the command has
/// actually finished, and its response carries the `fence_id` back.
pub const FLAG_FENCE: u32 = 1 << 0;
/// `VIRTIO_GPU_FLAG_INFO_RING_IDX`: the header's `ring_idx` names which of
/// a context's timelines the fence is on, rather than the context's own.
pub const FLAG_INFO_RING_IDX: u32 = 1 << 1;

/// What goes in a command's header besides its type: which context it is
/// for, and whether the device must fence it.
///
/// A 2D driver needs none of this -- it sends one command, waits for the
/// response and sends the next -- and [`Context::NONE`] is that. A 3D
/// command belongs to a context, and one whose result another command
/// depends on is fenced, which is how a driver stops waiting for each one
/// in turn.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Context {
    /// The context id, or 0 for a command that belongs to none.
    pub id: u32,
    /// The fence to ask for, if any: the device answers when the command
    /// has finished rather than when it has been read, and echoes this
    /// number back in the response's header.
    pub fence: Option<u64>,
    /// Which timeline of the context the fence is on, for a device that
    /// negotiated [`FEATURE_CONTEXT_INIT`]. `None` is the context's own.
    pub ring: Option<u8>,
}

impl Context {
    /// No context and no fence: what every 2D command carries.
    pub const NONE: Self = Self {
        id: 0,
        fence: None,
        ring: None,
    };

    /// The header's `flags`.
    #[must_use]
    pub const fn flags(self) -> u32 {
        let mut flags = 0;
        if self.fence.is_some() {
            flags |= FLAG_FENCE;
        }
        if self.ring.is_some() {
            flags |= FLAG_INFO_RING_IDX;
        }
        flags
    }

    /// Write the header's fields after its type, which is the caller's.
    ///
    /// Every byte from 4 to [`HEADER_LEN`] is written exactly once, padding
    /// included, so a caller's buffer need not be cleared first.
    fn write_with(self, put: &mut dyn FnMut(usize, &[u8])) {
        put(4, &self.flags().to_le_bytes());
        put(8, &self.fence.unwrap_or(0).to_le_bytes());
        put(16, &self.id.to_le_bytes());
        put(20, &[self.ring.unwrap_or(0), 0, 0, 0]);
    }
}
/// Bytes of `struct virtio_gpu_rect`.
pub const RECT_LEN: usize = 16;
/// Bytes of `struct virtio_gpu_mem_entry`.
pub const MEM_ENTRY_LEN: usize = 16;
/// Bytes of one `struct virtio_gpu_display_one`.
pub const DISPLAY_ONE_LEN: usize = RECT_LEN + 8;
/// Bytes of `struct virtio_gpu_resp_display_info`.
pub const DISPLAY_INFO_LEN: usize = HEADER_LEN + MAX_SCANOUTS * DISPLAY_ONE_LEN;

/// The most backing entries one `RESOURCE_ATTACH_BACKING` may carry: QEMU
/// refuses more (`virtio_gpu_create_mapping_iov`), and so does this module
/// rather than let the device decide.
pub const MAX_BACKING_ENTRIES: usize = 16384;

/// The largest width or height a scanout may report or a resource have. QEMU
/// allows up to 16384 per side on its virtio-gpu; a device claiming more is
/// broken.
pub const MAX_DIMENSION: u32 = 16384;

/// `VIRTIO_GPU_CAPSET_VIRGL`: virgl's first capability set, OpenGL as
/// virglrenderer's original protocol carried it.
pub const CAPSET_VIRGL: u32 = 1;
/// `VIRTIO_GPU_CAPSET_VIRGL2`: the second, which is what a virglrenderer
/// built this decade offers and what a GL renderer asks for.
pub const CAPSET_VIRGL2: u32 = 2;
/// `VIRTIO_GPU_CAPSET_VENUS`: Vulkan over virtio-gpu, which needs a Linux
/// host with KVM (`docs/GPU.md` §3).
pub const CAPSET_VENUS: u32 = 4;
/// `VIRTIO_GPU_CAPSET_DRM`: the native-context capability set.
pub const CAPSET_DRM: u32 = 6;

/// `VIRTIO_GPU_BLOB_MEM_GUEST`: a blob whose memory is the guest's pages,
/// given as backing entries.
pub const BLOB_MEM_GUEST: u32 = 1;
/// `VIRTIO_GPU_BLOB_MEM_HOST3D`: a blob whose memory the host's renderer
/// allocates, named by the `blob_id` a context gave it. What Venus makes
/// every host-visible allocation and ring as.
pub const BLOB_MEM_HOST3D: u32 = 2;
/// `VIRTIO_GPU_BLOB_MEM_HOST3D_GUEST`: host renderer memory with guest pages
/// behind it.
pub const BLOB_MEM_HOST3D_GUEST: u32 = 3;

/// `VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE`: the blob may be mapped into the
/// host-visible window with [`Command::ResourceMapBlob`].
pub const BLOB_FLAG_USE_MAPPABLE: u32 = 1 << 0;
/// `VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE`: the blob may be shared with another
/// context, as a dmabuf is on Linux.
pub const BLOB_FLAG_USE_SHAREABLE: u32 = 1 << 1;
/// `VIRTIO_GPU_BLOB_FLAG_USE_CROSS_DEVICE`: the blob may be shared with
/// another virtio device.
pub const BLOB_FLAG_USE_CROSS_DEVICE: u32 = 1 << 2;

/// `VIRTIO_GPU_MAP_CACHE_MASK`: which of [`Response::MapInfo`]'s bits are
/// the caching.
pub const MAP_CACHE_MASK: u32 = 0x0f;
/// `VIRTIO_GPU_MAP_CACHE_NONE`: the device says nothing about caching.
pub const MAP_CACHE_NONE: u32 = 0x00;
/// `VIRTIO_GPU_MAP_CACHE_CACHED`: map it write-back, as ordinary memory.
pub const MAP_CACHE_CACHED: u32 = 0x01;
/// `VIRTIO_GPU_MAP_CACHE_UNCACHED`: map it uncached.
pub const MAP_CACHE_UNCACHED: u32 = 0x02;
/// `VIRTIO_GPU_MAP_CACHE_WC`: map it write-combining.
pub const MAP_CACHE_WC: u32 = 0x03;

/// `VIRTIO_GPU_SHM_ID_HOST_VISIBLE`: the shared memory region
/// [`Command::ResourceMapBlob`] places blobs in, which a PCI device offers
/// as a `VIRTIO_PCI_CAP_SHARED_MEMORY_CFG` capability with this id.
pub const SHM_ID_HOST_VISIBLE: u8 = 1;

/// Bytes of `virtio_gpu_resource_create_blob` before its backing entries.
pub const CREATE_BLOB_LEN: usize = HEADER_LEN + 32;
/// Bytes of `struct virtio_gpu_resp_map_info`.
pub const MAP_INFO_LEN: usize = HEADER_LEN + 8;

/// Bytes of `struct virtio_gpu_box`: a rectangle with a depth, which is what
/// a 3D transfer names instead of a [`Rect`].
pub const BOX_LEN: usize = 24;

/// `VIRTIO_GPU_RESOURCE_FLAG_Y_0_TOP`: row zero is the top of the resource
/// rather than the bottom, which is the way every other thing in this tree
/// counts rows and the opposite of OpenGL's.
pub const RESOURCE_FLAG_Y_0_TOP: u32 = 1 << 0;

/// Bytes of `virtio_gpu_ctx_create`'s `debug_name`, which is not a C string:
/// `nlen` says how much of it is the name.
pub const CONTEXT_NAME_LEN: usize = 64;

/// `VIRTIO_GPU_CONTEXT_INIT_CAPSET_ID_MASK`: which capability set a context
/// is for, in the low byte of `context_init`.
///
/// Only meaningful to a device that granted [`FEATURE_CONTEXT_INIT`]. To one
/// that did not, `context_init` is zero and the context is whatever the
/// device's single renderer is.
pub const CONTEXT_INIT_CAPSET_MASK: u32 = 0x0000_00ff;

/// The largest capability set this driver will ask a device for.
///
/// A capset is a blob of what the host's renderer can do, and the driver
/// has to find room for one before it asks: `GET_CAPSET_INFO` says how big,
/// and the driver believes it. virglrenderer's `virgl_caps_v2` is under two
/// kilobytes, so a device naming a size past this is either broken or
/// asking for an allocation no driver should make on its word.
pub const MAX_CAPSET_SIZE: u32 = 64 * 1024;

/// `struct virtio_gpu_rect`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    /// Left.
    pub x: u32,
    /// Top.
    pub y: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

impl Rect {
    /// A rectangle at the origin.
    #[must_use]
    pub const fn sized(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    fn decode(bytes: &[u8], at: usize) -> Option<Self> {
        Some(Self {
            x: get32(bytes, at)?,
            y: get32(bytes, at + 4)?,
            width: get32(bytes, at + 8)?,
            height: get32(bytes, at + 12)?,
        })
    }

    /// Whether the rectangle lies inside a `width` × `height` resource.
    #[must_use]
    pub fn fits(self, width: u32, height: u32) -> bool {
        self.x
            .checked_add(self.width)
            .is_some_and(|end| end <= width)
            && self
                .y
                .checked_add(self.height)
                .is_some_and(|end| end <= height)
    }
}

/// `struct virtio_gpu_box`: where in a 3D resource a transfer goes.
///
/// A 2D transfer names a [`Rect`]; a 3D one names a box, because a resource
/// may have depth, layers and mip levels. A flat texture is a box one deep.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Box3d {
    /// Left.
    pub x: u32,
    /// Top.
    pub y: u32,
    /// Front.
    pub z: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// Depth, which is 1 for a flat texture.
    pub depth: u32,
}

impl Box3d {
    /// A box covering a flat `width` x `height` texture.
    #[must_use]
    pub const fn flat(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            z: 0,
            width,
            height,
            depth: 1,
        }
    }
}

/// `struct virtio_gpu_mem_entry`: one run of backing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MemEntry {
    /// The run's device address.
    pub addr: u64,
    /// Its length in bytes.
    pub length: u32,
}

/// A control command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command<'a> {
    /// Ask for every scanout's mode.
    GetDisplayInfo,
    /// Create a 2D resource.
    ResourceCreate2d {
        /// The id to give it, not 0.
        resource_id: u32,
        /// Its pixel format.
        format: Format,
        /// Its width.
        width: u32,
        /// Its height.
        height: u32,
    },
    /// Ask what the capability set at `index` is: which renderer it
    /// belongs to, and how big a buffer [`Command::GetCapset`] needs.
    ///
    /// `index` counts from zero and runs to the `num_capsets` in the
    /// device's configuration, which is *not* the capset id: a device with
    /// one capset at index 0 may call it [`CAPSET_VIRGL2`].
    GetCapsetInfo {
        /// Which of the device's capability sets, counting from zero.
        index: u32,
    },
    /// Fetch a capability set itself: the blob saying what the host's
    /// renderer can do, which a GL driver reads before it builds anything.
    GetCapset {
        /// Which set, by the id [`Command::GetCapsetInfo`] gave.
        capset_id: u32,
        /// Which version of it, at most the `max_version` that gave.
        capset_version: u32,
        /// How many bytes the response buffer has for the set, which is the
        /// `max_size` that gave. The device writes that many.
        max_size: u32,
    },
    /// Make a rendering context, which every 3D command afterwards names in
    /// its header.
    ///
    /// The id is the header's, not a field here: it is the driver's to
    /// choose and the device's to remember, and [`Context`] is how every
    /// command carries it.
    CtxCreate {
        /// Which capability set this context is for, for a device that
        /// granted [`FEATURE_CONTEXT_INIT`]; 0 for its default renderer.
        capset: u8,
        /// A name for the context, which is for a person reading the host's
        /// log and nothing else. Longer than [`CONTEXT_NAME_LEN`] is
        /// refused rather than cut, since a name that is not the name is
        /// worse than none.
        name: &'a str,
    },
    /// Destroy a context and everything the device kept for it.
    CtxDestroy,
    /// Let a context use a resource. A 3D command may only name a resource
    /// its own context has been given.
    CtxAttachResource {
        /// The resource.
        resource_id: u32,
    },
    /// Take it away again.
    CtxDetachResource {
        /// The resource.
        resource_id: u32,
    },
    /// Create a 3D resource: a texture or a buffer the host's renderer owns,
    /// which is what a context draws into and reads from.
    ///
    /// The fields are virgl's own and this module does not interpret them:
    /// `target`, `format` and `bind` are the renderer's enumerations, not
    /// virtio's, and a driver takes them from the capability set rather than
    /// from here.
    ResourceCreate3d {
        /// The id to give it, not 0.
        resource_id: u32,
        /// virgl's `PIPE_TEXTURE_*`.
        target: u32,
        /// virgl's `PIPE_FORMAT_*`, which is *not* [`Format`].
        format: u32,
        /// virgl's `PIPE_BIND_*`: what the resource may be used as.
        bind: u32,
        /// Its size.
        size: Box3d,
        /// How many array layers.
        array_size: u32,
        /// The highest mip level.
        last_level: u32,
        /// How many samples, for a multisampled target.
        samples: u32,
        /// [`RESOURCE_FLAG_Y_0_TOP`], or none.
        flags: u32,
    },
    /// Copy guest memory into a 3D resource the host owns.
    TransferToHost3d {
        /// Which part of the resource.
        region: Box3d,
        /// Where in the resource's backing the bytes start.
        offset: u64,
        /// The resource.
        resource_id: u32,
        /// Which mip level.
        level: u32,
        /// Bytes a row, or 0 for the resource's own.
        stride: u32,
        /// Bytes a layer, or 0 for the resource's own.
        layer_stride: u32,
    },
    /// The same the other way: the host's resource into guest memory.
    TransferFromHost3d {
        /// Which part of the resource.
        region: Box3d,
        /// Where in the resource's backing the bytes go.
        offset: u64,
        /// The resource.
        resource_id: u32,
        /// Which mip level.
        level: u32,
        /// Bytes a row, or 0 for the resource's own.
        stride: u32,
        /// Bytes a layer, or 0 for the resource's own.
        layer_stride: u32,
    },
    /// Hand the host's renderer a command stream to run.
    ///
    /// The bytes are virgl's protocol, not virtio's: this carries them and
    /// says how many there are, and `compositor/virgl` is what will write
    /// them (`docs/GPU.md` step 3b).
    Submit3d {
        /// The stream.
        commands: &'a [u8],
    },
    /// [`Command::Submit3d`] less its stream: the header and the `size`,
    /// for a driver that hands the device the stream in buffers of their
    /// own, after this one in the chain, rather than copying it in.
    ///
    /// The device reads a request as one run of bytes however the chain
    /// splits it -- QEMU's `virgl_cmd_submit_3d` copies the stream out of
    /// the whole chain from the end of the header -- so the stream can stay
    /// where its writer put it.
    Submit3dHeader {
        /// Bytes of the stream that follows.
        size: u32,
    },
    /// Destroy a resource.
    ResourceUnref {
        /// The resource.
        resource_id: u32,
    },
    /// Show a rectangle of a resource on a scanout, or with resource 0, turn
    /// the scanout off.
    SetScanout {
        /// The part of the resource to show.
        rect: Rect,
        /// The scanout.
        scanout_id: u32,
        /// The resource, or 0.
        resource_id: u32,
    },
    /// Show what changed in a rectangle of a resource on every scanout
    /// showing it.
    ResourceFlush {
        /// The rectangle.
        rect: Rect,
        /// The resource.
        resource_id: u32,
    },
    /// Copy a rectangle of a resource's guest backing to the host's copy.
    TransferToHost2d {
        /// The rectangle.
        rect: Rect,
        /// The byte offset of the rectangle's first pixel in the backing.
        offset: u64,
        /// The resource.
        resource_id: u32,
    },
    /// Give a resource guest pages as backing.
    ResourceAttachBacking {
        /// The resource.
        resource_id: u32,
        /// The backing, in order.
        entries: &'a [MemEntry],
    },
    /// Take a resource's backing away.
    ResourceDetachBacking {
        /// The resource.
        resource_id: u32,
    },
    /// Create a blob resource: bytes with no shape, which a context's own
    /// protocol gives meaning to. Venus makes every Vulkan allocation the
    /// guest maps as one.
    ///
    /// Sent inside the context that made the `blob_id`, for host memory;
    /// the header's context is how the device knows which renderer to ask.
    ResourceCreateBlob {
        /// The id to give it, not 0.
        resource_id: u32,
        /// [`BLOB_MEM_GUEST`], [`BLOB_MEM_HOST3D`] or
        /// [`BLOB_MEM_HOST3D_GUEST`].
        blob_mem: u32,
        /// [`BLOB_FLAG_USE_MAPPABLE`] and the rest.
        blob_flags: u32,
        /// The host renderer's name for the memory, for host blobs; 0 asks
        /// Venus's render server for plain shared memory.
        blob_id: u64,
        /// Its size in bytes, which is a whole number of pages.
        size: u64,
        /// Guest backing, for the kinds that have it; empty for
        /// [`BLOB_MEM_HOST3D`].
        entries: &'a [MemEntry],
    },
    /// Map a mappable blob into the host-visible window at `offset`, which
    /// the driver chose. The device answers with how to cache it.
    ResourceMapBlob {
        /// The blob.
        resource_id: u32,
        /// Where in the window, from its start; a whole number of pages.
        offset: u64,
    },
    /// Take a blob out of the window again.
    ResourceUnmapBlob {
        /// The blob.
        resource_id: u32,
    },
}

impl Command<'_> {
    /// The command's type code.
    #[must_use]
    pub const fn code(&self) -> u32 {
        match self {
            Self::GetDisplayInfo => CMD_GET_DISPLAY_INFO,
            Self::ResourceCreate2d { .. } => CMD_RESOURCE_CREATE_2D,
            Self::ResourceUnref { .. } => CMD_RESOURCE_UNREF,
            Self::SetScanout { .. } => CMD_SET_SCANOUT,
            Self::ResourceFlush { .. } => CMD_RESOURCE_FLUSH,
            Self::TransferToHost2d { .. } => CMD_TRANSFER_TO_HOST_2D,
            Self::ResourceAttachBacking { .. } => CMD_RESOURCE_ATTACH_BACKING,
            Self::ResourceDetachBacking { .. } => CMD_RESOURCE_DETACH_BACKING,
            Self::GetCapsetInfo { .. } => CMD_GET_CAPSET_INFO,
            Self::GetCapset { .. } => CMD_GET_CAPSET,
            Self::CtxCreate { .. } => CMD_CTX_CREATE,
            Self::CtxDestroy => CMD_CTX_DESTROY,
            Self::CtxAttachResource { .. } => CMD_CTX_ATTACH_RESOURCE,
            Self::CtxDetachResource { .. } => CMD_CTX_DETACH_RESOURCE,
            Self::ResourceCreate3d { .. } => CMD_RESOURCE_CREATE_3D,
            Self::TransferToHost3d { .. } => CMD_TRANSFER_TO_HOST_3D,
            Self::TransferFromHost3d { .. } => CMD_TRANSFER_FROM_HOST_3D,
            Self::Submit3d { .. } | Self::Submit3dHeader { .. } => CMD_SUBMIT_3D,
            Self::ResourceCreateBlob { .. } => CMD_RESOURCE_CREATE_BLOB,
            Self::ResourceMapBlob { .. } => CMD_RESOURCE_MAP_BLOB,
            Self::ResourceUnmapBlob { .. } => CMD_RESOURCE_UNMAP_BLOB,
        }
    }

    /// Bytes of the request.
    #[must_use]
    pub const fn len(&self) -> usize {
        match self {
            Self::GetDisplayInfo | Self::CtxDestroy => HEADER_LEN,
            Self::CtxCreate { .. } => HEADER_LEN + 8 + CONTEXT_NAME_LEN,
            // `virtio_gpu_resource_create_3d`: eleven words and a padding.
            Self::ResourceCreate3d { .. } => HEADER_LEN + 48,
            Self::TransferToHost3d { .. } | Self::TransferFromHost3d { .. } => {
                HEADER_LEN + BOX_LEN + 24
            }
            // The stream follows the `size` and its padding.
            Self::Submit3d { commands } => HEADER_LEN + 8 + commands.len(),
            Self::Submit3dHeader { .. } => HEADER_LEN + 8,
            Self::ResourceCreate2d { .. } => HEADER_LEN + 16,
            Self::ResourceUnref { .. }
            | Self::ResourceDetachBacking { .. }
            | Self::GetCapsetInfo { .. }
            | Self::GetCapset { .. }
            | Self::CtxAttachResource { .. }
            | Self::CtxDetachResource { .. } => HEADER_LEN + 8,
            Self::SetScanout { .. } | Self::ResourceFlush { .. } => HEADER_LEN + RECT_LEN + 8,
            Self::TransferToHost2d { .. } => HEADER_LEN + RECT_LEN + 16,
            Self::ResourceAttachBacking { entries, .. } => {
                HEADER_LEN + 8 + entries.len() * MEM_ENTRY_LEN
            }
            Self::ResourceCreateBlob { entries, .. } => {
                CREATE_BLOB_LEN + entries.len() * MEM_ENTRY_LEN
            }
            // `resource_id`, padding and the `offset`.
            Self::ResourceMapBlob { .. } => HEADER_LEN + 16,
            Self::ResourceUnmapBlob { .. } => HEADER_LEN + 8,
        }
    }

    /// Whether the request is empty, which none is.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Bytes the response buffer must have.
    #[must_use]
    pub const fn response_len(&self) -> usize {
        match *self {
            Self::GetDisplayInfo => DISPLAY_INFO_LEN,
            Self::GetCapsetInfo { .. } => CAPSET_INFO_LEN,
            // The set itself is as long as the device said it would be,
            // which the caller took from `GET_CAPSET_INFO` and passed back.
            Self::GetCapset { max_size, .. } => HEADER_LEN + max_size as usize,
            Self::ResourceMapBlob { .. } => MAP_INFO_LEN,
            _ => HEADER_LEN,
        }
    }

    /// The success response the command expects.
    #[must_use]
    pub const fn expects(&self) -> u32 {
        expects(self.code())
    }

    /// Encode the request into the start of `out`, returning its length.
    ///
    /// The header's flags, fence and context are zero: a 2D driver waits for
    /// each response and needs no fence. [`Command::encode_in`] is the same
    /// for a command that belongs to a context.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, GpuError> {
        self.encode_in(Context::NONE, out)
    }

    /// [`Command::encode`] for a command sent inside a context, or fenced,
    /// or both.
    pub fn encode_in(&self, context: Context, out: &mut [u8]) -> Result<usize, GpuError> {
        let len = self.len();
        let short = GpuError::BufferTooShort {
            needed: len,
            got: out.len(),
        };
        if out.len() < len {
            self.check()?;
            return Err(short);
        }
        self.write_with_in(context, |at, bytes| {
            if let Some(slot) = at
                .checked_add(bytes.len())
                .and_then(|end| out.get_mut(at..end))
            {
                slot.copy_from_slice(bytes);
            }
        })
    }

    /// Encode the request field by field, handing each field and its offset
    /// to `put`, and return its length. Every byte of the request is put
    /// exactly once, padding as zero, so memory `put` writes need not be
    /// cleared first. For a driver writing a request into shared memory that
    /// is not one slice, such as a large `RESOURCE_ATTACH_BACKING`.
    pub fn write_with(&self, put: impl FnMut(usize, &[u8])) -> Result<usize, GpuError> {
        self.write_with_in(Context::NONE, put)
    }

    /// [`Command::write_with`] for a command sent inside a context, or
    /// fenced, or both.
    pub fn write_with_in(
        &self,
        context: Context,
        mut put: impl FnMut(usize, &[u8]),
    ) -> Result<usize, GpuError> {
        self.check()?;
        let body = HEADER_LEN;
        put(0, &self.code().to_le_bytes());
        context.write_with(&mut put);
        let rect = |put: &mut dyn FnMut(usize, &[u8]), at: usize, rect: Rect| {
            put(at, &rect.x.to_le_bytes());
            put(at + 4, &rect.y.to_le_bytes());
            put(at + 8, &rect.width.to_le_bytes());
            put(at + 12, &rect.height.to_le_bytes());
        };
        match *self {
            Self::GetDisplayInfo => {}
            Self::ResourceCreate2d {
                resource_id,
                format,
                width,
                height,
            } => {
                put(body, &resource_id.to_le_bytes());
                put(body + 4, &(format as u32).to_le_bytes());
                put(body + 8, &width.to_le_bytes());
                put(body + 12, &height.to_le_bytes());
            }
            Self::ResourceUnref { resource_id } | Self::ResourceDetachBacking { resource_id } => {
                put(body, &resource_id.to_le_bytes());
                put(body + 4, &[0; 4]);
            }
            Self::SetScanout {
                rect: area,
                scanout_id,
                resource_id,
            } => {
                rect(&mut put, body, area);
                put(body + RECT_LEN, &scanout_id.to_le_bytes());
                put(body + RECT_LEN + 4, &resource_id.to_le_bytes());
            }
            Self::ResourceFlush {
                rect: area,
                resource_id,
            } => {
                rect(&mut put, body, area);
                put(body + RECT_LEN, &resource_id.to_le_bytes());
                put(body + RECT_LEN + 4, &[0; 4]);
            }
            Self::TransferToHost2d {
                rect: area,
                offset,
                resource_id,
            } => {
                rect(&mut put, body, area);
                put(body + RECT_LEN, &offset.to_le_bytes());
                put(body + RECT_LEN + 8, &resource_id.to_le_bytes());
                put(body + RECT_LEN + 12, &[0; 4]);
            }
            Self::CtxCreate { .. }
            | Self::CtxDestroy
            | Self::CtxAttachResource { .. }
            | Self::CtxDetachResource { .. }
            | Self::ResourceCreate3d { .. }
            | Self::TransferToHost3d { .. }
            | Self::TransferFromHost3d { .. }
            | Self::Submit3d { .. }
            | Self::Submit3dHeader { .. } => self.write_3d(body, &mut put),
            Self::GetCapsetInfo { index } => {
                put(body, &index.to_le_bytes());
                put(body + 4, &[0; 4]);
            }
            Self::GetCapset {
                capset_id,
                capset_version,
                ..
            } => {
                put(body, &capset_id.to_le_bytes());
                put(body + 4, &capset_version.to_le_bytes());
            }
            Self::ResourceAttachBacking {
                resource_id,
                entries,
            } => {
                put(body, &resource_id.to_le_bytes());
                // At most MAX_BACKING_ENTRIES, which `check` enforced.
                let count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
                put(body + 4, &count.to_le_bytes());
                put_entries(&mut put, body + 8, entries);
            }
            Self::ResourceCreateBlob { .. }
            | Self::ResourceMapBlob { .. }
            | Self::ResourceUnmapBlob { .. } => self.write_blob(body, &mut put),
        }
        Ok(self.len())
    }

    /// The body of the three blob commands, for the reason
    /// [`Command::write_3d`] is apart.
    fn write_blob(&self, body: usize, put: &mut dyn FnMut(usize, &[u8])) {
        match *self {
            Self::ResourceCreateBlob {
                resource_id,
                blob_mem,
                blob_flags,
                blob_id,
                size,
                entries,
            } => {
                put(body, &resource_id.to_le_bytes());
                put(body + 4, &blob_mem.to_le_bytes());
                put(body + 8, &blob_flags.to_le_bytes());
                // At most MAX_BACKING_ENTRIES, which `check` enforced.
                let count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
                put(body + 12, &count.to_le_bytes());
                put(body + 16, &blob_id.to_le_bytes());
                put(body + 24, &size.to_le_bytes());
                put_entries(put, CREATE_BLOB_LEN, entries);
            }
            Self::ResourceMapBlob {
                resource_id,
                offset,
            } => {
                put(body, &resource_id.to_le_bytes());
                put(body + 4, &[0; 4]);
                put(body + 8, &offset.to_le_bytes());
            }
            Self::ResourceUnmapBlob { resource_id } => {
                put(body, &resource_id.to_le_bytes());
                put(body + 4, &[0; 4]);
            }
            _ => {}
        }
    }

    /// The body of every 3D command -- virtio's `0x02xx` -- which is the
    /// bulk of the encoding and lives here so that
    /// [`Command::write_with_in`] stays one screen.
    ///
    /// Every field is virgl's or virtio's as `virtio_gpu.h` writes it, and
    /// the padding is written rather than left, as everywhere else here.
    fn write_3d(&self, body: usize, put: &mut dyn FnMut(usize, &[u8])) {
        match *self {
            Self::CtxCreate { capset, name } => {
                // `nlen` is how much of the fixed 64 bytes is the name; the
                // rest is padding and is written, not left.
                let bytes = name.as_bytes();
                let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                put(body, &len.to_le_bytes());
                put(
                    body + 4,
                    &(u32::from(capset) & CONTEXT_INIT_CAPSET_MASK).to_le_bytes(),
                );
                let bytes = bytes.get(..CONTEXT_NAME_LEN).unwrap_or(bytes);
                put(body + 8, bytes);
                let rest = CONTEXT_NAME_LEN.saturating_sub(bytes.len());
                let zeros = [0u8; CONTEXT_NAME_LEN];
                put(body + 8 + bytes.len(), zeros.get(..rest).unwrap_or(&[]));
            }
            Self::CtxDestroy => {}
            Self::CtxAttachResource { resource_id } | Self::CtxDetachResource { resource_id } => {
                put(body, &resource_id.to_le_bytes());
                put(body + 4, &[0; 4]);
            }
            Self::ResourceCreate3d {
                resource_id,
                target,
                format,
                bind,
                size,
                array_size,
                last_level,
                samples,
                flags,
            } => {
                put(body, &resource_id.to_le_bytes());
                put(body + 4, &target.to_le_bytes());
                put(body + 8, &format.to_le_bytes());
                put(body + 12, &bind.to_le_bytes());
                put(body + 16, &size.width.to_le_bytes());
                put(body + 20, &size.height.to_le_bytes());
                put(body + 24, &size.depth.to_le_bytes());
                put(body + 28, &array_size.to_le_bytes());
                put(body + 32, &last_level.to_le_bytes());
                put(body + 36, &samples.to_le_bytes());
                put(body + 40, &flags.to_le_bytes());
                put(body + 44, &[0; 4]);
            }
            Self::TransferToHost3d {
                region,
                offset,
                resource_id,
                level,
                stride,
                layer_stride,
            }
            | Self::TransferFromHost3d {
                region,
                offset,
                resource_id,
                level,
                stride,
                layer_stride,
            } => {
                put(body, &region.x.to_le_bytes());
                put(body + 4, &region.y.to_le_bytes());
                put(body + 8, &region.z.to_le_bytes());
                put(body + 12, &region.width.to_le_bytes());
                put(body + 16, &region.height.to_le_bytes());
                put(body + 20, &region.depth.to_le_bytes());
                put(body + BOX_LEN, &offset.to_le_bytes());
                put(body + BOX_LEN + 8, &resource_id.to_le_bytes());
                put(body + BOX_LEN + 12, &level.to_le_bytes());
                put(body + BOX_LEN + 16, &stride.to_le_bytes());
                put(body + BOX_LEN + 20, &layer_stride.to_le_bytes());
            }
            Self::Submit3d { commands } => {
                let size = u32::try_from(commands.len()).unwrap_or(u32::MAX);
                put(body, &size.to_le_bytes());
                put(body + 4, &[0; 4]);
                put(body + 8, commands);
            }
            Self::Submit3dHeader { size } => {
                put(body, &size.to_le_bytes());
                put(body + 4, &[0; 4]);
            }
            _ => {}
        }
    }

    /// Refuse a backing list with no entries or more than
    /// [`MAX_BACKING_ENTRIES`], a capability set larger than
    /// [`MAX_CAPSET_SIZE`], and a context name longer than
    /// [`CONTEXT_NAME_LEN`].
    fn check(&self) -> Result<(), GpuError> {
        match *self {
            Self::ResourceAttachBacking { entries, .. }
                if entries.is_empty() || entries.len() > MAX_BACKING_ENTRIES =>
            {
                Err(GpuError::BackingEntries(entries.len()))
            }
            Self::GetCapset { max_size, .. } if max_size == 0 || max_size > MAX_CAPSET_SIZE => {
                Err(GpuError::CapsetSize(max_size))
            }
            Self::CtxCreate { name, .. } if name.len() > CONTEXT_NAME_LEN => {
                Err(GpuError::ContextName(name.len()))
            }
            // A stream of no commands is nothing to submit, and one whose
            // length does not fit the `size` field cannot be described.
            Self::Submit3d { commands }
                if commands.is_empty() || u32::try_from(commands.len()).is_err() =>
            {
                Err(GpuError::StreamSize(commands.len()))
            }
            Self::Submit3dHeader { size: 0 } => Err(GpuError::StreamSize(0)),
            // A blob is whole pages, as Linux's `virtio_gpu_resource_create_blob`
            // requires, and has one of the three kinds of memory. Guest
            // memory comes with backing and host memory without: QEMU reads
            // the entries of a host blob as a list it must map, and a guest
            // blob with none has nothing behind it.
            Self::ResourceCreateBlob {
                blob_mem,
                size,
                entries,
                ..
            } => {
                let guest = matches!(blob_mem, BLOB_MEM_GUEST | BLOB_MEM_HOST3D_GUEST);
                if size == 0 || !size.is_multiple_of(PAGE_SIZE) {
                    Err(GpuError::BlobSize(size))
                } else if !matches!(
                    blob_mem,
                    BLOB_MEM_GUEST | BLOB_MEM_HOST3D | BLOB_MEM_HOST3D_GUEST
                ) {
                    Err(GpuError::BlobMemory(blob_mem))
                } else if entries.len() > MAX_BACKING_ENTRIES || guest == entries.is_empty() {
                    Err(GpuError::BackingEntries(entries.len()))
                } else {
                    Ok(())
                }
            }
            Self::ResourceMapBlob { offset, .. } if !offset.is_multiple_of(PAGE_SIZE) => {
                Err(GpuError::MisalignedPage(offset))
            }
            _ => Ok(()),
        }
    }
}

/// Put `entries` as `struct virtio_gpu_mem_entry`s from `at` on, padding and
/// all, which a backing list and a blob's guest memory are alike.
fn put_entries(put: &mut dyn FnMut(usize, &[u8]), at: usize, entries: &[MemEntry]) {
    for (index, entry) in entries.iter().enumerate() {
        let at = at + index * MEM_ENTRY_LEN;
        put(at, &entry.addr.to_le_bytes());
        put(at + 8, &entry.length.to_le_bytes());
        put(at + 12, &[0; 4]);
    }
}

/// The success response command `code` expects.
#[must_use]
pub const fn expects(code: u32) -> u32 {
    match code {
        CMD_GET_DISPLAY_INFO => RESP_OK_DISPLAY_INFO,
        CMD_GET_CAPSET_INFO => RESP_OK_CAPSET_INFO,
        CMD_GET_CAPSET => RESP_OK_CAPSET,
        CMD_RESOURCE_MAP_BLOB => RESP_OK_MAP_INFO,
        _ => RESP_OK_NODATA,
    }
}

/// Bytes of `struct virtio_gpu_resp_capset_info`.
pub const CAPSET_INFO_LEN: usize = HEADER_LEN + 16;

/// What `GET_CAPSET_INFO` said about one capability set.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CapsetInfo {
    /// Which renderer it belongs to: [`CAPSET_VIRGL2`] and the rest.
    pub id: u32,
    /// The highest version of it the host offers.
    pub max_version: u32,
    /// How many bytes [`Command::GetCapset`] needs room for.
    pub max_size: u32,
}

// ---------------------------------------------------------------------------
// The cursor queue, virtio 1.2 §5.7.6.10.
// ---------------------------------------------------------------------------

/// `VIRTIO_GPU_CMD_UPDATE_CURSOR`: a new image, read out of a resource.
pub const CMD_UPDATE_CURSOR: u32 = 0x0300;
/// `VIRTIO_GPU_CMD_MOVE_CURSOR`: the same image, somewhere else.
pub const CMD_MOVE_CURSOR: u32 = 0x0301;
/// Bytes of `struct virtio_gpu_update_cursor`, which both commands are.
pub const CURSOR_LEN: usize = 56;
/// The width and height of the one cursor image QEMU shows, and so of the
/// resource an `UPDATE_CURSOR` names (`cursor_alloc(64, 64)` in
/// `hw/display/virtio-gpu.c`).
pub const CURSOR_SIZE: u32 = 64;

/// A scanout's cursor as the cursor queue carries it: where it is, and which
/// resource's pixels it shows with which hotspot.
///
/// `x` and `y` are the image's top-left corner on the scanout, as Linux's
/// cursor plane sends them, and may be negative when the hotspot is near an
/// edge; they travel as the `u32` bits virtio gives them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Cursor {
    /// The scanout.
    pub scanout_id: u32,
    /// The image's left edge.
    pub x: i32,
    /// The image's top edge.
    pub y: i32,
    /// The resource holding the image, [`CURSOR_SIZE`] square; 0 for none.
    pub resource_id: u32,
    /// The hotspot, from the image's left edge.
    pub hot_x: u32,
    /// The hotspot, from the image's top edge.
    pub hot_y: u32,
}

impl Cursor {
    /// This cursor as an `UPDATE_CURSOR` (`update`, which reads the
    /// resource's pixels again) or a `MOVE_CURSOR`.
    ///
    /// A move carries the resource as well. QEMU's `update_cursor` shows the
    /// cursor after a move only if the move names one -- it passes
    /// `resource_id` as the visibility -- which is why Linux's driver sends
    /// its last update's structure again with only the type and the place
    /// changed, and why this has one encoding for both.
    #[must_use]
    pub fn encode(&self, update: bool) -> [u8; CURSOR_LEN] {
        let mut bytes = [0u8; CURSOR_LEN];
        let mut put = |at: usize, value: u32| {
            if let Some(slot) = bytes.get_mut(at..at + 4) {
                slot.copy_from_slice(&value.to_le_bytes());
            }
        };
        put(
            0,
            if update {
                CMD_UPDATE_CURSOR
            } else {
                CMD_MOVE_CURSOR
            },
        );
        // The rest of the header -- flags, fence, context, ring -- is zero:
        // the cursor queue fences nothing and belongs to no context.
        put(HEADER_LEN, self.scanout_id);
        put(HEADER_LEN + 4, self.x as u32);
        put(HEADER_LEN + 8, self.y as u32);
        put(HEADER_LEN + 16, self.resource_id);
        put(HEADER_LEN + 20, self.hot_x);
        put(HEADER_LEN + 24, self.hot_y);
        bytes
    }
}

/// One scanout's entry in `GET_DISPLAY_INFO`'s response.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Scanout {
    /// Its preferred position and size.
    pub rect: Rect,
    /// Whether a display is attached.
    pub enabled: bool,
    /// Flags, which virtio does not define yet.
    pub flags: u32,
}

/// A device's response.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "no allocator to box into; 384 bytes returned once per GET_DISPLAY_INFO"
)]
pub enum Response {
    /// Done, with nothing to say.
    NoData,
    /// Every scanout, in index order; those past `num_scanouts` are zero.
    DisplayInfo([Scanout; MAX_SCANOUTS]),
    /// What one capability set is, from `GET_CAPSET_INFO`.
    CapsetInfo(CapsetInfo),
    /// A capability set, from `GET_CAPSET`: how many bytes of the response
    /// buffer *after the header* the device wrote.
    ///
    /// The bytes themselves are left where they are. They are the host
    /// renderer's own blob, as long as the device said and no longer, and
    /// this module has nothing to say about what is in them.
    Capset {
        /// How many bytes of the set there are, from the header onwards.
        len: usize,
    },
    /// A blob is in the host-visible window, from `RESOURCE_MAP_BLOB`, and
    /// this is how the device says to cache it: [`MAP_CACHE_CACHED`] and the
    /// rest, under [`MAP_CACHE_MASK`].
    MapInfo {
        /// The device's word, whole; the caching is its low bits.
        map_info: u32,
    },
}

/// An error response, by its code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceError {
    /// `ERR_UNSPEC`.
    Unspecified,
    /// `ERR_OUT_OF_MEMORY`.
    OutOfMemory,
    /// `ERR_INVALID_SCANOUT_ID`.
    InvalidScanoutId,
    /// `ERR_INVALID_RESOURCE_ID`.
    InvalidResourceId,
    /// `ERR_INVALID_CONTEXT_ID`.
    InvalidContextId,
    /// `ERR_INVALID_PARAMETER`.
    InvalidParameter,
}

impl Response {
    /// Parse the response the device wrote into `buffer` for `command`, of
    /// which it says it wrote `written` bytes.
    pub fn parse(command: &Command<'_>, buffer: &[u8], written: u32) -> Result<Self, GpuError> {
        Self::parse_for(command.code(), buffer, written)
    }

    /// [`Response::parse`] for the command whose type code is `command`, for a
    /// driver that keeps only the code of the command in flight.
    pub fn parse_for(command: u32, buffer: &[u8], written: u32) -> Result<Self, GpuError> {
        let written = written as usize;
        if written > buffer.len() {
            return Err(GpuError::WroteTooMuch {
                written,
                buffer: buffer.len(),
            });
        }
        let bytes = buffer.get(..written).ok_or(GpuError::WroteTooMuch {
            written,
            buffer: buffer.len(),
        })?;
        let code = get32(bytes, 0).ok_or(GpuError::ResponseTooShort(written))?;
        if written < HEADER_LEN {
            return Err(GpuError::ResponseTooShort(written));
        }
        let error = match code {
            RESP_ERR_UNSPEC => Some(DeviceError::Unspecified),
            RESP_ERR_OUT_OF_MEMORY => Some(DeviceError::OutOfMemory),
            RESP_ERR_INVALID_SCANOUT_ID => Some(DeviceError::InvalidScanoutId),
            RESP_ERR_INVALID_RESOURCE_ID => Some(DeviceError::InvalidResourceId),
            RESP_ERR_INVALID_CONTEXT_ID => Some(DeviceError::InvalidContextId),
            RESP_ERR_INVALID_PARAMETER => Some(DeviceError::InvalidParameter),
            RESP_OK_NODATA | RESP_OK_DISPLAY_INFO | RESP_OK_CAPSET_INFO | RESP_OK_CAPSET
            | RESP_OK_MAP_INFO => None,
            other => return Err(GpuError::UnknownResponse(other)),
        };
        if let Some(error) = error {
            return Err(GpuError::Device(error));
        }
        if code != expects(command) {
            return Err(GpuError::UnexpectedResponse {
                command,
                response: code,
            });
        }
        if code == RESP_OK_NODATA {
            return Ok(Self::NoData);
        }
        if code == RESP_OK_CAPSET_INFO {
            if written < CAPSET_INFO_LEN {
                return Err(GpuError::ResponseTooShort(written));
            }
            let short = GpuError::ResponseTooShort(written);
            let info = CapsetInfo {
                id: get32(bytes, HEADER_LEN).ok_or(short)?,
                max_version: get32(bytes, HEADER_LEN + 4).ok_or(short)?,
                max_size: get32(bytes, HEADER_LEN + 8).ok_or(short)?,
            };
            // The size is what the driver will allocate next, on the
            // device's word alone, so it is held to what a capability set
            // can be rather than believed.
            if info.max_size > MAX_CAPSET_SIZE {
                return Err(GpuError::CapsetSize(info.max_size));
            }
            return Ok(Self::CapsetInfo(info));
        }
        if code == RESP_OK_CAPSET {
            // Everything after the header is the set. A device that wrote
            // only the header sent no capability set at all.
            let len = written.saturating_sub(HEADER_LEN);
            if len == 0 {
                return Err(GpuError::ResponseTooShort(written));
            }
            return Ok(Self::Capset { len });
        }
        if code == RESP_OK_MAP_INFO {
            if written < MAP_INFO_LEN {
                return Err(GpuError::ResponseTooShort(written));
            }
            let map_info = get32(bytes, HEADER_LEN).ok_or(GpuError::ResponseTooShort(written))?;
            return Ok(Self::MapInfo { map_info });
        }
        if written < DISPLAY_INFO_LEN {
            return Err(GpuError::ResponseTooShort(written));
        }
        let mut scanouts = [Scanout::default(); MAX_SCANOUTS];
        for (index, scanout) in scanouts.iter_mut().enumerate() {
            let at = HEADER_LEN + index * DISPLAY_ONE_LEN;
            let short = GpuError::ResponseTooShort(written);
            let rect = Rect::decode(bytes, at).ok_or(short)?;
            let enabled = get32(bytes, at + RECT_LEN).ok_or(short)? != 0;
            let flags = get32(bytes, at + RECT_LEN + 4).ok_or(short)?;
            if enabled
                && (rect.width == 0
                    || rect.height == 0
                    || rect.width > MAX_DIMENSION
                    || rect.height > MAX_DIMENSION
                    || rect.x.checked_add(rect.width).is_none()
                    || rect.y.checked_add(rect.height).is_none())
            {
                return Err(GpuError::ScanoutSize { index, rect });
            }
            *scanout = Scanout {
                rect,
                enabled,
                flags,
            };
        }
        Ok(Self::DisplayInfo(scanouts))
    }
}

/// Join the pages at `device_pages`, one device address per page and in
/// order, into backing entries in `out`, merging a page into the entry before
/// it only where its address follows that entry's end. Returns how many
/// entries were written.
pub fn backing_entries(device_pages: &[u64], out: &mut [MemEntry]) -> Result<usize, GpuError> {
    let page = u32::try_from(PAGE_SIZE).map_err(|_| GpuError::BackingEntries(0))?;
    let mut count = 0usize;
    for &addr in device_pages {
        if addr % PAGE_SIZE != 0 {
            return Err(GpuError::MisalignedPage(addr));
        }
        let joined = count
            .checked_sub(1)
            .and_then(|last| out.get_mut(last))
            .filter(|entry| {
                entry.addr.checked_add(u64::from(entry.length)) == Some(addr)
                    && entry.length.checked_add(page).is_some()
            });
        if let Some(entry) = joined {
            entry.length += page;
            continue;
        }
        if count >= MAX_BACKING_ENTRIES {
            return Err(GpuError::BackingEntries(count + 1));
        }
        let slot = out
            .get_mut(count)
            .ok_or(GpuError::BackingEntries(count + 1))?;
        *slot = MemEntry { addr, length: page };
        count += 1;
    }
    if count == 0 {
        return Err(GpuError::BackingEntries(0));
    }
    Ok(count)
}

/// What can go wrong speaking to a virtio-gpu device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GpuError {
    /// The configuration block is shorter than `struct virtio_gpu_config`.
    ConfigTooShort(u32),
    /// `num_scanouts` is 0 or above [`MAX_SCANOUTS`].
    Scanouts(u32),
    /// The request does not fit the buffer it is encoded into.
    BufferTooShort {
        /// Bytes the request needs.
        needed: usize,
        /// Bytes the buffer has.
        got: usize,
    },
    /// Backing with no entries, more than [`MAX_BACKING_ENTRIES`], or more
    /// than the output slice holds.
    BackingEntries(usize),
    /// A device address that is not the start of a page.
    MisalignedPage(u64),
    /// The device says it wrote more than the response buffer holds.
    WroteTooMuch {
        /// Bytes it says it wrote.
        written: usize,
        /// Bytes the buffer holds.
        buffer: usize,
    },
    /// The response is shorter than its header or its type's body.
    ResponseTooShort(usize),
    /// A response type virtio does not define.
    UnknownResponse(u32),
    /// A success response the command cannot produce.
    UnexpectedResponse {
        /// The command's code.
        command: u32,
        /// The response's code.
        response: u32,
    },
    /// An enabled scanout with a size no mode has.
    ScanoutSize {
        /// Its index.
        index: usize,
        /// What it reported.
        rect: Rect,
    },
    /// A capability set of no size, or larger than [`MAX_CAPSET_SIZE`].
    CapsetSize(u32),
    /// A context name longer than [`CONTEXT_NAME_LEN`].
    ContextName(usize),
    /// A command stream of no bytes, or more than its length field holds.
    StreamSize(usize),
    /// A blob of no size, or of a size that is not whole pages.
    BlobSize(u64),
    /// A blob memory kind virtio does not define.
    BlobMemory(u32),
    /// The device refused the command.
    Device(DeviceError),
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigTooShort(len) => {
                write!(
                    f,
                    "virtio-gpu configuration is {len} bytes, under {CONFIG_LEN}"
                )
            }
            Self::Scanouts(count) => write!(f, "virtio-gpu reports {count} scanouts"),
            Self::BufferTooShort { needed, got } => {
                write!(
                    f,
                    "a virtio-gpu request needs {needed} bytes, the buffer has {got}"
                )
            }
            Self::BackingEntries(count) => {
                write!(f, "{count} backing entries for one virtio-gpu resource")
            }
            Self::MisalignedPage(addr) => {
                write!(f, "device address {addr:#x} is not the start of a page")
            }
            Self::WroteTooMuch { written, buffer } => write!(
                f,
                "virtio-gpu says it wrote {written} bytes into a {buffer}-byte response"
            ),
            Self::ResponseTooShort(len) => write!(f, "a {len}-byte virtio-gpu response"),
            Self::UnknownResponse(code) => write!(f, "virtio-gpu response type {code:#x}"),
            Self::UnexpectedResponse { command, response } => write!(
                f,
                "virtio-gpu answered command {command:#x} with response {response:#x}"
            ),
            Self::ScanoutSize { index, rect } => write!(
                f,
                "virtio-gpu scanout {index} is {}x{} at {},{}",
                rect.width, rect.height, rect.x, rect.y
            ),
            Self::CapsetSize(size) => {
                write!(f, "virtio-gpu offered a {size}-byte capability set")
            }
            Self::ContextName(len) => {
                write!(f, "a {len}-byte virtio-gpu context name")
            }
            Self::StreamSize(len) => {
                write!(f, "a {len}-byte virgl command stream")
            }
            Self::BlobSize(size) => write!(f, "a {size}-byte virtio-gpu blob"),
            Self::BlobMemory(kind) => write!(f, "virtio-gpu blob memory kind {kind}"),
            Self::Device(error) => write!(f, "virtio-gpu refused the command: {error:?}"),
        }
    }
}

fn get32(bytes: &[u8], at: usize) -> Option<u32> {
    let field = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(field.try_into().ok()?))
}
