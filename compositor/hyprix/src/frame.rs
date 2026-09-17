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
    let Output {
        canvas,
        backend,
        origin,
        style,
        styles,
        scale,
    } = target;
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
    client: &Client,
    pools: &'a BTreeMap<ObjectId, Mapping>,
    surface: ObjectId,
) -> Option<Surface<'a>> {
    let state = client.surface(surface)?;
    let buffer = client.buffer(state.current.buffer?)?;
    let mapping = pools.get(&buffer.pool)?;
    let (start, end) = buffer.range()?;
    // The client may have shrunk nothing -- a pool only grows -- but a
    // mapping made before a resize is smaller than the pool is now, so the
    // range is checked against what is mapped rather than against the pool.
    let bytes = mapping.bytes().get(start..end)?;
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
