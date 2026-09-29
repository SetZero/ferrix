//! What hypridle does when the compositor or `loginctl` tells it something.
//!
//! Upstream's `CHypridle::onIdled`, `onResumed`, `onLocked`, `onUnlocked`,
//! `handleDbusLogin` and the `condition_cmd` retries at the bottom of
//! `enterEventLoop`, with the running of commands handed to a [`Run`] so the
//! rules are tested without starting anything.
//!
//! One part of upstream is not here: `onInhibit`, the count of D-Bus
//! `ScreenSaver.Inhibit` and logind `idle` inhibitors. Ferrix has neither, so
//! nothing could ever raise the count; a Wayland idle inhibitor is the
//! compositor's to honour, and it does, by not sending `idled` to a
//! notification made with `get_idle_notification`.

use std::time::{Duration, Instant};

use crate::config::{General, Rule};
use crate::log::Level;

/// How commands are run and lines are said.
pub trait Run {
    /// Start `command` under `/bin/sh -c` and do not wait for it.
    fn spawn(&mut self, command: &str);
    /// Run `command` under `/bin/sh -c`, wait, and give whether it exited 0.
    fn condition(&mut self, command: &str) -> bool;
    /// Say a line.
    fn log(&mut self, level: Level, line: &str);
}

/// One listener's state: upstream's `SIdleListener`.
#[derive(Clone, Debug)]
pub struct Listener {
    /// What the file said.
    pub rule: Rule,
    /// Whether `on-timeout` ran and `on-resume` has not yet.
    fired: bool,
    /// Whether `condition_cmd` said no and is to be asked again.
    pending: bool,
    /// When to ask it again.
    retry_at: Option<Instant>,
}

/// Everything hypridle keeps between events.
#[derive(Clone, Debug)]
pub struct Idle {
    /// `general { }`.
    pub general: General,
    /// The listeners, in file order: the index is the rule's number.
    pub listeners: Vec<Listener>,
    /// Whether the compositor last said the session is locked.
    pub locked: bool,
}

impl Idle {
    /// The state before anything has happened.
    #[must_use]
    pub fn new(general: General, rules: Vec<Rule>) -> Self {
        Self {
            general,
            listeners: rules
                .into_iter()
                .map(|rule| Listener {
                    rule,
                    fired: false,
                    pending: false,
                    retry_at: None,
                })
                .collect(),
            locked: false,
        }
    }

    /// Whether this listener's notification should ignore inhibitors, which
    /// is `get_input_idle_notification` rather than `get_idle_notification`.
    #[must_use]
    pub fn ignores_inhibitors(&self, index: usize) -> bool {
        self.general.ignore_wayland_inhibit
            || self
                .listeners
                .get(index)
                .is_some_and(|listener| listener.rule.ignore_inhibit)
    }

    /// The compositor said listener `index`'s timeout has passed.
    pub fn idled(&mut self, index: usize, now: Instant, run: &mut dyn Run) {
        let Some(listener) = self.listeners.get_mut(index) else {
            return;
        };
        run.log(
            Level::Log,
            &format!("Idled: rule {index} ({} s)", listener.rule.timeout),
        );
        if listener.rule.on_timeout.is_empty() {
            run.log(Level::Log, "Ignoring, onTimeout is empty.");
            return;
        }
        if !listener.rule.condition_cmd.is_empty() && !condition(run, &listener.rule.condition_cmd)
        {
            if listener.rule.condition_retry > 0 {
                run.log(
                    Level::Log,
                    &format!(
                        "condition_cmd blocked on-timeout, retrying in {}s",
                        listener.rule.condition_retry
                    ),
                );
                listener.pending = true;
                listener.retry_at = Some(now + retry(listener.rule.condition_retry));
            } else {
                run.log(
                    Level::Log,
                    "condition_cmd blocked on-timeout, no retry configured",
                );
            }
            return;
        }
        run.log(Level::Log, &format!("Running {}", listener.rule.on_timeout));
        listener.fired = true;
        run.spawn(&listener.rule.on_timeout);
    }

    /// The compositor said the seat is in use again, for listener `index`.
    pub fn resumed(&mut self, index: usize, run: &mut dyn Run) {
        // Any input ends every listener's wait for its condition, not only
        // this one's: upstream's comment in `onResumed` says why.
        for listener in &mut self.listeners {
            listener.pending = false;
            listener.retry_at = None;
        }
        let Some(listener) = self.listeners.get_mut(index) else {
            return;
        };
        run.log(Level::Log, &format!("Resumed: rule {index}"));
        if !listener.fired {
            run.log(
                Level::Log,
                &format!("Skipping onResumed: onTimeout was inhibited for rule {index}"),
            );
            return;
        }
        listener.fired = false;
        if listener.rule.on_resume.is_empty() {
            run.log(Level::Log, "Ignoring, onRestore is empty.");
            return;
        }
        run.log(Level::Log, &format!("Running {}", listener.rule.on_resume));
        run.spawn(&listener.rule.on_resume);
    }

    /// Ask again every `condition_cmd` whose retry is due.
    pub fn retry(&mut self, now: Instant, run: &mut dyn Run) {
        for (index, listener) in self.listeners.iter_mut().enumerate() {
            if !listener.pending || listener.retry_at.is_some_and(|at| now < at) {
                continue;
            }
            run.log(
                Level::Log,
                &format!("Retrying condition_cmd for rule {index}"),
            );
            if condition(run, &listener.rule.condition_cmd) {
                listener.pending = false;
                listener.retry_at = None;
                listener.fired = true;
                run.log(
                    Level::Log,
                    &format!("Condition met, running {}", listener.rule.on_timeout),
                );
                run.spawn(&listener.rule.on_timeout);
            } else {
                listener.retry_at = Some(now + retry(listener.rule.condition_retry));
                run.log(
                    Level::Log,
                    &format!(
                        "Condition still not met, retrying in {}s",
                        listener.rule.condition_retry
                    ),
                );
            }
        }
    }

    /// When the next retry is due, if one is waiting.
    #[must_use]
    pub fn next_retry(&self) -> Option<Instant> {
        self.listeners
            .iter()
            .filter(|listener| listener.pending)
            .filter_map(|listener| listener.retry_at)
            .min()
    }

    /// `loginctl lock-session`: what logind's `Lock` signal does upstream.
    pub fn lock_session(&mut self, run: &mut dyn Run) {
        run.log(Level::Log, "Got lock-session from loginctl");
        if !self.general.lock_cmd.is_empty() {
            run.log(
                Level::Log,
                &format!("Locking with {}", self.general.lock_cmd),
            );
            run.spawn(&self.general.lock_cmd);
        }
    }

    /// `loginctl unlock-session`: logind's `Unlock`.
    pub fn unlock_session(&mut self, run: &mut dyn Run) {
        run.log(Level::Log, "Got unlock-session from loginctl");
        if !self.general.unlock_cmd.is_empty() {
            run.log(
                Level::Log,
                &format!("Unlocking with {}", self.general.unlock_cmd),
            );
            run.spawn(&self.general.unlock_cmd);
        }
    }

    /// `hyprland_lock_notification_v1.locked`.
    pub fn session_locked(&mut self, run: &mut dyn Run) {
        run.log(Level::Log, "Wayland session got locked");
        self.locked = true;
        if !self.general.on_lock_cmd.is_empty() {
            run.spawn(&self.general.on_lock_cmd);
        }
    }

    /// `hyprland_lock_notification_v1.unlocked`.
    pub fn session_unlocked(&mut self, run: &mut dyn Run) {
        run.log(Level::Log, "Wayland session got unlocked");
        self.locked = false;
        if !self.general.on_unlock_cmd.is_empty() {
            run.spawn(&self.general.on_unlock_cmd);
        }
    }
}

/// Run a `condition_cmd` and say what it gave, as upstream's
/// `runConditionCmd` does.
fn condition(run: &mut dyn Run, command: &str) -> bool {
    run.log(Level::Log, &format!("Running condition_cmd: {command}"));
    run.condition(command)
}

/// A `condition_retry` as a wait.
fn retry(seconds: i64) -> Duration {
    Duration::from_secs(u64::try_from(seconds).unwrap_or(0))
}
