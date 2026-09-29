//! The resource keys of §5.5, each one controller file.
//!
//! | Key | File | Controller |
//! |---|---|---|
//! | `MemoryMax=` | `memory.max` | memory |
//! | `MemoryHigh=` | `memory.high` | memory |
//! | `TasksMax=` | `pids.max` | pids |
//! | `CPUWeight=` | `cpu.weight` | cpu |
//! | `CPUQuota=` | `cpu.max` | cpu |
//! | `IOWeight=` | `io.weight` | io |
//!
//! A key left unset leaves the kernel's default, which is no limit. A value
//! that is a share of something the manager cannot see, a percentage of the
//! machine's memory, is kept as the share, for the backend to resolve.

use crate::Warnings;
use crate::ini::Assignment;
use crate::keys::{self, Setter};
use crate::value::{self, ValueError};

/// A cgroup v2 controller init writes limits through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Controller {
    /// `cpu`.
    Cpu,
    /// `io`.
    Io,
    /// `memory`.
    Memory,
    /// `pids`.
    Pids,
}

impl Controller {
    /// Its name in `cgroup.controllers`.
    pub fn name(self) -> &'static str {
        match self {
            Controller::Cpu => "cpu",
            Controller::Io => "io",
            Controller::Memory => "memory",
            Controller::Pids => "pids",
        }
    }
}

/// A memory limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Memory {
    /// This many bytes.
    Bytes(u64),
    /// A share of the machine's memory, in hundredths of a percent.
    Share(u64),
    /// `infinity`: `max`.
    Infinity,
}

/// A limit on the number of tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tasks {
    /// This many.
    Count(u64),
    /// A share of the kernel's pid limit, in hundredths of a percent.
    Share(u64),
    /// `infinity`: `max`.
    Infinity,
}

/// A CPU weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuWeight {
    /// From 1 to 10000; the kernel's default is 100.
    Weight(u32),
    /// `idle`: run only when nothing else wants the CPU.
    Idle,
}

/// Every resource limit a unit sets; `None` leaves the kernel's default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    /// `MemoryMax=`.
    pub memory_max: Option<Memory>,
    /// `MemoryHigh=`.
    pub memory_high: Option<Memory>,
    /// `TasksMax=`.
    pub tasks_max: Option<Tasks>,
    /// `CPUWeight=`.
    pub cpu_weight: Option<CpuWeight>,
    /// `CPUQuota=`: hundredths of a percent of one CPU's time; 20000 is two
    /// CPUs.
    pub cpu_quota: Option<u64>,
    /// `IOWeight=`, from 1 to 10000.
    pub io_weight: Option<u32>,
}

impl Limits {
    /// Whether nothing is set.
    pub fn is_empty(&self) -> bool {
        *self == Limits::default()
    }

    /// The controllers the limits set need.
    pub fn controllers(&self) -> impl Iterator<Item = Controller> {
        [
            (
                self.memory_max.is_some() || self.memory_high.is_some(),
                Controller::Memory,
            ),
            (self.tasks_max.is_some(), Controller::Pids),
            (
                self.cpu_weight.is_some() || self.cpu_quota.is_some(),
                Controller::Cpu,
            ),
            (self.io_weight.is_some(), Controller::Io),
        ]
        .into_iter()
        .filter_map(|(set, controller)| set.then_some(controller))
    }
}

/// `infinity`, a percentage, or a size.
fn memory(text: &str) -> Result<Memory, ValueError> {
    if text == "infinity" {
        Ok(Memory::Infinity)
    } else if text.ends_with(['%', '‰', '‱']) {
        value::permyriad(text, true).map(Memory::Share)
    } else {
        value::size(text).map(Memory::Bytes)
    }
}

/// `infinity`, a percentage, or a count.
fn tasks(text: &str) -> Result<Tasks, ValueError> {
    if text == "infinity" {
        Ok(Tasks::Infinity)
    } else if text.ends_with(['%', '‰', '‱']) {
        value::permyriad(text, true).map(Tasks::Share)
    } else {
        text.parse()
            .map(Tasks::Count)
            .map_err(|_| ValueError::Invalid)
    }
}

/// A weight from 1 to 10000.
fn weight(text: &str) -> Result<u32, ValueError> {
    let weight: u32 = text.parse().map_err(|_| ValueError::Invalid)?;
    if (1..=10_000).contains(&weight) {
        Ok(weight)
    } else {
        Err(ValueError::Range)
    }
}

/// Set `field` from the assignment through `parse`, or clear it for an
/// empty value.
fn set<V>(
    field: &mut Option<V>,
    assignment: &Assignment,
    warnings: &mut Warnings,
    parse: impl FnOnce(&str) -> Result<V, ValueError>,
) {
    if assignment.value.is_empty() {
        *field = None;
    } else if let Some(value) = keys::parsed(assignment, warnings, parse) {
        *field = Some(value);
    }
}

/// The resource keys, for the kinds whose sections take them: `[Service]`,
/// `[Slice]` and `[Scope]`.
pub(crate) const KEYS: [(&str, Setter<Limits>); 6] = [
    ("MemoryMax", |l, a, w| set(&mut l.memory_max, a, w, memory)),
    ("MemoryHigh", |l, a, w| {
        set(&mut l.memory_high, a, w, memory);
    }),
    ("TasksMax", |l, a, w| set(&mut l.tasks_max, a, w, tasks)),
    ("CPUWeight", |l, a, w| {
        set(&mut l.cpu_weight, a, w, |text| {
            if text == "idle" {
                Ok(CpuWeight::Idle)
            } else {
                weight(text).map(CpuWeight::Weight)
            }
        });
    }),
    ("CPUQuota", |l, a, w| {
        set(&mut l.cpu_quota, a, w, |text| {
            match value::permyriad(text, false) {
                Ok(0) => Err(ValueError::Range),
                other => other,
            }
        });
    }),
    ("IOWeight", |l, a, w| set(&mut l.io_weight, a, w, weight)),
];
