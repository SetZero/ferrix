//! The card: `/dev/dri/card0` driven through the legacy mode-setting calls.

use std::ffi::CStr;
use std::io;

use ferrix_linux_abi::drm::{
    self, CardRes, CreateDumb, Crtc, CrtcPageFlip, FbCmd2, Field, GetBlob, GetConnector,
    GetEncoder, GetPlane, GetPlaneRes, GetProperty, Layout, MapDumb, ModeInfo, ObjGetProperties,
    PropertyEnum, SetClientCap,
};

use crate::modeset;

/// What the display test waits for, followed by the mode and the colour.
const MARKER: &str = "compositor: scanout";

/// The first card, and the name every other is made from.
const CARD: &CStr = c"/dev/dri/card0";

/// The most cards looked for: a machine with more screens than this has
/// them on one card's connectors, which is where they are looked for next.
const MAX_CARDS: u32 = 8;

/// An open card.
pub struct Card {
    fd: libc::c_int,
    /// `card0`, `card1`: the file's own name, which a monitor's name and the
    /// compositor's log line carry.
    name: String,
}

impl core::fmt::Debug for Card {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Card")
            .field("name", &self.name)
            .field("fd", &self.fd)
            .finish()
    }
}

impl Card {
    /// Open the card.
    ///
    /// # Errors
    ///
    /// Whatever `open` said. A card that is not there is a compositor with
    /// no screen, which is the one thing it cannot do without.
    pub fn open() -> io::Result<Self> {
        // SAFETY: CARD is a NUL-terminated path; the flags are constants.
        let fd = unsafe { libc::open(CARD.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            fd,
            name: "card0".to_owned(),
        })
    }

    /// Open `/dev/dri/card<index>`.
    ///
    /// # Errors
    ///
    /// Whatever `open` said, which for a card that is not there is `ENOENT`.
    pub fn open_index(index: u32) -> io::Result<Self> {
        let name = format!("card{index}");
        let path = std::ffi::CString::new(format!("/dev/dri/{name}"))
            .map_err(|_| io::Error::other("a card path with a NUL in it"))?;
        // SAFETY: `path` is a NUL-terminated path held across the call; the
        // flags are constants.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, name })
    }

    /// What the card is called, which is what a monitor's name is made from.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Run `request` with `value` as its argument, and read the kernel's
    /// answer back into it.
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
}

/// The address of `items` as a user pointer field.
fn address<T>(items: &mut [T]) -> u64 {
    items.as_mut_ptr() as usize as u64
}

/// What the modeset settled on.
#[derive(Clone, Debug)]
pub struct Plan {
    /// The connector the screen is on.
    pub connector: u32,
    /// The CRTC driving it.
    pub crtc: u32,
    /// The CRTC's index in `GETRESOURCES`' list, which `possible_crtcs` bit
    /// masks count in.
    pub crtc_index: usize,
    /// The mode it was set to.
    pub mode: ModeInfo,
    /// The monitor's name, made from the connector's type and number.
    pub name: String,
    /// What the monitor says it is: its `EDID`, read. `None` for a
    /// connector with no `EDID` property or a blob that is not one -- a
    /// virtual screen has neither.
    pub edid: Option<crate::Edid>,
}

impl Plan {
    /// The mode's size in pixels.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.mode.hdisplay as u32, self.mode.vdisplay as u32)
    }
}

/// Every card the machine has, in order, from `/dev/dri/card0` up until one
/// is not there.
///
/// A machine's screens are on one card's connectors or on a card each, and
/// which it is is the machine's business rather than the compositor's: both
/// are monitors to everything above this.
#[must_use]
pub fn cards() -> Vec<Card> {
    let mut cards = Vec::new();
    for index in 0..MAX_CARDS {
        match Card::open_index(index) {
            Ok(card) => cards.push(card),
            // The first gap ends the search, as `/dev/dri` numbers cards
            // from zero with no holes.
            Err(_) => break,
        }
    }
    cards
}

/// Find a connected connector with a mode, and a CRTC that can drive it.
///
/// # Errors
///
/// A card with no connected connector, or one whose encoder has no CRTC.
pub fn plan(card: &Card) -> io::Result<Plan> {
    plans(card)?
        .into_iter()
        .next()
        .ok_or_else(|| io::Error::other("no connected connector with a mode"))
}

/// Every connected connector of `card` with a mode, each with a CRTC of its
/// own.
///
/// A CRTC drives one connector at a time, so a connector whose only CRTC is
/// already taken by another is left out rather than made to share: two
/// monitors on one CRTC would be a clone, which is not what a second monitor
/// is for.
///
/// # Errors
///
/// Whatever the card said.
pub fn plans(card: &Card) -> io::Result<Vec<Plan>> {
    let mut resources = CardRes::ZERO;
    card.ioctl(drm::IOCTL_MODE_GETRESOURCES, &mut resources)?;
    let mut crtcs = vec![0u32; resources.count_crtcs as usize];
    let mut connectors = vec![0u32; resources.count_connectors as usize];
    let mut encoders = vec![0u32; resources.count_encoders as usize];
    resources = CardRes {
        crtc_id_ptr: address(&mut crtcs),
        connector_id_ptr: address(&mut connectors),
        encoder_id_ptr: address(&mut encoders),
        count_fbs: 0,
        ..resources
    };
    card.ioctl(drm::IOCTL_MODE_GETRESOURCES, &mut resources)?;

    let mut found: Vec<Plan> = Vec::new();
    let mut taken: Vec<u32> = Vec::new();
    for &connector_id in &connectors {
        let mut connector = GetConnector {
            connector_id,
            ..GetConnector::ZERO
        };
        card.ioctl(drm::IOCTL_MODE_GETCONNECTOR, &mut connector)?;
        if connector.connection != drm::CONNECTION_CONNECTED || connector.count_modes == 0 {
            continue;
        }
        let mut modes = vec![0u8; connector.count_modes as usize * ModeInfo::SIZE];
        let mut connector_encoders = vec![0u32; connector.count_encoders as usize];
        connector = GetConnector {
            modes_ptr: address(&mut modes),
            encoders_ptr: address(&mut connector_encoders),
            count_props: 0,
            ..connector
        };
        card.ioctl(drm::IOCTL_MODE_GETCONNECTOR, &mut connector)?;
        let modes: Vec<ModeInfo> = modes
            .chunks_exact(ModeInfo::SIZE)
            .filter_map(ModeInfo::read)
            .collect();
        let Some(mode) = modeset::choose_mode(&modes) else {
            continue;
        };
        let encoder_id = if connector.encoder_id != 0 {
            connector.encoder_id
        } else {
            connector_encoders.first().copied().unwrap_or(0)
        };
        let mut encoder = GetEncoder {
            encoder_id,
            ..GetEncoder::ZERO
        };
        card.ioctl(drm::IOCTL_MODE_GETENCODER, &mut encoder)?;
        // The CRTCs another connector of this card already has are not
        // offered again.
        let free: Vec<u32> = crtcs
            .iter()
            .copied()
            .filter(|id| !taken.contains(id))
            .collect();
        let already = (!taken.contains(&encoder.crtc_id)).then_some(encoder.crtc_id);
        let Some(crtc) = modeset::choose_crtc(
            already.unwrap_or(0),
            encoder.possible_crtcs & mask_of(&free, &crtcs),
            &free,
        ) else {
            continue;
        };
        let crtc_index = crtcs
            .iter()
            .position(|&id| id == crtc)
            .ok_or_else(|| io::Error::other("the encoder's CRTC is not the card's"))?;
        taken.push(crtc);
        found.push(Plan {
            connector: connector_id,
            crtc,
            crtc_index,
            mode,
            name: connector_name(&connector),
            // A connector that will not say what it is gets no
            // description, which is one monitor a `desc:` rule cannot
            // name -- not a card the compositor refuses to open.
            edid: connector_edid(card, connector_id).ok().flatten(),
        });
    }
    Ok(found)
}

/// The `possible_crtcs` bits of the CRTCs in `free`, which count in the
/// order `all` lists them.
fn mask_of(free: &[u32], all: &[u32]) -> u32 {
    let mut mask = 0u32;
    for (index, id) in all.iter().enumerate() {
        if free.contains(id)
            && let Ok(bit) = u32::try_from(index)
            && let Some(one) = 1u32.checked_shl(bit)
        {
            mask |= one;
        }
    }
    mask
}

/// What to call the monitor on a connector.
///
/// Hyprland names a monitor after its connector -- `DP-1`, `HDMI-A-2` -- and
/// a person's `monitor =` lines and `hyprctl monitors` both use that name.
/// The type is the connector's; the number is the caller's, because two
/// cards each have a `Virtual-1` and a person's two screens must not share
/// a name. [`rename`] gives them theirs.
fn connector_name(connector: &GetConnector) -> String {
    format!(
        "{}-{}",
        modeset::connector_type_name(connector.connector_type),
        connector.connector_type_id
    )
}

/// Number `plans` as wlroots numbers outputs: each connector type counted
/// from one across every card, so a machine with two virtio-gpu cards has
/// `Virtual-1` and `Virtual-2` rather than two `Virtual-1`s.
pub fn rename(plans: &mut [Plan]) {
    let mut counts: Vec<(String, u32)> = Vec::new();
    for plan in plans {
        let kind = plan
            .name
            .rsplit_once('-')
            .map_or(plan.name.clone(), |(kind, _)| kind.to_owned());
        let number = match counts.iter_mut().find(|(known, _)| *known == kind) {
            Some((_, count)) => {
                *count = count.saturating_add(1);
                *count
            }
            None => {
                counts.push((kind.clone(), 1));
                1
            }
        };
        plan.name = format!("{kind}-{number}");
    }
}

/// Fill the screen with one colour and show it: `compositor/blank`'s whole
/// job, and the proof that the path from a program to QEMU's window works.
///
/// # Errors
///
/// Whatever the card said.
pub fn show(card: &Card) -> io::Result<String> {
    let mut plan = plan(card)?;
    let (width, height) = (u32::from(plan.mode.hdisplay), u32::from(plan.mode.vdisplay));

    let mut dumb = CreateDumb {
        width,
        height,
        bpp: 32,
        ..CreateDumb::ZERO
    };
    card.ioctl(drm::IOCTL_MODE_CREATE_DUMB, &mut dumb)?;
    let mut map = MapDumb {
        handle: dumb.handle,
        ..MapDumb::ZERO
    };
    card.ioctl(drm::IOCTL_MODE_MAP_DUMB, &mut map)?;

    let size = usize::try_from(dumb.size).map_err(io::Error::other)?;
    let offset = libc::off_t::try_from(map.offset).map_err(io::Error::other)?;
    // SAFETY: a new shared mapping of the card at the offset MAP_DUMB gave,
    // of the size CREATE_DUMB gave; nothing else maps it.
    let base = unsafe {
        libc::mmap(
            core::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            card.fd,
            offset,
        )
    };
    if base == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `base` maps `size` bytes, readable and writable, for the rest
    // of the program, which never unmaps it.
    let pixels = unsafe { core::slice::from_raw_parts_mut(base.cast::<u8>(), size) };
    modeset::fill(
        pixels,
        width as usize,
        height as usize,
        dumb.pitch as usize,
        modeset::BACKGROUND,
        cfg!(feature = "negative-control"),
    );

    let mut framebuffer = FbCmd2 {
        width,
        height,
        pixel_format: drm::FORMAT_XRGB8888,
        handles: [dumb.handle, 0, 0, 0],
        pitches: [dumb.pitch, 0, 0, 0],
        ..FbCmd2::ZERO
    };
    card.ioctl(drm::IOCTL_MODE_ADDFB2, &mut framebuffer)?;

    let mut connectors = [plan.connector];
    let mut crtc = Crtc {
        set_connectors_ptr: address(&mut connectors),
        count_connectors: 1,
        crtc_id: plan.crtc,
        fb_id: framebuffer.fb_id,
        mode_valid: 1,
        mode: plan.mode,
        ..Crtc::ZERO
    };
    card.ioctl(drm::IOCTL_MODE_SETCRTC, &mut crtc)?;
    plan.mode = crtc.mode;

    let planes = planes(card)?;
    let described =
        modeset::describe_planes(&planes, plan.crtc_index, plan.crtc, framebuffer.fb_id)
            .map_err(io::Error::other)?;

    Ok(format!(
        "{MARKER} {} {width}x{height} colour 0x{:06x} {described}",
        modeset::mode_name(&plan.mode),
        modeset::BACKGROUND
    ))
}

/// Every plane the card lists once universal planes are asked for, read the
/// way Smithay's legacy path reads them (`backend/drm/mod.rs`, `planes` and
/// `plane_type`): `GETPLANERESOURCES`, `GETPLANE`, then each plane's
/// properties through `OBJ_GETPROPERTIES` and `GETPROPERTY`, looking for
/// `type`. Each list is asked for twice, counts first.
/// Every plane the card lists.
///
/// # Errors
///
/// Whatever the card said.
pub fn planes(card: &Card) -> io::Result<Vec<modeset::Plane>> {
    let mut universal = SetClientCap {
        capability: drm::CLIENT_CAP_UNIVERSAL_PLANES,
        value: 1,
    };
    card.ioctl(drm::IOCTL_SET_CLIENT_CAP, &mut universal)?;

    let mut resources = GetPlaneRes::ZERO;
    card.ioctl(drm::IOCTL_MODE_GETPLANERESOURCES, &mut resources)?;
    let mut ids = vec![0u32; resources.count_planes as usize];
    resources.plane_id_ptr = address(&mut ids);
    card.ioctl(drm::IOCTL_MODE_GETPLANERESOURCES, &mut resources)?;
    ids.truncate(resources.count_planes as usize);

    let mut planes = Vec::with_capacity(ids.len());
    for id in ids {
        let mut plane = GetPlane {
            plane_id: id,
            ..GetPlane::ZERO
        };
        card.ioctl(drm::IOCTL_MODE_GETPLANE, &mut plane)?;
        planes.push(modeset::Plane {
            id,
            crtc: plane.crtc_id,
            framebuffer: plane.fb_id,
            possible_crtcs: plane.possible_crtcs,
            kind: plane_type(card, id)?,
        });
    }
    Ok(planes)
}

/// What connector `id` says it is: its `EDID` property's blob, read.
///
/// `None` where the connector has no `EDID` property, where its value is
/// zero -- which is a connector with nothing plugged in, or a virtual one
/// that has nothing to say -- or where the bytes are not an EDID.
fn connector_edid(card: &Card, id: u32) -> io::Result<Option<crate::Edid>> {
    let mut request = ObjGetProperties {
        obj_id: id,
        obj_type: drm::MODE_OBJECT_CONNECTOR,
        ..ObjGetProperties::ZERO
    };
    card.ioctl(drm::IOCTL_MODE_OBJ_GETPROPERTIES, &mut request)?;
    let mut properties = vec![0u32; request.count_props as usize];
    let mut values = vec![0u64; request.count_props as usize];
    request.props_ptr = address(&mut properties);
    request.prop_values_ptr = address(&mut values);
    card.ioctl(drm::IOCTL_MODE_OBJ_GETPROPERTIES, &mut request)?;

    for (&property_id, &value) in properties.iter().zip(&values) {
        let mut property = GetProperty {
            prop_id: property_id,
            ..GetProperty::ZERO
        };
        card.ioctl(drm::IOCTL_MODE_GETPROPERTY, &mut property)?;
        if modeset::c_name(&property.name) != "EDID" {
            continue;
        }
        let Ok(blob_id) = u32::try_from(value) else {
            return Ok(None);
        };
        if blob_id == 0 {
            return Ok(None);
        }
        // Twice, as every counted DRM call is: once for the length, once
        // for the bytes.
        let mut blob = GetBlob {
            blob_id,
            ..GetBlob::ZERO
        };
        card.ioctl(drm::IOCTL_MODE_GETPROPBLOB, &mut blob)?;
        if blob.length == 0 {
            return Ok(None);
        }
        let mut bytes = vec![0u8; blob.length as usize];
        blob.data = address(&mut bytes);
        card.ioctl(drm::IOCTL_MODE_GETPROPBLOB, &mut blob)?;
        return Ok(crate::Edid::parse(&bytes));
    }
    Ok(None)
}

/// The name of plane `id`'s `type` value in the property's enum list, if the
/// plane has a `type` property.
fn plane_type(card: &Card, id: u32) -> io::Result<Option<String>> {
    let mut request = ObjGetProperties {
        obj_id: id,
        obj_type: drm::MODE_OBJECT_PLANE,
        ..ObjGetProperties::ZERO
    };
    card.ioctl(drm::IOCTL_MODE_OBJ_GETPROPERTIES, &mut request)?;
    let mut properties = vec![0u32; request.count_props as usize];
    let mut values = vec![0u64; request.count_props as usize];
    request.props_ptr = address(&mut properties);
    request.prop_values_ptr = address(&mut values);
    card.ioctl(drm::IOCTL_MODE_OBJ_GETPROPERTIES, &mut request)?;

    for (&property_id, &value) in properties.iter().zip(&values) {
        let mut property = GetProperty {
            prop_id: property_id,
            ..GetProperty::ZERO
        };
        card.ioctl(drm::IOCTL_MODE_GETPROPERTY, &mut property)?;
        if modeset::c_name(&property.name) != "type" {
            continue;
        }
        let mut enums = vec![0u8; property.count_enum_blobs as usize * PropertyEnum::SIZE];
        property = GetProperty {
            enum_blob_ptr: address(&mut enums),
            count_values: 0,
            ..property
        };
        card.ioctl(drm::IOCTL_MODE_GETPROPERTY, &mut property)?;
        let names: Vec<(u64, String)> = enums
            .chunks_exact(PropertyEnum::SIZE)
            .filter_map(PropertyEnum::read)
            .map(|entry| (entry.value, modeset::c_name(&entry.name)))
            .collect();
        return Ok(Some(
            modeset::enum_name(value, &names).unwrap_or_else(|| format!("value {value}")),
        ));
    }
    Ok(None)
}

/// A dumb buffer: memory the card can scan out of, mapped for the program to
/// draw into.
///
/// "Dumb" is the kernel's own word for a buffer with no format beyond a size
/// and a pitch: there is no GPU here, so a compositor draws every pixel
/// itself and hands the card the result.
#[derive(Debug)]
pub struct Dumb {
    /// The `ADDFB2` id, which a `SETCRTC` or a `PAGE_FLIP` names.
    pub framebuffer: u32,
    /// Bytes from one row's start to the next.
    pub pitch: u32,
    /// The mapping, for the whole of the program's life.
    pixels: &'static mut [u8],
    /// The handle, kept so the buffer can be freed.
    handle: u32,
}

impl Dumb {
    /// Make a `width` by `height` `XRGB8888` buffer on `card`, map it, and
    /// give the card a framebuffer id for it.
    ///
    /// # Errors
    ///
    /// Whatever the card said.
    pub fn new(card: &Card, width: u32, height: u32) -> io::Result<Self> {
        let mut dumb = CreateDumb {
            width,
            height,
            bpp: 32,
            ..CreateDumb::ZERO
        };
        card.ioctl(drm::IOCTL_MODE_CREATE_DUMB, &mut dumb)?;
        let mut map = MapDumb {
            handle: dumb.handle,
            ..MapDumb::ZERO
        };
        card.ioctl(drm::IOCTL_MODE_MAP_DUMB, &mut map)?;

        let size = usize::try_from(dumb.size).map_err(io::Error::other)?;
        let offset = libc::off_t::try_from(map.offset).map_err(io::Error::other)?;
        let pixels = card.map(size, offset)?;

        let mut framebuffer = FbCmd2 {
            width,
            height,
            pixel_format: drm::FORMAT_XRGB8888,
            handles: [dumb.handle, 0, 0, 0],
            pitches: [dumb.pitch, 0, 0, 0],
            ..FbCmd2::ZERO
        };
        card.ioctl(drm::IOCTL_MODE_ADDFB2, &mut framebuffer)?;

        Ok(Self {
            framebuffer: framebuffer.fb_id,
            pitch: dumb.pitch,
            pixels,
            handle: dumb.handle,
        })
    }

    /// The memory, to draw into.
    #[must_use]
    pub fn pixels(&mut self) -> &mut [u8] {
        self.pixels
    }

    /// The memory, to read back: what a test compares and what a PPM dump
    /// writes out.
    #[must_use]
    pub fn shown(&self) -> &[u8] {
        self.pixels
    }

    /// The handle the kernel knows the buffer by.
    #[must_use]
    pub const fn handle(&self) -> u32 {
        self.handle
    }
}

impl Card {
    /// Show `buffer` on the plan's CRTC, which is the first frame: a page
    /// flip needs a mode already set.
    ///
    /// # Errors
    ///
    /// Whatever the card said.
    pub fn set_mode(&self, plan: &Plan, buffer: &Dumb) -> io::Result<ModeInfo> {
        let mut connectors = [plan.connector];
        let mut crtc = Crtc {
            set_connectors_ptr: address(&mut connectors),
            count_connectors: 1,
            crtc_id: plan.crtc,
            fb_id: buffer.framebuffer,
            mode_valid: 1,
            mode: plan.mode,
            ..Crtc::ZERO
        };
        self.ioctl(drm::IOCTL_MODE_SETCRTC, &mut crtc)?;
        Ok(crtc.mode)
    }

    /// Show `buffer` at the next vertical blank.
    ///
    /// The event the flip promises is read from the card's descriptor and is
    /// what a frame loop waits on; this asks for it and does not wait, so a
    /// caller that never reads the descriptor still flips -- one frame
    /// behind, which for a compositor drawing on demand is no frame behind at
    /// all.
    ///
    /// # Errors
    ///
    /// Whatever the card said.
    pub fn page_flip(&self, plan: &Plan, buffer: &Dumb) -> io::Result<()> {
        let mut flip = CrtcPageFlip {
            crtc_id: plan.crtc,
            fb_id: buffer.framebuffer,
            flags: drm::PAGE_FLIP_EVENT,
            ..CrtcPageFlip::ZERO
        };
        self.ioctl(drm::IOCTL_MODE_PAGE_FLIP, &mut flip)
    }

    /// Map `size` bytes of the card at `offset`.
    ///
    /// The mapping lives as long as the program: a compositor's scanout
    /// buffers are made once and drawn into for ever, and unmapping one while
    /// the card scans it out is a screen full of whatever came next.
    fn map(&self, size: usize, offset: libc::off_t) -> io::Result<&'static mut [u8]> {
        #[expect(
            unsafe_code,
            reason = "AUDIT: mmap is not in std; the call maps the card this process opened at the offset MAP_DUMB gave, and the result is checked against MAP_FAILED"
        )]
        // SAFETY: a null hint lets the kernel choose the address; `self.fd`
        // is the card this process opened.
        let base = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.fd,
                offset,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        #[expect(
            unsafe_code,
            reason = "AUDIT: the mapping is never unmapped, so a 'static slice of it is sound; a scanout buffer unmapped while the card reads it is a screen full of whatever came next"
        )]
        // SAFETY: `base` maps `size` bytes, readable and writable, and
        // nothing ever unmaps it.
        let pixels = unsafe { core::slice::from_raw_parts_mut(base.cast::<u8>(), size) };
        Ok(pixels)
    }
}
