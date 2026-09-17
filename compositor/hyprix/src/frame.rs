//! Turning what the clients have committed into one frame.

use std::collections::BTreeMap;

use compositor_layout::Rect;
use compositor_layout::{MonitorLayout, WindowId};
use compositor_render::{
    Canvas, Damage, Format, LayerFrame, Style, Surface, Target, render_with_layers,
};
use compositor_server::Client;
use compositor_wire::ObjectId;

use crate::backend::Backend;
use crate::pool::Mapping;
use crate::state::Slot;

/// Where a frame is drawn: the canvas, the screen it goes to, where the
/// monitor is in the space every window's rectangle is in, and how a window
/// is drawn.
#[derive(Debug)]
pub struct Output<'a> {
    /// The canvas the frame is composed on.
    pub canvas: &'a mut Canvas,
    /// The screen it is shown on.
    pub backend: &'a mut dyn Backend,
    /// Where the monitor is in the global space.
    pub origin: (i64, i64),
    /// The colours a window is drawn with, and what a `windowrule` changed
    /// for one of them.
    pub style: &'a Style,
    /// What each window is drawn with where a rule said something else.
    pub styles: &'a BTreeMap<WindowId, compositor_render::WindowStyle>,
    /// How many buffer pixels one logical pixel is: `monitor = ..., 2`.
    /// Everything above the renderer works in logical pixels, and this is
    /// where they become the screen's own.
    pub scale: f64,
    /// Where the pointer is and what it looks like, or `None` for a screen
    /// with no pointer on it.
    pub cursor: Option<Cursor>,
    /// The ramps a night-light set on this screen, if one did.
    pub gamma: Option<Gamma>,
    /// The surface a drag is carrying, drawn at the pointer.
    ///
    /// Its rectangle's position is where the pointer is; the size is the
    /// surface's own, because a drag icon is whatever the client drew and
    /// not something the compositor sizes.
    pub drag_icon: Option<Placed>,
    /// What to copy from the canvas to the screen, which is not always what
    /// was drawn on the canvas: a backend with two buffers is drawing into
    /// the one that holds the frame before last, so it is owed that frame's
    /// damage as well as this one's. `crate::damage` says the rest.
    pub present: Damage,
}

/// A night-light's three ramps, one entry a level.
///
/// `zwlr_gamma_control_v1` hands the compositor a descriptor holding three
/// tables of sixteen-bit entries -- red, then green, then blue -- and every
/// level a pixel can have is looked up in its channel's table on the way to
/// the screen. That is what `gammastep` and `hyprsunset` do to make an
/// evening screen warmer.
///
/// A real compositor hands the table to the connector and the hardware does
/// the lookup. This one has no such hardware -- the screen is memory -- so
/// the lookup is done here, once a frame, over the pixels that were drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gamma {
    /// One byte a level, taken from the top half of the protocol's
    /// sixteen: the screen is eight bits a channel.
    ramps: [[u8; SIZE]; 3],
}

/// How many entries each ramp has, which is what the client was told.
const SIZE: usize = 256;

impl Gamma {
    /// Read the three ramps off a descriptor, or `None` if they are not
    /// there.
    ///
    /// The descriptor is this compositor's once the client has sent it, and
    /// is closed here whatever it held.
    #[must_use]
    pub fn read(fd: compositor_wire::Fd) -> Option<Self> {
        use std::io::Read as _;
        use std::os::fd::FromRawFd as _;
        #[expect(
            unsafe_code,
            reason = "AUDIT: the descriptor arrived on this compositor's own socket and is claimed; File takes it and closes it"
        )]
        // SAFETY: a descriptor this process received and owns, claimed from
        // the connection so nothing else will close it.
        let mut file = unsafe { std::fs::File::from_raw_fd(fd.0) };
        let mut bytes = [0u8; SIZE * 3 * 2];
        file.read_exact(&mut bytes).ok()?;
        let mut ramps = [[0u8; SIZE]; 3];
        for (channel, ramp) in ramps.iter_mut().enumerate() {
            for (level, entry) in ramp.iter_mut().enumerate() {
                // Little-endian sixteen-bit, which is what a client writes
                // from a `uint16_t` array; the screen keeps the top byte.
                let at = (channel * SIZE + level) * 2 + 1;
                *entry = bytes.get(at).copied().unwrap_or(0);
            }
        }
        Some(Self { ramps })
    }

    /// Put one screen's pixels through the ramps, in place, over the part
    /// of it this frame copied from the canvas.
    ///
    /// Only that part: the ramps are applied to the screen's buffer rather
    /// than to the canvas, so a pixel the frame did not copy has been
    /// through them already and putting it through a second time would
    /// warm it twice.
    ///
    /// The buffer is `XRGB8888` or `ARGB8888`; either way the three colour
    /// bytes are the low three of each little-endian word and the fourth is
    /// left alone.
    pub fn apply(&self, buffer: &mut [u8], stride: u32, damage: &Damage) {
        let stride = stride as usize;
        for rect in damage.rects() {
            let (Ok(left), Ok(width)) = (usize::try_from(rect.x), usize::try_from(rect.width))
            else {
                continue;
            };
            for y in rect.y..rect.bottom() {
                let Ok(row) = usize::try_from(y) else {
                    continue;
                };
                let start = row
                    .saturating_mul(stride)
                    .saturating_add(left.saturating_mul(4));
                let end = start.saturating_add(width.saturating_mul(4));
                let Some(span) = buffer.get_mut(start..end) else {
                    continue;
                };
                self.rows(span);
            }
        }
    }

    /// One run of pixels through the ramps.
    fn rows(&self, span: &mut [u8]) {
        for pixel in span.chunks_exact_mut(4) {
            for (at, channel) in [(2usize, 0usize), (1, 1), (0, 2)] {
                let Some(ramp) = self.ramps.get(channel) else {
                    continue;
                };
                let Some(byte) = pixel.get_mut(at) else {
                    continue;
                };
                if let Some(mapped) = ramp.get(usize::from(*byte)) {
                    *byte = *mapped;
                }
            }
        }
    }
}

/// The pointer, as the frame draws it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cursor {
    /// Where its hotspot is, in the space every window's rectangle is in.
    pub at: (i64, i64),
    /// The client's own cursor surface and the hotspot inside it, or `None`
    /// for the compositor's built-in arrow.
    pub surface: Option<(usize, ObjectId, (i32, i32))>,
    /// Whether it is drawn at all: a client may ask for no pointer.
    pub shown: bool,
}

/// Where one layer surface is, and whose it is.
///
/// The compositor works the rectangle out from the protocol's anchor rules
/// each time the layer surfaces change; this is what the drawing needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Placed {
    /// Which connection.
    pub client: usize,
    /// The `wl_surface` under the layer surface.
    pub surface: ObjectId,
    /// Where it goes, in the global space.
    pub rect: Rect,
    /// Whether it is drawn above the windows.
    pub above: bool,
    /// What its `layerrule` lines gave it.
    pub rules: LayerRules,
}

/// What a surface's `layerrule` lines came to, as the frame reads them.
///
/// A copy of the fields the drawing uses, rather than the whole of
/// `compositor_config::Layered`: a `Placed` is compared with the last pass's
/// and a `String` in it would allocate on every frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct LayerRules {
    /// `blur`: what is behind it is blurred.
    pub blur: bool,
    /// `dim_around`: everything behind it is darkened while it is up,
    /// which is what a launcher does to the desktop.
    pub dim_around: bool,
    /// `abovelock`: it is drawn over the session lock.
    pub above_lock: bool,
    /// `noscreenshare`: a screenshot leaves it out.
    pub no_screen_share: bool,
    /// `order`: where it goes among its own layer's surfaces, a higher
    /// number nearer the top.
    pub order: i64,
}

/// Which client and which surface a window's pixels come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Source {
    /// Which connection.
    pub client: usize,
    /// The `wl_surface`.
    pub surface: ObjectId,
}

/// Draw every window of `output` into `backend`, taking each one's pixels
/// from the buffer its client committed.
///
/// A window whose client has committed nothing, or whose buffer is in a pool
/// that is not mapped, is drawn as its border and background alone -- which is
/// what a window that has not painted yet looks like, and is better than
/// refusing to draw the frame.
pub fn draw(
    target: &mut Output<'_>,
    output: &MonitorLayout,
    clients: &[Slot],
    sources: &BTreeMap<WindowId, Source>,
    layers: &[Placed],
    damage: &Damage,
) -> Result<(), String> {
    draw_windows(target, output, clients, sources, layers, damage)
}

/// Draw the screen while the session is locked: the lock's own surface and
/// nothing else.
///
/// `locked` is where the lock surface's pixels are, or `None` for a screen
/// the lock has not covered yet, and `over` the layer surfaces a
/// `layerrule = abovelock` asked to be drawn on top of it. A screen with no lock surface is drawn as
/// the background alone -- black, since the style's background is what the
/// compositor clears to -- because what it must *not* show is what was on
/// it before. That is the whole point of the protocol: the compositor stops
/// drawing the windows the moment the lock is taken, before the client has
/// drawn anything at all.
pub fn draw_locked(
    target: &mut Output<'_>,
    output: &MonitorLayout,
    clients: &[Slot],
    locked: Option<Placed>,
    over: &[Placed],
    damage: &Damage,
) -> Result<(), String> {
    let empty = MonitorLayout {
        windows: Vec::new(),
        ..output.clone()
    };
    let sources = BTreeMap::new();
    // The lock's own surface first, and then whatever a `layerrule =
    // abovelock` asked to be drawn over it -- an on-screen keyboard, which
    // is the whole reason that rule exists.
    let layers: Vec<Placed> = locked.into_iter().chain(over.iter().copied()).collect();
    draw_windows(target, &empty, clients, &sources, &layers, damage)
}

/// Draw a screen that `dpms off` has turned off: black, and nothing else.
///
/// Hyprland turns the connector itself off, which a virtual screen has no
/// equivalent of; what a person sees is the same, and what the compositor
/// must *not* show -- the windows that were there -- is gone either way.
pub fn draw_dark(target: &mut Output<'_>, damage: &Damage) -> Result<(), String> {
    let Output {
        canvas,
        backend,
        present,
        ..
    } = target;
    canvas.clear(compositor_render::Color(0xff00_0000), damage);
    let (width, height) = backend.size();
    let stride = backend.stride();
    let mut screen = Target::new(backend.buffer(), width, height, stride)
        .map_err(|error| format!("the screen's buffer is not one: {error:?}"))?;
    canvas
        .present(&mut screen, present)
        .map_err(|error| format!("the frame does not fit the screen: {error:?}"))?;
    backend.present().map_err(|error| error.to_string())
}

/// The two above, which differ only in what they are given to draw.
fn draw_windows(
    target: &mut Output<'_>,
    output: &MonitorLayout,
    clients: &[Slot],
    sources: &BTreeMap<WindowId, Source>,
    layers: &[Placed],
    damage: &Damage,
) -> Result<(), String> {
    let Output {
        canvas,
        backend,
        origin,
        style,
        styles,
        scale,
        cursor,
        gamma,
        drag_icon,
        present,
    } = target;
    let gamma = *gamma;
    let drag_icon = *drag_icon;
    let cursor = *cursor;
    let (origin, scale) = (*origin, *scale);
    // The layout, the decorations and the layer surfaces are all in logical
    // pixels; a scaled monitor draws each of them as `scale` buffer pixels.
    let output = &compositor_render::scaled(output, origin, scale);
    let style = &style.at_scale(scale);
    // Gather every window's pixels first: `render` takes them all at once, so
    // each borrow of a mapping has to live as long as the call.
    let mut surfaces: BTreeMap<WindowId, Surface<'_>> = BTreeMap::new();
    for placed in &output.windows {
        let Some(source) = sources.get(&placed.window) else {
            continue;
        };
        let Some(slot) = clients.get(source.client) else {
            continue;
        };
        if let Some(surface) = pixels(slot.client(), slot.pools(), source.surface) {
            let _ = surfaces.insert(placed.window, surface);
        }
    }

    // The bars and wallpapers, in the order they were made, which is the
    // order they were placed in.
    let drawn: Vec<LayerFrame<'_>> = layers
        .iter()
        .map(|placed| LayerFrame {
            rect: scale_rect(placed.rect, origin, scale),
            above: placed.above,
            surface: clients
                .get(placed.client)
                .and_then(|slot| pixels(slot.client(), slot.pools(), placed.surface)),
            dim_around: placed.rules.dim_around,
            blur: placed.rules.blur,
        })
        .collect();

    // Every window's rectangle is in the global space all monitors share;
    // the canvas is this monitor's, so the origin is where the monitor is.
    let styles = compositor_render::Styles {
        base: style,
        windows: styles,
    };
    let _ = render_with_layers(canvas, output, origin, &styles, &surfaces, &drawn, damage);

    // The drag icon under the pointer and over everything else: what a
    // drag looks like is a thing following the pointer, and a compositor
    // that drew it under a window would have a drag nobody can see.
    if let Some(icon) = drag_icon
        && let Some(at) = drag_rect(clients, &icon, origin, scale)
        && let Some(slot) = clients.get(icon.client)
        && let Some(surface) = pixels(slot.client(), slot.pools(), icon.surface)
    {
        canvas.composite(&surface, at, damage);
    }

    // The pointer last, over everything: it is not a window, not a layer
    // surface and not part of the layout, and a compositor that drew it
    // under a menu would have a pointer nobody can follow.
    //
    // The arrow is kept here rather than made once and held, because it is
    // 24x24 and a frame that has to allocate it is a frame that has already
    // blurred a window.
    let arrow;
    if let Some(cursor) = cursor.filter(|cursor| cursor.shown)
        && let Some(at) = cursor_rect(clients, &cursor, origin, scale)
    {
        let own = cursor.surface.and_then(|(client, surface, _)| {
            let slot = clients.get(client)?;
            pixels(slot.client(), slot.pools(), surface)
        });
        let surface = match own {
            Some(surface) => Some(surface),
            None => {
                arrow = compositor_render::cursor::arrow();
                compositor_render::cursor::surface(&arrow).ok()
            }
        };
        if let Some(surface) = surface {
            canvas.composite(&surface, at, damage);
        }
    }

    let (width, height) = backend.size();
    let stride = backend.stride();
    let mut target = Target::new(backend.buffer(), width, height, stride)
        .map_err(|error| format!("the screen's buffer is not one: {error:?}"))?;
    canvas
        .present(&mut target, present)
        .map_err(|error| format!("the frame does not fit the screen: {error:?}"))?;
    // The night-light's ramps, over what was drawn: on hardware the
    // connector does this, and here the compositor does -- the same picture
    // by a slower road.
    if let Some(gamma) = gamma {
        gamma.apply(backend.buffer(), stride, present);
    }
    backend.present().map_err(|error| error.to_string())
}

/// One rectangle in the buffer pixels of a monitor at `scale`, grown away
/// from the monitor's own corner, as `compositor_render::scaled` grows a
/// window's.
fn scale_rect(rect: Rect, origin: (i64, i64), scale: f64) -> Rect {
    if (scale - 1.0).abs() < f64::EPSILON {
        return rect;
    }
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "a screen's pixels are far inside f64's exact range, and a scaled length is \
                  rounded to the pixel it lands on"
    )]
    let grow = |value: i64| (value as f64 * scale).round() as i64;
    let at = |value: i64, from: i64| from.saturating_add(grow(value.saturating_sub(from)));
    Rect::new(
        at(rect.x, origin.0),
        at(rect.y, origin.1),
        grow(rect.width),
        grow(rect.height),
    )
}

/// One rectangle of the global space in the screen's own pixels: scaled,
/// and moved into the canvas's own corner.
///
/// Everything above the renderer is in logical pixels in the space all the
/// monitors share; a canvas begins at its monitor's corner and counts in
/// the screen's own pixels, and this is the one conversion between them.
pub(crate) fn local(rect: Rect, origin: (i64, i64), scale: f64) -> Rect {
    let at = scale_rect(rect, origin, scale);
    Rect::new(
        at.x.saturating_sub(origin.0),
        at.y.saturating_sub(origin.1),
        at.width,
        at.height,
    )
}

/// Whether a surface can be seen through, which is half of what decides
/// whether the renderer draws a blur behind it.
///
/// A format with alpha may be translucent anywhere; one without it never
/// is. A surface with nothing committed shows nothing, and nothing is
/// blurred behind it.
pub(crate) fn translucent(clients: &[Slot], client: usize, surface: ObjectId) -> bool {
    clients
        .get(client)
        .and_then(|slot| pixels(slot.client(), slot.pools(), surface))
        .is_some_and(|pixels| pixels.format() == Format::Argb8888)
}

/// Where the surface a drag is carrying is drawn, in the screen's own
/// pixels, or `None` when the client has drawn nothing to carry.
///
/// The size is the surface's own: a drag icon is whatever the client drew
/// and not something the compositor sizes.
pub(crate) fn drag_rect(
    clients: &[Slot],
    icon: &Placed,
    origin: (i64, i64),
    scale: f64,
) -> Option<Rect> {
    let slot = clients.get(icon.client)?;
    let surface = pixels(slot.client(), slot.pools(), icon.surface)?;
    Some(local(
        Rect::new(
            icon.rect.x,
            icon.rect.y,
            i64::from(surface.width()),
            i64::from(surface.height()),
        ),
        origin,
        scale,
    ))
}

/// Where the pointer's pixels go, in the screen's own pixels, or `None` for
/// a pointer that is not drawn at all.
///
/// The client's own cursor surface where it set one, and the built-in arrow
/// where it did not; the hotspot is what the position names, so the
/// rectangle begins that far above and left of it.
pub(crate) fn cursor_rect(
    clients: &[Slot],
    cursor: &Cursor,
    origin: (i64, i64),
    scale: f64,
) -> Option<Rect> {
    if !cursor.shown {
        return None;
    }
    let own = cursor.surface.and_then(|(client, surface, hotspot)| {
        let slot = clients.get(client)?;
        let pixels = pixels(slot.client(), slot.pools(), surface)?;
        Some(((pixels.width(), pixels.height()), hotspot))
    });
    let ((width, height), hotspot) = own.unwrap_or((
        (
            compositor_render::cursor::SIDE,
            compositor_render::cursor::SIDE,
        ),
        compositor_render::cursor::HOTSPOT,
    ));
    Some(local(
        Rect::new(
            cursor.at.0.saturating_sub(i64::from(hotspot.0)),
            cursor.at.1.saturating_sub(i64::from(hotspot.1)),
            i64::from(width),
            i64::from(height),
        ),
        origin,
        scale,
    ))
}

/// The pixels a surface is showing, if it is showing any.
fn pixels<'a>(
    client: &'a Client,
    pools: &'a BTreeMap<ObjectId, Mapping>,
    surface: ObjectId,
) -> Option<Surface<'a>> {
    let state = client.surface(surface)?;
    let buffer = client.buffer(state.current.buffer?)?;
    // A `wp_single_pixel_buffer_v1` is in no pool: the colour is the
    // buffer, four bytes held on the buffer itself. A window drawn from one
    // is scaled to its rectangle like any other, so one pixel fills it.
    let bytes = match buffer.solid.as_ref() {
        Some(colour) => colour.as_slice(),
        None => {
            let mapping = pools.get(&buffer.pool)?;
            let (start, end) = buffer.range()?;
            // The client may have shrunk nothing -- a pool only grows --
            // but a mapping made before a resize is smaller than the pool
            // is now, so the range is checked against what is mapped rather
            // than against the pool.
            mapping.bytes().get(start..end)?
        }
    };
    let format = match buffer.format {
        compositor_server::Format::Argb8888 => Format::Argb8888,
        compositor_server::Format::Xrgb8888 => Format::Xrgb8888,
    };
    Surface::new(
        bytes,
        u32::try_from(buffer.width).ok()?,
        u32::try_from(buffer.height).ok()?,
        u32::try_from(buffer.stride).ok()?,
        format,
    )
    .ok()
}
