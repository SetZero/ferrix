//! `/dev/dri/card<N>` as a program opens it: the DRM/KMS subset
//! `docs/DISPLAY.md` §2.3 answers.
//!
//! One open at a time (§5, a written deviation from Linux's many opens with
//! one master): the open holds the dumb buffers it created, the framebuffers
//! it added and the page-flip events it has not read. The card has one
//! connector, one encoder and one CRTC, with fixed object ids. Dumb buffers
//! are ranges of the card VMO the core hands out, and takes back decommitted
//! once the device has let go of them; a program maps them through the
//! card's own inode, whose `mapping()` is that VMO, at the offset
//! `MODE_MAP_DUMB` gives.
//!
//! A handle is the open's name for a buffer; the core names it by an id no
//! other buffer on the card ever has. `DESTROY_DUMB` of a buffer a
//! framebuffer still refers to only takes the handle away, as on Linux: the
//! buffer is let go of with its last framebuffer.
//!
//! `SETCRTC` and `PAGE_FLIP` wait for the driver's reply, returning once the
//! device has the frame, and a page flip's event is queued then. Letting go
//! of a buffer does not wait.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_displayctl::message::{MAX_DIMENSION, Rect, Status};
use ferrix_linux_abi::drm::{
    self, CardRes, ClipRect, CreateDumb, Crtc, CrtcPageFlip, DestroyDumb, Event, EventVblank,
    FbCmd, FbCmd2, FbDirtyCmd, Field, GetCap, GetConnector, GetEncoder, Layout, MapDumb, ModeInfo,
    SetClientCap, Version,
};
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::socket::Width;
use ferrix_vfs::{Inode, Metadata, Readiness, Result as VfsResult};

use super::{Card, CardError};
use ferrix_sync::SleepLock;

use crate::sched::WaitQueue;
use crate::sync::{SchedParker, SpinLock};
use crate::syscall::process::{self, Process};
use crate::syscall::uaccess;
use crate::timer;

/// The CRTC's object id.
const CRTC_ID: u32 = 1;
/// The encoder's.
const ENCODER_ID: u32 = 2;
/// The connector's.
const CONNECTOR_ID: u32 = 3;

/// This build's pointer width, which `DRM_IOCTL_VERSION`'s layout follows.
const NATIVE: Width = if size_of::<usize>() == 8 {
    Width::Bits64
} else {
    Width::Bits32
};

/// The most page-flip events kept unread.
const MAX_EVENTS: usize = 64;

/// The most framebuffers one open may have, as the session caps buffers.
const MAX_FRAMEBUFFERS: usize = 64;

/// A dumb buffer.
#[derive(Clone, Copy, Debug)]
struct Dumb {
    handle: u32,
    /// The core's id for it.
    buffer: u32,
    offset: u64,
    width: u32,
    height: u32,
    pitch: u32,
    /// `DESTROY_DUMB` took the handle while a framebuffer still refers to it.
    destroyed: bool,
}

/// A framebuffer: a dumb buffer as `ADDFB` named it.
#[derive(Clone, Copy, Debug)]
struct Framebuffer {
    id: u32,
    handle: u32,
    buffer: u32,
    width: u32,
    height: u32,
}

#[derive(Debug, Default)]
struct OpenState {
    dumbs: Vec<Dumb>,
    framebuffers: Vec<Framebuffer>,
    next_handle: u32,
    next_framebuffer: u32,
    /// The framebuffer on the CRTC, and the mode it was set with.
    shown: Option<(u32, ModeInfo)>,
    events: VecDeque<[u8; 32]>,
    sequence: u32,
}

/// One open of a card.
pub(crate) struct CardFile {
    card: Arc<Card>,
    state: SpinLock<OpenState>,
    readable: WaitQueue,
    /// Held across a whole mode change (`SETCRTC`, `PAGE_FLIP`, `RMFB`), so
    /// two threads' changes cannot leave `shown` disagreeing with the device.
    /// A sleep lock: the changes wait for the driver, and no spin lock is
    /// held while it is taken.
    modeset: SleepLock<()>,
}

impl core::fmt::Debug for CardFile {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CardFile")
            .field("card", &self.card)
            .finish_non_exhaustive()
    }
}

impl CardFile {
    /// Open `card`, which only one open may hold at a time.
    pub(crate) fn open(card: Arc<Card>) -> Result<Arc<CardFile>, Errno> {
        if card.is_gone() {
            return Err(Errno::ENXIO);
        }
        if card
            .opened
            .compare_exchange(
                false,
                true,
                core::sync::atomic::Ordering::AcqRel,
                core::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return Err(Errno::EBUSY);
        }
        Ok(Arc::new(CardFile {
            card,
            state: SpinLock::new(OpenState {
                next_handle: 1,
                next_framebuffer: 1,
                ..OpenState::default()
            }),
            readable: WaitQueue::new(),
            modeset: SleepLock::new((), &SchedParker),
        }))
    }
}

impl Drop for CardFile {
    /// Take the screen off and let go of the buffers without waiting: the
    /// replies go to the card's task, which drops them. A buffer whose
    /// detach the device refuses keeps its range, which the core never hands
    /// out again.
    fn drop(&mut self) {
        let state = self.state.get_mut();
        let buffers: Vec<u32> = state.dumbs.iter().map(|dumb| dumb.buffer).collect();
        self.card.release(&buffers);
        self.card
            .opened
            .store(false, core::sync::atomic::Ordering::Release);
    }
}

impl Inode for CardFile {
    fn metadata(&self) -> Metadata {
        self.card.metadata()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        self.read_stream(buf, false)
    }

    /// Readable with a page-flip event to read, or once the driver is gone,
    /// when a read returns 0; never writable, as on Linux.
    fn poll(&self) -> Readiness {
        let waiting = !self.state.lock().events.is_empty();
        Readiness {
            readable: waiting || self.card.is_gone(),
            writable: false,
            hangup: false,
            error: false,
        }
    }

    /// The events queue's wakes, and the card's, which a driver that goes
    /// wakes.
    fn poll_changes(&self) -> Option<u64> {
        Some(
            self.readable
                .wakes()
                .wrapping_add(self.card.changed.wakes()),
        )
    }

    /// Put back events a read took but could not deliver, as Linux's
    /// `drm_read` does, so a bad buffer loses no flip.
    fn unread_stream(&self, bytes: &[u8]) {
        let mut state = self.state.lock();
        for chunk in bytes.chunks_exact(EventVblank::SIZE).rev() {
            let mut event = [0u8; 32];
            event.copy_from_slice(chunk);
            state.events.push_front(event);
        }
        drop(state);
        self.readable.wake_all();
    }

    /// Page-flip events, whole records only. A blocking read ends with a
    /// restart code when the reader has a signal to take.
    fn read_stream(&self, buf: &mut [u8], nonblock: bool) -> VfsResult<usize> {
        if buf.len() < EventVblank::SIZE {
            return Err(Errno::EINVAL);
        }
        let mut written = 0;
        let mut take = || {
            let mut state = self.state.lock();
            while buf.len() - written >= EventVblank::SIZE {
                let Some(event) = state.events.pop_front() else {
                    break;
                };
                if let Some(slot) = buf.get_mut(written..written + EventVblank::SIZE) {
                    slot.copy_from_slice(&event);
                }
                written += EventVblank::SIZE;
            }
            written > 0 || self.card.is_gone()
        };
        if nonblock {
            if take() {
                return Ok(written);
            }
            return Err(Errno::EAGAIN);
        }
        let caller = process::current();
        let killed = || {
            caller
                .as_ref()
                .is_some_and(|process| process.signal_pending())
        };
        let _ = self
            .readable
            .wait_until_deadline(|| take() || killed(), u64::MAX);
        if written == 0 && !self.card.is_gone() && killed() {
            return Err(Errno::ERESTARTSYS);
        }
        Ok(written)
    }
}

/// The open card `file` reads and writes go to, if it is one.
pub(crate) fn of(io: &Arc<dyn Inode>) -> Option<Arc<CardFile>> {
    Arc::clone(io).into_any().downcast::<CardFile>().ok()
}

fn card_error(error: CardError) -> Errno {
    match error {
        CardError::Request(_) => Errno::EINVAL,
        CardError::NoRoom => Errno::ENOMEM,
        CardError::Busy => Errno::EBUSY,
        CardError::Gone => Errno::ENODEV,
        CardError::TimedOut => Errno::ETIMEDOUT,
    }
}

fn status_error(status: Status) -> Result<(), Errno> {
    match status {
        Status::Ok => Ok(()),
        Status::OutOfMemory | Status::PinFailed => Err(Errno::ENOMEM),
        Status::DeviceRefused | Status::Invalid => Err(Errno::EIO),
    }
}

fn read_arg<L: Layout>(process: &Process, arg: u64) -> Result<L, Errno> {
    let mut bytes = vec![0u8; L::SIZE];
    uaccess::copy_from_user(process.space(), arg, &mut bytes).map_err(|_| Errno::EFAULT)?;
    L::read(&bytes).ok_or(Errno::EFAULT)
}

fn write_arg<L: Layout>(process: &Process, arg: u64, value: &L) -> Result<usize, Errno> {
    let mut bytes = vec![0u8; L::SIZE];
    value.write(&mut bytes).ok_or(Errno::EFAULT)?;
    uaccess::copy_to_user(process.space(), arg, &bytes).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}

/// Copy `ids` to the user array at `at` if it has room for `capacity`.
fn write_ids(process: &Process, at: u64, capacity: u32, ids: &[u32]) -> Result<(), Errno> {
    if at == 0 || (capacity as usize) < ids.len() {
        return Ok(());
    }
    let bytes: Vec<u8> = ids.iter().flat_map(|id| id.to_le_bytes()).collect();
    uaccess::copy_to_user(process.space(), at, &bytes).map_err(|_| Errno::EFAULT)
}

/// The mode a scanout's preferred size makes: 60 Hz, sync pulses where a
/// monitor would put them, named `WxH`.
fn mode_for(width: u32, height: u32) -> ModeInfo {
    let mut mode = ModeInfo::ZERO;
    let clamp = |value: u32| u16::try_from(value).unwrap_or(u16::MAX);
    mode.hdisplay = clamp(width);
    mode.hsync_start = clamp(width + 16);
    mode.hsync_end = clamp(width + 48);
    mode.htotal = clamp(width + 160);
    mode.vdisplay = clamp(height);
    mode.vsync_start = clamp(height + 3);
    mode.vsync_end = clamp(height + 9);
    mode.vtotal = clamp(height + 23);
    mode.vrefresh = 60;
    mode.clock = (width + 160) * (height + 23) * 60 / 1000;
    mode.r#type = drm::MODE_TYPE_PREFERRED | drm::MODE_TYPE_DRIVER;
    let mut name = [0u8; drm::DISPLAY_MODE_LEN];
    let mut cursor = 0;
    let mut push = |bytes: &[u8]| {
        for &byte in bytes {
            if let Some(slot) = name.get_mut(cursor) {
                *slot = byte;
                cursor += 1;
            }
        }
    };
    push(&decimal(width));
    push(b"x");
    push(&decimal(height));
    mode.name = name;
    mode
}

fn decimal(mut value: u32) -> Vec<u8> {
    let mut digits = Vec::new();
    loop {
        digits.push(b'0' + (value % 10) as u8);
        value /= 10;
        if value == 0 {
            break;
        }
    }
    digits.reverse();
    digits
}

/// Answer `request` on the open card `file`, or `ENOTTY` for one this
/// subset does not have.
pub(crate) fn ioctl(
    process: &Process,
    file: &CardFile,
    request: u32,
    arg: u64,
) -> Result<usize, Errno> {
    match request {
        drm::IOCTL_SET_MASTER | drm::IOCTL_DROP_MASTER => Ok(0),
        request if request == drm::ioctl_version(NATIVE) => version(process, arg),
        drm::IOCTL_GET_CAP => get_cap(process, arg),
        drm::IOCTL_SET_CLIENT_CAP => {
            let cap: SetClientCap = read_arg(process, arg)?;
            match cap.capability {
                drm::CLIENT_CAP_UNIVERSAL_PLANES | drm::CLIENT_CAP_ATOMIC => Err(Errno::EOPNOTSUPP),
                _ => Err(Errno::EINVAL),
            }
        }
        drm::IOCTL_MODE_GETRESOURCES => get_resources(process, file, arg),
        drm::IOCTL_MODE_GETCONNECTOR => connector(process, &file.card, arg),
        drm::IOCTL_MODE_GETENCODER => get_encoder(process, arg),
        drm::IOCTL_MODE_GETCRTC => get_crtc(process, file, arg),
        drm::IOCTL_MODE_CREATE_DUMB => create_dumb(process, file, arg),
        drm::IOCTL_MODE_MAP_DUMB => map_dumb(process, file, arg),
        drm::IOCTL_MODE_DESTROY_DUMB => {
            let destroy: DestroyDumb = read_arg(process, arg)?;
            destroy_dumb(file, destroy.handle).map(|()| 0)
        }
        drm::IOCTL_MODE_ADDFB => add_fb(process, file, arg),
        drm::IOCTL_MODE_ADDFB2 => add_fb2(process, file, arg),
        drm::IOCTL_MODE_RMFB => {
            let mut bytes = [0u8; 4];
            uaccess::copy_from_user(process.space(), arg, &mut bytes).map_err(|_| Errno::EFAULT)?;
            let _modeset = file.modeset.lock();
            remove_framebuffer(file, u32::from_le_bytes(bytes)).map(|()| 0)
        }
        drm::IOCTL_MODE_SETCRTC => {
            let _modeset = file.modeset.lock();
            set_crtc(process, file, arg)
        }
        drm::IOCTL_MODE_PAGE_FLIP => {
            let _modeset = file.modeset.lock();
            page_flip(process, file, arg)
        }
        drm::IOCTL_MODE_DIRTYFB => dirty_fb(process, file, arg),
        _ => Err(Errno::ENOTTY),
    }
}

fn get_cap(process: &Process, arg: u64) -> Result<usize, Errno> {
    let mut cap: GetCap = read_arg(process, arg)?;
    cap.value = match cap.capability {
        drm::CAP_DUMB_BUFFER | drm::CAP_TIMESTAMP_MONOTONIC | drm::CAP_CRTC_IN_VBLANK_EVENT => 1,
        drm::CAP_DUMB_PREFERRED_DEPTH => 24,
        drm::CAP_DUMB_PREFER_SHADOW => 0,
        _ => return Err(Errno::EINVAL),
    };
    write_arg(process, arg, &cap)
}

fn get_resources(process: &Process, file: &CardFile, arg: u64) -> Result<usize, Errno> {
    let mut resources: CardRes = read_arg(process, arg)?;
    let framebuffers: Vec<u32> = file
        .state
        .lock()
        .framebuffers
        .iter()
        .map(|fb| fb.id)
        .collect();
    write_ids(
        process,
        resources.fb_id_ptr,
        resources.count_fbs,
        &framebuffers,
    )?;
    write_ids(
        process,
        resources.crtc_id_ptr,
        resources.count_crtcs,
        &[CRTC_ID],
    )?;
    write_ids(
        process,
        resources.connector_id_ptr,
        resources.count_connectors,
        &[CONNECTOR_ID],
    )?;
    write_ids(
        process,
        resources.encoder_id_ptr,
        resources.count_encoders,
        &[ENCODER_ID],
    )?;
    resources.count_fbs = u32::try_from(framebuffers.len()).unwrap_or(u32::MAX);
    resources.count_crtcs = 1;
    resources.count_connectors = 1;
    resources.count_encoders = 1;
    resources.min_width = 1;
    resources.min_height = 1;
    resources.max_width = MAX_DIMENSION;
    resources.max_height = MAX_DIMENSION;
    write_arg(process, arg, &resources)
}

fn get_encoder(process: &Process, arg: u64) -> Result<usize, Errno> {
    let mut encoder: GetEncoder = read_arg(process, arg)?;
    if encoder.encoder_id != ENCODER_ID {
        return Err(Errno::ENOENT);
    }
    encoder.encoder_type = drm::ENCODER_VIRTUAL;
    encoder.crtc_id = CRTC_ID;
    encoder.possible_crtcs = 1;
    encoder.possible_clones = 0;
    write_arg(process, arg, &encoder)
}

fn get_crtc(process: &Process, file: &CardFile, arg: u64) -> Result<usize, Errno> {
    let mut crtc: Crtc = read_arg(process, arg)?;
    if crtc.crtc_id != CRTC_ID {
        return Err(Errno::ENOENT);
    }
    let shown = file.state.lock().shown;
    crtc.fb_id = shown.map_or(0, |(id, _)| id);
    crtc.mode_valid = u32::from(shown.is_some());
    crtc.mode = shown.map_or(ModeInfo::ZERO, |(_, mode)| mode);
    crtc.x = 0;
    crtc.y = 0;
    crtc.gamma_size = 0;
    write_arg(process, arg, &crtc)
}

fn map_dumb(process: &Process, file: &CardFile, arg: u64) -> Result<usize, Errno> {
    let mut map: MapDumb = read_arg(process, arg)?;
    map.offset = file
        .state
        .lock()
        .dumbs
        .iter()
        .find(|dumb| dumb.handle == map.handle && !dumb.destroyed)
        .map(|dumb| dumb.offset)
        .ok_or(Errno::ENOENT)?;
    write_arg(process, arg, &map)
}

fn add_fb(process: &Process, file: &CardFile, arg: u64) -> Result<usize, Errno> {
    let mut command: FbCmd = read_arg(process, arg)?;
    if command.bpp != 32 || command.depth != 24 {
        return Err(Errno::EINVAL);
    }
    command.fb_id = add_framebuffer(
        file,
        command.handle,
        command.width,
        command.height,
        command.pitch,
    )?;
    write_arg(process, arg, &command)
}

fn add_fb2(process: &Process, file: &CardFile, arg: u64) -> Result<usize, Errno> {
    let mut command: FbCmd2 = read_arg(process, arg)?;
    let [handle, extra_handles @ ..] = command.handles;
    if command.pixel_format != drm::FORMAT_XRGB8888
        || command.flags != 0
        || command.offsets != [0; 4]
        || extra_handles != [0; 3]
    {
        return Err(Errno::EINVAL);
    }
    let [pitch, ..] = command.pitches;
    command.fb_id = add_framebuffer(file, handle, command.width, command.height, pitch)?;
    write_arg(process, arg, &command)
}

fn page_flip(process: &Process, file: &CardFile, arg: u64) -> Result<usize, Errno> {
    let flip: CrtcPageFlip = read_arg(process, arg)?;
    if flip.crtc_id != CRTC_ID || flip.reserved != 0 || flip.flags & !drm::PAGE_FLIP_EVENT != 0 {
        return Err(Errno::EINVAL);
    }
    let mode = file
        .state
        .lock()
        .shown
        .map(|(_, mode)| mode)
        .ok_or(Errno::EINVAL)?;
    show(file, flip.fb_id, mode, false)?;
    if flip.flags & drm::PAGE_FLIP_EVENT != 0 {
        queue_event(file, flip.user_data);
    }
    Ok(0)
}

fn dirty_fb(process: &Process, file: &CardFile, arg: u64) -> Result<usize, Errno> {
    let dirty: FbDirtyCmd = read_arg(process, arg)?;
    let framebuffer = find_framebuffer(file, dirty.fb_id)?;
    let rect = dirty_rect(process, &dirty, &framebuffer)?;
    file.card
        .flush(framebuffer.buffer, rect)
        .map_err(card_error)
        .and_then(status_error)
        .map(|()| 0)
}

fn version(process: &Process, arg: u64) -> Result<usize, Errno> {
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
    version.version_major = 0;
    version.version_minor = 1;
    version.version_patchlevel = 0;
    version.name_len = give(version.name, version.name_len, b"virtio_gpu")?;
    version.date_len = give(version.date, version.date_len, b"0")?;
    version.desc_len = give(version.desc, version.desc_len, b"virtio GPU")?;
    version.write(NATIVE, &mut bytes).ok_or(Errno::EFAULT)?;
    uaccess::copy_to_user(process.space(), arg, &bytes).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}

fn connector(process: &Process, card: &Card, arg: u64) -> Result<usize, Errno> {
    let mut connector: GetConnector = read_arg(process, arg)?;
    if connector.connector_id != CONNECTOR_ID {
        return Err(Errno::ENOENT);
    }
    let preferred = card.modes().first().copied().filter(|mode| mode.enabled);
    let modes: Vec<ModeInfo> = preferred
        .map(|mode| mode_for(mode.width, mode.height))
        .into_iter()
        .collect();
    if connector.modes_ptr != 0 && connector.count_modes as usize >= modes.len() {
        let mut bytes = vec![0u8; modes.len() * ModeInfo::SIZE];
        for (index, mode) in modes.iter().enumerate() {
            let slot = bytes
                .get_mut(index * ModeInfo::SIZE..)
                .ok_or(Errno::EFAULT)?;
            mode.write(slot).ok_or(Errno::EFAULT)?;
        }
        uaccess::copy_to_user(process.space(), connector.modes_ptr, &bytes)
            .map_err(|_| Errno::EFAULT)?;
    }
    write_ids(
        process,
        connector.encoders_ptr,
        connector.count_encoders,
        &[ENCODER_ID],
    )?;
    connector.count_modes = u32::try_from(modes.len()).unwrap_or(0);
    connector.count_props = 0;
    connector.count_encoders = 1;
    connector.encoder_id = ENCODER_ID;
    connector.connector_type = drm::CONNECTOR_VIRTUAL;
    connector.connector_type_id = 1;
    connector.connection = if preferred.is_some() {
        drm::CONNECTION_CONNECTED
    } else {
        drm::CONNECTION_DISCONNECTED
    };
    connector.mm_width = 0;
    connector.mm_height = 0;
    connector.subpixel = drm::SUBPIXEL_UNKNOWN;
    connector.pad = 0;
    write_arg(process, arg, &connector)
}

fn create_dumb(process: &Process, file: &CardFile, arg: u64) -> Result<usize, Errno> {
    let mut create: CreateDumb = read_arg(process, arg)?;
    if create.bpp != 32
        || create.flags != 0
        || !(1..=MAX_DIMENSION).contains(&create.width)
        || !(1..=MAX_DIMENSION).contains(&create.height)
    {
        return Err(Errno::EINVAL);
    }
    let pitch = create.width * 4;
    let (buffer, offset, status) = file
        .card
        .attach(create.width, create.height, pitch)
        .map_err(card_error)?;
    status_error(status)?;
    let size = (u64::from(pitch) * u64::from(create.height)).div_ceil(PAGE_SIZE) * PAGE_SIZE;
    let handle = {
        let mut state = file.state.lock();
        let handle = state.next_handle;
        match handle.checked_add(1) {
            Some(next) => {
                state.next_handle = next;
                state.dumbs.push(Dumb {
                    handle,
                    buffer,
                    offset,
                    width: create.width,
                    height: create.height,
                    pitch,
                    destroyed: false,
                });
                Some(handle)
            }
            None => None,
        }
    };
    let Some(handle) = handle else {
        file.card.let_go(&[buffer]);
        return Err(Errno::ENOMEM);
    };
    create.handle = handle;
    create.pitch = pitch;
    create.size = size;
    write_arg(process, arg, &create)
}

/// Take `handle` away. The buffer goes now, or with the last framebuffer
/// that refers to it.
fn destroy_dumb(file: &CardFile, handle: u32) -> Result<(), Errno> {
    let gone = {
        let mut state = file.state.lock();
        let referred = state.framebuffers.iter().any(|fb| fb.handle == handle);
        let at = state
            .dumbs
            .iter()
            .position(|dumb| dumb.handle == handle && !dumb.destroyed)
            .ok_or(Errno::ENOENT)?;
        if referred {
            if let Some(dumb) = state.dumbs.get_mut(at) {
                dumb.destroyed = true;
            }
            None
        } else {
            Some(state.dumbs.remove(at).buffer)
        }
    };
    if let Some(buffer) = gone {
        file.card.let_go(&[buffer]);
    }
    Ok(())
}

fn add_framebuffer(
    file: &CardFile,
    handle: u32,
    width: u32,
    height: u32,
    pitch: u32,
) -> Result<u32, Errno> {
    let mut state = file.state.lock();
    let dumb = state
        .dumbs
        .iter()
        .find(|dumb| dumb.handle == handle && !dumb.destroyed)
        .copied()
        .ok_or(Errno::ENOENT)?;
    if width == 0
        || height == 0
        || width > dumb.width
        || height > dumb.height
        || pitch != dumb.pitch
    {
        return Err(Errno::EINVAL);
    }
    if state.framebuffers.len() >= MAX_FRAMEBUFFERS {
        return Err(Errno::ENOSPC);
    }
    let id = state.next_framebuffer;
    state.next_framebuffer = id.checked_add(1).ok_or(Errno::ENOMEM)?;
    state.framebuffers.push(Framebuffer {
        id,
        handle,
        buffer: dumb.buffer,
        width,
        height,
    });
    Ok(id)
}

fn find_framebuffer(file: &CardFile, id: u32) -> Result<Framebuffer, Errno> {
    file.state
        .lock()
        .framebuffers
        .iter()
        .find(|fb| fb.id == id)
        .copied()
        .ok_or(Errno::ENOENT)
}

fn remove_framebuffer(file: &CardFile, id: u32) -> Result<(), Errno> {
    let framebuffer = find_framebuffer(file, id)?;
    let shown = file
        .state
        .lock()
        .shown
        .is_some_and(|(shown, _)| shown == id);
    if shown {
        file.card
            .scanout(0, 0, Rect::default())
            .map_err(card_error)?;
        file.state.lock().shown = None;
    }
    let orphaned = {
        let mut state = file.state.lock();
        state.framebuffers.retain(|fb| fb.id != framebuffer.id);
        let still = state
            .framebuffers
            .iter()
            .any(|fb| fb.handle == framebuffer.handle);
        let at = state
            .dumbs
            .iter()
            .position(|dumb| dumb.handle == framebuffer.handle && dumb.destroyed)
            .filter(|_| !still);
        at.map(|at| state.dumbs.remove(at).buffer)
    };
    if let Some(buffer) = orphaned {
        file.card.let_go(&[buffer]);
    }
    Ok(())
}

/// Put framebuffer `id` on the CRTC at `mode`, setting the scanout again
/// even if it is shown when `reset`, and wait for the device to have it.
fn show(file: &CardFile, id: u32, mode: ModeInfo, reset: bool) -> Result<(), Errno> {
    let framebuffer = find_framebuffer(file, id)?;
    let (width, height) = (u32::from(mode.hdisplay), u32::from(mode.vdisplay));
    if width == 0 || height == 0 || width > framebuffer.width || height > framebuffer.height {
        return Err(Errno::ENOSPC);
    }
    let rect = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let already = file
        .state
        .lock()
        .shown
        .is_some_and(|(shown, _)| shown == id);
    if reset || !already {
        file.card
            .scanout(0, framebuffer.buffer, rect)
            .map_err(card_error)?;
        // The device shows it from here, whether or not the flush goes.
        file.state.lock().shown = Some((id, mode));
    }
    let status = file
        .card
        .flush(framebuffer.buffer, rect)
        .map_err(card_error)?;
    status_error(status)
}

/// `SETCRTC` as Linux answers it: no mode and no connectors turns the CRTC
/// off; a mode takes a framebuffer, `-1` meaning the one shown, and the one
/// connector.
fn set_crtc(process: &Process, file: &CardFile, arg: u64) -> Result<usize, Errno> {
    let crtc: Crtc = read_arg(process, arg)?;
    if crtc.crtc_id != CRTC_ID {
        return Err(Errno::ENOENT);
    }
    if crtc.mode_valid == 0 {
        if crtc.count_connectors != 0 {
            return Err(Errno::EINVAL);
        }
        file.card
            .scanout(0, 0, Rect::default())
            .map_err(card_error)?;
        file.state.lock().shown = None;
        return Ok(0);
    }
    if crtc.x != 0 || crtc.y != 0 || crtc.count_connectors != 1 {
        return Err(Errno::EINVAL);
    }
    let mut connector = [0u8; 4];
    uaccess::copy_from_user(process.space(), crtc.set_connectors_ptr, &mut connector)
        .map_err(|_| Errno::EFAULT)?;
    if u32::from_le_bytes(connector) != CONNECTOR_ID {
        return Err(Errno::EINVAL);
    }
    let fb = if crtc.fb_id == u32::MAX {
        file.state
            .lock()
            .shown
            .map(|(id, _)| id)
            .ok_or(Errno::EINVAL)?
    } else {
        crtc.fb_id
    };
    // A new mode on the framebuffer already shown is set again.
    show(file, fb, crtc.mode, true)?;
    Ok(0)
}

fn dirty_rect(
    process: &Process,
    dirty: &FbDirtyCmd,
    framebuffer: &Framebuffer,
) -> Result<Rect, Errno> {
    let whole = Rect {
        x: 0,
        y: 0,
        width: framebuffer.width,
        height: framebuffer.height,
    };
    if dirty.num_clips == 0 || dirty.clips_ptr == 0 {
        return Ok(whole);
    }
    if dirty.num_clips > drm::FB_DIRTY_MAX_CLIPS {
        return Err(Errno::EINVAL);
    }
    let mut bytes = vec![0u8; dirty.num_clips as usize * ClipRect::SIZE];
    uaccess::copy_from_user(process.space(), dirty.clips_ptr, &mut bytes)
        .map_err(|_| Errno::EFAULT)?;
    let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0, 0);
    for clip in bytes
        .chunks_exact(ClipRect::SIZE)
        .filter_map(ClipRect::read)
    {
        left = left.min(u32::from(clip.x1));
        top = top.min(u32::from(clip.y1));
        right = right.max(u32::from(clip.x2)).min(framebuffer.width);
        bottom = bottom.max(u32::from(clip.y2)).min(framebuffer.height);
    }
    if left >= right || top >= bottom {
        return Ok(whole);
    }
    Ok(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

fn queue_event(file: &CardFile, user_data: u64) {
    let nanos = timer::now_nanos();
    let mut state = file.state.lock();
    state.sequence = state.sequence.wrapping_add(1);
    let event = EventVblank {
        base: Event {
            r#type: drm::EVENT_FLIP_COMPLETE,
            length: EventVblank::SIZE as u32,
        },
        user_data,
        tv_sec: u32::try_from(nanos / 1_000_000_000).unwrap_or(u32::MAX),
        tv_usec: u32::try_from(nanos % 1_000_000_000 / 1000).unwrap_or(0),
        sequence: state.sequence,
        crtc_id: CRTC_ID,
    };
    let mut bytes = [0u8; 32];
    if event.write(&mut bytes).is_some() && state.events.len() < MAX_EVENTS {
        state.events.push_back(bytes);
    }
    drop(state);
    file.readable.wake_all();
}
