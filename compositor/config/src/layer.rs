//! `layerrule = <rule>, <namespace>`: what a bar, a wallpaper or a
//! notification is drawn with.
//!
//! The `zwlr_layer_shell_v1` half of `windowrule`. A layer surface has no
//! title and no application id -- it has a *namespace*, which is what it
//! passed to `get_layer_surface`, and that is the only thing a rule can
//! match on. `waybar` calls itself `waybar`, `swaync` calls itself
//! `swaync-control-center`, and a person writes
//!
//! ```text
//! layerrule = blur, waybar
//! layerrule = ignorealpha 0.5, waybar
//! layerrule = order 10, notifications
//! ```
//!
//! Hyprland matches the namespace as a regular expression, as it does a
//! window's class, so `^(waybar)$` and `waybar` both work and mean
//! different things.
//!
//! # What is drawn and what is only recorded
//!
//! `blur` is drawn: `compositor/render` blurs what is behind a translucent
//! layer surface, which is what makes a bar look like Hyprland's.
//! `abovelock` is drawn: the surface goes over the session lock, which is
//! the whole reason an on-screen keyboard can be used on a lock screen.
//! `order` is obeyed: it decides where a surface goes among its own
//! layer's.
//!
//! The rest are read, kept and not acted on, and each for a reason.
//! `noanim` has nothing to turn off, because a layer surface is not
//! animated here. `dimaround`, `xray` and `blurpopups` each need a second
//! render pass -- the first two read what is *behind* the frame being
//! drawn, and this renderer draws one pass over one canvas.
//! `noscreenshare` needs the same: a screenshot here is the screen's own
//! buffer, and leaving one surface out of it means drawing the frame
//! again without it. They are parsed rather than refused so that a
//! person's configuration is not a wall of diagnostics, and recorded so
//! that the compositor can act on them when the renderer can.

use compositor_regex::Regex;

/// One `layerrule` line.
#[derive(Clone, Debug)]
pub struct LayerRule {
    /// The namespace it matches.
    pub namespace: Regex,
    /// What it does.
    pub effect: LayerEffect,
}

/// What a `layerrule` asks for.
#[derive(Clone, PartialEq, Debug)]
pub enum LayerEffect {
    /// `blur`: what is behind the surface is blurred, which is what makes a
    /// bar with a translucent background look like Hyprland's.
    Blur,
    /// `blurpopups`: the same for the popups it opens.
    BlurPopups,
    /// `ignorealpha <fraction>`: a pixel less opaque than this is left out
    /// of what the blur is drawn under.
    IgnoreAlpha(f32),
    /// `dimaround`: everything else is dimmed while it is up, which is what
    /// a launcher does.
    DimAround,
    /// `xray`: the blur reads the wallpaper rather than what is under it.
    Xray,
    /// `noanim`: it does not animate.
    NoAnim,
    /// `order <n>`: where it goes among the surfaces of its own layer, a
    /// higher number nearer the top.
    Order(i64),
    /// `abovelock [true]`: it is drawn over the session lock, which is what
    /// an on-screen keyboard needs to be usable on a lock screen.
    AboveLock(bool),
    /// `noscreenshare`: it is left out of a screenshot.
    NoScreenShare,
    /// `animation <style>`: which animation it uses, recorded as written.
    Animation(String),
}

impl LayerRule {
    /// Read one `layerrule` line's value: the rule, a comma, the namespace.
    ///
    /// # Errors
    ///
    /// A sentence saying what is wrong, which the caller reports as a
    /// diagnostic and carries on -- one bad line must not cost a person
    /// their whole configuration.
    pub fn parse(value: &str) -> Result<Self, String> {
        let (rule, namespace) = value
            .split_once(',')
            .ok_or_else(|| format!("invalid layerrule {value}: expected a rule and a namespace"))?;
        let namespace = Regex::new(namespace.trim())
            .map_err(|why| format!("invalid layerrule namespace: {why}"))?;
        let rule = rule.trim();
        let (word, rest) = rule.split_once(' ').unwrap_or((rule, ""));
        let rest = rest.trim();
        let effect = match word {
            "blur" => LayerEffect::Blur,
            "blurpopups" => LayerEffect::BlurPopups,
            "dimaround" => LayerEffect::DimAround,
            "xray" => LayerEffect::Xray,
            "noanim" => LayerEffect::NoAnim,
            "noscreenshare" => LayerEffect::NoScreenShare,
            "ignorealpha" => LayerEffect::IgnoreAlpha(
                rest.parse()
                    .map_err(|_| format!("invalid layerrule ignorealpha {rest}"))?,
            ),
            "order" => LayerEffect::Order(
                rest.parse()
                    .map_err(|_| format!("invalid layerrule order {rest}"))?,
            ),
            // Hyprland's `abovelock` takes an optional boolean, and the
            // bare word means true.
            "abovelock" => LayerEffect::AboveLock(!matches!(rest, "0" | "false" | "no")),
            "animation" => LayerEffect::Animation(rest.to_owned()),
            other => return Err(format!("invalid layerrule {other}")),
        };
        Ok(Self { namespace, effect })
    }

    /// Whether this rule is about a surface with that namespace.
    #[must_use]
    pub fn matches(&self, namespace: &str) -> bool {
        self.namespace.matches(namespace)
    }
}

/// What every rule that matched one surface comes to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layered {
    /// Whether what is behind it is blurred.
    pub blur: bool,
    /// The same for its popups.
    pub blur_popups: bool,
    /// How opaque a pixel has to be to count, for the blur.
    pub ignore_alpha: Option<f32>,
    /// Whether everything else is dimmed while it is up.
    pub dim_around: bool,
    /// Whether the blur reads the wallpaper rather than what is under it.
    pub xray: bool,
    /// Whether it animates.
    pub animates: bool,
    /// Where it goes among its own layer's surfaces.
    pub order: i64,
    /// Whether it is drawn over the session lock.
    pub above_lock: bool,
    /// Whether a screenshot leaves it out.
    pub no_screen_share: bool,
    /// Which animation it uses.
    pub animation: String,
}

impl Layered {
    /// What `rules` say about a surface called `namespace`.
    ///
    /// Later lines win, as they do everywhere else in the configuration.
    #[must_use]
    pub fn of(rules: &[LayerRule], namespace: &str) -> Self {
        let mut out = Self {
            animates: true,
            ..Self::default()
        };
        for rule in rules.iter().filter(|rule| rule.matches(namespace)) {
            match &rule.effect {
                LayerEffect::Blur => out.blur = true,
                LayerEffect::BlurPopups => out.blur_popups = true,
                LayerEffect::IgnoreAlpha(fraction) => out.ignore_alpha = Some(*fraction),
                LayerEffect::DimAround => out.dim_around = true,
                LayerEffect::Xray => out.xray = true,
                LayerEffect::NoAnim => out.animates = false,
                LayerEffect::Order(order) => out.order = *order,
                LayerEffect::AboveLock(above) => out.above_lock = *above,
                LayerEffect::NoScreenShare => out.no_screen_share = true,
                LayerEffect::Animation(style) => out.animation.clone_from(style),
            }
        }
        out
    }
}
