//! The card: `/dev/dri/card0` driven through the legacy mode-setting calls.

use std::ffi::CStr;
use std::io;

use ferrix_linux_abi::drm::{
    self, CardRes, CreateDumb, Crtc, FbCmd2, Field, GetConnector, GetEncoder, GetPlane,
    GetPlaneRes, GetProperty, Layout, MapDumb, ModeInfo, ObjGetProperties, PropertyEnum,
    SetClientCap,
};

use crate::modeset;

/// What the display test waits for, followed by the mode and the colour.
const MARKER: &str = "compositor: scanout";

/// The card.
const CARD: &CStr = c"/dev/dri/card0";

/// An open card.
pub(crate) struct Card {
    fd: libc::c_int,
}

impl Card {
    pub(crate) fn open() -> io::Result<Self> {
        // SAFETY: CARD is a NUL-terminated path; the flags are constants.
        let fd = unsafe { libc::open(CARD.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd })
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
struct Plan {
    connector: u32,
    crtc: u32,
    /// The CRTC's index in `GETRESOURCES`' list, which `possible_crtcs` bit
    /// masks count in.
    crtc_index: usize,
    mode: ModeInfo,
}

fn plan(card: &Card) -> io::Result<Plan> {
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
        let crtc = modeset::choose_crtc(encoder.crtc_id, encoder.possible_crtcs, &crtcs)
            .ok_or_else(|| io::Error::other("no CRTC for the connector's encoder"))?;
        let crtc_index = crtcs
            .iter()
            .position(|&id| id == crtc)
            .ok_or_else(|| io::Error::other("the encoder's CRTC is not the card's"))?;
        return Ok(Plan {
            connector: connector_id,
            crtc,
            crtc_index,
            mode,
        });
    }
    Err(io::Error::other("no connected connector with a mode"))
}

pub(crate) fn show(card: &Card) -> io::Result<String> {
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
fn planes(card: &Card) -> io::Result<Vec<modeset::Plane>> {
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
