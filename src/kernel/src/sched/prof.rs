//! PROFILE, NOT FOR LANDING: where a channel round trip spends its time.
//!
//! Cumulative counter ticks and counts per span, printed when the built-in
//! shell exits.

use core::sync::atomic::{AtomicU64, Ordering};

/// The spans.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Span {
    /// `choose_next`, whole.
    Choose = 0,
    /// The address space swap.
    Space,
    /// The user state swap.
    UserState,
    /// Arming or stopping the timer.
    Timer,
    /// `sched::wake`.
    Wake,
    /// A timer interrupt's handler.
    TimerIrq,
    /// `arch::switch_to` and `finish_switch`.
    Switch,
    /// `channel_write`.
    ChanWrite,
    /// `channel_read`.
    ChanRead,
    /// `object_wait_one`, whole, including the sleep.
    WaitOne,
    /// The idle loop's halt.
    Halt,
    /// IPIs sent.
    Ipi,
    /// `wake_all` with nobody on the queue.
    WakeNone,
    /// The waiter list's push.
    WaitPush,
    /// Sub-span A.
    A,
    /// Sub-span B.
    B,
    /// Sub-span C.
    C,
    /// Sub-span D.
    D,
    /// choose: lock.
    E,
    /// choose: account + wake_sleepers.
    F,
    /// choose: detach + file sleeper.
    G,
    /// choose: pick_next.
    H,
    /// choose: arm_timer.
    I,
    /// wake: lock.
    J,
    /// wake: remove_sleeper + deadline.
    K,
    /// wake: insert.
    L,
    /// wait: current + list + mark.
    M,
    /// wait: unqueue after.
    N,
    /// native_call: current().
    O,
}

/// How many spans.
const SPANS: usize = 29;

/// Names, in order.
const NAMES: [&str; SPANS] = [
    "choose", "space", "ustate", "timer", "wake", "timerirq", "syscall", "chwrite", "chread",
    "waitone", "halt", "ipi", "leftsched", "sync", "wr-write", "wr-wait", "c", "wr-whole", "ch-lock", "ch-acct", "ch-file",
    "ch-pick", "ch-arm", "wk-lock", "wk-sleep", "wk-ins", "wt-list", "wt-unq", "nc-curr",
];

static TICKS: [AtomicU64; SPANS] = [const { AtomicU64::new(0) }; SPANS];
static COUNTS: [AtomicU64; SPANS] = [const { AtomicU64::new(0) }; SPANS];

/// The counter now.
pub(crate) fn now() -> u64 {
    crate::arch::counter_now()
}

/// Add the time since `start` to `span`.
pub(crate) fn add(span: Span, start: u64) {
    let i = span as usize;
    if let (Some(t), Some(c)) = (TICKS.get(i), COUNTS.get(i)) {
        let _ = t.fetch_add(now().wrapping_sub(start), Ordering::Relaxed);
        let _ = c.fetch_add(1, Ordering::Relaxed);
    }
}

/// Print every span: count, and nanoseconds per occurrence.
pub(crate) fn print() {
    let hz = crate::arch::counter_hz().max(1);
    for (i, name) in NAMES.iter().enumerate() {
        let t = TICKS.get(i).map_or(0, |a| a.load(Ordering::Relaxed));
        let c = COUNTS.get(i).map_or(0, |a| a.load(Ordering::Relaxed));
        let per = if c == 0 {
            0
        } else {
            u128::from(t) * 1_000_000_000 / u128::from(hz) / u128::from(c)
        };
        crate::console::println!("  prof {name:>9} count={c:>9} ns/each={per}");
    }
}

/// Forget everything counted so far.
pub(crate) fn reset() {
    for (t, c) in TICKS.iter().zip(COUNTS.iter()) {
        t.store(0, Ordering::Relaxed);
        c.store(0, Ordering::Relaxed);
    }
}

/// Adds its span when dropped.
pub(crate) struct Guard(pub(crate) Span, pub(crate) u64);

impl Drop for Guard {
    fn drop(&mut self) {
        add(self.0, self.1);
    }
}
