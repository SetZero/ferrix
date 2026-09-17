//! Hyprland's animations: its curves, its tree of names, and the values they
//! move.
//!
//! What makes a Hyprland window slide rather than jump is three things: a
//! cubic bezier named in the configuration ([`Bezier`]), a tree of animation
//! names each of which inherits from its parent ([`Tree`]), and a value that
//! knows where it began, where it is going and when it started ([`Moving`]).
//!
//! Nothing here holds a window, a surface or a clock: a [`Moving`] is asked
//! what it is at a time the caller gives it. So every curve, every
//! inheritance rule and every interpolation is host-tested against the
//! numbers Hyprland's own source produces, and the compositor above only has
//! to call it once a frame.
//!
//! # What is read from the configuration
//!
//! `compositor/config` keeps `bezier` and `animation` lines as they were
//! written, because what they mean is this crate's business. [`read`] turns
//! them into the curves and the tree, reporting each line it could not use
//! the way Hyprland reports one -- as a diagnostic, with the rest of the file
//! still applied.

mod bezier;
mod moving;
mod tree;

pub use bezier::{BAKED, Bezier};
pub use moving::Moving;
pub use tree::{NODES, Settings, Tree};

use std::collections::BTreeMap;

use compositor_config::Raw;

/// The curves a configuration named, with Hyprland's two built in.
#[derive(Clone, Debug)]
pub struct Curves {
    curves: BTreeMap<String, Bezier>,
}

impl Default for Curves {
    fn default() -> Self {
        Self::new()
    }
}

impl Curves {
    /// Just the built-in curves: `default` and `linear`, which Hyprland's
    /// animation manager adds and `removeAllBeziers` puts back.
    #[must_use]
    pub fn new() -> Self {
        let mut curves = BTreeMap::new();
        let _ = curves.insert("default".to_owned(), Bezier::default_curve());
        let _ = curves.insert("linear".to_owned(), Bezier::linear());
        Self { curves }
    }

    /// Add one, as a `bezier` line does.
    pub fn add(&mut self, name: &str, first: (f32, f32), second: (f32, f32)) {
        let _ = self
            .curves
            .insert(name.to_owned(), Bezier::new(first, second));
    }

    /// Every curve by name, in name order, for `hyprctl animations`.
    pub fn named(&self) -> impl Iterator<Item = (&str, &Bezier)> {
        self.curves
            .iter()
            .map(|(name, curve)| (name.as_str(), curve))
    }

    /// The curve `name` names, or `default` for one that was never declared.
    ///
    /// That fallback is `CAnimationManager::getBezier`'s: a name nothing
    /// declared is the default curve rather than no animation at all.
    #[must_use]
    pub fn get(&self, name: &str) -> &Bezier {
        self.curves
            .get(name)
            .or_else(|| self.curves.get("default"))
            .unwrap_or(&FALLBACK)
    }

    /// Whether `name` was declared.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.curves.contains_key(name)
    }

    /// How many curves there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.curves.len()
    }

    /// Whether there are none, which cannot happen: two are built in.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.curves.is_empty()
    }
}

/// The curve a `Curves` with no `default` would give, which cannot happen.
static FALLBACK: std::sync::LazyLock<Bezier> = std::sync::LazyLock::new(Bezier::default_curve);

/// What could not be read, in Hyprland's own words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// The line as it was written.
    pub line: String,
    /// Why it was not used.
    pub reason: String,
}

/// Read a configuration's `bezier` and `animation` lines.
///
/// In order, because a `bezier` has to be declared before the `animation`
/// that names it -- which is Hyprland's rule too, and the reason its own
/// parser keeps the lines in order.
#[must_use]
pub fn read(lines: &[Raw]) -> (Curves, Tree, Vec<Diagnostic>) {
    let mut curves = Curves::new();
    let mut tree = Tree::new();
    let mut said = Vec::new();
    for raw in lines {
        let reason = match raw.keyword.as_str() {
            "bezier" => bezier_line(&raw.value, &mut curves),
            "animation" => animation_line(&raw.value, &curves, &mut tree),
            _ => None,
        };
        if let Some(reason) = reason {
            said.push(Diagnostic {
                line: format!("{} = {}", raw.keyword, raw.value),
                reason,
            });
        }
    }
    (curves, tree, said)
}

/// `bezier = NAME, X0, Y0, X1, Y1`.
fn bezier_line(value: &str, curves: &mut Curves) -> Option<String> {
    let fields: Vec<&str> = value.split(',').map(str::trim).collect();
    let [name, x0, y0, x1, y1] = fields.as_slice() else {
        return Some("a bezier is a name and four numbers".to_owned());
    };
    let number = |text: &str| text.parse::<f32>().ok().filter(|value| value.is_finite());
    let (Some(x0), Some(y0), Some(x1), Some(y1)) = (number(x0), number(y0), number(x1), number(y1))
    else {
        return Some("a bezier's points are numbers".to_owned());
    };
    if name.is_empty() {
        return Some("a bezier needs a name".to_owned());
    }
    curves.add(name, (x0, y0), (x1, y1));
    None
}

/// `animation = NAME, ONOFF, SPEED, CURVE[, STYLE]`.
///
/// The refusals are Hyprland's `handleAnimation`, word for word where it has
/// words: `no such animation`, `invalid animation on/off state`, `invalid
/// speed`, `no such bezier`. A line that is off takes speed 1 and the
/// default curve whatever else it said, as that function does.
fn animation_line(value: &str, curves: &Curves, tree: &mut Tree) -> Option<String> {
    let fields: Vec<&str> = value.split(',').map(str::trim).collect();
    let name = fields.first().copied().unwrap_or("");
    if !Tree::has(name) {
        return Some("no such animation".to_owned());
    }
    let on = match fields.get(1).copied().unwrap_or("") {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" | "" => false,
        _ => return Some("invalid animation on/off state".to_owned()),
    };
    if !on {
        let _ = tree.set(
            name,
            Settings {
                enabled: false,
                speed: 1.0,
                curve: "default".to_owned(),
                style: String::new(),
            },
        );
        return None;
    }
    let Some(speed) = fields
        .get(2)
        .and_then(|text| text.parse::<f32>().ok())
        .filter(|speed| speed.is_finite() && *speed > 0.0)
    else {
        return Some("invalid speed".to_owned());
    };
    let curve = fields.get(3).copied().unwrap_or("");
    if !curves.has(curve) {
        return Some("no such bezier".to_owned());
    }
    let _ = tree.set(
        name,
        Settings {
            enabled: true,
            speed,
            curve: curve.to_owned(),
            style: fields.get(4).copied().unwrap_or("").to_owned(),
        },
    );
    None
}

#[cfg(test)]
mod tests;
