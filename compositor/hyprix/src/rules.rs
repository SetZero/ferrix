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
//! `float`, `tile`, `size`, `move`, `center`, `workspace` (with `silent`),
//! `fullscreen`, `maximize` and `no_focus`. The rest of what a rule can say
//! -- `opacity`, `rounding`, `border_size`, `no_blur`, `no_shadow`,
//! `no_dim` -- is about how one window is *drawn*, and this renderer draws
//! every window with one style; a rule that asks for one of those is
//! reported as not applied rather than quietly ignored.

use compositor_config::{Config, Effect, Window, WindowRule};
use compositor_layout::{Rect, State, WindowId};

/// Every `windowrule` line, read.
#[derive(Debug, Default)]
pub struct Rules {
    rules: Vec<WindowRule>,
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
        Self { rules }
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
        &self,
        window: WindowId,
        what: &Window<'_>,
        state: &mut State,
        report: &mut dyn FnMut(&str),
    ) -> bool {
        let effects: Vec<&Effect> = self
            .rules
            .iter()
            .filter(|rule| rule.matches(what))
            .flat_map(|rule| rule.effects.iter())
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
        let mut changed = false;
        for effect in effects {
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
                // How one window is drawn, which this renderer cannot do
                // yet: said rather than ignored.
                other => report(&format!(
                    "hyprix: windowrule {other:?} is not applied yet; it is about how one window \
                     is drawn and every window is drawn with one style"
                )),
            }
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
