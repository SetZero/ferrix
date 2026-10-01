//! The restart policy (§5.4), alone, as `devmgr` will use it.

use core::time::Duration;

use crate::event::Exit;
use crate::kind::Restart;
use crate::restart::{Backoff, Budget, Decision, Ended, Policy, wanted};
use crate::time::Instant;
use crate::value::Signal;

#[test]
fn which_ends_restart_is_systemds_table() {
    let ends = [
        Ended::Success,
        Ended::ExitCode,
        Ended::Signal,
        Ended::Timeout,
        Ended::Watchdog,
        Ended::CoreDump,
        Ended::OomKill,
    ];
    let row = |restart| ends.map(|ended| wanted(restart, ended));
    assert_eq!(row(Restart::No), [false; 7]);
    assert_eq!(row(Restart::Always), [true; 7]);
    assert_eq!(
        row(Restart::OnSuccess),
        [true, false, false, false, false, false, false]
    );
    assert_eq!(
        row(Restart::OnFailure),
        [false, true, true, true, true, true, true]
    );
    assert_eq!(
        row(Restart::OnAbnormal),
        [false, false, true, true, true, true, false]
    );
    assert_eq!(
        row(Restart::OnAbort),
        [false, false, true, false, false, true, false]
    );
    assert_eq!(
        row(Restart::OnWatchdog),
        [false, false, false, false, true, false, false]
    );
    assert!(!wanted(Restart::Always, Ended::StartLimitHit));
}

#[test]
fn clean_signals_are_success() {
    let signal = |number, core| {
        Ended::of(Exit::Signal {
            signal: Signal(number),
            core,
        })
    };
    assert_eq!(Ended::of(Exit::Code(0)), Ended::Success);
    assert_eq!(Ended::of(Exit::Code(2)), Ended::ExitCode);
    for clean in [1, 2, 13, 15] {
        assert_eq!(signal(clean, false), Ended::Success, "{clean}");
    }
    assert_eq!(signal(9, false), Ended::Signal);
    assert_eq!(signal(11, true), Ended::CoreDump);
}

#[test]
fn the_backoff_doubles_to_32_times() {
    let mut backoff = Backoff::new(Duration::from_millis(100));
    let delays: [u64; 8] =
        core::array::from_fn(|_| u64::try_from(backoff.next_delay().as_millis()).unwrap());
    assert_eq!(delays, [100, 200, 400, 800, 1600, 3200, 3200, 3200]);
    backoff.reset();
    assert_eq!(backoff.next_delay(), Duration::from_millis(100));
    let mut zero = Backoff::new(Duration::ZERO);
    assert_eq!(zero.next_delay(), Duration::ZERO);
    for _ in 0..100 {
        let _ = zero.next_delay();
    }
    assert_eq!(zero.steps(), 101, "counting past the cap does not overflow");
}

#[test]
fn a_budget_with_a_clock_is_a_rate() {
    let mut budget = Budget::new(5, Duration::from_secs(10));
    let at = |secs: u64| Some(Instant::from_millis(secs * 1000));
    for second in 0..5 {
        assert!(budget.start(at(second)), "start {second}");
    }
    assert!(!budget.start(at(5)), "a sixth within ten seconds");
    assert!(!budget.start(at(9)));
    assert!(budget.start(at(10)), "the first has aged out");
    assert!(!budget.start(at(10)));
    budget.reset();
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_budget_without_a_clock_is_a_count() {
    let mut budget = Budget::new(8, Duration::MAX);
    for start in 0..8 {
        assert!(budget.start(None), "start {start}");
    }
    assert!(!budget.start(None), "devmgr's eight restarts, counted");
    assert!(
        Budget::new(0, Duration::from_secs(10)).start(None),
        "a burst of 0 is no limit"
    );
    assert!(Budget::new(3, Duration::ZERO).unlimited());
}

#[test]
fn the_policy_decides_now_at_or_never() {
    let mut policy = Policy::new(
        Restart::OnFailure,
        Duration::from_secs(1),
        3,
        Duration::from_secs(60),
    );
    let t = Instant::from_millis(1000);
    assert_eq!(policy.decide(Ended::Success, Some(t)), Decision::Stay);
    assert_eq!(
        policy.decide(Ended::ExitCode, Some(t)),
        Decision::At(t + Duration::from_secs(1))
    );
    assert_eq!(
        policy.decide(Ended::ExitCode, Some(t)),
        Decision::At(t + Duration::from_secs(2))
    );
    assert_eq!(
        policy.decide(Ended::ExitCode, None),
        Decision::Now,
        "no clock, no delay"
    );
    assert_eq!(policy.decide(Ended::ExitCode, Some(t)), Decision::GiveUp);
    policy.reset();
    assert!(policy.start(Some(t)));
    assert_eq!(policy.backoff.steps(), 0);
}
