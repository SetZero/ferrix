//! Restarting (§5.4): whether an end restarts a service, after how long,
//! and when to give up.
//!
//! Three parts, each usable alone, because `devmgr` has the same problem
//! with no clock (`docs/DEVMGR.md` §4) and shares this code:
//!
//! * [`wanted`]: systemd's table of which ends each `Restart=` restarts.
//! * [`Backoff`]: `RestartSec=`, doubled on each restart in a row, up to 32
//!   times it.
//! * [`Budget`]: the start limit, `StartLimitBurst=` starts within
//!   `StartLimitIntervalSec=`. Every start is counted, asked for or not, as
//!   systemd counts it. Given no clock, it is a plain count of starts.
//!
//! [`Policy`] puts them together: restart now, restart at `t`, or give up.

use alloc::collections::VecDeque;
use core::time::Duration;

use crate::event::Exit;
use crate::kind::Restart;
use crate::time::Instant;
use crate::value::Signal;

/// How a service ended, in the terms `Restart=` is decided by: systemd's
/// service results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ended {
    /// Exit status 0, or a clean signal: `SIGHUP`, `SIGINT`, `SIGTERM` or
    /// `SIGPIPE`.
    Success,
    /// Another exit status.
    ExitCode,
    /// Another signal.
    Signal,
    /// A signal that dumped core.
    CoreDump,
    /// A start or stop that took longer than its timeout.
    Timeout,
    /// A watchdog timeout.
    Watchdog,
    /// The kernel's OOM kill reached it (§5.5).
    OomKill,
    /// It could not be started at all: a spawn that failed.
    Resources,
    /// The start limit was spent (§5.4), so it was not started.
    StartLimitHit,
}

impl Ended {
    /// How an exit ends a service.
    pub fn of(exit: Exit) -> Ended {
        const CLEAN: [Signal; 4] = [Signal::HUP, Signal::INT, Signal::TERM, Signal(13)];
        match exit {
            Exit::Code(0) => Ended::Success,
            Exit::Code(_) => Ended::ExitCode,
            Exit::Signal { signal, .. } if CLEAN.contains(&signal) => Ended::Success,
            Exit::Signal { core: true, .. } => Ended::CoreDump,
            Exit::Signal { .. } => Ended::Signal,
        }
    }

    /// systemd's name for it, as `svc status` shows it.
    pub fn name(self) -> &'static str {
        match self {
            Ended::Success => "success",
            Ended::ExitCode => "exit-code",
            Ended::Signal => "signal",
            Ended::CoreDump => "core-dump",
            Ended::Timeout => "timeout",
            Ended::Watchdog => "watchdog",
            Ended::OomKill => "oom-kill",
            Ended::Resources => "resources",
            Ended::StartLimitHit => "start-limit-hit",
        }
    }
}

/// Whether `restart` restarts a service that ended so: the table in
/// `systemd.service(5)`.
pub fn wanted(restart: Restart, ended: Ended) -> bool {
    if ended == Ended::StartLimitHit {
        return false;
    }
    match restart {
        Restart::No => false,
        Restart::Always => true,
        Restart::OnSuccess => ended == Ended::Success,
        Restart::OnFailure => ended != Ended::Success,
        Restart::OnAbnormal => matches!(
            ended,
            Ended::Signal | Ended::CoreDump | Ended::Timeout | Ended::Watchdog
        ),
        Restart::OnAbort => matches!(ended, Ended::Signal | Ended::CoreDump),
        Restart::OnWatchdog => ended == Ended::Watchdog,
    }
}

/// The delay before a restart: `RestartSec=` for the first, doubled for
/// each restart in a row after it, up to 32 times `RestartSec=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    base: Duration,
    /// Restarts in a row so far.
    steps: u32,
}

impl Backoff {
    /// The most a delay grows to, in multiples of `RestartSec=`.
    pub const CAP: u32 = 32;

    /// A backoff starting at `base`.
    pub fn new(base: Duration) -> Self {
        Self { base, steps: 0 }
    }

    /// The delay before the next restart, counting it as one more in a row.
    pub fn next_delay(&mut self) -> Duration {
        let factor = 1u32
            .checked_shl(self.steps)
            .unwrap_or(Self::CAP)
            .min(Self::CAP);
        self.steps = self.steps.saturating_add(1);
        self.base.saturating_mul(factor)
    }

    /// Restarts in a row so far.
    pub fn steps(&self) -> u32 {
        self.steps
    }

    /// Start counting again: the service was started by request, or
    /// `reset-failed` was asked for.
    pub fn reset(&mut self) {
        self.steps = 0;
    }
}

/// The start limit: at most `burst` starts within `interval`. With a clock
/// it is a rate; with none, a count of every start since the last reset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    burst: u32,
    interval: Duration,
    /// The starts that still count: their times, `None` when there was no
    /// clock.
    starts: VecDeque<Option<Instant>>,
}

impl Budget {
    /// A budget of `burst` starts in `interval`; an interval of zero, or a
    /// burst of zero, turns the limit off, as in systemd.
    pub fn new(burst: u32, interval: Duration) -> Self {
        Self {
            burst,
            interval,
            starts: VecDeque::new(),
        }
    }

    /// Whether the limit is off.
    pub fn unlimited(&self) -> bool {
        self.burst == 0 || self.interval.is_zero()
    }

    /// Count a start at `now`. `false` if it is over the budget, in which
    /// case the start must not happen; it is not counted.
    pub fn start(&mut self, now: Option<Instant>) -> bool {
        if self.unlimited() {
            return true;
        }
        if let Some(now) = now {
            while let Some(&Some(first)) = self.starts.front() {
                if now.saturating_since(first) < self.interval {
                    break;
                }
                let _ = self.starts.pop_front();
            }
        }
        let burst = usize::try_from(self.burst).unwrap_or(usize::MAX);
        if self.starts.len() >= burst {
            return false;
        }
        self.starts.push_back(now);
        true
    }

    /// Starts counted now.
    pub fn used(&self) -> usize {
        self.starts.len()
    }

    /// Forget every start: `reset-failed`.
    pub fn reset(&mut self) {
        self.starts.clear();
    }
}

/// What to do about an end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Leave it stopped: `Restart=` does not restart this end.
    Stay,
    /// Restart now: the delay is zero, or there is no clock to wait on.
    Now,
    /// Restart at this time.
    At(Instant),
    /// The start limit is spent: the unit is `failed`, with the result
    /// `start-limit-hit`, until `reset-failed` or a new start.
    GiveUp,
}

/// `Restart=`, its backoff and its budget, for one unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// `Restart=`.
    pub restart: Restart,
    /// The delays.
    pub backoff: Backoff,
    /// The start limit.
    pub budget: Budget,
}

impl Policy {
    /// A policy from a unit's settings.
    pub fn new(restart: Restart, delay: Duration, burst: u32, interval: Duration) -> Self {
        Self {
            restart,
            backoff: Backoff::new(delay),
            budget: Budget::new(burst, interval),
        }
    }

    /// Decide about an end at `now`. A restart it decides on is counted
    /// against the budget already, so the start that follows must not be
    /// counted again.
    pub fn decide(&mut self, ended: Ended, now: Option<Instant>) -> Decision {
        if !wanted(self.restart, ended) {
            return Decision::Stay;
        }
        if !self.budget.start(now) {
            return Decision::GiveUp;
        }
        let delay = self.backoff.next_delay();
        match now {
            Some(now) if !delay.is_zero() => Decision::At(now + delay),
            _ => Decision::Now,
        }
    }

    /// A start asked for, rather than decided on: counted against the
    /// budget, and the backoff starts again. `false` if the budget is spent.
    pub fn start(&mut self, now: Option<Instant>) -> bool {
        self.backoff.reset();
        self.budget.start(now)
    }

    /// `reset-failed`.
    pub fn reset(&mut self) {
        self.backoff.reset();
        self.budget.reset();
    }
}
