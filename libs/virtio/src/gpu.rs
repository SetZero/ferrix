//! virtio-gpu's device protocol, 2D half: its configuration space, its feature
//! bits, the control commands a scanout needs, and the device's responses.
//!
//! Virtio 1.2 §5.7 defines the GPU device. What iteration 1 of the display
//! (`docs/DISPLAY.md`) needs of it is the 2D subset: ask which scanouts exist,
//! create a resource, give it guest pages as backing, point a scanout at it,
//! and tell the device which rectangle changed. Each of those is a command
//! here, encoded into bytes, and each response is parsed back. What drives a
//! device — the queue, the order of commands, what to do when it misbehaves —
//! is `ferrix-virtio-gpu`'s. 3D, blob resources, capability sets, EDID and the
//! cursor queue are later.
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

/// `VIRTIO_GPU_RESP_OK_NODATA`.
pub const RESP_OK_NODATA: u32 = 0x1100;
/// `VIRTIO_GPU_RESP_OK_DISPLAY_INFO`.
pub const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
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

    fn encode(self, out: &mut [u8], at: usize) -> Option<()> {
        put32(out, at, self.x)?;
        put32(out, at + 4, self.y)?;
        put32(out, at + 8, self.width)?;
        put32(out, at + 12, self.height)
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
        }
    }

    /// Bytes of the request.
    #[must_use]
    pub const fn len(&self) -> usize {
        match self {
            Self::GetDisplayInfo => HEADER_LEN,
            Self::ResourceCreate2d { .. } => HEADER_LEN + 16,
            Self::ResourceUnref { .. } | Self::ResourceDetachBacking { .. } => HEADER_LEN + 8,
            Self::SetScanout { .. } | Self::ResourceFlush { .. } => HEADER_LEN + RECT_LEN + 8,
            Self::TransferToHost2d { .. } => HEADER_LEN + RECT_LEN + 16,
            Self::ResourceAttachBacking { entries, .. } => {
                HEADER_LEN + 8 + entries.len() * MEM_ENTRY_LEN
            }
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
        match self {
            Self::GetDisplayInfo => DISPLAY_INFO_LEN,
            _ => HEADER_LEN,
        }
    }

    /// The success response the command expects.
    #[must_use]
    pub const fn expects(&self) -> u32 {
        match self {
            Self::GetDisplayInfo => RESP_OK_DISPLAY_INFO,
            _ => RESP_OK_NODATA,
        }
    }

    /// Encode the request into the start of `out`, returning its length.
    ///
    /// The header's flags, fence and context are zero: a 2D driver waits for
    /// each response and needs no fence.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, GpuError> {
        let len = self.len();
        if let Self::ResourceAttachBacking { entries, .. } = self
            && (entries.is_empty() || entries.len() > MAX_BACKING_ENTRIES)
        {
            return Err(GpuError::BackingEntries(entries.len()));
        }
        let short = GpuError::BufferTooShort {
            needed: len,
            got: out.len(),
        };
        let out = out.get_mut(..len).ok_or(short)?;
        out.fill(0);
        self.encode_body(out).map(|()| len).ok_or(short)
    }

    fn encode_body(&self, out: &mut [u8]) -> Option<()> {
        put32(out, 0, self.code())?;
        let body = HEADER_LEN;
        match *self {
            Self::GetDisplayInfo => {}
            Self::ResourceCreate2d {
                resource_id,
                format,
                width,
                height,
            } => {
                put32(out, body, resource_id)?;
                put32(out, body + 4, format as u32)?;
                put32(out, body + 8, width)?;
                put32(out, body + 12, height)?;
            }
            Self::ResourceUnref { resource_id } | Self::ResourceDetachBacking { resource_id } => {
                put32(out, body, resource_id)?;
            }
            Self::SetScanout {
                rect,
                scanout_id,
                resource_id,
            } => {
                rect.encode(out, body)?;
                put32(out, body + RECT_LEN, scanout_id)?;
                put32(out, body + RECT_LEN + 4, resource_id)?;
            }
            Self::ResourceFlush { rect, resource_id } => {
                rect.encode(out, body)?;
                put32(out, body + RECT_LEN, resource_id)?;
            }
            Self::TransferToHost2d {
                rect,
                offset,
                resource_id,
            } => {
                rect.encode(out, body)?;
                put64(out, body + RECT_LEN, offset)?;
                put32(out, body + RECT_LEN + 8, resource_id)?;
            }
            Self::ResourceAttachBacking {
                resource_id,
                entries,
            } => {
                put32(out, body, resource_id)?;
                put32(out, body + 4, u32::try_from(entries.len()).ok()?)?;
                for (index, entry) in entries.iter().enumerate() {
                    let at = body + 8 + index * MEM_ENTRY_LEN;
                    put64(out, at, entry.addr)?;
                    put32(out, at + 8, entry.length)?;
                }
            }
        }
        Some(())
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
            RESP_OK_NODATA | RESP_OK_DISPLAY_INFO => None,
            other => return Err(GpuError::UnknownResponse(other)),
        };
        if let Some(error) = error {
            return Err(GpuError::Device(error));
        }
        if code != command.expects() {
            return Err(GpuError::UnexpectedResponse {
                command: command.code(),
                response: code,
            });
        }
        if code == RESP_OK_NODATA {
            return Ok(Self::NoData);
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
            Self::Device(error) => write!(f, "virtio-gpu refused the command: {error:?}"),
        }
    }
}

fn get32(bytes: &[u8], at: usize) -> Option<u32> {
    let field = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(field.try_into().ok()?))
}

fn put32(out: &mut [u8], at: usize, value: u32) -> Option<()> {
    out.get_mut(at..at.checked_add(4)?)?
        .copy_from_slice(&value.to_le_bytes());
    Some(())
}

fn put64(out: &mut [u8], at: usize, value: u64) -> Option<()> {
    out.get_mut(at..at.checked_add(8)?)?
        .copy_from_slice(&value.to_le_bytes());
    Some(())
}
