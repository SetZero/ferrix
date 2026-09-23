//! The controllers, `cgroup.controllers`, and writes to
//! `cgroup.subtree_control`.
//!
//! A set of controllers prints as their names in Linux's subsystem order,
//! one space between, and a newline -- a bare newline when there are none
//! (`cgroup_print_ss_mask`). A write to `cgroup.subtree_control` is parsed as
//! `cgroup_subtree_control_write` parses it: surrounding whitespace dropped,
//! then tokens split on single spaces, empty ones skipped, each `+name` or
//! `-name`. A later token for a controller overrides an earlier one, so
//! `+memory -memory` disables it. A name the kernel does not have, or a token
//! without its sign, refuses the whole write, and nothing of it is applied.

use crate::Refusal;
use crate::write::strip;
use alloc::vec::Vec;

/// A controller `docs/ARCHITECTURE.md` §6 names, in Linux's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Controller {
    /// `cpu`: weight and bandwidth handed to the scheduling classes.
    Cpu,
    /// `io`: per-cgroup bandwidth in the block layer's scheduler.
    Io,
    /// `memory`: where charges, reclaim and the OOM kill are scoped.
    Memory,
    /// `pids`: how many tasks a subtree may hold.
    Pids,
}

/// Every controller, in order.
pub const ALL: [Controller; 4] = [
    Controller::Cpu,
    Controller::Io,
    Controller::Memory,
    Controller::Pids,
];

impl Controller {
    /// The name the files use.
    pub fn name(self) -> &'static str {
        match self {
            Controller::Cpu => "cpu",
            Controller::Io => "io",
            Controller::Memory => "memory",
            Controller::Pids => "pids",
        }
    }

    /// Its bit in a [`Set`].
    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// A set of controllers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Set(u8);

impl Set {
    /// No controller.
    pub const EMPTY: Set = Set(0);

    /// Whether `controller` is in it.
    pub fn contains(self, controller: Controller) -> bool {
        self.0 & controller.bit() != 0
    }

    /// It, with `controller` added.
    #[must_use]
    pub fn with(self, controller: Controller) -> Set {
        Set(self.0 | controller.bit())
    }

    /// It, with `controller` taken out.
    #[must_use]
    pub fn without(self, controller: Controller) -> Set {
        Set(self.0 & !controller.bit())
    }

    /// Whether it holds nothing.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every controller in it is also in `other`.
    pub fn is_subset(self, other: Set) -> bool {
        self.0 & !other.0 == 0
    }

    /// Its controllers, in order.
    pub fn iter(self) -> impl Iterator<Item = Controller> {
        ALL.into_iter()
            .filter(move |controller| self.contains(*controller))
    }
}

/// Append the set as `cgroup.controllers` and `cgroup.subtree_control` print
/// it.
pub fn render(out: &mut Vec<u8>, set: Set) {
    for (index, controller) in set.iter().enumerate() {
        if index != 0 {
            out.push(b' ');
        }
        out.extend_from_slice(controller.name().as_bytes());
    }
    out.push(b'\n');
}

/// What a write to `cgroup.subtree_control` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Change {
    /// Controllers to enable for the children.
    pub enable: Set,
    /// Controllers to disable for them.
    pub disable: Set,
}

/// Parse a write to `cgroup.subtree_control`. `known` is the set the kernel
/// has built: a name outside it is refused as Linux refuses one whose
/// subsystem is not enabled.
///
/// # Errors
///
/// [`Refusal::Invalid`] for a token with no sign or a name not in `known`.
pub fn parse_change(text: &[u8], known: Set) -> Result<Change, Refusal> {
    let mut change = Change::default();
    for token in strip(text).split(|&byte| byte == b' ') {
        let Some((&sign, name)) = token.split_first() else {
            continue;
        };
        let controller = known
            .iter()
            .find(|controller| controller.name().as_bytes() == name)
            .ok_or(Refusal::Invalid)?;
        match sign {
            b'+' => {
                change.enable = change.enable.with(controller);
                change.disable = change.disable.without(controller);
            }
            b'-' => {
                change.disable = change.disable.with(controller);
                change.enable = change.enable.without(controller);
            }
            _ => return Err(Refusal::Invalid),
        }
    }
    Ok(change)
}
