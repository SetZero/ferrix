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
/// the lock has not covered yet. A screen with no lock surface is drawn as
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
    damage: &Damage,
) -> Result<(), String> {
    let empty = MonitorLayout {
        windows: Vec::new(),
        ..output.clone()
    };
    let sources = BTreeMap::new();
    let layers: Vec<Placed> = locked.into_iter().collect();
    draw_windows(target, &empty, clients, &sources, &layers, damage)
}

/// Draw a screen that `dpms off` has turned off: black, and nothing else.
///
/// Hyprland turns the connector itself off, which a virtual screen has no
/// equivalent of; what a person sees is the same, and what the compositor
/// must *not* show -- the windows that were there -- is gone either way.
pub fn draw_dark(target: &mut Output<'_>, damage: &Damage) -> Result<(), String> {
    let Output {
        canvas, backend, ..
    } = target;
    canvas.clear(compositor_render::Color(0xff00_0000), damage);
    let (width, height) = backend.size();
    let stride = backend.stride();
    let mut screen = Target::new(backend.buffer(), width, height, stride)
        .map_err(|error| format!("the screen's buffer is not one: {error:?}"))?;
    canvas
        .present(&mut screen, damage)
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
    } = target;
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
        })
        .collect();

    // Every window's rectangle is in the global space all monitors share;
    // the canvas is this monitor's, so the origin is where the monitor is.
    let styles = compositor_render::Styles {
        base: style,
        windows: styles,
    };
    let _ = render_with_layers(canvas, output, origin, &styles, &surfaces, &drawn, damage);

    // The pointer last, over everything: it is not a window, not a layer
    // surface and not part of the layout, and a compositor that drew it
    // under a menu would have a pointer nobody can follow.
    //
    // The arrow is kept here rather than made once and held, because it is
    // 24x24 and a frame that has to allocate it is a frame that has already
    // blurred a window.
    let arrow;
    if let Some(cursor) = cursor.filter(|cursor| cursor.shown) {
        let own = cursor.surface.and_then(|(client, surface, hotspot)| {
            let slot = clients.get(client)?;
            Some((pixels(slot.client(), slot.pools(), surface)?, hotspot))
        });
        let (surface, hotspot) = match own {
            Some((surface, hotspot)) => (Some(surface), hotspot),
            None => {
                arrow = compositor_render::cursor::arrow();
                (
                    compositor_render::cursor::surface(&arrow).ok(),
                    compositor_render::cursor::HOTSPOT,
                )
            }
        };
        if let Some(surface) = surface {
            let at = scale_rect(
                Rect::new(
                    cursor.at.0.saturating_sub(i64::from(hotspot.0)),
                    cursor.at.1.saturating_sub(i64::from(hotspot.1)),
                    i64::from(surface.width()),
                    i64::from(surface.height()),
                ),
                origin,
                scale,
            );
            // The canvas is this monitor's, so the rectangle is moved into
            // its own pixels the way every other one is.
            let at = Rect::new(
                at.x.saturating_sub(origin.0),
                at.y.saturating_sub(origin.1),
                at.width,
                at.height,
            );
            canvas.composite(&surface, at, damage);
        }
    }

    let (width, height) = backend.size();
    let stride = backend.stride();
    let mut target = Target::new(backend.buffer(), width, height, stride)
        .map_err(|error| format!("the screen's buffer is not one: {error:?}"))?;
    canvas
        .present(&mut target, damage)
        .map_err(|error| format!("the frame does not fit the screen: {error:?}"))?;
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
