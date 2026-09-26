//! The master layout: Hyprland's `MasterAlgorithm.cpp`.
//!
//! A workspace's tiled windows are a list, and some of them are masters.
//! One master alone fills the workspace. With a stack, the masters take
//! `mfact` of the workspace on the side `master:orientation` names and
//! share that column between them, and the stack windows share the rest
//! equally, in list order, top to bottom or left to right.
//!
//! A new window goes to the end of the list, to the front with
//! `master:new_on_top`, or beside the focused one with
//! `master:new_on_active`, and becomes a master when `master:new_status`
//! says so or when it is the first. The old master keeps its place in the
//! list, so it joins the stack there, as in Hyprland. When the last master
//! closes, the first window of the list takes over.
//!
//! `layoutmsg addmaster` and `removemaster` move a window between the two
//! columns, which is how a person gets two masters side by side; `center`
//! puts the masters in the middle with the stack in two columns beside
//! them.
//!
//! Where it departs from Hyprland: the master's share is one number for
//! the whole workspace rather than one per node, so a stack window cannot
//! yet be resized on its own.

use crate::WindowId;
use crate::geometry::Area;
use crate::settings::{NewOnActive, NewStatus, Orientation, Settings};

/// One workspace's master layout.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Master {
    /// Every tiled window, the masters among them, in list order.
    order: Vec<WindowId>,
    /// Which of them are masters, in list order. Empty only when the list
    /// is.
    ///
    /// Hyprland marks each node `isMaster` and keeps them in one list;
    /// this is that mark, kept apart so the list order is the list order.
    masters: Vec<WindowId>,
}

impl Master {
    /// Add `new`, when `focused` is the workspace's focused tiled window.
    pub(crate) fn insert(&mut self, new: WindowId, focused: Option<WindowId>, settings: &Settings) {
        let becomes_master = self.masters.is_empty()
            || match settings.master.new_status {
                NewStatus::Master => true,
                NewStatus::Slave => false,
                NewStatus::Inherit => focused.is_some_and(|id| self.masters.contains(&id)),
            };
        // `master:new_on_active`: beside the focused window rather than at
        // one end of the stack. A window that is master with only itself
        // for a master is skipped, as `addTarget`'s own test does: putting
        // the new window beside it would make it the master instead.
        let beside = match settings.master.new_on_active {
            NewOnActive::End => None,
            NewOnActive::Before | NewOnActive::After if becomes_master => None,
            NewOnActive::Before | NewOnActive::After => focused
                .filter(|id| !self.masters.contains(id))
                .and_then(|id| self.order.iter().position(|held| *held == id)),
        };
        match beside {
            Some(at) if settings.master.new_on_active == NewOnActive::Before => {
                self.order.insert(at, new);
            }
            Some(at) => self.order.insert(at.saturating_add(1), new),
            None if settings.master.new_on_top => self.order.insert(0, new),
            None => self.order.push(new),
        }
        if becomes_master {
            self.masters = vec![new];
        }
        self.order_masters();
    }

    /// Put `masters` back into list order, which is the order they are laid
    /// out in.
    fn order_masters(&mut self) {
        self.masters = self
            .order
            .iter()
            .copied()
            .filter(|id| self.masters.contains(id))
            .collect();
    }

    /// Remove `window`; if it was the last master, the first window left
    /// takes over.
    pub(crate) fn remove(&mut self, window: WindowId) {
        self.order.retain(|id| *id != window);
        self.masters.retain(|id| *id != window);
        if self.masters.is_empty()
            && let Some(first) = self.order.first().copied()
        {
            self.masters = vec![first];
        }
    }

    /// `layoutmsg addmaster`: `window` becomes a master, or the first
    /// window that is not one does when it already is.
    ///
    /// Gives whether anything changed. `small` is
    /// `master:allow_small_split`: without it Hyprland refuses to make a
    /// master when that would leave fewer than two windows in the stack,
    /// because a stack of one beside two masters is not what the message
    /// is for.
    pub(crate) fn add_master(&mut self, window: WindowId, small: bool) -> bool {
        if self.masters.len().saturating_add(2) > self.order.len() && !small {
            return false;
        }
        let wanted = if self.masters.contains(&window) || !self.contains(window) {
            self.stack().next()
        } else {
            Some(window)
        };
        let Some(wanted) = wanted else {
            return false;
        };
        self.masters.push(wanted);
        self.order_masters();
        true
    }

    /// `layoutmsg removemaster`: `window` stops being a master, or the last
    /// master does when `window` is not one.
    ///
    /// Gives whether anything changed. Hyprland refuses with fewer than two
    /// windows or fewer than two masters: a workspace with no master at all
    /// is not a state its layout has.
    pub(crate) fn remove_master(&mut self, window: WindowId) -> bool {
        if self.order.len() < 2 || self.masters.len() < 2 {
            return false;
        }
        let wanted = if self.masters.contains(&window) {
            Some(window)
        } else {
            self.masters.last().copied()
        };
        let Some(wanted) = wanted else {
            return false;
        };
        self.masters.retain(|id| *id != wanted);
        true
    }

    /// Which windows are masters, in list order.
    ///
    /// What `layoutmsg addmaster` and `removemaster` change, and the only
    /// way to see it from outside: two masters look like two windows in
    /// one column, which a rectangle alone cannot tell from a stack.
    pub(crate) fn masters(&self) -> &[WindowId] {
        &self.masters
    }

    /// Exchange two windows' places, or rename one to the other.
    pub(crate) fn swap(&mut self, a: WindowId, b: WindowId) {
        let exchange = |id: WindowId| {
            if id == a {
                b
            } else if id == b {
                a
            } else {
                id
            }
        };
        for id in &mut self.order {
            *id = exchange(*id);
        }
        for id in &mut self.masters {
            *id = exchange(*id);
        }
    }

    /// The first master, which is `None` only when the list is empty.
    pub(crate) fn master(&self) -> Option<WindowId> {
        self.masters.first().copied()
    }

    /// `layoutmsg swapwithmaster`: exchange `window` with the master, or,
    /// when `window` *is* the master, with the first of the stack.
    ///
    /// The two change places in the list as well, so a second call puts
    /// them back -- which is what makes the message a toggle in Hyprland.
    pub(crate) fn swap_with_master(&mut self, window: WindowId) -> bool {
        let Some(master) = self.master() else {
            return false;
        };
        let other = if window == master {
            let Some(first) = self.stack().next() else {
                return false;
            };
            first
        } else if self.contains(window) {
            window
        } else {
            return false;
        };
        self.swap(master, other);
        // `swap` renames both, and the master is now the window that was in
        // the other's place; the master stays whichever window holds the
        // master's slot, which `swap` has already seen to.
        true
    }

    /// `layoutmsg swapnext` and `swapprev`: exchange `window` with the one
    /// after or before it in list order, wrapping around.
    pub(crate) fn swap_along(&mut self, window: WindowId, back: bool) -> bool {
        let Some(at) = self.order.iter().position(|id| *id == window) else {
            return false;
        };
        if self.order.len() < 2 {
            return false;
        }
        let to = if back {
            at.checked_sub(1).unwrap_or(self.order.len() - 1)
        } else {
            (at + 1) % self.order.len()
        };
        self.order.swap(at, to);
        true
    }

    /// `layoutmsg rollnext` and `rollprev`: turn the whole list around by
    /// one, so every window moves to the next slot and the last wraps to
    /// the front. The master slot keeps its place and takes whichever
    /// window rolls into it.
    pub(crate) fn roll(&mut self, back: bool) {
        if self.order.len() < 2 {
            return;
        }
        // The master *slots* keep their places and take whichever windows
        // roll into them, which is what makes the message a carousel.
        let slots: Vec<usize> = self
            .masters
            .iter()
            .filter_map(|master| self.order.iter().position(|id| id == master))
            .collect();
        if back {
            self.order.rotate_left(1);
        } else {
            self.order.rotate_right(1);
        }
        self.masters = slots
            .into_iter()
            .filter_map(|at| self.order.get(at).copied())
            .collect();
    }

    /// Whether `window` is in the list.
    pub(crate) fn contains(&self, window: WindowId) -> bool {
        self.order.contains(&window)
    }

    /// The masters, then the stack, each in list order.
    pub(crate) fn windows(&self) -> Vec<WindowId> {
        self.masters.iter().copied().chain(self.stack()).collect()
    }

    /// The stack in order: every window that is not a master.
    fn stack(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.order
            .iter()
            .copied()
            .filter(|id| !self.masters.contains(id))
    }

    /// Each window's box, when the workspace's work area is `area`.
    pub(crate) fn slots(&self, area: Area, settings: &Settings) -> Vec<(WindowId, Area)> {
        let masters: Vec<WindowId> = self.masters.clone();
        if masters.is_empty() {
            return Vec::new();
        }
        let stack: Vec<WindowId> = self.stack().collect();
        let mfact = settings.master.mfact;
        // `center` is only centred once the stack is long enough; below
        // that it is the fallback orientation, which is what keeps one
        // window from being a narrow strip in the middle of an empty
        // screen (`CMasterAlgorithm::recalculateSpace`).
        let centred = settings.master.orientation == Orientation::Center
            && stack.len() >= settings.master.slave_count_for_center;
        let orientation = match settings.master.orientation {
            Orientation::Center if !centred => settings.master.center_fallback,
            other => other,
        };
        if stack.is_empty() {
            // `master:always_keep_position`: the master keeps its share of
            // the screen rather than filling it, so that opening a second
            // window does not move the first.
            if !centred && settings.master.always_keep_position {
                let width = area.w * mfact;
                let x = match orientation {
                    Orientation::Right => area.x + area.w - width,
                    Orientation::Center => area.x + (area.w - width) / 2.0,
                    _ => area.x,
                };
                let mut out = Vec::with_capacity(masters.len());
                share(
                    &masters,
                    Area {
                        x,
                        w: width,
                        ..area
                    },
                    true,
                    &mut out,
                );
                return out;
            }
            // Every window is a master, so they share the whole workspace
            // the way a stack would.
            let mut out = Vec::with_capacity(masters.len());
            let vertical = !matches!(orientation, Orientation::Top | Orientation::Bottom);
            share(&masters, area, vertical, &mut out);
            return out;
        }
        let mut out = Vec::with_capacity(self.order.len());
        match orientation {
            Orientation::Center => {
                // The master in the middle, and the stack in two columns
                // beside it. The columns alternate from the fallback side,
                // and the odd window goes to the other one:
                // `centerSlaveColumns`.
                let width = area.w * mfact;
                let beside = (area.w - width) / 2.0;
                share(
                    &masters,
                    Area {
                        x: area.x + beside,
                        w: width,
                        ..area
                    },
                    true,
                    &mut out,
                );
                let right_first = settings.master.center_fallback == Orientation::Right;
                let (mut left, mut right) = (Vec::new(), Vec::new());
                for (at, &window) in stack.iter().enumerate() {
                    if (at % 2 == 0) == right_first {
                        right.push(window);
                    } else {
                        left.push(window);
                    }
                }
                share(&left, Area { w: beside, ..area }, true, &mut out);
                share(
                    &right,
                    Area {
                        x: area.x + beside + width,
                        w: area.w - beside - width,
                        ..area
                    },
                    true,
                    &mut out,
                );
            }
            Orientation::Left | Orientation::Right => {
                let width = area.w * mfact;
                let left = orientation == Orientation::Left;
                let (master_x, stack_x) = if left {
                    (area.x, area.x + width)
                } else {
                    (area.x + area.w - width, area.x)
                };
                share(
                    &masters,
                    Area {
                        x: master_x,
                        w: width,
                        ..area
                    },
                    true,
                    &mut out,
                );
                let column = Area {
                    x: stack_x,
                    w: area.w - width,
                    ..area
                };
                share(&stack, column, true, &mut out);
            }
            Orientation::Top | Orientation::Bottom => {
                let height = area.h * mfact;
                let top = orientation == Orientation::Top;
                let (master_y, stack_y) = if top {
                    (area.y, area.y + height)
                } else {
                    (area.y + area.h - height, area.y)
                };
                share(
                    &masters,
                    Area {
                        y: master_y,
                        h: height,
                        ..area
                    },
                    false,
                    &mut out,
                );
                let row = Area {
                    y: stack_y,
                    h: area.h - height,
                    ..area
                };
                share(&stack, row, false, &mut out);
            }
        }
        out
    }
}

/// Divide `area` equally among `windows`, one below the other when
/// `vertical`, else side by side. As Hyprland does, each takes what is left
/// divided by how many are left, and the last takes the rest.
fn share(windows: &[WindowId], area: Area, vertical: bool, out: &mut Vec<(WindowId, Area)>) {
    let mut left = if vertical { area.h } else { area.w };
    let mut next = if vertical { area.y } else { area.x };
    let mut count = windows.len();
    for &window in windows {
        let size = if count > 1 { left / count as f64 } else { left };
        let slot = if vertical {
            Area {
                y: next,
                h: size,
                ..area
            }
        } else {
            Area {
                x: next,
                w: size,
                ..area
            }
        };
        out.push((window, slot));
        next += size;
        left -= size;
        count = count.saturating_sub(1);
    }
}
