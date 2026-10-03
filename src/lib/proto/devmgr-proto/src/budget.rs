//! A device's pin budget, as `devmgr` chooses it (`docs/NVIDIA.md` §12.2).
//!
//! The kernel gives every device a default budget, one driver's worst case,
//! and `devmgr` alone may set another, by `SET_LIMIT` on the device handle
//! it never hands a driver. Every kind of device `devmgr` drives today keeps
//! the default. A GPU driven by NVIDIA's own driver, `nvrm`, does not: its
//! pins are GSP's firmware and every client's memory, far past 256 MiB. Its
//! rule is the customer's (2026-10-02): an eighth of RAM, and at least
//! 1 GiB, which the kernel's ceiling -- twice every raised budget within a
//! quarter of RAM -- may cut. Every outcome is a line; there is no silent
//! fallback to the default.
//!
//! Pure functions of what `device_get_limit` answers, tested on the host.
//! `devmgr`'s `Gpu` kind calls [`GpuBudget::plan`] through
//! [`crate::gpu::hand_over`] before it starts `nvrm` (NVIDIA's N1b).

use core::fmt;

/// Bytes in a page, the unit every budget is counted in.
pub const PAGE_BYTES: u64 = 4096;

/// Pages in a MiB.
const MIB_PAGES: u64 = 1024 * 1024 / PAGE_BYTES;

/// Pages in 1 GiB: the floor of `nvrm`'s budget.
pub const GIB_PAGES: u64 = 1024 * MIB_PAGES;

/// The fewest pages `nvrm` can run with: GSP's firmware, its logs and rings,
/// and one client's first channel. 256 MiB until NVIDIA's N1d measures it.
pub const NVRM_MIN_PIN_PAGES: u64 = 256 * MIB_PAGES;

/// What `devmgr` does about a GPU's pin budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuBudget {
    /// Set `pages`: an eighth of RAM, or the 1 GiB floor, fits the room.
    Whole {
        /// The budget.
        pages: u64,
    },
    /// Set `pages`: what was wanted did not fit the room, so the budget is
    /// half the room, and the floor yields to the ceiling.
    Cut {
        /// The budget.
        pages: u64,
        /// What was wanted.
        wanted: u64,
        /// The machine's RAM, in pages, as four times the ceiling.
        ram: u64,
    },
    /// Do not start the GPU: the budget it could have is under
    /// [`NVRM_MIN_PIN_PAGES`].
    TooSmall {
        /// The budget it could have.
        pages: u64,
    },
    /// Do not start the GPU: the kernel refused the budget with
    /// `NO_MEMORY`, as another device took room in between.
    Refused {
        /// The budget refused.
        pages: u64,
    },
}

impl GpuBudget {
    /// The budget for a GPU, from the kernel's `ceiling` and the `room` of
    /// it other devices leave, both in pages as `device_get_limit` answers
    /// them.
    ///
    /// Wanted is `max(ceiling / 2, 1 GiB)`: an eighth of RAM, with the floor.
    /// If twice it is more than the room, the budget is half the room
    /// instead. Below [`NVRM_MIN_PIN_PAGES`] the GPU is not started. A budget
    /// is whole MiB, rounded down.
    #[must_use]
    pub fn plan(ceiling: u64, room: u64) -> GpuBudget {
        let wanted = whole_mib((ceiling / 2).max(GIB_PAGES));
        let ram = ceiling.saturating_mul(4);
        let budget = if wanted.saturating_mul(2) > room {
            GpuBudget::Cut {
                pages: whole_mib(room / 2),
                wanted,
                ram,
            }
        } else {
            GpuBudget::Whole { pages: wanted }
        };
        match budget {
            GpuBudget::Cut { pages, .. } if pages < NVRM_MIN_PIN_PAGES => {
                GpuBudget::TooSmall { pages }
            }
            budget => budget,
        }
    }

    /// What to pass `device_set_limit`, or `None` when the GPU is not
    /// started.
    #[must_use]
    pub const fn to_set(self) -> Option<u64> {
        match self {
            GpuBudget::Whole { pages } | GpuBudget::Cut { pages, .. } => Some(pages),
            GpuBudget::TooSmall { .. } | GpuBudget::Refused { .. } => None,
        }
    }

    /// What the plan becomes when `device_set_limit` answers `NO_MEMORY`.
    #[must_use]
    pub const fn refused(self) -> GpuBudget {
        match self {
            GpuBudget::Whole { pages }
            | GpuBudget::Cut { pages, .. }
            | GpuBudget::TooSmall { pages }
            | GpuBudget::Refused { pages } => GpuBudget::Refused { pages },
        }
    }

    /// The line `devmgr` says it with, for the GPU at `location`, the PCI
    /// address word `device_info` gives.
    #[must_use]
    pub const fn line(self, location: u32) -> Line {
        Line {
            budget: self,
            location,
        }
    }
}

/// `pages`, rounded down to whole MiB.
const fn whole_mib(pages: u64) -> u64 {
    pages - pages % MIB_PAGES
}

/// [`GpuBudget::line`]'s text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    /// The outcome.
    budget: GpuBudget,
    /// The PCI address word: segment in bits 31:16, bus in 15:8, devfn in
    /// 7:0.
    location: u32,
}

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mib = |pages: u64| pages / MIB_PAGES;
        let segment = self.location >> 16;
        let [devfn, bus, _, _] = self.location.to_le_bytes();
        write!(f, "devmgr   gpu ")?;
        if segment != 0 {
            write!(f, "{segment:04x}:")?;
        }
        write!(f, "{bus:02x}:{:02x}.{}", devfn >> 3, devfn & 7)?;
        match self.budget {
            GpuBudget::Whole { pages } => write!(
                f,
                ": pin budget {} MiB (an eighth of RAM, at least 1 GiB)",
                mib(pages)
            ),
            GpuBudget::Cut { pages, wanted, ram } => write!(
                f,
                ": pin budget {} MiB, cut from {} MiB: the 1 GiB floor yields to the kernel's \
                 ceiling ({} MiB of RAM)",
                mib(pages),
                mib(wanted),
                mib(ram)
            ),
            GpuBudget::TooSmall { pages } => write!(
                f,
                " not started: its pin budget of {} MiB is under the {} MiB nvrm needs",
                mib(pages),
                mib(NVRM_MIN_PIN_PAGES)
            ),
            GpuBudget::Refused { pages } => write!(
                f,
                " not started: the kernel refused its pin budget of {} MiB (past the ceiling)",
                mib(pages)
            ),
        }
    }
}
