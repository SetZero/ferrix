//! One bar's own options, as `bar.cpp` reads them.
//!
//! `position` (top, bottom, left, right; top by default) anchors the layer
//! surface to that edge and the two beside it -- unless `width` (for a top
//! or bottom bar) or `height` (for a left or right one) is set, which
//! leaves it the size it asked for, centred. `layer` is `bottom` unless it
//! says `top` or `overlay`. `height` and `width` are unsigned integers;
//! anything else is warned of and ignored. `margin` is a number or a
//! string of one to four numbers, and `margin-top` and the others override
//! it. `spacing` is between the modules of each section. `exclusive`
//! (true), `passthrough` (false), `fixed-center` (true), `no-center`,
//! `name` (the namespace and a class on the window) and `mode` follow
//! waybar's `bar_mode` presets.
//!
//! Left and right bars are laid out as rows here: a vertical bar is not
//! carried out, and says so.

use crate::diag::Diagnostics;
use crate::json::Value;

/// A bar's edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    /// Top.
    Top,
    /// Bottom.
    Bottom,
    /// Left.
    Left,
    /// Right.
    Right,
}

impl Position {
    /// The class `window#waybar` carries.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Position::Top => "top",
            Position::Bottom => "bottom",
            Position::Left => "left",
            Position::Right => "right",
        }
    }
}

/// Which layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerName {
    /// `bottom`, the default.
    Bottom,
    /// `top`.
    Top,
    /// `overlay`.
    Overlay,
}

/// A bar's options.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// `position`.
    pub position: Position,
    /// `layer`.
    pub layer: LayerName,
    /// `height`, 0 for as tall as its content.
    pub height: u32,
    /// `width`, 0 for the output's.
    pub width: u32,
    /// Margins: top, right, bottom, left.
    pub margin: [i32; 4],
    /// `spacing`.
    pub spacing: i32,
    /// `name`.
    pub name: String,
    /// Whether the bar reserves its height (`exclusive`).
    pub exclusive: bool,
    /// Whether clicks pass through (`passthrough`).
    pub passthrough: bool,
    /// `fixed-center`.
    pub fixed_center: bool,
    /// `no-center`.
    pub no_center: bool,
    /// `start_hidden`.
    pub start_hidden: bool,
    /// The modules of each section.
    pub modules: [Vec<String>; 3],
}

fn parse_margin(text: &str) -> Option<[i32; 4]> {
    let numbers: Vec<i32> = text
        .split_whitespace()
        .map(|word| word.parse::<i32>().ok())
        .collect::<Option<Vec<_>>>()?;
    Some(match numbers.as_slice() {
        [a] => [*a; 4],
        [v, h] => [*v, *h, *v, *h],
        [t, h, b] => [*t, *h, *b, *h],
        [t, r, b, l] => [*t, *r, *b, *l],
        _ => return None,
    })
}

/// The mode presets, `Bar::PRESET_MODES`: layer, exclusive, passthrough.
fn preset(mode: &str) -> Option<(LayerName, bool, bool)> {
    Some(match mode {
        "default" | "dock" => (LayerName::Bottom, true, false),
        "hide" => (LayerName::Overlay, false, false),
        "invisible" => (LayerName::Bottom, false, true),
        "overlay" => (LayerName::Overlay, false, true),
        _ => return None,
    })
}

impl Options {
    /// Read a bar object.
    pub fn read(config: &Value, diag: &mut Diagnostics) -> Self {
        let position = match config.get("position").as_str() {
            Some("bottom") => Position::Bottom,
            Some("left") => Position::Left,
            Some("right") => Position::Right,
            _ => Position::Top,
        };
        if matches!(position, Position::Left | Position::Right) {
            diag.warn(format!(
                "\"position\": \"{}\" is not carried out: this waybar lays a bar out as a row",
                position.name()
            ));
        }
        if config.has("height") && !config.get("height").is_uint() {
            diag.warn("Invalid type for 'height', expected unsigned integer".to_owned());
        }
        let uint = |key: &str| {
            let value = config.get(key);
            if value.is_uint() {
                value
                    .as_i64()
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0)
            } else {
                0
            }
        };
        let int = |key: &str| {
            let value = config.get(key);
            value
                .is_int()
                .then(|| value.as_i64().and_then(|n| i32::try_from(n).ok()))
                .flatten()
        };
        let mut margin = [0; 4];
        if ["margin-top", "margin-right", "margin-bottom", "margin-left"]
            .iter()
            .any(|key| config.get(key).is_int())
        {
            margin = [
                int("margin-top").unwrap_or(0),
                int("margin-right").unwrap_or(0),
                int("margin-bottom").unwrap_or(0),
                int("margin-left").unwrap_or(0),
            ];
        } else if let Some(text) = config.get("margin").as_str() {
            match parse_margin(text) {
                Some(found) => margin = found,
                None => diag.warn(format!("Invalid margins: {text}")),
            }
        } else if let Some(all) = int("margin") {
            margin = [all; 4];
        }
        // The default mode takes the bar's own `layer`, `exclusive` and
        // `passthrough`; a named `mode` replaces them.
        let (mut layer, mut exclusive, mut passthrough) = (LayerName::Bottom, true, false);
        match config.get("layer").as_str() {
            Some("top") => layer = LayerName::Top,
            Some("overlay") => layer = LayerName::Overlay,
            _ => {}
        }
        if config.get("exclusive").is_bool() {
            exclusive = config.get("exclusive").as_bool();
        }
        if config.get("passthrough").is_bool() {
            passthrough = config.get("passthrough").as_bool();
        }
        if let Some(mode) = config.get("mode").as_str() {
            match preset(mode) {
                Some(found) if mode != "default" => (layer, exclusive, passthrough) = found,
                Some(_) => {}
                None => diag.warn(format!("Unknown mode \"{mode}\" requested")),
            }
        }
        let list = |key: &str| {
            config
                .get(key)
                .items()
                .iter()
                .map(Value::as_string)
                .collect()
        };
        Self {
            position,
            layer,
            height: uint("height"),
            width: uint("width"),
            margin,
            spacing: int("spacing").unwrap_or(0),
            name: config.get("name").as_string(),
            exclusive,
            passthrough,
            fixed_center: !config.get("fixed-center").is_bool()
                || config.get("fixed-center").as_bool(),
            no_center: config.get("no-center").as_bool(),
            start_hidden: config.get("start_hidden").as_bool(),
            modules: [
                list("modules-left"),
                list("modules-center"),
                list("modules-right"),
            ],
        }
    }

    /// The bar's namespace, which `layerrule`s match: `name`, or `waybar`.
    #[must_use]
    pub fn namespace(&self) -> &str {
        if self.name.is_empty() {
            "waybar"
        } else {
            &self.name
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LayerName, Options, Position};
    use crate::diag::Diagnostics;
    use crate::json::parse;

    #[test]
    fn the_users_bar() {
        let config = parse(r#"{"layer": "top", "position": "top", "height": 40, "spacing": 6, "modules-left": ["custom/launcher", "hyprland/window"]}"#)
            .unwrap_or(crate::json::Value::Null);
        let mut diag = Diagnostics::default();
        let options = Options::read(&config, &mut diag);
        assert_eq!(options.position, Position::Top);
        assert_eq!(options.layer, LayerName::Top);
        assert_eq!(options.height, 40);
        assert_eq!(options.spacing, 6);
        assert!(options.exclusive && !options.passthrough && options.fixed_center);
        assert_eq!(
            options.modules[0],
            vec!["custom/launcher", "hyprland/window"]
        );
        assert_eq!(options.namespace(), "waybar");
        assert!(diag.lines.is_empty());
    }

    #[test]
    fn margins_and_modes() {
        let mut diag = Diagnostics::default();
        let config = parse(r#"{"margin": "4 8", "mode": "overlay", "height": "40"}"#)
            .unwrap_or(crate::json::Value::Null);
        let options = Options::read(&config, &mut diag);
        assert_eq!(options.margin, [4, 8, 4, 8]);
        assert_eq!(options.layer, LayerName::Overlay);
        assert!(!options.exclusive && options.passthrough);
        assert_eq!(options.height, 0);
        assert!(diag.has("Invalid type for 'height'"));
        let config =
            parse(r#"{"margin": 3, "margin-left": 9}"#).unwrap_or(crate::json::Value::Null);
        assert_eq!(Options::read(&config, &mut diag).margin, [0, 0, 0, 9]);
    }
}
