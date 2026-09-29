//! One screen's lock surface: the widgets `getOrCreateWidgetsFor` makes for
//! it, each with the state its upstream class keeps, and the frame they
//! draw.
//!
//! Nothing here talks to the compositor, runs a program or reads a clock:
//! the time comes in with every call, text and pictures come through
//! [`Assets`], and a command a widget wants run comes out as a [`Job`]
//! whose output is handed back with [`Scene::finished`]. So a whole frame
//! -- the customer's clock panel, the field with three dots, the fail text
//! -- is a host test.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use compositor_config::Color;
use tiny_skia::{BlendMode, Paint, Pixmap, PixmapMut, PixmapRef, Transform};

use crate::blur;
use crate::config::{
    Animations, Background, Config, Gradient, Image, InputField, Label, Shadow, Shape, Widget,
};
use crate::format::{Context, Formatted, format};
use crate::layout::{place, rounding_for_border_box, rounding_for_box};
use crate::paint::{self, Rect};
use crate::session::Session;
use crate::tween::Tween;

/// A screen a lock surface covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    /// Its connector, `DP-1`.
    pub name: String,
    /// Its description.
    pub description: String,
    /// Its size in the surface's pixels.
    pub width: u32,
    /// Its height.
    pub height: u32,
}

/// How the lines of a text line up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TextAlign {
    /// `left`, and anything upstream does not know.
    #[default]
    Left,
    /// `center`.
    Center,
    /// `right`.
    Right,
}

impl TextAlign {
    /// `parseTextAlignment`.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text {
            "center" => Self::Center,
            "right" => Self::Right,
            _ => Self::Left,
        }
    }
}

/// A text to draw: hyprgraphics' `STextResourceData`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextRequest {
    /// Pango markup; plain text where it does not parse.
    pub text: String,
    /// A Pango font description, `"Ubuntu Light"`.
    pub font: String,
    /// The size in points.
    pub size: i64,
    /// The colour of text no span colours, opaque.
    pub color: u32,
    /// How lines line up.
    pub align: TextAlign,
}

/// Where text and pictures come from.
pub trait Assets {
    /// `request` drawn, premultiplied, the size of its logical rectangle;
    /// `None` for nothing to draw.
    fn text(&mut self, request: &TextRequest) -> Option<Rc<Pixmap>>;
    /// The picture at `path`, or `None` where it cannot be read.
    fn image(&mut self, path: &Path) -> Option<Rc<Pixmap>>;
}

/// What the widgets are drawn from this frame.
#[derive(Clone, Copy, Debug)]
pub struct View<'a> {
    /// Milliseconds on the program's clock.
    pub now: u64,
    /// The whole lock's opacity, as the fade in and out have it.
    pub opacity: f32,
    /// The password and the last check.
    pub session: &'a Session,
    /// What the variables stand for.
    pub context: &'a Context,
    /// The screen as it was before the lock, where screencopy gave it.
    pub screenshot: Option<PixmapRef<'a>>,
}

/// A command a widget wants run, whose standard output it wants back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// The widget, to hand the output back to.
    pub widget: usize,
    /// The shell command line.
    pub line: String,
}

/// A command's output as a path: its last newline gone and `file://` off.
fn output_path(output: &str) -> String {
    let path = output.strip_suffix('\0').unwrap_or(output);
    let path = path.strip_suffix('\n').unwrap_or(path);
    path.strip_prefix("file://").unwrap_or(path).to_owned()
}

/// When a file last changed, as `last_write_time` compares it.
fn modified(path: &str) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// The blur a shadow is, from its widget's `shadow_*`.
fn shadow_params(shadow: &Shadow) -> blur::Params {
    let channel = |value: u8| f32::from(value) / 255.0;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "a blur's size and passes, small numbers from the file"
    )]
    blur::Params {
        size: shadow.size.max(0) as f32,
        passes: shadow.passes.clamp(0, 16) as u32,
        noise: 0.0,
        contrast: 0.0,
        brightness: 0.0,
        vibrancy: 0.0,
        vibrancy_darkness: 0.0,
        colorize: Some([
            channel(shadow.color.red()),
            channel(shadow.color.green()),
            channel(shadow.color.blue()),
        ]),
        boost_alpha: shadow.boost as f32,
    }
}

/// A widget's shadow, kept until what it is a shadow of changes.
#[derive(Debug, Default)]
struct ShadowCache {
    key: Option<String>,
    made: Option<(Pixmap, f32, f32)>,
}

impl ShadowCache {
    /// Draw the shadow for `key`, making it with `draw` if `key` changed.
    fn draw(
        &mut self,
        pixmap: &mut PixmapMut<'_>,
        shadow: &Shadow,
        key: String,
        bounds: Rect,
        alpha: f32,
        draw: impl FnOnce(&mut PixmapMut<'_>, f32, f32),
    ) {
        if shadow.passes <= 0 {
            return;
        }
        if self.key.as_ref() != Some(&key) {
            self.made = paint::shadow(bounds, &shadow_params(shadow), draw);
            self.key = Some(key);
        }
        if let Some((made, x, y)) = &self.made {
            paint::blit(pixmap, made.as_ref(), *x, *y, alpha, 0.0);
        }
    }
}

// ---------------------------------------------------------------------------
// label
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct LabelItem {
    config: Label,
    formatted: Formatted,
    /// What is drawn: the formatted text, or the command's output.
    shown: String,
    pending: bool,
    next: Option<u64>,
    shadow: ShadowCache,
    trim: bool,
    /// Where it was last drawn, for a click.
    drawn: Option<Rect>,
}

impl LabelItem {
    fn new(config: &Label, context: &Context, trim: bool, now: u64) -> Self {
        let formatted = format(&config.text, context);
        let mut item = Self {
            config: config.clone(),
            shown: if formatted.cmd {
                String::new()
            } else {
                formatted.text.clone()
            },
            formatted,
            pending: false,
            next: None,
            shadow: ShadowCache::default(),
            trim,
            drawn: None,
        };
        // A command runs at once; `due` starts it.
        item.next = if item.formatted.cmd {
            Some(now)
        } else {
            item.plant(now)
        };
        item
    }

    /// `plantTimer`.
    fn plant(&self, now: u64) -> Option<u64> {
        match self.formatted.update_every_ms {
            0 if self.formatted.allow_force_update => Some(now + 3_600_000),
            0 => None,
            every => Some(now + every),
        }
    }

    /// `onTimerUpdate`, and the timer planted again.
    fn update(&mut self, context: &Context, now: u64) -> Option<String> {
        let first =
            self.formatted.cmd && self.next == Some(now) && self.shown.is_empty() && !self.pending;
        let old = self.formatted.text.clone();
        self.formatted = format(&self.config.text, context);
        self.next = self.plant(now);
        if !first && self.formatted.text == old && !self.formatted.always_update {
            return None;
        }
        if self.formatted.cmd {
            if self.pending {
                crate::say(
                    "Trying to update label, but a resource is still pending! Skipping update.",
                );
                return None;
            }
            self.pending = true;
            return Some(self.formatted.text.clone());
        }
        self.shown.clone_from(&self.formatted.text);
        None
    }

    fn finished(&mut self, output: &str) {
        self.pending = false;
        self.shown = if self.trim {
            output.trim_matches([' ', '\n', '\r', '\t']).to_owned()
        } else {
            output.to_owned()
        };
    }

    fn request(&self) -> TextRequest {
        TextRequest {
            text: self.shown.clone(),
            font: self.config.font_family.clone(),
            size: self.config.font_size,
            color: self.config.color.0 | 0xFF00_0000,
            align: TextAlign::parse(&self.config.text_align),
        }
    }

    fn draw(
        &mut self,
        pixmap: &mut PixmapMut<'_>,
        viewport: (f64, f64),
        view: &View<'_>,
        assets: &mut dyn Assets,
    ) {
        if self.shown.is_empty() {
            return;
        }
        let Some(texture) = assets.text(&self.request()) else {
            return;
        };
        let size = (f64::from(texture.width()), f64::from(texture.height()));
        let offset = self.config.position.absolute(viewport);
        let angle = self.config.rotate.to_radians();
        let (placed, _) = place(
            viewport,
            size,
            offset,
            &self.config.halign,
            &self.config.valign,
            angle,
        );
        #[expect(clippy::cast_possible_truncation, reason = "a screen position")]
        let (x, y) = (placed.x as f32, placed.top() as f32);
        #[expect(clippy::cast_possible_truncation, reason = "an angle in degrees")]
        let degrees = self.config.rotate as f32;
        let bounds = Rect::new(x, y, size_f32(size.0), size_f32(size.1));
        let texture_for_shadow = Rc::clone(&texture);
        self.shadow.draw(
            pixmap,
            &self.config.shadow,
            self.shown.clone(),
            bounds,
            view.opacity,
            |into, dx, dy| {
                paint::blit(
                    into,
                    texture_for_shadow.as_ref().as_ref(),
                    x + dx,
                    y + dy,
                    1.0,
                    degrees,
                );
            },
        );
        self.drawn = Some(bounds);
        let alpha = f32::from(self.config.color.alpha()) / 255.0;
        paint::blit(
            pixmap,
            texture.as_ref().as_ref(),
            x,
            y,
            view.opacity * alpha,
            degrees,
        );
    }
}

#[expect(clippy::cast_possible_truncation, reason = "a widget's size in pixels")]
fn size_f32(value: f64) -> f32 {
    value as f32
}

// ---------------------------------------------------------------------------
// shape
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ShapeItem {
    config: Shape,
    /// The border box and the shape inside it.
    border_box: Rect,
    shape_box: Rect,
    shadow: ShadowCache,
}

impl ShapeItem {
    fn new(config: &Shape, viewport: (f64, f64)) -> Self {
        let size = config.size.absolute(viewport);
        #[expect(clippy::cast_precision_loss, reason = "a border in pixels")]
        let border = config.border_size as f64;
        let real = (size.0 + 2.0 * border, size.1 + 2.0 * border);
        let angle = config.rotate.to_radians();
        let offset = if angle == 0.0 { 0.0 } else { 1.0 };
        let offset_position = config.position.absolute(viewport);
        let (placed, problem) = if config.xray {
            place(
                viewport,
                size,
                offset_position,
                &config.halign,
                &config.valign,
                0.0,
            )
        } else {
            place(
                viewport,
                (real.0 + offset * 2.0, real.1 + offset * 2.0),
                offset_position,
                &config.halign,
                &config.valign,
                angle,
            )
        };
        if let Some(problem) = problem {
            crate::say(&problem);
        }
        let (x, top) = (size_f32(placed.x), size_f32(placed.top()));
        let (border_box, shape_box) = if config.xray {
            let shape = Rect::new(x, top, size_f32(size.0), size_f32(size.1));
            (shape.grown(size_f32(border)), shape)
        } else {
            let offset = size_f32(offset);
            let border_box =
                Rect::new(x + offset, top + offset, size_f32(real.0), size_f32(real.1));
            (border_box, border_box.grown(-size_f32(border)))
        };
        Self {
            config: config.clone(),
            border_box,
            shape_box,
            shadow: ShadowCache::default(),
        }
    }

    fn paint(&self, pixmap: &mut PixmapMut<'_>, dx: f32, dy: f32, alpha: f32) {
        let moved = |rect: Rect| Rect::new(rect.x + dx, rect.y + dy, rect.width, rect.height);
        let (border_box, shape_box) = (moved(self.border_box), moved(self.shape_box));
        #[expect(clippy::cast_precision_loss, reason = "a rounding in pixels")]
        let round = rounding_for_box(
            f64::from(shape_box.width),
            f64::from(shape_box.height),
            self.config.rounding,
        ) as f32;
        #[expect(clippy::cast_precision_loss, reason = "a rounding in pixels")]
        let border_round = rounding_for_border_box(
            f64::from(border_box.width),
            f64::from(border_box.height),
            self.config.rounding,
            self.config.border_size,
        ) as f32;
        #[expect(clippy::cast_precision_loss, reason = "a border in pixels")]
        let border = self.config.border_size as f32;
        if border > 0.0 {
            paint::ring(
                pixmap,
                border_box,
                border,
                border_round,
                &self.config.border_color,
                alpha,
            );
        }
        paint::fill(pixmap, shape_box, round, self.config.color, alpha);
    }

    fn draw(&mut self, pixmap: &mut PixmapMut<'_>, view: &View<'_>) {
        if self.config.xray {
            if let Some(rect) = tiny_skia::Rect::from_xywh(
                self.shape_box.x,
                self.shape_box.y,
                self.shape_box.width,
                self.shape_box.height,
            ) {
                let paint = Paint {
                    blend_mode: BlendMode::Clear,
                    ..Paint::default()
                };
                pixmap.fill_rect(rect, &paint, Transform::identity(), None);
            }
            return;
        }
        let mut cache = std::mem::take(&mut self.shadow);
        let this = &*self;
        cache.draw(
            pixmap,
            &this.config.shadow,
            String::new(),
            this.border_box,
            view.opacity,
            |into, dx, dy| this.paint(into, dx, dy, 1.0),
        );
        self.shadow = cache;
        self.paint(pixmap, 0.0, 0.0, view.opacity);
    }
}

// ---------------------------------------------------------------------------
// image
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ImageItem {
    config: Image,
    path: String,
    loaded: Option<Rc<Pixmap>>,
    /// The picture with its border and rounding, as upstream's `imageFB`.
    framed: Option<Pixmap>,
    wanted: bool,
    modified: Option<SystemTime>,
    revision: u64,
    next: Option<u64>,
    pending: bool,
    shadow: ShadowCache,
    /// Where it was last drawn, for a click.
    drawn: Option<Rect>,
}

impl ImageItem {
    fn new(config: &Image, now: u64) -> Self {
        let next = match config.reload_time {
            0 => Some(now + 3_600_000),
            seconds if seconds > 0 => Some(now + u64::try_from(seconds).unwrap_or(0) * 1000),
            _ => None,
        };
        Self {
            config: config.clone(),
            path: config.path.clone(),
            loaded: None,
            framed: None,
            wanted: true,
            modified: (config.reload_time > -1)
                .then(|| modified(&config.path))
                .flatten(),
            revision: 0,
            next,
            pending: false,
            shadow: ShadowCache::default(),
            drawn: None,
        }
    }

    fn plant(&mut self, now: u64) {
        self.next = match self.config.reload_time {
            0 => Some(now + 3_600_000),
            seconds if seconds > 0 => Some(now + u64::try_from(seconds).unwrap_or(0) * 1000),
            _ => None,
        };
    }

    /// `onTimerUpdate`.
    fn update(&mut self, now: u64) -> Option<String> {
        self.plant(now);
        if self.pending {
            crate::say("Trying to update image, but a resource is still pending! Skipping update.");
            return None;
        }
        if !self.config.reload_cmd.is_empty() {
            self.pending = true;
            return Some(self.config.reload_cmd.clone());
        }
        let path = self.path.clone();
        self.reload(path);
        None
    }

    fn finished(&mut self, output: &str) {
        self.pending = false;
        let path = output_path(output);
        if path.is_empty() {
            return;
        }
        self.reload(path);
    }

    fn reload(&mut self, path: String) {
        let changed = modified(&path);
        if changed.is_none() {
            crate::say(&format!("image: cannot read {path}"));
            return;
        }
        if path == self.path && changed == self.modified {
            return;
        }
        self.revision = if path == self.path {
            self.revision + 1
        } else {
            0
        };
        self.modified = changed;
        self.path = path;
        self.wanted = true;
    }

    fn frame(&mut self, assets: &mut dyn Assets) {
        if !self.wanted {
            return;
        }
        self.wanted = false;
        let Some(image) = assets.image(Path::new(&self.path)) else {
            return;
        };
        #[expect(clippy::cast_precision_loss, reason = "sizes in pixels")]
        let (size, border) = (self.config.size as f32, self.config.border_size as f32);
        let (width, height) = (image.width() as f32, image.height() as f32);
        let scale = (size / width).max(size / height);
        let texture = Rect::new(
            border,
            border,
            (width * scale).round(),
            (height * scale).round(),
        );
        let border_box = Rect::new(
            0.0,
            0.0,
            texture.width + 2.0 * border,
            texture.height + 2.0 * border,
        )
        .rounded();
        #[expect(clippy::cast_precision_loss, reason = "a rounding in pixels")]
        let round = rounding_for_box(
            f64::from(texture.width),
            f64::from(texture.height),
            self.config.rounding,
        ) as f32;
        #[expect(clippy::cast_precision_loss, reason = "a rounding in pixels")]
        let border_round = rounding_for_border_box(
            f64::from(border_box.width),
            f64::from(border_box.height),
            self.config.rounding,
            self.config.border_size,
        ) as f32;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small positive size"
        )]
        let Some(mut framed) = Pixmap::new(
            border_box.width.max(1.0) as u32,
            border_box.height.max(1.0) as u32,
        ) else {
            return;
        };
        if border > 0.0 {
            paint::ring(
                &mut framed.as_mut(),
                border_box,
                border,
                border_round,
                &self.config.border_color,
                1.0,
            );
        }
        paint::picture(
            &mut framed.as_mut(),
            image.as_ref().as_ref(),
            texture,
            texture,
            round,
            1.0,
        );
        self.loaded = Some(image);
        self.framed = Some(framed);
        self.shadow = ShadowCache::default();
    }

    fn draw(
        &mut self,
        pixmap: &mut PixmapMut<'_>,
        viewport: (f64, f64),
        view: &View<'_>,
        assets: &mut dyn Assets,
    ) {
        self.frame(assets);
        let Some(framed) = &self.framed else {
            return;
        };
        let size = (f64::from(framed.width()), f64::from(framed.height()));
        let angle = self.config.rotate.to_radians();
        let (placed, _) = place(
            viewport,
            size,
            self.config.position.absolute(viewport),
            &self.config.halign,
            &self.config.valign,
            angle,
        );
        let (x, y) = (size_f32(placed.x), size_f32(placed.top()));
        #[expect(clippy::cast_possible_truncation, reason = "an angle in degrees")]
        let degrees = self.config.rotate as f32;
        let bounds = Rect::new(x, y, size_f32(size.0), size_f32(size.1));
        let key = format!("{}#{}", self.path, self.revision);
        self.shadow.draw(
            pixmap,
            &self.config.shadow,
            key,
            bounds,
            view.opacity,
            |into, dx, dy| {
                paint::blit(into, framed.as_ref(), x + dx, y + dy, 1.0, degrees);
            },
        );
        self.drawn = Some(bounds);
        paint::blit(pixmap, framed.as_ref(), x, y, view.opacity, degrees);
    }
}

// ---------------------------------------------------------------------------
// background
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct BackgroundItem {
    config: Background,
    path: String,
    screenshot: bool,
    /// The picture covering the screen, blurred: `blurredFB`.
    prepared: Option<Pixmap>,
    /// The picture being faded to, while a reload crossfades.
    incoming: Option<Pixmap>,
    crossfade: Tween,
    wanted: bool,
    modified: Option<SystemTime>,
    next: Option<u64>,
    pending: bool,
    viewport: (f64, f64),
}

impl BackgroundItem {
    fn new(config: &Background, viewport: (f64, f64), animations: &Animations, now: u64) -> Self {
        let screenshot = config.path == "screenshot";
        let mut item = Self {
            config: config.clone(),
            path: config.path.clone(),
            screenshot,
            prepared: None,
            incoming: None,
            crossfade: Tween::new(0.0, animations, "fadeIn"),
            wanted: !config.path.is_empty(),
            modified: None,
            next: None,
            pending: false,
            viewport,
        };
        if !config.reload_cmd.is_empty() && config.path.is_empty() {
            // Upstream runs it before the first frame; here it runs at
            // once and the colour shows until it answers.
            item.next = Some(now);
        } else if !config.reload_cmd.is_empty() && config.reload_time > -1 {
            if !screenshot {
                item.modified = modified(&config.path);
            }
            item.plant(now);
        }
        item
    }

    fn plant(&mut self, now: u64) {
        self.next = match self.config.reload_time {
            0 => Some(now + 3_600_000),
            seconds if seconds > 0 => Some(now + u64::try_from(seconds).unwrap_or(0) * 1000),
            _ => None,
        };
    }

    fn update(&mut self, now: u64) -> Option<String> {
        self.plant(now);
        if self.pending || self.config.reload_cmd.is_empty() {
            return None;
        }
        self.pending = true;
        Some(self.config.reload_cmd.clone())
    }

    fn finished(&mut self, output: &str, now: u64) {
        self.pending = false;
        let path = output_path(output);
        if path.is_empty() {
            return;
        }
        let changed = modified(&path);
        if changed.is_none() {
            crate::say(&format!("background: cannot read {path}"));
            return;
        }
        if path == self.path && changed == self.modified {
            return;
        }
        let first = self.path.is_empty() && self.prepared.is_none();
        self.modified = changed;
        self.path = path;
        self.wanted = true;
        if !first {
            self.crossfade.warp(0.0);
            self.crossfade.set(1.0, now);
        }
    }

    /// The picture at the screen's size, blurred as the widget says.
    fn prepare(&self, source: PixmapRef<'_>) -> Option<Pixmap> {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a screen's size"
        )]
        let (width, height) = (self.viewport.0 as u32, self.viewport.1 as u32);
        let mut prepared = Pixmap::new(width.max(1), height.max(1))?;
        #[expect(clippy::cast_precision_loss, reason = "a texture's size")]
        let place = paint::cover(
            (source.width() as f32, source.height() as f32),
            (size_f32(self.viewport.0), size_f32(self.viewport.1)),
        );
        let whole = Rect::new(
            0.0,
            0.0,
            size_f32(self.viewport.0),
            size_f32(self.viewport.1),
        );
        paint::picture(&mut prepared.as_mut(), source, place, whole, 0.0, 1.0);
        if self.config.blur_passes > 0 {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "the file's blur settings, small numbers"
            )]
            let params = blur::Params {
                size: self.config.blur_size.max(0) as f32,
                passes: self.config.blur_passes.clamp(0, 16) as u32,
                noise: self.config.noise as f32,
                contrast: self.config.contrast as f32,
                brightness: self.config.brightness as f32,
                vibrancy: self.config.vibrancy as f32,
                vibrancy_darkness: self.config.vibrancy_darkness as f32,
                colorize: None,
                boost_alpha: 1.0,
            };
            paint::blur_pixmap(&mut prepared, &params);
        }
        Some(prepared)
    }

    fn draw(&mut self, pixmap: &mut PixmapMut<'_>, view: &View<'_>, assets: &mut dyn Assets) {
        if self.screenshot {
            if self.prepared.is_none()
                && let Some(shot) = view.screenshot
            {
                self.prepared = self.prepare(shot);
            }
        } else if self.wanted {
            self.wanted = false;
            let loaded = assets
                .image(Path::new(&self.path))
                .and_then(|image| self.prepare(image.as_ref().as_ref()));
            if loaded.is_none() {
                crate::say(&format!("background: cannot read {}", self.path));
            }
            if self.prepared.is_none() {
                self.prepared = loaded;
            } else {
                self.incoming = loaded;
            }
        }
        if self.incoming.is_some() && !self.crossfade.moving(view.now) {
            self.prepared = self.incoming.take();
        }
        let whole = Rect::new(
            0.0,
            0.0,
            size_f32(self.viewport.0),
            size_f32(self.viewport.1),
        );
        let shot = |pixmap: &mut PixmapMut<'_>| {
            if let Some(shot) = view.screenshot {
                #[expect(clippy::cast_precision_loss, reason = "a texture's size")]
                let place = paint::cover(
                    (shot.width() as f32, shot.height() as f32),
                    (whole.width, whole.height),
                );
                paint::picture(pixmap, shot, place, whole, 0.0, 1.0);
            }
        };
        let Some(prepared) = &self.prepared else {
            if view.opacity < 1.0 && view.screenshot.is_some() {
                shot(pixmap);
                paint::fill(pixmap, whole, 0.0, self.config.color, view.opacity);
            } else {
                paint::fill(pixmap, whole, 0.0, self.config.color, 1.0);
            }
            return;
        };
        if view.opacity < 1.0 && view.screenshot.is_some() {
            shot(pixmap);
            paint::blit(pixmap, prepared.as_ref(), 0.0, 0.0, view.opacity, 0.0);
        } else if let Some(incoming) = &self.incoming {
            paint::blit(pixmap, prepared.as_ref(), 0.0, 0.0, 1.0, 0.0);
            #[expect(clippy::cast_possible_truncation, reason = "a fraction")]
            let mix = self.crossfade.at(view.now) as f32;
            paint::blit(pixmap, incoming.as_ref(), 0.0, 0.0, mix, 0.0);
        } else {
            paint::blit(pixmap, prepared.as_ref(), 0.0, 0.0, 1.0, 0.0);
        }
    }

    fn animating(&self, now: u64) -> bool {
        self.incoming.is_some() && self.crossfade.moving(now)
    }
}

// ---------------------------------------------------------------------------
// input field
// ---------------------------------------------------------------------------

/// A colour moving on `inputFieldColors`.
#[derive(Debug)]
struct ColorTween {
    from: Color,
    to: Color,
    progress: Tween,
}

impl ColorTween {
    fn new(color: Color, animations: &Animations) -> Self {
        Self {
            from: color,
            to: color,
            progress: Tween::new(1.0, animations, "inputFieldColors"),
        }
    }

    fn at(&self, now: u64) -> Color {
        #[expect(clippy::cast_possible_truncation, reason = "a fraction")]
        let t = self.progress.at(now) as f32;
        let mix = |shift: u32| {
            let (a, b) = (
                f32::from(u8::try_from((self.from.0 >> shift) & 0xFF).unwrap_or(0)),
                f32::from(u8::try_from((self.to.0 >> shift) & 0xFF).unwrap_or(0)),
            );
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "between two bytes"
            )]
            let byte = (a + (b - a) * t).round().clamp(0.0, 255.0) as u32;
            byte << shift
        };
        Color(mix(24) | mix(16) | mix(8) | mix(0))
    }

    fn set(&mut self, color: Color, now: u64) {
        if color == self.to {
            return;
        }
        self.from = self.at(now);
        self.to = color;
        self.progress.warp(0.0);
        self.progress.set(1.0, now);
    }
}

#[derive(Debug)]
struct FieldItem {
    config: InputField,
    config_size: (f64, f64),
    config_position: (f64, f64),
    viewport: (f64, f64),
    fade: Tween,
    fade_out_at: Option<u64>,
    allow_fade_out: bool,
    dots: Tween,
    width: Tween,
    inner: ColorTween,
    outer: ColorTween,
    /// The outer gradient when it is more than one colour: not animated.
    outer_gradient: Gradient,
    font: Color,
    placeholder: Option<TextRequest>,
    placeholder_attempts: Option<usize>,
    shadow: ShadowCache,
}

impl FieldItem {
    fn new(config: &InputField, viewport: (f64, f64), animations: &Animations) -> Self {
        let mut config = config.clone();
        config.dots_size = config.dots_size.clamp(0.001, 0.8);
        config.dots_spacing = config.dots_spacing.clamp(-1.0, 1.0);
        if config.capslock_color.fallback {
            config.capslock_color = config.fail_color.clone();
        }
        let config_size = config.size.absolute(viewport);
        Self {
            config_position: config.position.absolute(viewport),
            fade: Tween::new(0.0, animations, "inputFieldFade"),
            fade_out_at: None,
            allow_fade_out: false,
            dots: Tween::new(0.0, animations, "inputFieldDots"),
            width: Tween::new(config_size.0, animations, "inputFieldWidth"),
            inner: ColorTween::new(config.inner_color, animations),
            outer: ColorTween::new(config.outer_color.first(), animations),
            outer_gradient: config.outer_color.clone(),
            font: config.font_color,
            placeholder: None,
            placeholder_attempts: None,
            shadow: ShadowCache::default(),
            config_size,
            viewport,
            config,
        }
    }

    /// `updateFade`.
    fn update_fade(&mut self, used: bool, now: u64) {
        if !self.config.fade_on_empty {
            self.fade.warp(1.0);
            return;
        }
        if used {
            self.allow_fade_out = false;
            self.fade_out_at = None;
        }
        if self.fade_out_at.is_some_and(|at| now >= at) {
            self.fade_out_at = None;
            self.allow_fade_out = true;
        }
        if !used && self.fade.goal() != 0.0 {
            if self.allow_fade_out || self.config.fade_timeout == 0 {
                self.fade.set(0.0, now);
                self.allow_fade_out = false;
            } else if self.fade_out_at.is_none() {
                self.fade_out_at = Some(now + u64::try_from(self.config.fade_timeout).unwrap_or(0));
            }
        } else if used && self.fade.goal() != 1.0 {
            self.fade.set(1.0, now);
        }
    }

    /// `updateDots`.
    fn update_dots(&mut self, length: usize, checking: bool, now: u64) {
        #[expect(clippy::cast_precision_loss, reason = "a password's length")]
        let length = length as f64;
        if (self.dots.goal() - length).abs() < f64::EPSILON {
            return;
        }
        if checking && self.config.check_text.is_empty() {
            return;
        }
        if length == 0.0 {
            self.dots.warp(0.0);
        } else {
            self.dots.set(length, now);
        }
    }

    /// `updateColors`.
    fn update_colors(&mut self, session: &Session, length: usize, now: u64) {
        let borderless = self.config.outline_thickness == 0;
        let num = if self.config.invert_numlock {
            !session.num_lock
        } else {
            session.num_lock
        };
        let (checking, failing) = (session.checking(), session.failing());
        let mut target: Option<&Gradient> = None;
        if session.caps_lock && num && !self.config.bothlock_color.fallback {
            target = Some(&self.config.bothlock_color);
        } else if session.caps_lock {
            target = Some(&self.config.capslock_color);
        } else if num && !self.config.numlock_color.fallback {
            target = Some(&self.config.numlock_color);
        }
        if checking {
            target = Some(&self.config.check_color);
        } else if failing && length == 0 {
            target = Some(&self.config.fail_color);
        }
        let mut outer = &self.config.outer_color;
        let mut inner = self.config.inner_color;
        let mut font = self.config.font_color;
        if failing {
            font = self.config.fail_color.first();
        } else if checking && !self.config.check_text.is_empty() {
            font = self.config.check_color.first();
        }
        if let Some(target) = target {
            if borderless && self.config.swap_font_color {
                font = target.first();
            } else if borderless {
                inner = target.first();
                font = self.config.font_color;
            } else {
                outer = target;
            }
        }
        let outer = outer.clone();
        if !borderless {
            self.outer.set(outer.first(), now);
            self.outer_gradient = outer;
        }
        self.inner.set(inner, now);
        self.font = font;
    }

    /// `updatePlaceholder`: the text to show in an empty field, when it
    /// changes.
    fn update_placeholder(&mut self, view: &View<'_>, length: usize) {
        let session = view.session;
        if length != 0 {
            if session.failing() {
                self.placeholder = None;
            }
            return;
        }
        if session.failing() && self.placeholder_attempts == Some(session.attempts) {
            return;
        }
        let text = if session.failing() {
            self.placeholder_attempts = Some(session.attempts);
            format(&self.config.fail_text, view.context).text
        } else if session.checking() && !self.config.check_text.is_empty() {
            format(&self.config.check_text, view.context).text
        } else {
            format(&self.config.placeholder_text, view.context).text
        };
        let swap = self.config.outline_thickness == 0 && self.config.swap_font_color;
        if !swap
            && self
                .placeholder
                .as_ref()
                .is_some_and(|request| request.text == text)
        {
            return;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the field's height, as upstream's int"
        )]
        let height = self.config_size.1 as i64;
        self.placeholder = Some(TextRequest {
            text,
            font: self.config.font_family.clone(),
            size: height / 4,
            color: self.font.0 | 0xFF00_0000,
            align: TextAlign::Left,
        });
    }

    /// `updateWidth`.
    fn update_width(&mut self, length: usize, assets: &mut dyn Assets, now: u64) {
        let mut target = self.config_size.0;
        if length == 0
            && let Some(request) = &self.placeholder
            && let Some(texture) = assets.text(request)
        {
            target = f64::from(texture.width()) + self.config_size.1;
        }
        let target = target.max(self.config_size.0);
        if (self.width.goal() - target).abs() > f64::EPSILON {
            self.width.set(target, now);
        }
    }

    fn field_box(&self, now: u64) -> Rect {
        let size = (self.width.at(now), self.config_size.1);
        let (placed, _) = place(
            self.viewport,
            size,
            self.config_position,
            &self.config.halign,
            &self.config.valign,
            0.0,
        );
        Rect::new(
            size_f32(placed.x),
            size_f32(placed.top()),
            size_f32(size.0),
            size_f32(size.1),
        )
    }

    fn draw(
        &mut self,
        pixmap: &mut PixmapMut<'_>,
        view: &View<'_>,
        assets: &mut dyn Assets,
    ) -> bool {
        let now = view.now;
        let session = view.session;
        let length = session.length();
        let checking = session.checking();
        self.update_fade(length > 0 || checking, now);
        self.update_dots(length, checking, now);
        self.update_colors(session, length, now);
        self.update_placeholder(view, length);
        self.update_width(length, assets, now);

        #[expect(clippy::cast_possible_truncation, reason = "a fraction")]
        let fade = self.fade.at(now) as f32;
        let field = self.field_box(now);
        #[expect(clippy::cast_precision_loss, reason = "a thickness in pixels")]
        let thickness = self.config.outline_thickness as f32;
        let outer_box = field.grown(thickness);
        let alpha = fade * view.opacity;

        if !self.width.moving(now) {
            let mut cache = std::mem::take(&mut self.shadow);
            let this = &*self;
            let key = format!("{field:?}{fade}{length}");
            cache.draw(
                pixmap,
                &this.config.shadow,
                key,
                outer_box,
                view.opacity * fade,
                |into, dx, dy| {
                    this.paint_box(into, dx, dy, 1.0, now);
                },
            );
            self.shadow = cache;
        }
        self.paint_box(pixmap, 0.0, 0.0, alpha, now);
        self.paint_dots(pixmap, field, view, assets);

        let placeholder_shown = length == 0 && self.placeholder.is_some();
        if placeholder_shown
            && (!checking || !self.config.check_text.is_empty())
            && let Some(request) = &self.placeholder
            && let Some(texture) = assets.text(request)
        {
            let x = field.x + field.width / 2.0 - size_f32(f64::from(texture.width())) / 2.0;
            let y = field.y + field.height / 2.0 - size_f32(f64::from(texture.height())) / 2.0;
            // Cut to the field, as upstream's scissor cuts it.
            let left = (field.x - x).max(0.0);
            let right =
                (x + size_f32(f64::from(texture.width())) - (field.x + field.width)).max(0.0);
            if let Some(clipped) = cut_sides(texture.as_ref(), left, right) {
                paint::blit(pixmap, clipped.as_ref(), x, y, view.opacity * fade, 0.0);
            }
        }
        self.fade.moving(now)
            || self.dots.moving(now)
            || self.width.moving(now)
            || self.inner.progress.moving(now)
            || self.outer.progress.moving(now)
            || self.fade_out_at.is_some()
    }

    /// The outline and the inside.
    fn paint_box(&self, pixmap: &mut PixmapMut<'_>, dx: f32, dy: f32, alpha: f32, now: u64) {
        let field = self.field_box(now);
        let field = Rect::new(field.x + dx, field.y + dy, field.width, field.height);
        #[expect(clippy::cast_precision_loss, reason = "a thickness in pixels")]
        let thickness = self.config.outline_thickness as f32;
        let outer_box = field.grown(thickness);
        if thickness > 0.0 {
            #[expect(clippy::cast_precision_loss, reason = "a rounding in pixels")]
            let round = rounding_for_border_box(
                f64::from(outer_box.width),
                f64::from(outer_box.height),
                self.config.rounding,
                self.config.outline_thickness,
            ) as f32;
            let gradient = if self.outer_gradient.colors.len() > 1 {
                self.outer_gradient.clone()
            } else {
                Gradient::solid(self.outer.at(now).0)
            };
            paint::ring(pixmap, outer_box, thickness, round, &gradient, alpha);
        }
        #[expect(clippy::cast_precision_loss, reason = "a rounding in pixels")]
        let round = rounding_for_box(
            f64::from(field.width),
            f64::from(field.height),
            self.config.rounding,
        ) as f32;
        paint::fill(pixmap, field, round, self.inner.at(now), alpha);
    }

    /// The dots, as upstream places them.
    fn paint_dots(
        &self,
        pixmap: &mut PixmapMut<'_>,
        field: Rect,
        view: &View<'_>,
        assets: &mut dyn Assets,
    ) {
        let now = view.now;
        #[expect(clippy::cast_possible_truncation, reason = "a fraction")]
        let fade = self.fade.at(now) as f32;
        let font_alpha = f32::from(self.font.alpha()) / 255.0 * fade * view.opacity;
        #[expect(clippy::cast_possible_truncation, reason = "a dot's size")]
        let side = ((f64::from(field.height) * self.config.dots_size * 0.5).round_ties_even() * 2.0)
            as f32;
        let mut pass = (side, side);
        let text = if self.config.dots_text_format.is_empty() {
            None
        } else {
            let request = TextRequest {
                text: self.config.dots_text_format.clone(),
                font: self.config.font_family.clone(),
                #[expect(clippy::cast_possible_truncation, reason = "a font size")]
                size: ((self.config_size.1 * self.config.dots_size * 0.5).round_ties_even() * 2.0)
                    as i64,
                color: self.config.font_color.0 | 0xFF00_0000,
                align: TextAlign::Left,
            };
            let texture = assets.text(&request);
            if let Some(texture) = &texture {
                pass = (
                    size_f32(f64::from(texture.width())),
                    size_f32(f64::from(texture.height())),
                );
            }
            texture
        };
        #[expect(clippy::cast_possible_truncation, reason = "a spacing")]
        let spacing = (f64::from(pass.0) * self.config.dots_spacing).floor() as f32;
        #[expect(clippy::cast_possible_truncation, reason = "a dot count")]
        let current = self.dots.at(now) as f32;
        let pad = (field.height - pass.1) / 2.0;
        let area = field.width - pad * 2.0;
        let most = (area / (pass.0 + spacing)).round();
        let floored = current.floor();
        let width = (pass.0 + spacing) * current - spacing;
        let mut start = if self.config.dots_center {
            (area - width) / 2.0 + pad
        } else {
            pad
        };
        if current > most {
            start = (field.width + most * (pass.0 + spacing) - spacing - 2.0 * width) / 2.0;
        }
        #[expect(clippy::cast_precision_loss, reason = "a rounding in pixels")]
        let rounding = match self.config.dots_rounding {
            -1 => pass.0 / 2.0,
            -2 if self.config.rounding == -1 => pass.0 / 2.0,
            -2 => self.config.rounding as f32 * size_f32(self.config.dots_size),
            other => other as f32,
        };
        let mut index = 0.0f32;
        while index < current {
            let at = index;
            index += 1.0;
            if at < floored - most {
                continue;
            }
            let mut alpha = font_alpha;
            #[expect(
                clippy::float_cmp,
                reason = "upstream compares the float count with its floor"
            )]
            if current != floored {
                if at == floored {
                    alpha *= (current - floored) * view.opacity;
                } else if at == floored - most {
                    alpha *= (1.0 - current + floored) * view.opacity;
                }
            }
            let x = field.x + start + at * (pass.0 + spacing);
            let y = field.y + field.height / 2.0 - pass.1 / 2.0;
            match &text {
                Some(texture) => paint::blit(pixmap, texture.as_ref().as_ref(), x, y, alpha, 0.0),
                None if self.config.dots_text_format.is_empty() => {
                    paint::fill(
                        pixmap,
                        Rect::new(x, y, pass.0, pass.1),
                        rounding,
                        Color(self.font.0 | 0xFF00_0000),
                        alpha,
                    );
                }
                None => break,
            }
        }
    }
}

/// `texture` with `left` and `right` pixels cleared off its sides.
fn cut_sides(texture: &Pixmap, left: f32, right: f32) -> Option<Pixmap> {
    let mut clipped = texture.clone();
    if left <= 0.0 && right <= 0.0 {
        return Some(clipped);
    }
    let clear = Paint {
        blend_mode: BlendMode::Clear,
        ..Paint::default()
    };
    #[expect(clippy::cast_precision_loss, reason = "a texture's size")]
    let (width, height) = (clipped.width() as f32, clipped.height() as f32);
    for rect in [
        tiny_skia::Rect::from_xywh(0.0, 0.0, left, height),
        tiny_skia::Rect::from_xywh(width - right, 0.0, right, height),
    ]
    .into_iter()
    .flatten()
    {
        clipped.fill_rect(rect, &clear, Transform::identity(), None);
    }
    Some(clipped)
}

// ---------------------------------------------------------------------------
// the scene
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum Item {
    Background(Box<BackgroundItem>),
    Shape(ShapeItem),
    Image(ImageItem),
    Field(Box<FieldItem>),
    Label(LabelItem),
}

/// One screen's widgets.
#[derive(Debug)]
pub struct Scene {
    viewport: (f64, f64),
    items: Vec<Item>,
}

impl Scene {
    /// The widgets `config` puts on `screen`, in drawing order.
    #[must_use]
    pub fn new(config: &Config, screen: &Screen, context: &Context, now: u64) -> Self {
        let viewport = (f64::from(screen.width), f64::from(screen.height));
        let items =
            config
                .widgets_for(&screen.name, &screen.description)
                .into_iter()
                .map(|widget| match widget {
                    Widget::Background(background) => Item::Background(Box::new(
                        BackgroundItem::new(background, viewport, &config.animations, now),
                    )),
                    Widget::Shape(shape) => Item::Shape(ShapeItem::new(shape, viewport)),
                    Widget::Image(image) => Item::Image(ImageItem::new(image, now)),
                    Widget::InputField(field) => Item::Field(Box::new(FieldItem::new(
                        field,
                        viewport,
                        &config.animations,
                    ))),
                    Widget::Label(label) => Item::Label(LabelItem::new(
                        label,
                        context,
                        config.general.text_trim,
                        now,
                    )),
                })
                .collect();
        Self { viewport, items }
    }

    /// How many widgets it has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether it has none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether it has a password field: upstream draws one only where the
    /// file puts one, and a screen without one takes a password all the
    /// same.
    #[must_use]
    pub fn has_field(&self) -> bool {
        self.items.iter().any(|item| matches!(item, Item::Field(_)))
    }

    /// Run the timers that are due by `now`: the commands they want run,
    /// and whether any text changed.
    pub fn due(&mut self, context: &Context, now: u64) -> (Vec<Job>, bool) {
        let mut jobs = Vec::new();
        let mut changed = false;
        for (widget, item) in self.items.iter_mut().enumerate() {
            let line = match item {
                Item::Label(label) if label.next.is_some_and(|at| at <= now) => {
                    let before = label.shown.clone();
                    let line = label.update(context, now);
                    changed |= label.shown != before;
                    line
                }
                Item::Background(background) if background.next.is_some_and(|at| at <= now) => {
                    background.update(now)
                }
                Item::Image(image) if image.next.is_some_and(|at| at <= now) => image.update(now),
                _ => None,
            };
            if let Some(line) = line {
                jobs.push(Job { widget, line });
            }
        }
        (jobs, changed)
    }

    /// `enqueueForceUpdateTimers`: every label that shows an authentication
    /// variable is formatted again now.
    pub fn force_update(&mut self, context: &Context, now: u64) -> Vec<Job> {
        let mut jobs = Vec::new();
        for (widget, item) in self.items.iter_mut().enumerate() {
            if let Item::Label(label) = item
                && label.formatted.allow_force_update
                && let Some(line) = label.update(context, now)
            {
                jobs.push(Job { widget, line });
            }
        }
        jobs
    }

    /// A job's output.
    pub fn finished(&mut self, widget: usize, output: &str, now: u64) {
        match self.items.get_mut(widget) {
            Some(Item::Label(label)) => label.finished(output),
            Some(Item::Background(background)) => background.finished(output, now),
            Some(Item::Image(image)) => image.finished(output),
            _ => {}
        }
    }

    /// `onClick`: the `onclick` commands of every widget under `(x, y)`,
    /// in the surface's pixels, as upstream runs each widget whose box
    /// holds the point.
    #[must_use]
    pub fn click(&self, x: f32, y: f32) -> Vec<String> {
        let inside = |rect: Rect| {
            x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
        };
        self.items
            .iter()
            .filter_map(|item| match item {
                Item::Label(label) => label.drawn.map(|rect| (rect, &label.config.onclick)),
                Item::Image(image) => image.drawn.map(|rect| (rect, &image.config.onclick)),
                Item::Shape(shape) => Some((shape.border_box, &shape.config.onclick)),
                Item::Background(_) | Item::Field(_) => None,
            })
            .filter(|(rect, command)| !command.is_empty() && inside(*rect))
            .map(|(_, command)| command.clone())
            .collect()
    }

    /// The next moment a timer is due.
    #[must_use]
    pub fn next_deadline(&self) -> Option<u64> {
        self.items
            .iter()
            .filter_map(|item| match item {
                Item::Label(label) => label.next,
                Item::Background(background) => background.next,
                Item::Image(image) => image.next,
                Item::Field(field) => field.fade_out_at,
                Item::Shape(_) => None,
            })
            .min()
    }

    /// Draw a frame. Whether something is still moving, so another frame
    /// should follow.
    pub fn draw(
        &mut self,
        pixmap: &mut PixmapMut<'_>,
        view: &View<'_>,
        assets: &mut dyn Assets,
    ) -> bool {
        let mut moving = false;
        for item in &mut self.items {
            match item {
                Item::Background(background) => {
                    background.draw(pixmap, view, assets);
                    moving |= background.animating(view.now);
                }
                Item::Shape(shape) => shape.draw(pixmap, view),
                Item::Image(image) => image.draw(pixmap, self.viewport, view, assets),
                Item::Field(field) => moving |= field.draw(pixmap, view, assets),
                Item::Label(label) => label.draw(pixmap, self.viewport, view, assets),
            }
        }
        moving
    }
}

/// The path of a picture, as upstream's `absolutePath` makes it: `~/`
/// expanded.
#[must_use]
pub fn absolute(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests;
