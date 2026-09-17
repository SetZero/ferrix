//! A `monitor =` line, read into what the compositor does with it.
//!
//! Hyprland's form is `monitor = name, resolution, position, scale`, with
//! further keyword pairs after it (`ConfigManager::handleMonitor`). An empty
//! name is the rule for every monitor no other rule names, which is what
//! `monitor = , preferred, auto, 1` in every example configuration is.
//!
//! What is read here: the name, `disable`, the resolution as `preferred` or
//! `WIDTHxHEIGHT` with an optional `@REFRESH`, the position as `auto` or
//! `XxY`, and the scale as `auto` or a number. `transform`, `mirror`,
//! `bitdepth`, `vrr` and the rest are not done, and a line that carries one
//! says so rather than being read as if it were not there.

/// What a `monitor =` line asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorRule {
    /// The monitor it names, or empty for every monitor no other rule
    /// names.
    pub name: String,
    /// `monitor = name, disable`: the monitor is left dark.
    pub disabled: bool,
    /// The mode it asks for.
    pub mode: Mode,
    /// Where the monitor goes.
    pub position: Position,
    /// How many buffer pixels a logical one is.
    pub scale: Scale,
}

/// The resolution field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    /// `preferred`, `highres`, `highrr`: whatever the connector says is
    /// best, which for virtio-gpu is its only mode.
    Preferred,
    /// `WIDTHxHEIGHT[@REFRESH]`.
    Fixed {
        /// In pixels.
        width: u32,
        /// In pixels.
        height: u32,
        /// In hertz, if the line gave one.
        refresh: Option<f64>,
    },
}

/// The position field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// `auto`: to the right of the monitors already placed.
    Auto,
    /// `XxY`: where the line says, in logical pixels.
    At(i64, i64),
}

/// The scale field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scale {
    /// `auto`: the compositor chooses, which here is 1.
    Auto,
    /// A number.
    Fixed(f64),
}

impl MonitorRule {
    /// Read a `monitor =` line's value.
    ///
    /// # Errors
    ///
    /// Hyprland's own wording for a field it cannot read, and a sentence for
    /// the fields this compositor does not do yet.
    pub fn parse(value: &str) -> Result<Self, String> {
        let mut fields = value.split(',').map(str::trim);
        let name = fields.next().unwrap_or("").to_owned();
        let resolution = fields.next().unwrap_or("preferred");
        if resolution.eq_ignore_ascii_case("disable") || resolution.eq_ignore_ascii_case("disabled")
        {
            return Ok(Self {
                name,
                disabled: true,
                mode: Mode::Preferred,
                position: Position::Auto,
                scale: Scale::Auto,
            });
        }
        let mode = parse_mode(resolution)?;
        let position = parse_position(fields.next().unwrap_or("auto"))?;
        let scale = parse_scale(fields.next().unwrap_or("auto"))?;
        let rest: Vec<&str> = fields.filter(|field| !field.is_empty()).collect();
        if !rest.is_empty() {
            return Err(format!(
                "monitor: {} is not done yet; the name, resolution, position and scale are",
                rest.join(", ")
            ));
        }
        Ok(Self {
            name,
            disabled: false,
            mode,
            position,
            scale,
        })
    }

    /// Whether this rule is the one for a monitor called `name` that
    /// describes itself as `description`.
    ///
    /// Three forms, which are `CMonitor::matchesStaticSelector`'s: a rule
    /// with no name at all is every monitor's, a rule beginning `desc:`
    /// matches the *start* of the description, and any other name is the
    /// connector's, exactly.
    ///
    /// The prefix is Hyprland's and it matters: a description is the make,
    /// the model and the serial, and `desc:Dell Inc. DELL P2418D` names
    /// every Dell P2418D on the machine while the serial after it names
    /// one.
    #[must_use]
    pub fn matches(&self, name: &str, description: &str) -> bool {
        if self.name.is_empty() {
            return true;
        }
        match self.name.strip_prefix("desc:") {
            Some(wanted) => {
                let wanted = wanted.trim();
                !wanted.is_empty() && description.starts_with(wanted)
            }
            None => self.name == name,
        }
    }

    /// The scale as a number: `auto` is 1, and a scale at or below zero is
    /// not one, so it is 1 as well.
    #[must_use]
    pub fn scale_factor(&self) -> f64 {
        match self.scale {
            Scale::Auto => 1.0,
            Scale::Fixed(scale) if scale > 0.0 => scale,
            Scale::Fixed(_) => 1.0,
        }
    }
}

/// The resolution field, which is a size or a word.
fn parse_mode(text: &str) -> Result<Mode, String> {
    // Hyprland's words for "let the connector decide". `highres` and
    // `highrr` choose between modes, and virtio-gpu has one, so all three
    // mean the same thing here.
    if matches!(
        text.to_ascii_lowercase().as_str(),
        "" | "preferred" | "highres" | "highrr" | "maxwidth"
    ) {
        return Ok(Mode::Preferred);
    }
    let (size, refresh) = match text.split_once('@') {
        Some((size, rate)) => {
            let rate: f64 = rate
                .trim()
                .parse()
                .map_err(|_| format!("Invalid refresh rate {rate}"))?;
            (size, Some(rate))
        }
        None => (text, None),
    };
    let (width, height) = size
        .split_once('x')
        .ok_or_else(|| format!("Invalid resolution {text}"))?;
    let width: u32 = width
        .trim()
        .parse()
        .map_err(|_| format!("Invalid resolution {text}"))?;
    let height: u32 = height
        .trim()
        .parse()
        .map_err(|_| format!("Invalid resolution {text}"))?;
    Ok(Mode::Fixed {
        width,
        height,
        refresh,
    })
}

/// The position field.
fn parse_position(text: &str) -> Result<Position, String> {
    let lower = text.to_ascii_lowercase();
    if lower.is_empty() || lower == "auto" {
        return Ok(Position::Auto);
    }
    if lower.starts_with("auto-") {
        return Err(format!(
            "monitor: {text} is not done yet; `auto` puts a monitor to the right of the last"
        ));
    }
    let (x, y) = text
        .split_once('x')
        .ok_or_else(|| format!("Invalid position {text}"))?;
    let x: i64 = x
        .trim()
        .parse()
        .map_err(|_| format!("Invalid position {text}"))?;
    let y: i64 = y
        .trim()
        .parse()
        .map_err(|_| format!("Invalid position {text}"))?;
    Ok(Position::At(x, y))
}

/// The scale field.
fn parse_scale(text: &str) -> Result<Scale, String> {
    let lower = text.to_ascii_lowercase();
    if lower.is_empty() || lower == "auto" {
        return Ok(Scale::Auto);
    }
    let scale: f64 = text
        .trim()
        .parse()
        .map_err(|_| format!("Invalid scale {text}"))?;
    // A scale that is zero, negative or not a number at all is not one; a
    // comparison that says so without a negation reads the same and keeps
    // NaN on the refusing side.
    if scale <= 0.0 || scale.is_nan() {
        return Err(format!("Invalid scale {text}"));
    }
    Ok(Scale::Fixed(scale))
}
