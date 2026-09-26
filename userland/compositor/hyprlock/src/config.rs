//! hyprlock's configuration: every option `src/config/ConfigManager.cpp`
//! declares, with its default, and the widgets the file describes.
//!
//! The five widget kinds are hyprlang *special categories* keyed
//! anonymously: every `background { … }` block is a background of its own,
//! and a key it does not set keeps the default. [`Config::widgets`] lists
//! them the way `getWidgetConfigs` does -- every background, then every
//! shape, image, input field and label, each kind in the order written --
//! which is the order the renderer then sorts by `zindex`.
//!
//! As hyprlang does, a bad line is a [`Diagnostic`] and the rest of the
//! file still applies: `Config has errors: … Proceeding ignoring faulty
//! entries` is what upstream says, and a lock screen that refused to lock
//! over a typo would be worse than one that drew a default.

use std::collections::BTreeMap;
use std::path::PathBuf;

use compositor_config::Color;
use compositor_hyprlang::{Document, Schema, SpecialKey};

/// hyprlang's `INT`, read as `configStringToInt` reads it.
fn parse_int(text: &str) -> Result<i64, String> {
    compositor_hyprlang::value::int(text)
}

/// hyprlang's `FLOAT`.
fn parse_float(text: &str) -> Result<f64, String> {
    compositor_hyprlang::value::float(text)
}

/// A colour, as hyprlang's `INT` reads one.
fn parse_color(text: &str) -> Result<Color, String> {
    parse_int(text).map(|number| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "hyprlang stores a colour as an INT and hyprlock reads its low 32 bits"
        )]
        let low = number as u32;
        Color(low)
    })
}

/// The options outside the widgets, as `ConfigManager::init` adds them.
const OPTIONS: [&str; 14] = [
    "general:text_trim",
    "general:hide_cursor",
    "general:ignore_empty_input",
    "general:immediate_render",
    "general:fractional_scaling",
    "general:screencopy_mode",
    "general:fail_timeout",
    "auth:pam:enabled",
    "auth:pam:module",
    "auth:fingerprint:enabled",
    "auth:fingerprint:ready_message",
    "auth:fingerprint:present_message",
    "auth:fingerprint:retry_delay",
    "animations:enabled",
];

/// `SHADOWABLE`.
const SHADOWABLE: [&str; 4] = [
    "shadow_size",
    "shadow_passes",
    "shadow_color",
    "shadow_boost",
];

/// Each widget kind's options, as `addSpecialConfigValue` adds them.
const WIDGET_OPTIONS: [(&str, &[&str]); 5] = [
    (
        "background",
        &[
            "monitor",
            "path",
            "color",
            "blur_size",
            "blur_passes",
            "noise",
            "contrast",
            "brightness",
            "vibrancy",
            "vibrancy_darkness",
            "zindex",
            "reload_time",
            "reload_cmd",
            "crossfade_time",
        ],
    ),
    (
        "shape",
        &[
            "monitor",
            "size",
            "rounding",
            "border_size",
            "border_color",
            "color",
            "position",
            "halign",
            "valign",
            "rotate",
            "xray",
            "zindex",
            "onclick",
        ],
    ),
    (
        "image",
        &[
            "monitor",
            "path",
            "size",
            "rounding",
            "border_size",
            "border_color",
            "position",
            "halign",
            "valign",
            "rotate",
            "reload_time",
            "reload_cmd",
            "zindex",
            "onclick",
        ],
    ),
    (
        "input-field",
        &[
            "monitor",
            "size",
            "inner_color",
            "outer_color",
            "outline_thickness",
            "dots_size",
            "dots_center",
            "dots_spacing",
            "dots_rounding",
            "dots_text_format",
            "fade_on_empty",
            "fade_timeout",
            "font_color",
            "font_family",
            "halign",
            "valign",
            "position",
            "placeholder_text",
            "hide_input",
            "hide_input_base_color",
            "rounding",
            "check_color",
            "fail_color",
            "fail_text",
            "check_text",
            "capslock_color",
            "numlock_color",
            "bothlock_color",
            "invert_numlock",
            "swap_font_color",
            "zindex",
        ],
    ),
    (
        "label",
        &[
            "monitor",
            "position",
            "color",
            "font_size",
            "text",
            "font_family",
            "halign",
            "valign",
            "rotate",
            "text_align",
            "zindex",
            "onclick",
        ],
    ),
];

/// What `hyprlock.conf` may hold, for `userland/compositor/hyprlang`.
#[must_use]
pub fn schema() -> Schema {
    let mut schema = Schema::new()
        .options(&OPTIONS)
        .keyword("bezier")
        .keyword("animation")
        .source();
    for (kind, options) in WIDGET_OPTIONS {
        let mut all: Vec<&str> = options.to_vec();
        if kind != "background" {
            all.extend(SHADOWABLE);
        }
        schema = schema.special(kind, SpecialKey::Anonymous, &all);
    }
    schema
}

/// A position or size: two numbers, each in pixels or a percentage of the
/// screen (`CLayoutValueData`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// The first number.
    pub x: f64,
    /// The second.
    pub y: f64,
    /// Whether the first was written with `%`.
    pub relative_x: bool,
    /// Whether the second was.
    pub relative_y: bool,
}

impl Layout {
    /// Two numbers in pixels.
    #[must_use]
    pub const fn pixels(x: f64, y: f64) -> Self {
        Self {
            x,
            y,
            relative_x: false,
            relative_y: false,
        }
    }

    /// In pixels, on a screen `viewport` big (`getAbsolute`).
    #[must_use]
    pub fn absolute(&self, viewport: (f64, f64)) -> (f64, f64) {
        (
            if self.relative_x {
                self.x / 100.0 * viewport.0
            } else {
                self.x
            },
            if self.relative_y {
                self.y / 100.0 * viewport.1
            } else {
                self.y
            },
        )
    }

    /// `configHandleLayoutOption`.
    fn parse(value: &str) -> Result<Self, String> {
        let Some((left, right)) = value.split_once(',') else {
            return Err(format!("expected two comma seperated values, got {value}"));
        };
        let right = right.strip_prefix(' ').unwrap_or(right);
        if right.contains(',') {
            return Err(format!("too many arguments in {value}"));
        }
        let (left, relative_x) = match left.strip_suffix('%') {
            Some(number) => (number, true),
            None => (left, false),
        };
        let (right, relative_y) = match right.strip_suffix('%') {
            Some(number) => (number, true),
            None => (right, false),
        };
        let number =
            |text: &str| stof(text).ok_or_else(|| format!("invalid layout values: {value}"));
        Ok(Self {
            x: number(left)?,
            y: number(right)?,
            relative_x,
            relative_y,
        })
    }
}

/// C++'s `std::stof`: leading whitespace, then the longest number there is,
/// and whatever follows ignored.
fn stof(text: &str) -> Option<f64> {
    let text = text.trim_start();
    let end = text
        .char_indices()
        .take_while(|(at, c)| {
            c.is_ascii_digit() || *c == '.' || ((*c == '-' || *c == '+') && *at == 0) || *c == 'e'
        })
        .map(|(at, c)| at + c.len_utf8())
        .last()?;
    (1..=end).rev().find_map(|cut| {
        text.get(..cut)
            .and_then(|number| number.parse::<f64>().ok())
    })
}

/// A colour or a gradient of up to ten, with an angle (`CGradientValueData`).
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    /// The colours, at least one.
    pub colors: Vec<Color>,
    /// The angle in radians.
    pub angle: f32,
    /// Whether this is the empty default rather than something written: an
    /// unset `capslock_color` falls back to `fail_color`.
    pub fallback: bool,
}

impl Gradient {
    /// One colour.
    #[must_use]
    pub fn solid(color: u32) -> Self {
        Self {
            colors: vec![Color(color)],
            angle: 0.0,
            fallback: false,
        }
    }

    /// The first colour, which is what a text or an inner fill takes of it.
    #[must_use]
    pub fn first(&self) -> Color {
        self.colors.first().copied().unwrap_or(Color(0))
    }

    /// `configHandleGradientSet`. What could be read is kept even when a
    /// part could not, as upstream keeps it.
    fn parse(value: &str) -> (Self, Option<String>) {
        let mut gradient = Self {
            colors: Vec::new(),
            angle: 0.0,
            fallback: false,
        };
        let mut error = None;
        let mut rolling = value.to_owned();
        while !rolling.is_empty() {
            let space = rolling.find(' ');
            let last = space.is_none();
            let mut var = space
                .and_then(|at| rolling.get(..at))
                .unwrap_or(&rolling)
                .to_owned();
            if var.contains("rgb") {
                match rolling.find(')') {
                    Some(close) if close + 1 < rolling.len() => {
                        var = rolling.get(..=close).unwrap_or("").to_owned();
                        rolling = rolling.get(close + 2..).unwrap_or("").trim().to_owned();
                    }
                    _ => {
                        var = rolling.trim().to_owned();
                        rolling.clear();
                    }
                }
            } else if let Some(at) = var.find("deg") {
                match var
                    .get(..at)
                    .and_then(|degrees| degrees.parse::<i32>().ok())
                {
                    Some(degrees) => {
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "degrees as written, a few hundred at most"
                        )]
                        let degrees = degrees as f32;
                        gradient.angle = degrees.to_radians();
                    }
                    None => error = Some(format!("Error parsing gradient {value}")),
                }
                break;
            } else {
                rolling = if last {
                    String::new()
                } else {
                    space
                        .and_then(|at| rolling.get(at + 1..))
                        .unwrap_or("")
                        .trim()
                        .to_owned()
                };
            }
            if gradient.colors.len() >= 10 {
                error = Some(format!("Error parsing gradient {value}: max colors is 10."));
                break;
            }
            if var.is_empty() {
                continue;
            }
            match parse_color(&var) {
                Ok(color) => gradient.colors.push(color),
                Err(why) => error = Some(format!("Error parsing gradient {value}: {why}")),
            }
        }
        if value.is_empty() {
            gradient.fallback = true;
            gradient.colors.push(Color(0));
        }
        if gradient.colors.is_empty() {
            error = Some(format!("Error parsing gradient {value}: No colors?"));
            gradient.colors.push(Color(0));
        }
        (gradient, error)
    }
}

/// `SHADOWABLE`: a widget's shadow, which is the widget itself blurred and
/// coloured behind it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    /// `shadow_size`: the blur's size.
    pub size: i64,
    /// `shadow_passes`: the blur's passes, and zero for no shadow.
    pub passes: i64,
    /// `shadow_color`.
    pub color: Color,
    /// `shadow_boost`: how much the alpha is raised.
    pub boost: f64,
}

impl Default for Shadow {
    fn default() -> Self {
        Self {
            size: 3,
            passes: 0,
            color: Color(0xFF00_0000),
            boost: 1.2,
        }
    }
}

/// `background { … }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Background {
    /// `monitor`: empty for every screen.
    pub monitor: String,
    /// `path`: an image, `screenshot`, or empty for the colour alone.
    pub path: String,
    /// `color`, which shows where no image does.
    pub color: Color,
    /// `blur_size`.
    pub blur_size: i64,
    /// `blur_passes`: zero for no blur.
    pub blur_passes: i64,
    /// `noise`.
    pub noise: f64,
    /// `contrast`.
    pub contrast: f64,
    /// `brightness`.
    pub brightness: f64,
    /// `vibrancy`.
    pub vibrancy: f64,
    /// `vibrancy_darkness`.
    pub vibrancy_darkness: f64,
    /// `zindex`.
    pub zindex: i64,
    /// `reload_time`: seconds between runs of `reload_cmd`, -1 for never.
    pub reload_time: i64,
    /// `reload_cmd`: a command whose output is the image's path.
    pub reload_cmd: String,
    /// `crossfade_time`.
    pub crossfade_time: f64,
}

impl Default for Background {
    fn default() -> Self {
        Self {
            monitor: String::new(),
            path: String::new(),
            color: Color(0xFF11_1111),
            blur_size: 8,
            blur_passes: 0,
            noise: 0.0117,
            contrast: 0.8917,
            brightness: 0.8172,
            vibrancy: 0.1686,
            vibrancy_darkness: 0.05,
            zindex: -1,
            reload_time: -1,
            reload_cmd: String::new(),
            crossfade_time: -1.0,
        }
    }
}

/// `shape { … }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    /// `monitor`.
    pub monitor: String,
    /// `size`.
    pub size: Layout,
    /// `rounding`: -1 for as round as the box allows.
    pub rounding: i64,
    /// `border_size`.
    pub border_size: i64,
    /// `border_color`.
    pub border_color: Gradient,
    /// `color`.
    pub color: Color,
    /// `position`.
    pub position: Layout,
    /// `halign`.
    pub halign: String,
    /// `valign`.
    pub valign: String,
    /// `rotate`, in degrees.
    pub rotate: f64,
    /// `xray`: cut a hole through everything under it.
    pub xray: bool,
    /// `zindex`.
    pub zindex: i64,
    /// The shadow.
    pub shadow: Shadow,
    /// `onclick`.
    pub onclick: String,
}

impl Default for Shape {
    fn default() -> Self {
        Self {
            monitor: String::new(),
            size: Layout::pixels(100.0, 100.0),
            rounding: 0,
            border_size: 0,
            border_color: Gradient::solid(0xFF00_CFE6),
            color: Color(0xFF11_1111),
            position: Layout::pixels(0.0, 0.0),
            halign: "center".to_owned(),
            valign: "center".to_owned(),
            rotate: 0.0,
            xray: false,
            zindex: 0,
            shadow: Shadow::default(),
            onclick: String::new(),
        }
    }
}

/// `image { … }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    /// `monitor`.
    pub monitor: String,
    /// `path`.
    pub path: String,
    /// `size`: the shorter side, in pixels.
    pub size: i64,
    /// `rounding`.
    pub rounding: i64,
    /// `border_size`.
    pub border_size: i64,
    /// `border_color`.
    pub border_color: Gradient,
    /// `position`.
    pub position: Layout,
    /// `halign`.
    pub halign: String,
    /// `valign`.
    pub valign: String,
    /// `rotate`.
    pub rotate: f64,
    /// `reload_time`.
    pub reload_time: i64,
    /// `reload_cmd`.
    pub reload_cmd: String,
    /// `zindex`.
    pub zindex: i64,
    /// The shadow.
    pub shadow: Shadow,
    /// `onclick`.
    pub onclick: String,
}

impl Default for Image {
    fn default() -> Self {
        Self {
            monitor: String::new(),
            path: String::new(),
            size: 150,
            rounding: -1,
            border_size: 4,
            border_color: Gradient::solid(0xFFDD_DDDD),
            position: Layout::pixels(0.0, 0.0),
            halign: "center".to_owned(),
            valign: "center".to_owned(),
            rotate: 0.0,
            reload_time: -1,
            reload_cmd: String::new(),
            zindex: 0,
            shadow: Shadow::default(),
            onclick: String::new(),
        }
    }
}

/// `input-field { … }`.
#[derive(Clone, Debug, PartialEq)]
pub struct InputField {
    /// `monitor`.
    pub monitor: String,
    /// `size`.
    pub size: Layout,
    /// `inner_color`.
    pub inner_color: Color,
    /// `outer_color`.
    pub outer_color: Gradient,
    /// `outline_thickness`.
    pub outline_thickness: i64,
    /// `dots_size`: a fraction of the field's height.
    pub dots_size: f64,
    /// `dots_center`.
    pub dots_center: bool,
    /// `dots_spacing`: a fraction of a dot.
    pub dots_spacing: f64,
    /// `dots_rounding`: -1 for circles, -2 for the field's own.
    pub dots_rounding: i64,
    /// `dots_text_format`: a text to draw instead of each dot.
    pub dots_text_format: String,
    /// `fade_on_empty`.
    pub fade_on_empty: bool,
    /// `fade_timeout`, in milliseconds.
    pub fade_timeout: i64,
    /// `font_color`.
    pub font_color: Color,
    /// `font_family`.
    pub font_family: String,
    /// `halign`.
    pub halign: String,
    /// `valign`.
    pub valign: String,
    /// `position`.
    pub position: Layout,
    /// `placeholder_text`, Pango markup.
    pub placeholder_text: String,
    /// `hide_input`.
    pub hide_input: bool,
    /// `hide_input_base_color`.
    pub hide_input_base_color: Color,
    /// `rounding`.
    pub rounding: i64,
    /// `check_color`.
    pub check_color: Gradient,
    /// `fail_color`.
    pub fail_color: Gradient,
    /// `fail_text`.
    pub fail_text: String,
    /// `check_text`.
    pub check_text: String,
    /// `capslock_color`.
    pub capslock_color: Gradient,
    /// `numlock_color`.
    pub numlock_color: Gradient,
    /// `bothlock_color`.
    pub bothlock_color: Gradient,
    /// `invert_numlock`.
    pub invert_numlock: bool,
    /// `swap_font_color`.
    pub swap_font_color: bool,
    /// `zindex`.
    pub zindex: i64,
    /// The shadow.
    pub shadow: Shadow,
}

impl Default for InputField {
    fn default() -> Self {
        let unset = Gradient::parse("").0;
        Self {
            monitor: String::new(),
            size: Layout::pixels(400.0, 90.0),
            inner_color: Color(0xFFDD_DDDD),
            outer_color: Gradient::solid(0xFF11_1111),
            outline_thickness: 4,
            dots_size: 0.25,
            dots_center: true,
            dots_spacing: 0.2,
            dots_rounding: -1,
            dots_text_format: String::new(),
            fade_on_empty: true,
            fade_timeout: 2000,
            font_color: Color(0xFF00_0000),
            font_family: "Sans".to_owned(),
            halign: "center".to_owned(),
            valign: "center".to_owned(),
            position: Layout::pixels(0.0, 0.0),
            placeholder_text: "<i>Input Password</i>".to_owned(),
            hide_input: false,
            hide_input_base_color: Color(0xEE00_FF99),
            rounding: -1,
            check_color: Gradient::solid(0xFF22_CC88),
            fail_color: Gradient::solid(0xFFCC_2222),
            fail_text: "<i>$FAIL</i>".to_owned(),
            check_text: String::new(),
            capslock_color: unset.clone(),
            numlock_color: unset.clone(),
            bothlock_color: unset,
            invert_numlock: false,
            swap_font_color: false,
            zindex: 0,
            shadow: Shadow::default(),
        }
    }
}

/// `label { … }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    /// `monitor`.
    pub monitor: String,
    /// `position`.
    pub position: Layout,
    /// `color`.
    pub color: Color,
    /// `font_size`, in points.
    pub font_size: i64,
    /// `text`: markup, with `$TIME` and the rest, or `cmd[…] command`.
    pub text: String,
    /// `font_family`.
    pub font_family: String,
    /// `halign`.
    pub halign: String,
    /// `valign`.
    pub valign: String,
    /// `rotate`.
    pub rotate: f64,
    /// `text_align`: `left`, `center` or `right` between lines.
    pub text_align: String,
    /// `zindex`.
    pub zindex: i64,
    /// The shadow.
    pub shadow: Shadow,
    /// `onclick`.
    pub onclick: String,
}

impl Default for Label {
    fn default() -> Self {
        Self {
            monitor: String::new(),
            position: Layout::pixels(0.0, 0.0),
            color: Color(0xFFFF_FFFF),
            font_size: 16,
            text: "Sample Text".to_owned(),
            font_family: "Sans".to_owned(),
            halign: "center".to_owned(),
            valign: "center".to_owned(),
            rotate: 0.0,
            text_align: String::new(),
            zindex: 0,
            shadow: Shadow::default(),
            onclick: String::new(),
        }
    }
}

/// One widget.
#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "a file has a handful of widgets, each made once; boxing one kind buys nothing"
)]
pub enum Widget {
    /// A background.
    Background(Background),
    /// A shape.
    Shape(Shape),
    /// An image.
    Image(Image),
    /// The password field.
    InputField(InputField),
    /// A label.
    Label(Label),
}

impl Widget {
    /// The category's name, as the file writes it.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Background(_) => "background",
            Self::Shape(_) => "shape",
            Self::Image(_) => "image",
            Self::InputField(_) => "input-field",
            Self::Label(_) => "label",
        }
    }

    /// `monitor`.
    #[must_use]
    pub fn monitor(&self) -> &str {
        match self {
            Self::Background(widget) => &widget.monitor,
            Self::Shape(widget) => &widget.monitor,
            Self::Image(widget) => &widget.monitor,
            Self::InputField(widget) => &widget.monitor,
            Self::Label(widget) => &widget.monitor,
        }
    }

    /// `zindex`.
    #[must_use]
    pub const fn zindex(&self) -> i64 {
        match self {
            Self::Background(widget) => widget.zindex,
            Self::Shape(widget) => widget.zindex,
            Self::Image(widget) => widget.zindex,
            Self::InputField(widget) => widget.zindex,
            Self::Label(widget) => widget.zindex,
        }
    }

    /// Whether it is drawn on the output `name` described as `description`:
    /// `getOrCreateWidgetsFor`'s test. An empty `monitor` is every output;
    /// otherwise it is the connector exactly, or a prefix of the
    /// description, with or without `desc:` in front.
    #[must_use]
    pub fn shown_on(&self, name: &str, description: &str) -> bool {
        let monitor = self.monitor();
        monitor.is_empty()
            || monitor == name
            || description.starts_with(monitor)
            || format!("desc:{description}").starts_with(monitor)
    }

    fn fresh(kind: &str) -> Option<Self> {
        Some(match kind {
            "background" => Self::Background(Background::default()),
            "shape" => Self::Shape(Shape::default()),
            "image" => Self::Image(Image::default()),
            "input-field" => Self::InputField(InputField::default()),
            "label" => Self::Label(Label::default()),
            _ => return None,
        })
    }

    /// Set one key. `Ok(false)` for a key the kind does not have.
    fn set(&mut self, key: &str, value: &str) -> Result<bool, String> {
        if let Some(shadow) = self.shadow_mut()
            && set_shadow(shadow, key, value)?
        {
            return Ok(true);
        }
        match self {
            Self::Background(widget) => set_background(widget, key, value),
            Self::Shape(widget) => set_shape(widget, key, value),
            Self::Image(widget) => set_image(widget, key, value),
            Self::InputField(widget) => set_input_field(widget, key, value),
            Self::Label(widget) => set_label(widget, key, value),
        }
    }

    fn shadow_mut(&mut self) -> Option<&mut Shadow> {
        match self {
            Self::Background(_) => None,
            Self::Shape(widget) => Some(&mut widget.shadow),
            Self::Image(widget) => Some(&mut widget.shadow),
            Self::InputField(widget) => Some(&mut widget.shadow),
            Self::Label(widget) => Some(&mut widget.shadow),
        }
    }
}

/// hyprlang's `INT`.
fn int(value: &str) -> Result<i64, String> {
    parse_int(value)
}

/// hyprlang's `INT`, read as a colour.
fn color(value: &str) -> Result<Color, String> {
    parse_color(value)
}

/// hyprlang's `FLOAT`.
fn float(value: &str) -> Result<f64, String> {
    parse_float(value)
}

/// A custom gradient value: kept even with an error, as upstream keeps it.
fn gradient(target: &mut Gradient, value: &str) -> Result<bool, String> {
    let (parsed, error) = Gradient::parse(value);
    *target = parsed;
    error.map_or(Ok(true), Err)
}

fn set_shadow(shadow: &mut Shadow, key: &str, value: &str) -> Result<bool, String> {
    match key {
        "shadow_size" => shadow.size = int(value)?,
        "shadow_passes" => shadow.passes = int(value)?,
        "shadow_color" => shadow.color = color(value)?,
        "shadow_boost" => shadow.boost = float(value)?,
        _ => return Ok(false),
    }
    Ok(true)
}

fn set_background(widget: &mut Background, key: &str, value: &str) -> Result<bool, String> {
    match key {
        "monitor" => widget.monitor = value.to_owned(),
        "path" => widget.path = value.to_owned(),
        "color" => widget.color = color(value)?,
        "blur_size" => widget.blur_size = int(value)?,
        "blur_passes" => widget.blur_passes = int(value)?,
        "noise" => widget.noise = float(value)?,
        "contrast" => widget.contrast = float(value)?,
        "brightness" => widget.brightness = float(value)?,
        "vibrancy" => widget.vibrancy = float(value)?,
        "vibrancy_darkness" => widget.vibrancy_darkness = float(value)?,
        "zindex" => widget.zindex = int(value)?,
        "reload_time" => widget.reload_time = int(value)?,
        "reload_cmd" => widget.reload_cmd = value.to_owned(),
        "crossfade_time" => widget.crossfade_time = float(value)?,
        _ => return Ok(false),
    }
    Ok(true)
}

fn set_shape(widget: &mut Shape, key: &str, value: &str) -> Result<bool, String> {
    match key {
        "monitor" => widget.monitor = value.to_owned(),
        "size" => widget.size = Layout::parse(value)?,
        "rounding" => widget.rounding = int(value)?,
        "border_size" => widget.border_size = int(value)?,
        "border_color" => return gradient(&mut widget.border_color, value),
        "color" => widget.color = color(value)?,
        "position" => widget.position = Layout::parse(value)?,
        "halign" => widget.halign = value.to_owned(),
        "valign" => widget.valign = value.to_owned(),
        "rotate" => widget.rotate = float(value)?,
        "xray" => widget.xray = int(value)? != 0,
        "zindex" => widget.zindex = int(value)?,
        "onclick" => widget.onclick = value.to_owned(),
        _ => return Ok(false),
    }
    Ok(true)
}

fn set_image(widget: &mut Image, key: &str, value: &str) -> Result<bool, String> {
    match key {
        "monitor" => widget.monitor = value.to_owned(),
        "path" => widget.path = value.to_owned(),
        "size" => widget.size = int(value)?,
        "rounding" => widget.rounding = int(value)?,
        "border_size" => widget.border_size = int(value)?,
        "border_color" => return gradient(&mut widget.border_color, value),
        "position" => widget.position = Layout::parse(value)?,
        "halign" => widget.halign = value.to_owned(),
        "valign" => widget.valign = value.to_owned(),
        "rotate" => widget.rotate = float(value)?,
        "reload_time" => widget.reload_time = int(value)?,
        "reload_cmd" => widget.reload_cmd = value.to_owned(),
        "zindex" => widget.zindex = int(value)?,
        "onclick" => widget.onclick = value.to_owned(),
        _ => return Ok(false),
    }
    Ok(true)
}

fn set_input_field(widget: &mut InputField, key: &str, value: &str) -> Result<bool, String> {
    match key {
        "monitor" => widget.monitor = value.to_owned(),
        "size" => widget.size = Layout::parse(value)?,
        "inner_color" => widget.inner_color = color(value)?,
        "outer_color" => return gradient(&mut widget.outer_color, value),
        "outline_thickness" => widget.outline_thickness = int(value)?,
        "dots_size" => widget.dots_size = float(value)?,
        "dots_center" => widget.dots_center = int(value)? != 0,
        "dots_spacing" => widget.dots_spacing = float(value)?,
        "dots_rounding" => widget.dots_rounding = int(value)?,
        "dots_text_format" => widget.dots_text_format = value.to_owned(),
        "fade_on_empty" => widget.fade_on_empty = int(value)? != 0,
        "fade_timeout" => widget.fade_timeout = int(value)?,
        "font_color" => widget.font_color = color(value)?,
        "font_family" => widget.font_family = value.to_owned(),
        "halign" => widget.halign = value.to_owned(),
        "valign" => widget.valign = value.to_owned(),
        "position" => widget.position = Layout::parse(value)?,
        "placeholder_text" => widget.placeholder_text = value.to_owned(),
        "hide_input" => widget.hide_input = int(value)? != 0,
        "hide_input_base_color" => widget.hide_input_base_color = color(value)?,
        "rounding" => widget.rounding = int(value)?,
        "check_color" => return gradient(&mut widget.check_color, value),
        "fail_color" => return gradient(&mut widget.fail_color, value),
        "fail_text" => widget.fail_text = value.to_owned(),
        "check_text" => widget.check_text = value.to_owned(),
        "capslock_color" => return gradient(&mut widget.capslock_color, value),
        "numlock_color" => return gradient(&mut widget.numlock_color, value),
        "bothlock_color" => return gradient(&mut widget.bothlock_color, value),
        "invert_numlock" => widget.invert_numlock = int(value)? != 0,
        "swap_font_color" => widget.swap_font_color = int(value)? != 0,
        "zindex" => widget.zindex = int(value)?,
        _ => return Ok(false),
    }
    Ok(true)
}

fn set_label(widget: &mut Label, key: &str, value: &str) -> Result<bool, String> {
    match key {
        "monitor" => widget.monitor = value.to_owned(),
        "position" => widget.position = Layout::parse(value)?,
        "color" => widget.color = color(value)?,
        "font_size" => widget.font_size = int(value)?,
        "text" => widget.text = value.to_owned(),
        "font_family" => widget.font_family = value.to_owned(),
        "halign" => widget.halign = value.to_owned(),
        "valign" => widget.valign = value.to_owned(),
        "rotate" => widget.rotate = float(value)?,
        "text_align" => widget.text_align = value.to_owned(),
        "zindex" => widget.zindex = int(value)?,
        "onclick" => widget.onclick = value.to_owned(),
        _ => return Ok(false),
    }
    Ok(true)
}

/// `general { … }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct General {
    /// `text_trim`: trim a command label's output.
    pub text_trim: bool,
    /// `hide_cursor`.
    pub hide_cursor: bool,
    /// `ignore_empty_input`: Enter on an empty field does nothing.
    pub ignore_empty_input: bool,
    /// `immediate_render`.
    pub immediate_render: bool,
    /// `fractional_scaling`: 0 off, 1 on, 2 automatic.
    pub fractional_scaling: i64,
    /// `screencopy_mode`: 0 for the GPU, 1 for shared memory.
    pub screencopy_mode: i64,
    /// `fail_timeout`: how long the fail text shows, in milliseconds.
    pub fail_timeout: i64,
}

impl Default for General {
    fn default() -> Self {
        Self {
            text_trim: true,
            hide_cursor: false,
            ignore_empty_input: false,
            immediate_render: false,
            fractional_scaling: 2,
            screencopy_mode: 0,
            fail_timeout: 2000,
        }
    }
}

/// `auth { … }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Auth {
    /// `pam:enabled`: on Ferrix, the password check of [`crate::auth`].
    pub pam: bool,
    /// `pam:module`: the PAM service, which Ferrix has no notion of.
    pub pam_module: String,
    /// `fingerprint:enabled`: through fprintd over D-Bus, which Ferrix has
    /// neither of.
    pub fingerprint: bool,
    /// `fingerprint:ready_message`.
    pub fingerprint_ready: String,
    /// `fingerprint:present_message`.
    pub fingerprint_present: String,
    /// `fingerprint:retry_delay`.
    pub fingerprint_retry_delay: i64,
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            pam: true,
            pam_module: "hyprlock".to_owned(),
            fingerprint: false,
            fingerprint_ready: "(Scan fingerprint to unlock)".to_owned(),
            fingerprint_present: "Scanning fingerprint".to_owned(),
            fingerprint_retry_delay: 250,
        }
    }
}

/// One node of hyprlock's animation tree, as set.
#[derive(Clone, Debug, PartialEq)]
pub struct Animation {
    /// Whether it animates at all.
    pub enabled: bool,
    /// Its speed, in tenths of a second.
    pub speed: f32,
    /// The curve's name.
    pub bezier: String,
}

/// hyprlock's animation tree: `global` above `fade` and `inputField`, each
/// with its children, a node unset taking its parent's.
pub const NODES: [(&str, &str); 9] = [
    ("global", ""),
    ("fade", "global"),
    ("inputField", "global"),
    ("inputFieldColors", "inputField"),
    ("inputFieldFade", "inputField"),
    ("inputFieldWidth", "inputField"),
    ("inputFieldDots", "inputField"),
    ("fadeIn", "fade"),
    ("fadeOut", "fade"),
];

/// `animations { … }`: the switch, the curves and the tree.
#[derive(Clone, Debug)]
pub struct Animations {
    /// `animations:enabled`.
    pub enabled: bool,
    /// The curves, `default` and `linear` built in.
    pub curves: compositor_anim::Curves,
    set: BTreeMap<String, Animation>,
}

impl Default for Animations {
    fn default() -> Self {
        let mut set = BTreeMap::new();
        let _ = set.insert(
            "global".to_owned(),
            Animation {
                enabled: true,
                speed: 8.0,
                bezier: "default".to_owned(),
            },
        );
        let _ = set.insert(
            "inputFieldColors".to_owned(),
            Animation {
                enabled: true,
                speed: 8.0,
                bezier: "linear".to_owned(),
            },
        );
        Self {
            enabled: true,
            curves: compositor_anim::Curves::new(),
            set,
        }
    }
}

impl Animations {
    /// The settings `name` has: its own, or the nearest ancestor's.
    #[must_use]
    pub fn get(&self, name: &str) -> Animation {
        let mut at = name;
        loop {
            if let Some(found) = self.set.get(at) {
                return found.clone();
            }
            match NODES.iter().find(|(node, _)| *node == at) {
                Some((_, parent)) if !parent.is_empty() => at = parent,
                _ => {
                    return Animation {
                        enabled: true,
                        speed: 8.0,
                        bezier: "default".to_owned(),
                    };
                }
            }
        }
    }

    /// How long `name` takes, in milliseconds; zero for one that warps.
    #[must_use]
    pub fn duration(&self, name: &str) -> f32 {
        let node = self.get(name);
        if !self.enabled || !node.enabled {
            return 0.0;
        }
        node.speed * 100.0
    }

    /// Turn `name` off, as `--no-fade-in` turns `fadeIn` off.
    pub fn disable(&mut self, name: &str) {
        let _ = self.set.insert(
            name.to_owned(),
            Animation {
                enabled: false,
                speed: 0.0,
                bezier: "default".to_owned(),
            },
        );
    }

    /// `bezier = NAME, X0, Y0, X1, Y1`.
    fn bezier(&mut self, value: &str) -> Result<(), String> {
        let args: Vec<&str> = value.split(',').map(str::trim).collect();
        let arg = |at: usize| args.get(at).copied().unwrap_or("");
        for at in 1..=4 {
            if arg(at).is_empty() {
                return Err("too few arguments".to_owned());
            }
        }
        let number = |at: usize| {
            stof(arg(at))
                .map(|value| {
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "a control point, a small number, stored as float as upstream does"
                    )]
                    let value = value as f32;
                    value
                })
                .ok_or_else(|| "invalid bezier arguments".to_owned())
        };
        let points = ((number(1)?, number(2)?), (number(3)?, number(4)?));
        if !arg(5).is_empty() {
            return Err("too many arguments".to_owned());
        }
        self.curves.add(arg(0), points.0, points.1);
        Ok(())
    }

    /// `animation = NAME, ONOFF, SPEED, CURVE`.
    fn animation(&mut self, value: &str) -> Result<(), String> {
        let args: Vec<&str> = value.split(',').map(str::trim).collect();
        let arg = |at: usize| args.get(at).copied().unwrap_or("");
        let name = arg(0);
        if !NODES.iter().any(|(node, _)| *node == name) {
            return Err("no such animation".to_owned());
        }
        let enabled = parse_int(arg(1)).unwrap_or(-1);
        if !(0..=1).contains(&enabled) {
            return Err("invalid animation on/off state".to_owned());
        }
        if enabled == 0 {
            self.disable(name);
            return Ok(());
        }
        let speed = match parse_float(arg(2)) {
            Ok(speed) if speed > 0.0 => {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a speed in tenths of a second, stored as float as upstream does"
                )]
                let speed = speed as f32;
                speed
            }
            _ => return Err("invalid speed".to_owned()),
        };
        let bezier = arg(3);
        let known = self.curves.has(bezier);
        let _ = self.set.insert(
            name.to_owned(),
            Animation {
                enabled: true,
                speed,
                bezier: if known { bezier } else { "default" }.to_owned(),
            },
        );
        if known {
            Ok(())
        } else {
            Err("no such bezier".to_owned())
        }
    }
}

/// A line of the file that did not apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// The file.
    pub file: PathBuf,
    /// The line, from one.
    pub line: usize,
    /// hyprlang's or hyprlock's wording.
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Config error in file {} at line {}: {}",
            self.file.display(),
            self.line,
            self.message
        )
    }
}

/// The whole configuration.
#[derive(Clone, Debug)]
pub struct Config {
    /// `general`.
    pub general: General,
    /// `auth`.
    pub auth: Auth,
    /// `animations`.
    pub animations: Animations,
    /// The widgets, in `getWidgetConfigs`'s order.
    pub widgets: Vec<Widget>,
    /// What did not apply.
    pub diagnostics: Vec<Diagnostic>,
}

impl Config {
    /// Read a file's text.
    #[must_use]
    pub fn parse(text: &str, file: &std::path::Path) -> Self {
        Self::from_document(&compositor_hyprlang::parse(&schema(), text, file))
    }

    /// Build the configuration from what hyprlang read.
    #[must_use]
    pub fn from_document(document: &Document) -> Self {
        let mut general = General::default();
        let mut auth = Auth::default();
        let mut animations = Animations::default();
        let mut diagnostics: Vec<Diagnostic> = document
            .diagnostics
            .iter()
            .map(|diagnostic| Diagnostic {
                file: diagnostic.file.clone(),
                line: diagnostic.line,
                message: diagnostic.message.clone(),
            })
            .collect();
        let mut problem = |file: &std::path::Path, line: usize, message: String| {
            diagnostics.push(Diagnostic {
                file: file.to_owned(),
                line,
                message,
            });
        };
        for setting in &document.options {
            if let Err(error) = set_global(
                &mut general,
                &mut auth,
                &mut animations,
                &setting.name,
                &setting.value,
            ) {
                problem(&setting.file, setting.line, error);
            }
        }
        for keyword in &document.keywords {
            let result = match keyword.name.rsplit(':').next().unwrap_or("") {
                "bezier" => animations.bezier(&keyword.value),
                "animation" => animations.animation(&keyword.value),
                _ => Ok(()),
            };
            if let Err(error) = result {
                problem(&keyword.file, keyword.line, error);
            }
        }
        let mut instances = Vec::new();
        for instance in &document.instances {
            let Some(mut widget) = Widget::fresh(&instance.category) else {
                continue;
            };
            for setting in &instance.values {
                match widget.set(&setting.name, &setting.value) {
                    Ok(true) => {}
                    Ok(false) => problem(
                        &setting.file,
                        setting.line,
                        format!(
                            "config option <{}:{}> does not exist.",
                            instance.category, setting.name
                        ),
                    ),
                    Err(error) => problem(&setting.file, setting.line, error),
                }
            }
            instances.push(widget);
        }
        diagnostics.sort_by_key(|diagnostic| diagnostic.line);
        // `getWidgetConfigs`: every background, then every shape, image,
        // input field and label.
        let mut widgets = Vec::with_capacity(instances.len());
        for kind in ["background", "shape", "image", "input-field", "label"] {
            widgets.extend(
                instances
                    .iter()
                    .filter(|widget| widget.kind() == kind)
                    .cloned(),
            );
        }
        Self {
            general,
            auth,
            animations,
            widgets,
            diagnostics,
        }
    }

    /// The lines this program reads and cannot carry out on Ferrix, each
    /// a sentence naming it.
    #[must_use]
    pub fn unsupported(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.auth.fingerprint {
            lines.push(
                "auth:fingerprint:enabled: fingerprints go through fprintd over D-Bus, and Ferrix has neither; only the password unlocks"
                    .to_owned(),
            );
        }
        if self.auth.pam_module != "hyprlock" {
            lines.push(format!(
                "auth:pam:module = {}: Ferrix has no PAM; the password is checked against /etc/shadow whatever the module",
                self.auth.pam_module
            ));
        }
        for widget in &self.widgets {
            if let Widget::InputField(field) = widget
                && field.hide_input
            {
                lines.push(
                    "input-field:hide_input: drawn as plain dots on Ferrix for now".to_owned(),
                );
            }
        }
        lines
    }

    /// The widgets drawn on one output, in drawing order: filtered by
    /// `monitor`, then sorted by `zindex` keeping the file's order among
    /// equals, which is what upstream's sort does for a file of this size.
    #[must_use]
    pub fn widgets_for(&self, name: &str, description: &str) -> Vec<&Widget> {
        let mut shown: Vec<&Widget> = self
            .widgets
            .iter()
            .filter(|widget| widget.shown_on(name, description))
            .collect();
        shown.sort_by_key(|widget| widget.zindex());
        shown
    }
}

/// A key outside any widget.
fn set_global(
    general: &mut General,
    auth: &mut Auth,
    animations: &mut Animations,
    path: &str,
    value: &str,
) -> Result<(), String> {
    let flag = |value: &str| int(value).map(|number| number != 0);
    match path {
        "general:text_trim" => general.text_trim = flag(value)?,
        "general:hide_cursor" => general.hide_cursor = flag(value)?,
        "general:ignore_empty_input" => general.ignore_empty_input = flag(value)?,
        "general:immediate_render" => general.immediate_render = flag(value)?,
        "general:fractional_scaling" => general.fractional_scaling = int(value)?,
        "general:screencopy_mode" => general.screencopy_mode = int(value)?,
        "general:fail_timeout" => general.fail_timeout = int(value)?,
        "auth:pam:enabled" => auth.pam = flag(value)?,
        "auth:pam:module" => auth.pam_module = value.to_owned(),
        "auth:fingerprint:enabled" => auth.fingerprint = flag(value)?,
        "auth:fingerprint:ready_message" => auth.fingerprint_ready = value.to_owned(),
        "auth:fingerprint:present_message" => auth.fingerprint_present = value.to_owned(),
        "auth:fingerprint:retry_delay" => auth.fingerprint_retry_delay = int(value)?,
        "animations:enabled" => animations.enabled = flag(value)?,
        _ => return Err(format!("config option <{path}> does not exist.")),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
