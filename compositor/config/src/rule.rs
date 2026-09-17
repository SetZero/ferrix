//! A `windowrule =` line, read into what the compositor does with it.
//!
//! Hyprland 0.56's form is a list of comma-separated fields, each a name and
//! a value with a space between them; a field whose name begins `match:` is
//! something the window must be, and every other field is something to do to
//! it (`CConfigManager::handleWindowrule`):
//!
//! ```text
//! windowrule = float, match:class ^(foot)$
//! windowrule = size 800 600, match:title ^(Save.*)$, match:float 1
//! ```
//!
//! `windowrulev2` is refused, with Hyprland's own words, because that is
//! what Hyprland 0.56 does with it: the two syntaxes were merged and the old
//! one taken away.
//!
//! What a rule matches on is a regular expression for the four names a
//! window has and a yes-or-no for the states it can be in;
//! `compositor/regex` says which patterns are understood. What a rule does
//! is the list on [`Effect`] -- the parts of Hyprland's that the layout and
//! the renderer here can carry out.

use compositor_regex::Regex;

/// What a window must be for a rule to apply to it.
#[derive(Clone, Debug)]
pub enum Matcher {
    /// `match:class`: the application id, which Hyprland calls the class.
    Class(Regex),
    /// `match:title`.
    Title(Regex),
    /// `match:initial_class`: the class it had when it opened.
    InitialClass(Regex),
    /// `match:initial_title`.
    InitialTitle(Regex),
    /// `match:float`: whether it floats.
    Floating(bool),
    /// `match:fullscreen`.
    Fullscreen(bool),
    /// `match:focus`: whether it is the focused window.
    Focused(bool),
}

/// What a rule does to a window it matches.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// `float`: take it out of the tiling.
    Float,
    /// `tile`: put it back in, which is what a window is by default.
    Tile,
    /// `size <width> <height>`, each in pixels or as a percentage of the
    /// monitor.
    Size(Length, Length),
    /// `move <x> <y>`, the same, or `center`.
    Move(Length, Length),
    /// `center`: put it in the middle of the monitor.
    Center,
    /// `workspace <id>`, and whether the focus follows it there.
    Workspace {
        /// The workspace, as a `workspace` dispatcher would name it.
        target: String,
        /// `silent`: the window goes and the focus stays.
        silent: bool,
    },
    /// `fullscreen`.
    Fullscreen,
    /// `maximize`.
    Maximize,
    /// `no_focus`: it opens without taking the focus.
    NoFocus,
    /// `opacity <a>`: how much of it shows.
    Opacity(f32),
    /// `rounding <n>`: how far its corners are cut.
    Rounding(i64),
    /// `border_size <n>`.
    BorderSize(i64),
    /// `no_blur`, `no_shadow`, `no_dim`: a decoration this window does not
    /// get.
    Without(Decoration),
}

/// A decoration a rule can take away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoration {
    /// `no_blur`.
    Blur,
    /// `no_shadow`.
    Shadow,
    /// `no_dim`.
    Dim,
}

/// A length a rule gives: pixels, or a percentage of the monitor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    /// A number of pixels.
    Pixels(i64),
    /// A share of the monitor's own width or height, from zero to one.
    Share(f64),
}

impl Length {
    /// The length in pixels, against a monitor `whole` pixels long.
    #[must_use]
    pub fn against(self, whole: i64) -> i64 {
        match self {
            Self::Pixels(pixels) => pixels,
            Self::Share(share) => {
                #[expect(
                    clippy::cast_precision_loss,
                    clippy::cast_possible_truncation,
                    reason = "a monitor's pixels are far inside f64's exact range"
                )]
                let scaled = (whole as f64 * share).round() as i64;
                scaled
            }
        }
    }

    /// Read `50%` or `500`.
    fn parse(text: &str) -> Result<Self, String> {
        if let Some(number) = text.strip_suffix('%') {
            let share: f64 = number
                .trim()
                .parse()
                .map_err(|_| format!("`{text}` is not a percentage"))?;
            return Ok(Self::Share(share / 100.0));
        }
        text.trim()
            .parse()
            .map(Self::Pixels)
            .map_err(|_| format!("`{text}` is not a length"))
    }
}

/// One `windowrule =` line.
#[derive(Clone, Debug)]
pub struct WindowRule {
    /// What it does, in the order the line gave them.
    pub effects: Vec<Effect>,
    /// What the window must be. A rule with none matches every window,
    /// which is what a line with no `match:` field means.
    pub matchers: Vec<Matcher>,
}

impl WindowRule {
    /// Read a `windowrule =` line's value.
    ///
    /// # Errors
    ///
    /// Hyprland's own wording for a field it cannot read.
    pub fn parse(value: &str) -> Result<Self, String> {
        let mut effects = Vec::new();
        let mut matchers = Vec::new();
        for field in value.split(',').map(str::trim).filter(|f| !f.is_empty()) {
            match field.strip_prefix("match:") {
                Some(rest) => matchers.push(matcher(rest)?),
                None => effects.push(effect(field)?),
            }
        }
        if effects.is_empty() {
            return Err(format!("windowrule: `{value}` does nothing"));
        }
        Ok(Self { effects, matchers })
    }

    /// Whether this rule applies to a window that is `what`.
    #[must_use]
    pub fn matches(&self, what: &Window<'_>) -> bool {
        self.matchers.iter().all(|matcher| match matcher {
            Matcher::Class(pattern) => pattern.matches(what.class),
            Matcher::Title(pattern) => pattern.matches(what.title),
            Matcher::InitialClass(pattern) => pattern.matches(what.initial_class),
            Matcher::InitialTitle(pattern) => pattern.matches(what.initial_title),
            Matcher::Floating(wanted) => what.floating == *wanted,
            Matcher::Fullscreen(wanted) => what.fullscreen == *wanted,
            Matcher::Focused(wanted) => what.focused == *wanted,
        })
    }
}

/// A window, as a rule sees it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Window<'a> {
    /// `xdg_toplevel.set_app_id`, which Hyprland calls the class.
    pub class: &'a str,
    /// `xdg_toplevel.set_title`.
    pub title: &'a str,
    /// The class it opened with.
    pub initial_class: &'a str,
    /// The title it opened with.
    pub initial_title: &'a str,
    /// Whether it floats.
    pub floating: bool,
    /// Whether it is fullscreen.
    pub fullscreen: bool,
    /// Whether it has the focus.
    pub focused: bool,
}

/// One `match:` field.
fn matcher(field: &str) -> Result<Matcher, String> {
    let (name, value) = field
        .split_once(' ')
        .ok_or_else(|| format!("invalid field {field}: missing a value"))?;
    let value = value.trim();
    let pattern = || Regex::new(value).map_err(|why| format!("invalid prop {name}: {why}"));
    let yes_or_no = || match value {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(format!(
            "invalid prop {name}: `{other}` is not a yes or a no"
        )),
    };
    match name {
        "class" => pattern().map(Matcher::Class),
        "title" => pattern().map(Matcher::Title),
        "initial_class" => pattern().map(Matcher::InitialClass),
        "initial_title" => pattern().map(Matcher::InitialTitle),
        "float" => yes_or_no().map(Matcher::Floating),
        "fullscreen" => yes_or_no().map(Matcher::Fullscreen),
        "focus" => yes_or_no().map(Matcher::Focused),
        other => Err(format!("invalid prop {other}")),
    }
}

/// One effect field.
fn effect(field: &str) -> Result<Effect, String> {
    let (name, value) = match field.split_once(' ') {
        Some((name, value)) => (name, value.trim()),
        None => (field, ""),
    };
    let two = || -> Result<(Length, Length), String> {
        let (first, second) = value
            .split_once(char::is_whitespace)
            .ok_or_else(|| format!("invalid field {name}: it takes two lengths"))?;
        Ok((Length::parse(first)?, Length::parse(second.trim())?))
    };
    let number = |what: &str| -> Result<i64, String> {
        value
            .parse()
            .map_err(|_| format!("invalid field {name}: `{value}` is not {what}"))
    };
    match name {
        "float" => Ok(Effect::Float),
        "tile" => Ok(Effect::Tile),
        "center" => Ok(Effect::Center),
        "fullscreen" => Ok(Effect::Fullscreen),
        "maximize" => Ok(Effect::Maximize),
        "no_focus" => Ok(Effect::NoFocus),
        "no_blur" => Ok(Effect::Without(Decoration::Blur)),
        "no_shadow" => Ok(Effect::Without(Decoration::Shadow)),
        "no_dim" => Ok(Effect::Without(Decoration::Dim)),
        "size" => two().map(|(width, height)| Effect::Size(width, height)),
        "move" => {
            if value == "center" {
                return Ok(Effect::Center);
            }
            two().map(|(x, y)| Effect::Move(x, y))
        }
        "workspace" => {
            let (target, silent) = match value.strip_suffix("silent") {
                Some(head) => (head.trim(), true),
                None => (value, false),
            };
            if target.is_empty() {
                return Err("invalid field workspace: it takes a workspace".to_owned());
            }
            Ok(Effect::Workspace {
                target: target.to_owned(),
                silent,
            })
        }
        "opacity" => {
            // Hyprland takes one number, or two for the focused and
            // unfocused states; the first is what this carries.
            let first = value.split_whitespace().next().unwrap_or("");
            let opacity: f32 = first
                .parse()
                .map_err(|_| format!("invalid field opacity: `{value}` is not a number"))?;
            if !(0.0..=1.0).contains(&opacity) {
                return Err(format!("invalid field opacity: {opacity} is not a share"));
            }
            Ok(Effect::Opacity(opacity))
        }
        "rounding" => number("a number of pixels").map(Effect::Rounding),
        "border_size" => number("a number of pixels").map(Effect::BorderSize),
        other => Err(format!("invalid field type {other}")),
    }
}
