//! The `windowrule` lines, and what they do to a window when it opens.
//!
//! `compositor/config` reads a rule into what it matches and what it does;
//! this is where the doing happens. Hyprland applies a window's rules when
//! it maps, and so does this: the window is already in the layout by then,
//! so a rule that floats it, moves it, sizes it or sends it somewhere is
//! done through the same calls a dispatcher makes.
//!
//! # What is carried out
//!
//! The layout's: `float`, `tile`, `size`, `move`, `center`, `workspace`
//! (with `silent`), `fullscreen`, `maximize` and `no_focus`, through the
//! same calls a dispatcher makes.
//!
//! And the renderer's: `opacity`, `rounding`, `border_size`, `no_blur`,
//! `no_shadow` and `no_dim`, which are what *one* window is drawn with. They
//! are kept here by window, and the frame reads them: a window with no rule
//! is drawn as every window is.

use std::collections::BTreeMap;

use compositor_config::{Config, Decoration, Effect, Window, WindowRule};
use compositor_layout::{Rect, State, WindowId};
use compositor_render::WindowStyle;

/// Every `windowrule` line, read.
#[derive(Debug, Default)]
pub struct Rules {
    rules: Vec<WindowRule>,
    /// What a rule gave a window to be drawn with, by window.
    styles: BTreeMap<WindowId, WindowStyle>,
    /// `suppress_event`: what each window asks for and does not get.
    suppressed: BTreeMap<WindowId, Vec<String>>,
    /// Every effect a rule asked for that this compositor does not carry
    /// out, so that it can be reported rather than silently dropped.
    unhandled: Vec<String>,
}

impl Rules {
    /// Read the rules in `config`, saying what could not be read.
    pub fn new(config: &Config, report: &mut dyn FnMut(&str)) -> Self {
        let mut rules = Vec::new();
        for raw in &config.window_rules {
            match WindowRule::parse(&raw.value) {
                Ok(rule) => rules.push(rule),
                Err(why) => report(&format!("hyprix: windowrule = {}: {why}", raw.value)),
            }
        }
        Self {
            rules,
            styles: BTreeMap::new(),
            suppressed: BTreeMap::new(),
            unhandled: Vec::new(),
        }
    }

    /// Whether a rule told this window it does not get `event`.
    ///
    /// `suppress_event maximize` is the one a real configuration writes: a
    /// tiling compositor decides that, and an application which asks on
    /// startup would otherwise fight the layout.
    #[must_use]
    pub fn suppresses(&self, window: WindowId, event: &str) -> bool {
        self.suppressed
            .get(&window)
            .is_some_and(|events| events.iter().any(|held| held == event))
    }

    /// Every effect a rule asked for that is not carried out here.
    #[must_use]
    pub fn unhandled(&self) -> &[String] {
        &self.unhandled
    }

    /// What each window is drawn with, for the frame.
    #[must_use]
    pub const fn styles(&self) -> &BTreeMap<WindowId, WindowStyle> {
        &self.styles
    }

    /// `setprop`: change one of a window's drawn properties by hand, the
    /// way a `windowrule` would have.
    ///
    /// Gives whether the name was one of the properties. Hyprland's
    /// `setprop` reaches the same field a rule sets, so a property set here
    /// outlives the rules and is forgotten with the window.
    pub fn set_property(&mut self, window: WindowId, name: &str, value: &str) -> bool {
        // Hyprland's `unset` puts a property back to what the style says.
        let unset = value.eq_ignore_ascii_case("unset");
        let yes = matches!(value, "1" | "true" | "yes" | "on") || value.is_empty();
        let number = value.parse::<i64>().ok();
        let fraction = value.parse::<f32>().ok();
        // A value that is not one leaves the window as it was, rather than
        // making a property up.
        let known = match name {
            "alpha" | "alphafullscreen" | "opacity" => unset || fraction.is_some(),
            "rounding" | "bordersize" => unset || number.is_some(),
            "noblur" | "noshadow" | "nodim" => true,
            _ => false,
        };
        if !known {
            return false;
        }
        let style = self.styles.entry(window).or_default();
        match name {
            "alpha" | "alphafullscreen" | "opacity" => style.opacity = fraction.filter(|_| !unset),
            "rounding" => style.rounding = number.filter(|_| !unset),
            "bordersize" => style.border = number.filter(|_| !unset),
            "noblur" => style.blur = unset || !yes,
            "noshadow" => style.shadow = unset || !yes,
            "nodim" => style.dim = unset || !yes,
            _ => return false,
        }
        true
    }

    /// Forget a window that has gone.
    pub fn window_gone(&mut self, window: WindowId) {
        let _ = self.styles.remove(&window);
        let _ = self.suppressed.remove(&window);
    }

    /// How many rules there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Apply every rule that matches a window that has just mapped.
    ///
    /// Gives whether anything changed, which is what the loop needs to know
    /// to draw again.
    pub fn apply(
        &mut self,
        window: WindowId,
        what: &Window<'_>,
        state: &mut State,
        report: &mut dyn FnMut(&str),
    ) -> bool {
        let effects: Vec<Effect> = self
            .rules
            .iter()
            .filter(|rule| rule.matches(what))
            .flat_map(|rule| rule.effects.iter())
            .cloned()
            .collect();
        if effects.is_empty() {
            return false;
        }
        // The monitor the window is on, whose size a percentage is of.
        let area = state
            .workspace_of(window)
            .and_then(|workspace| state.workspace_monitor(workspace))
            .and_then(|monitor| state.monitors().find(|found| found.id == monitor))
            .map(|monitor| monitor.rect)
            .unwrap_or_default();
        // A window that floats takes a rectangle, which the `size`, `move`
        // and `center` effects fill in; the effects are read first so that
        // the order they are written in does not matter.
        let mut floating = None;
        let mut size = None;
        let mut position = None;
        let mut centred = false;
        let mut pin = false;
        let mut pseudo = false;
        let mut changed = false;
        // What the window is drawn with, which starts as what every window
        // is drawn with.
        let mut style = WindowStyle::default();
        let mut styled = false;
        for effect in &effects {
            match effect {
                Effect::Float => floating = Some(true),
                Effect::Tile => floating = Some(false),
                Effect::Size(width, height) => {
                    size = Some((width.against(area.width), height.against(area.height)));
                }
                Effect::Move(x, y) => {
                    position = Some((x.against(area.width), y.against(area.height)));
                }
                Effect::Center => centred = true,
                Effect::Opacity(opacity) => {
                    style.opacity = Some(*opacity);
                    styled = true;
                }
                Effect::Rounding(rounding) => {
                    style.rounding = Some(*rounding);
                    styled = true;
                }
                Effect::BorderSize(width) => {
                    style.border = Some(*width);
                    styled = true;
                }
                Effect::Without(decoration) => {
                    match decoration {
                        Decoration::Blur => style.blur = false,
                        Decoration::Shadow => style.shadow = false,
                        Decoration::Dim => style.dim = false,
                    }
                    styled = true;
                }
                Effect::Workspace { target, silent } => {
                    let name = if *silent {
                        "movetoworkspacesilent"
                    } else {
                        "movetoworkspace"
                    };
                    match state.dispatch_str(name, target) {
                        Ok(made) => changed |= !made.is_empty(),
                        Err(error) => {
                            report(&format!("hyprix: windowrule workspace {target}: {error:?}"));
                        }
                    }
                }
                Effect::Fullscreen | Effect::Maximize => {
                    let mode = if matches!(effect, Effect::Maximize) {
                        "1"
                    } else {
                        "0"
                    };
                    if let Ok(made) = state.dispatch_str("fullscreen", mode) {
                        changed |= !made.is_empty();
                    }
                }
                Effect::NoFocus => {
                    // The window took the focus when it opened; give it back
                    // to whatever had it, which is what `no_focus` means.
                    let previous = state
                        .windows_in_focus_order()
                        .into_iter()
                        .find(|other| *other != window);
                    if let Some(previous) = previous {
                        let _ = state.focus_window(previous);
                        changed = true;
                    }
                }
                // Both are acted on after the floating is settled: `pin`
                // only takes on a floating window, and a line that says
                // `float, pin` means the two in that order however they
                // are written.
                Effect::Pin => pin = true,
                Effect::Pseudo => pseudo = true,
                Effect::Tag(name) => {
                    // `+` so that a rule applied twice does not turn the
                    // tag off again, which a bare name would.
                    if let Ok(made) = state.dispatch_str("tagwindow", &format!("+{name}")) {
                        changed |= !made.is_empty();
                    }
                }
                Effect::Suppress(events) => {
                    self.suppressed
                        .entry(window)
                        .or_default()
                        .extend(events.iter().cloned());
                }
                // Read, kept and not carried out; `unhandled` says which,
                // so `hyprctl` can report what a rule asked for.
                Effect::Unhandled(name) => {
                    self.unhandled.push(name.clone());
                }
            }
        }
        if styled {
            let _ = self.styles.insert(window, style);
            changed = true;
        }

        // Floating last, so that a `size` written after a `float` is still
        // the size the window floats at.
        if floating == Some(true) || size.is_some() || position.is_some() || centred {
            let rect = rectangle(state, window, area, size, position, centred);
            if state.float_window(window, rect).is_ok() {
                changed = true;
            }
        } else if floating == Some(false)
            && state.is_floating(window)
            && let Ok(made) = state.dispatch_str("togglefloating", "")
        {
            changed |= !made.is_empty();
        }
        // `pin` and `pseudo` are the focused window's dispatchers, and the
        // window a rule is about need not be the focused one -- a rule
        // fires when a window maps, and a window may map without taking
        // the focus. So the focus goes there and comes back, which is what
        // the modal path does for the same reason.
        if (pin && !state.is_pinned(window)) || (pseudo && !state.is_pseudo(window)) {
            let was = state.focused_window();
            if state.focus_window(window).is_ok() {
                if pin && !state.is_pinned(window) {
                    changed |= state
                        .dispatch_str("pin", "")
                        .is_ok_and(|made| !made.is_empty());
                }
                if pseudo && !state.is_pseudo(window) {
                    changed |= state
                        .dispatch_str("pseudo", "")
                        .is_ok_and(|made| !made.is_empty());
                }
                if let Some(was) = was {
                    let _ = state.focus_window(was);
                }
            }
        }
        changed
    }
}

/// Where a floating window a rule made goes.
///
/// The size it asked for, or the one it has; the place it asked for, the
/// middle of the monitor when it asked to be centred, or where it is.
fn rectangle(
    state: &State,
    window: WindowId,
    area: Rect,
    size: Option<(i64, i64)>,
    position: Option<(i64, i64)>,
    centred: bool,
) -> Rect {
    let current = state
        .layout()
        .iter()
        .flat_map(|output| output.windows.iter())
        .find(|placed| placed.window == window)
        .map(|placed| placed.rect)
        .unwrap_or(area);
    let (width, height) = size.unwrap_or((current.width, current.height));
    let (x, y) = if centred {
        (
            area.x.saturating_add(area.width.saturating_sub(width) / 2),
            area.y
                .saturating_add(area.height.saturating_sub(height) / 2),
        )
    } else {
        position.map_or((current.x, current.y), |(x, y)| {
            (area.x.saturating_add(x), area.y.saturating_add(y))
        })
    };
    Rect::new(x, y, width.max(1), height.max(1))
}

#[cfg(test)]
mod tests {
    use compositor_config::{Config, NoSources, parse};
    use compositor_layout::{Monitor, MonitorId, Rect, State, WindowId};

    use super::Rules;

    fn config(text: &str) -> Config {
        let parsed = parse("t.conf", text, &mut NoSources);
        assert_eq!(parsed.diagnostics, [], "the test configuration parses");
        parsed.config
    }

    /// One 1920x1080 monitor with one window on it.
    fn state(config: &Config) -> State {
        let mut state = State::from_config(config);
        let _changes = state
            .add_monitor(Monitor {
                scale: 1.0,
                name: "Virtual-1".to_owned(),
                id: MonitorId(1),
                rect: Rect::new(0, 0, 1920, 1080),
                reserved: compositor_config::Gaps::all(0),
                description: String::new(),
                made: <(String, String, String)>::default(),
            })
            .expect("a monitor");
        let _opened = state.open_window(WindowId(1)).expect("a window");
        state
    }

    fn what<'a>(class: &'a str) -> compositor_config::Window<'a> {
        compositor_config::Window {
            class,
            initial_class: class,
            ..compositor_config::Window::default()
        }
    }

    /// `suppress_event maximize` is recorded against the window, which is
    /// how the compositor knows to ignore what the client asks for.
    ///
    /// `nazuna`'s first `windowrule` line is exactly this, against every
    /// class, and it is only worth writing because the compositor obeys
    /// `set_maximized` by default.
    #[test]
    fn suppress_event_is_recorded_against_the_window() {
        let config = config("windowrule = suppress_event maximize, match:class .*\n");
        let mut state = state(&config);
        let mut rules = Rules::new(&config, &mut |_| {});
        assert_eq!(rules.len(), 1);
        let _changed = rules.apply(WindowId(1), &what("foot"), &mut state, &mut |_| {});
        assert!(rules.suppresses(WindowId(1), "maximize"));
        assert!(!rules.suppresses(WindowId(1), "fullscreen"));
        // And it is forgotten with the window, rather than being handed to
        // whatever window id comes next.
        rules.window_gone(WindowId(1));
        assert!(!rules.suppresses(WindowId(1), "maximize"));
    }

    /// `pin` and `tag` reach the layout, and an effect this compositor does
    /// not carry out is kept by name rather than losing the rest of the
    /// line.
    #[test]
    fn pin_and_tag_are_carried_out_and_the_rest_is_kept() {
        // `pin` takes only on a floating window, here as in Hyprland,
        // so the rule says both -- and in the order the effects are
        // written, which the line reverses on purpose.
        let config = config(
            "windowrule = pin, float, match:class ^(foot)$\n\
             windowrule = tag music, no_shortcuts_inhibit true, match:class ^(foot)$\n",
        );
        let mut state = state(&config);
        let mut rules = Rules::new(&config, &mut |_| {});
        assert_eq!(rules.len(), 2);
        let _changed = rules.apply(WindowId(1), &what("foot"), &mut state, &mut |_| {});
        assert!(state.is_pinned(WindowId(1)), "the rule pinned it");
        assert_eq!(state.tags_of(WindowId(1)), ["music"]);
        assert_eq!(rules.unhandled(), ["no_shortcuts_inhibit"]);
    }
}
