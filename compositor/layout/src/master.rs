//! The master layout: Hyprland's `MasterLayout.cpp`, with one master.
//!
//! A workspace's tiled windows are a list, and one of them is the master.
//! Alone, the master fills the workspace. With a stack, it takes `mfact` of
//! the workspace on the side `master:orientation` names, and the stack
//! windows share the rest equally, in list order, top to bottom or left to
//! right.
//!
//! A new window goes to the end of the list, or the front with
//! `master:new_on_top`, and becomes the master when `master:new_status` says
//! so or when it is the first. The old master keeps its place in the list,
//! so it joins the stack there, as in Hyprland. When the master closes, the
//! first window of the list takes over.
//!
//! Where it departs from Hyprland: Hyprland allows several masters, which
//! only `addmaster` creates, and that dispatcher is not implemented; the
//! `center` orientation lays out as `left`; the master's share is not yet
//! resizable per workspace.

use crate::WindowId;
use crate::geometry::Area;
use crate::settings::{NewStatus, Orientation, Settings};

/// One workspace's master layout.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Master {
    /// Every tiled window, the master among them, in list order.
    order: Vec<WindowId>,
    /// The master, `None` only when the list is empty.
    master: Option<WindowId>,
}

impl Master {
    /// Add `new`, when `focused` is the workspace's focused tiled window.
    pub(crate) fn insert(&mut self, new: WindowId, focused: Option<WindowId>, settings: &Settings) {
        let becomes_master = self.master.is_none()
            || match settings.master.new_status {
                NewStatus::Master => true,
                NewStatus::Slave => false,
                NewStatus::Inherit => focused.is_some() && focused == self.master,
            };
        if settings.master.new_on_top {
            self.order.insert(0, new);
        } else {
            self.order.push(new);
        }
        if becomes_master {
            self.master = Some(new);
        }
    }

    /// Remove `window`; if it was the master, the first window left takes
    /// over.
    pub(crate) fn remove(&mut self, window: WindowId) {
        self.order.retain(|id| *id != window);
        if self.master == Some(window) {
            self.master = self.order.first().copied();
        }
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
        self.master = self.master.map(exchange);
    }

    /// The master, which is `None` only when the list is empty.
    pub(crate) const fn master(&self) -> Option<WindowId> {
        self.master
    }

    /// `layoutmsg swapwithmaster`: exchange `window` with the master, or,
    /// when `window` *is* the master, with the first of the stack.
    ///
    /// The two change places in the list as well, so a second call puts
    /// them back -- which is what makes the message a toggle in Hyprland.
    pub(crate) fn swap_with_master(&mut self, window: WindowId) -> bool {
        let Some(master) = self.master else {
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
        let master = self
            .master
            .and_then(|master| self.order.iter().position(|id| *id == master));
        if back {
            self.order.rotate_left(1);
        } else {
            self.order.rotate_right(1);
        }
        if let Some(at) = master {
            self.master = self.order.get(at).copied();
        }
    }

    /// Whether `window` is in the list.
    pub(crate) fn contains(&self, window: WindowId) -> bool {
        self.order.contains(&window)
    }

    /// The master, then the stack in order.
    pub(crate) fn windows(&self) -> Vec<WindowId> {
        self.master.into_iter().chain(self.stack()).collect()
    }

    /// The stack in order.
    fn stack(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.order
            .iter()
            .copied()
            .filter(|id| Some(*id) != self.master)
    }

    /// Each window's box, when the workspace's work area is `area`.
    pub(crate) fn slots(&self, area: Area, settings: &Settings) -> Vec<(WindowId, Area)> {
        let Some(master) = self.master else {
            return Vec::new();
        };
        let stack: Vec<WindowId> = self.stack().collect();
        if stack.is_empty() {
            return vec![(master, area)];
        }
        let mfact = settings.master.mfact;
        let mut out = Vec::with_capacity(stack.len().saturating_add(1));
        match settings.master.orientation {
            Orientation::Left | Orientation::Right => {
                let width = area.w * mfact;
                let left = settings.master.orientation == Orientation::Left;
                let (master_x, stack_x) = if left {
                    (area.x, area.x + width)
                } else {
                    (area.x + area.w - width, area.x)
                };
                out.push((
                    master,
                    Area {
                        x: master_x,
                        w: width,
                        ..area
                    },
                ));
                let column = Area {
                    x: stack_x,
                    w: area.w - width,
                    ..area
                };
                share(&stack, column, true, &mut out);
            }
            Orientation::Top | Orientation::Bottom => {
                let height = area.h * mfact;
                let top = settings.master.orientation == Orientation::Top;
                let (master_y, stack_y) = if top {
                    (area.y, area.y + height)
                } else {
                    (area.y + area.h - height, area.y)
                };
                out.push((
                    master,
                    Area {
                        y: master_y,
                        h: height,
                        ..area
                    },
                ));
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
