//! Time: a counter that always runs, and an interrupt that arrives later.
//!
//! Two different things, deliberately kept apart. The *counter* answers "how
//! long since boot" and is read, never waited on — the HPET's main counter on
//! x86-64, `CNTVCT_EL0` on AArch64. The *timer* is an interrupt scheduled for
//! a future instant: the local APIC timer, or `CNTV_CVAL_EL0`. A kernel that
//! conflates them ends up measuring elapsed time by counting its own ticks,
//! which measures the interrupt rate rather than the passage of time and
//! cannot notice when the two disagree.
//!
//! Stage 3's exit criterion depends on that separation: it counts ticks with
//! one and measures how long they took with the other.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// HANGDBG: temporary. Whether the one-shot or periodic timer is armed.
pub(crate) static HANGDBG_ARMED: AtomicBool = AtomicBool::new(false);

use crate::arch;
use crate::irq::{self, IrqError};

/// Nanoseconds in a second, as the unit conversions want it.
const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// Timer interrupts taken since boot.
static TICKS: AtomicU64 = AtomicU64::new(0);

/// The period to re-arm with on each tick, or zero for a one-shot.
static INTERVAL: AtomicU64 = AtomicU64::new(0);

/// When the next periodic tick is due, on [`now_nanos`]'s timescale.
///
/// A periodic timer is a *schedule*, not a delay repeated: tick `n` is due at
/// `start + n * interval`, and the instant it actually arrived has no say in
/// when tick `n + 1` is due. Keeping the deadline here rather than re-deriving
/// it from the clock inside the handler is what makes that true.
static DEADLINE: AtomicU64 = AtomicU64::new(0);

/// How far behind the schedule may fall before it is abandoned rather than
/// caught up.
///
/// Catching up matters: a tick delivered late must not push the next one late
/// as well, or a periodic timer on a busy machine drifts without bound. But
/// catching up without a limit is its own failure — a kernel held off for a
/// second with a millisecond period owes a thousand interrupts, and delivering
/// them back to back is a storm that arrives exactly when the machine is least
/// able to absorb it. Past this many intervals the debt is written off and the
/// schedule restarts from now.
const MAX_CATCH_UP: u64 = 16;

/// Register the tick handler on whichever interrupt this machine's timer uses.
///
/// # Errors
///
/// Whatever [`irq::register`] reports, which at this point can only mean two
/// subsystems claimed the same line.
pub(crate) fn init() -> Result<(), IrqError> {
    irq::register(arch::timer_irq(), on_tick)
}

/// What a timer interrupt does.
///
/// Re-arming here rather than in hardware's periodic mode is a deliberate
/// choice: AArch64's generic timer has no periodic mode at all — it compares
/// against an absolute instant — so a kernel that relied on the local APIC's
/// would need two different notions of "periodic". One-shot is the primitive
/// both machines have, and stage 5's tickless scheduler wants exactly that.
fn on_tick(_irq: u32) {
    let _ = TICKS.fetch_add(1, Ordering::Relaxed);
    let interval = INTERVAL.load(Ordering::Relaxed);
    if interval != 0 {
        // **From the deadline that just passed, not from now.** Arming for
        // `interval` here would make the period `interval` plus however long
        // this interrupt took to arrive and be handled, every single time —
        // so the error would not average out, it would accumulate, and a
        // timer asked for a thousand ticks a second would deliver however
        // many the machine's interrupt latency allowed. Under an emulator
        // that is a factor of two.
        let next = DEADLINE.load(Ordering::Relaxed).saturating_add(interval);
        arm_periodic(next, interval);
    } else {
        // **Not optional, and not symmetry for its own sake.** AArch64's timer
        // interrupt is level triggered: the line stays asserted for as long as
        // the comparator is in the past. A one-shot that returned without
        // disarming would be acknowledged, re-asserted before the handler had
        // returned, and the machine would take that interrupt forever.
        arch::timer_disarm();
        HANGDBG_ARMED.store(false, Ordering::Relaxed);
    }
    // The scheduler arms this timer for the moment its processor next has a
    // decision to make, so every expiry is one. It only sets a flag: the
    // decision itself is made at interrupt exit, once the controller has been
    // acknowledged. Nothing happens here before the scheduler is up.
    crate::sched::timer_expired();
}

/// Fire the timer interrupt once, `nanos` from now.
pub(crate) fn after(nanos: u64) {
    INTERVAL.store(0, Ordering::Relaxed);
    HANGDBG_ARMED.store(true, Ordering::Relaxed);
    arch::timer_arm(nanos);
}

/// Fire the timer interrupt every `nanos` until [`stop`].
///
/// Every `nanos` from *now*, and thereafter on that schedule: the periods are
/// measured from the deadlines they were due at rather than from the instants
/// the interrupts arrived, so a late tick does not make its successors late.
pub(crate) fn every(nanos: u64) {
    let next = now_nanos().saturating_add(nanos);
    INTERVAL.store(nanos, Ordering::Relaxed);
    arm_periodic(next, nanos);
}

/// Record `next` as the deadline and arm for it, resynchronising if the
/// schedule has fallen too far behind to be worth catching up.
fn arm_periodic(next: u64, interval: u64) {
    let now = now_nanos();
    let behind = now.saturating_sub(next);
    let next = if behind > interval.saturating_mul(MAX_CATCH_UP) {
        now.saturating_add(interval)
    } else {
        next
    };
    DEADLINE.store(next, Ordering::Relaxed);
    HANGDBG_ARMED.store(true, Ordering::Relaxed);
    arm_at(next);
}

/// Arm the hardware for an absolute deadline.
///
/// The architecture layer takes a delay because that is what a countdown timer
/// like the local APIC's can be given, so the subtraction happens here — once,
/// against the same counter every deadline is expressed in.
///
/// A one-shot at an absolute deadline is what a sleeping task wants, since it
/// knows when it should wake rather than how long it has left; stage 5 can
/// make this public the moment it has a caller for it.
fn arm_at(deadline: u64) {
    // A deadline in the past becomes a delay of zero, which the architecture
    // layer arms as its smallest possible interval: late, but arriving, which
    // is the only useful reading of "wake me at a time that has passed".
    arch::timer_arm(deadline.saturating_sub(now_nanos()));
}

/// Stop the timer. The counter keeps running; it always does.
pub(crate) fn stop() {
    INTERVAL.store(0, Ordering::Relaxed);
    HANGDBG_ARMED.store(false, Ordering::Relaxed);
    arch::timer_disarm();
}

/// Timer interrupts taken since boot.
pub(crate) fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Nanoseconds since the counter started, which is some point inside firmware.
///
/// Only differences between two of these mean anything. The arithmetic is done
/// in 128 bits because the obvious 64-bit form overflows after about eighteen
/// seconds at a 1 `GHz` counter, which is exactly long enough to pass every
/// test and fail on a real machine.
pub(crate) fn now_nanos() -> u64 {
    let hz = arch::counter_hz();
    if hz == 0 {
        return 0;
    }
    let scaled = u128::from(arch::counter_now()) * NANOS_PER_SECOND / u128::from(hz);
    // Saturating rather than truncating: a wrong answer that is obviously
    // wrong beats one that looks plausible.
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

/// How fast the free-running counter counts.
pub(crate) fn counter_hz() -> u64 {
    arch::counter_hz()
}
