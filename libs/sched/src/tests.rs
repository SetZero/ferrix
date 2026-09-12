//! Tests for the scheduling logic.
//!
//! Three kinds, and the last two are the ones worth having. The small tests
//! pin one rule each. The randomised test drives the queue through tens of
//! thousands of mixed operations, checks every invariant after each, and
//! checks every pick against a brute-force search over the same entities —
//! which is what would catch a tree that picked a plausible entity rather than
//! the right one. And the simulations run a CPU for simulated minutes and
//! require EEVDF's own bound at every decision: no entity's lag, measured in
//! real service against its weighted share, may exceed the largest request
//! anyone was given. That bound is what stage 5's boot test measures on a
//! running machine; proving it here first is what makes a failure there a bug
//! in the kernel's half rather than in this one.

extern crate std;

use alloc::vec::Vec;

use super::*;

/// Three milliseconds, the slice the kernel uses.
const SLICE: u64 = 3_000_000;

fn queue() -> RunQueue<u64> {
    RunQueue::new(Config { slice_ns: SLICE }).unwrap()
}

#[track_caller]
fn check<T>(queue: &RunQueue<T>) {
    assert_eq!(
        queue.check_invariants(),
        Ok(()),
        "the queue broke its own invariant"
    );
}

/// A small deterministic generator: xorshift64*, so a failure reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

// ---------------------------------------------------------------------------
// Weights
// ---------------------------------------------------------------------------

#[test]
fn nice_zero_is_the_unit_weight() {
    assert_eq!(weight_of_nice(0), Some(NICE_0_WEIGHT), "nice 0 is the unit");
}

#[test]
fn nice_outside_the_table_has_no_weight() {
    assert_eq!(weight_of_nice(-21), None, "below -20");
    assert_eq!(weight_of_nice(20), None, "above 19");
    assert_eq!(weight_of_nice(i32::MIN), None, "the far end");
}

#[test]
fn each_nice_level_is_about_a_quarter_heavier_than_the_next() {
    for nice in -20..19 {
        let this = f64::from(weight_of_nice(nice).unwrap());
        let next = f64::from(weight_of_nice(nice + 1).unwrap());
        let ratio = this / next;
        assert!(
            (1.20..1.30).contains(&ratio),
            "nice {nice} is {ratio} times nice {}",
            nice + 1
        );
    }
}

// ---------------------------------------------------------------------------
// One rule each
// ---------------------------------------------------------------------------

#[test]
fn a_zero_slice_is_refused() {
    assert_eq!(
        RunQueue::<u64>::new(Config { slice_ns: 0 }).map(|_| ()),
        Err(SchedError::ZeroSlice),
        "a zero slice"
    );
}

#[test]
fn an_empty_queue_picks_nothing() {
    let mut queue = queue();
    assert_eq!(queue.pick_next(), None, "nothing to pick");
    assert!(queue.is_empty(), "and nothing there");
    assert!(!queue.should_preempt(), "nothing to preempt for");
    check(&queue);
}

#[test]
fn a_zero_weight_is_refused_and_the_payload_handed_back() {
    let mut queue = queue();
    let refused = queue.enqueue(1, 77, EntityState::new(0)).unwrap_err();
    assert_eq!(refused.reason, SchedError::ZeroWeight, "why");
    assert_eq!(refused.payload, 77, "the payload comes back");
    assert!(queue.is_empty(), "nothing was queued");
}

#[test]
fn an_identifier_is_refused_twice_whether_queued_or_running() {
    let mut queue = queue();
    queue.enqueue(1, 1, EntityState::new(1024)).unwrap();
    let queued = queue.enqueue(1, 2, EntityState::new(1024)).unwrap_err();
    assert_eq!(queued.reason, SchedError::Duplicate(1), "while queued");

    assert_eq!(queue.pick_next(), Some(&1), "now running");
    let running = queue.enqueue(1, 3, EntityState::new(1024)).unwrap_err();
    assert_eq!(running.reason, SchedError::Duplicate(1), "while running");
    check(&queue);
}

#[test]
fn the_only_entity_runs_and_keeps_running() {
    let mut queue = queue();
    queue.enqueue(7, 70, EntityState::new(1024)).unwrap();
    for _ in 0..10 {
        assert_eq!(queue.pick_next(), Some(&70), "the only candidate");
        let _ = queue.update_curr(SLICE);
        check(&queue);
    }
    assert!(!queue.should_preempt(), "nothing to give way to");
}

#[test]
fn equal_entities_take_turns_a_slice_at_a_time() {
    let mut queue = queue();
    for id in 1..=3 {
        queue.enqueue(id, id, EntityState::new(1024)).unwrap();
    }
    let mut order = Vec::new();
    for _ in 0..9 {
        order.push(*queue.pick_next().unwrap());
        assert!(queue.update_curr(SLICE), "a whole slice is spent");
        check(&queue);
    }
    assert_eq!(order, [1, 2, 3, 1, 2, 3, 1, 2, 3], "round robin, in effect");
}

#[test]
fn running_charges_virtual_time_in_inverse_proportion_to_weight() {
    let mut queue = queue();
    queue.enqueue(1, 1, EntityState::new(2048)).unwrap();
    let _ = queue.pick_next();
    let before = queue.get(1).unwrap().vruntime;
    let _ = queue.update_curr(1_000_000);
    let view = queue.get(1).unwrap();
    assert_eq!(
        view.vruntime - before,
        500_000,
        "twice the weight, half the rate"
    );
    assert_eq!(view.sum_exec, 1_000_000, "real time is real time");
}

#[test]
fn a_spent_slice_asks_for_a_new_deadline_from_where_the_entity_is() {
    let mut queue = queue();
    queue.enqueue(1, 1, EntityState::new(1024)).unwrap();
    let _ = queue.pick_next();
    assert!(!queue.update_curr(SLICE - 1), "one nanosecond short");
    assert!(queue.update_curr(1_000), "and past it");
    let view = queue.get(1).unwrap();
    assert_eq!(
        view.deadline,
        view.vruntime + SLICE,
        "the next request runs from here, overrun forgiven"
    );
}

#[test]
fn the_remaining_slice_is_real_time_whatever_the_weight() {
    for weight in [15, 1024, 2048, 88761] {
        // A slice is stored as virtual time, and converting back rounds down
        // once per virtual nanosecond lost — so the error a heavy entity sees
        // is its weight in units of the unit weight, and no more. Eighty-seven
        // nanoseconds at nice -20, against a three-millisecond slice.
        let tolerance = u64::from(weight) / u64::from(NICE_0_WEIGHT) + 1;
        let mut queue = queue();
        queue.enqueue(1, 1, EntityState::new(weight)).unwrap();
        let _ = queue.pick_next();
        let remaining = queue.remaining_ns().unwrap();
        assert!(
            remaining.abs_diff(SLICE) <= tolerance,
            "weight {weight} has {remaining} ns left of a {SLICE} ns slice"
        );
        let _ = queue.update_curr(SLICE / 3);
        let remaining = queue.remaining_ns().unwrap();
        assert!(
            remaining.abs_diff(SLICE - SLICE / 3) <= tolerance,
            "weight {weight} has {remaining} ns left after a third"
        );
    }
}

#[test]
fn an_earlier_deadline_that_is_not_eligible_waits() {
    let mut queue = queue();
    queue.enqueue(1, 1, EntityState::new(1024)).unwrap();
    // A heavy entity arriving a little ahead of its share. Its slice is a
    // sliver in virtual time — a heavy entity reaches the same real slice in
    // far less of it — so its deadline lands before the light entity's even
    // though it starts ahead of the average, and it is not eligible.
    //
    // Two microseconds of lag rather than sixty: placement scales lag by the
    // new total weight over the old, which for a weight of 88761 arriving
    // beside one of 1024 is a factor of eighty-seven.
    let ahead = EntityState {
        weight: 88761,
        vlag: -2_000,
        sum_exec: 0,
    };
    queue.enqueue(2, 2, ahead).unwrap();
    check(&queue);

    let light = queue.get(1).unwrap();
    let heavy = queue.get(2).unwrap();
    assert!(
        before(heavy.deadline, light.deadline),
        "the heavy one sorts first"
    );
    assert!(heavy.lag < 0, "and has had more than its share");
    assert_eq!(queue.pick_next(), Some(&1), "so the eligible one runs");
}

#[test]
fn a_new_entity_arrives_owed_nothing() {
    let mut queue = queue();
    queue.enqueue(1, 1, EntityState::new(1024)).unwrap();
    let _ = queue.pick_next();
    let _ = queue.update_curr(10 * SLICE);
    queue.enqueue(2, 2, EntityState::new(3121)).unwrap();
    let lag = queue.lag(2).unwrap();
    assert!(lag.abs() <= 1, "a newcomer has lag {lag}");
    check(&queue);
}

#[test]
fn lag_survives_leaving_and_coming_back() {
    let mut queue = queue();
    for id in 1..=4 {
        queue
            .enqueue(
                id,
                id,
                EntityState::new(weight_of_nice(id as i32 - 2).unwrap()),
            )
            .unwrap();
    }
    // Run a few slices so the lags are not all zero.
    for _ in 0..5 {
        let _ = queue.pick_next();
        let _ = queue.update_curr(SLICE);
    }
    let _ = queue.pick_next();
    let running = queue.current_id().unwrap();
    let sleeper = (1..=4).find(|id| *id != running).unwrap();
    let lag_before = queue.lag(sleeper).unwrap();

    let (payload, state) = queue.remove(sleeper).unwrap();
    assert_eq!(state.vlag, lag_before, "it leaves with the lag it had");
    check(&queue);

    queue.enqueue(sleeper, payload, state).unwrap();
    let lag_after = queue.lag(sleeper).unwrap();
    assert!(
        lag_after.abs_diff(lag_before) <= 2,
        "it left with {lag_before} and came back with {lag_after}"
    );
    check(&queue);
}

#[test]
fn lag_carried_back_is_clamped_to_two_slices() {
    let mut queue = queue();
    queue.enqueue(1, 1, EntityState::new(1024)).unwrap();
    let owed = EntityState {
        weight: 1024,
        vlag: i64::MAX / 2,
        sum_exec: 0,
    };
    queue.enqueue(2, 2, owed).unwrap();
    let lag = queue.lag(2).unwrap();
    assert!(
        lag.abs_diff(2 * SLICE as i64) <= 2,
        "a long sleep collects {lag}, not a lifetime"
    );
}

#[test]
fn virtual_time_is_the_weighted_average_of_runtimes() {
    let mut rng = Rng(0x5EED);
    let mut queue = queue();
    for id in 0..20 {
        let weight = weight_of_nice(rng.below(40) as i32 - 20).unwrap();
        queue.enqueue(id, id, EntityState::new(weight)).unwrap();
        let _ = queue.pick_next();
        let _ = queue.update_curr(rng.below(SLICE));
    }
    let mut weighted = 0i128;
    let mut total = 0i128;
    queue.for_each(|view| {
        weighted += i128::from(view.weight) * i128::from(view.vruntime);
        total += i128::from(view.weight);
    });
    let exact = weighted.div_euclid(total) as u64;
    assert_eq!(
        queue.avg_vruntime(),
        exact,
        "the queue's average is the average"
    );
}

#[test]
fn yielding_lets_the_next_deadline_go_first() {
    let mut queue = queue();
    queue.enqueue(1, 1, EntityState::new(1024)).unwrap();
    queue.enqueue(2, 2, EntityState::new(1024)).unwrap();
    assert_eq!(queue.pick_next(), Some(&1), "one first");
    let _ = queue.update_curr(SLICE / 10);
    queue.yield_curr();
    assert_eq!(
        queue.pick_next(),
        Some(&2),
        "then two, having been yielded to"
    );
}

#[test]
fn a_wakeup_with_an_earlier_eligible_deadline_preempts() {
    let mut queue = queue();
    queue.enqueue(1, 1, EntityState::new(1024)).unwrap();
    let _ = queue.pick_next();
    let _ = queue.update_curr(SLICE / 2);
    assert!(!queue.should_preempt(), "nothing to preempt for");

    // Owed service: behind the running entity, and with a deadline before
    // the one the running entity is working towards.
    let owed = EntityState {
        weight: 1024,
        vlag: 1_000_000,
        sum_exec: 0,
    };
    queue.enqueue(2, 2, owed).unwrap();
    assert!(queue.should_preempt(), "it is owed, and due sooner");
    assert_eq!(queue.pick_next(), Some(&2), "and runs");
}

#[test]
fn a_wakeup_that_is_ahead_does_not_preempt() {
    let mut queue = queue();
    queue.enqueue(1, 1, EntityState::new(1024)).unwrap();
    let _ = queue.pick_next();
    let ahead = EntityState {
        weight: 1024,
        vlag: -1_000_000,
        sum_exec: 0,
    };
    queue.enqueue(2, 2, ahead).unwrap();
    assert!(!queue.should_preempt(), "it has had more than its share");
}

#[test]
fn the_steal_candidate_is_the_latest_deadline_that_may_move() {
    let mut queue = queue();
    for id in 1..=5 {
        queue.enqueue(id, id, EntityState::new(1024)).unwrap();
        let _ = queue.pick_next();
        let _ = queue.update_curr(SLICE / 7);
    }
    let _ = queue.pick_next();
    let running = queue.current_id().unwrap();

    let mut latest = None;
    queue.for_each(|view| {
        if !view.running {
            latest = Some(view.id);
        }
    });
    assert_eq!(
        queue.latest_where(|_| true),
        latest,
        "the last in deadline order"
    );
    assert_ne!(
        queue.latest_where(|_| true),
        Some(running),
        "never the running one"
    );
    assert_eq!(queue.latest_where(|_| false), None, "nothing may move");
}

#[test]
fn removing_the_running_entity_by_name_works_like_remove_curr() {
    let mut queue = queue();
    queue.enqueue(1, 10, EntityState::new(1024)).unwrap();
    queue.enqueue(2, 20, EntityState::new(1024)).unwrap();
    let _ = queue.pick_next();
    let running = queue.current_id().unwrap();
    let (payload, _) = queue.remove(running).unwrap();
    assert_eq!(payload, running * 10, "the running entity's payload");
    assert_eq!(queue.current(), None, "nothing running now");
    assert_eq!(queue.len(), 1, "one left");
    check(&queue);
}

#[test]
fn virtual_time_that_wraps_still_orders() {
    let mut queue = queue();
    queue.enqueue(1, 1, EntityState::new(1024)).unwrap();
    queue.enqueue(2, 2, EntityState::new(1024)).unwrap();
    // Each quarter of the range is well under the half the comparison needs,
    // and eight of them wrap the counter twice.
    let quarter = u64::MAX / 4;
    let mut last = 0;
    for step in 0..8 {
        let running = *queue.pick_next().unwrap();
        assert_ne!(running, last, "step {step}: the two still alternate");
        last = running;
        assert!(
            queue.update_curr(quarter),
            "a quarter of the range is a slice"
        );
        check(&queue);
    }
}

// ---------------------------------------------------------------------------
// Everything at once
// ---------------------------------------------------------------------------

/// The pick a brute-force search over `queue`'s entities would make: the
/// earliest deadline, then the lowest identifier, among those whose lag is
/// not negative.
fn brute_force_pick(queue: &RunQueue<u64>) -> Option<u64> {
    let mut best: Option<(u64, u64)> = None;
    queue.for_each(|view| {
        if view.lag < 0 {
            return;
        }
        let better = match best {
            None => true,
            Some((deadline, id)) => {
                before(view.deadline, deadline) || (view.deadline == deadline && view.id < id)
            }
        };
        if better {
            best = Some((view.deadline, view.id));
        }
    });
    best.map(|(_, id)| id)
}

#[test]
fn random_operations_keep_every_invariant_and_pick_what_brute_force_picks() {
    let mut rng = Rng(0xFE44_1C5E_ED00_0001);
    let mut queue = queue();
    let mut away: Vec<(u64, EntityState)> = Vec::new();
    let mut next_id = 0u64;
    let mut picks = 0u32;

    for _ in 0..40_000 {
        match rng.below(10) {
            0 | 1 if queue.len() < 64 => {
                let weight = weight_of_nice(rng.below(40) as i32 - 20).unwrap();
                queue
                    .enqueue(next_id, next_id, EntityState::new(weight))
                    .unwrap();
                next_id += 1;
            }
            2 if !away.is_empty() => {
                let (id, state) = away.swap_remove(rng.below(away.len() as u64) as usize);
                queue.enqueue(id, id, state).unwrap();
            }
            3 if queue.queued() > 0 => {
                let target = queue.latest_where(|_| true).unwrap();
                let (_, state) = queue.remove(target).unwrap();
                away.push((target, state));
            }
            4 => {
                if let Some((id, _, state)) = queue.remove_curr() {
                    away.push((id, state));
                }
            }
            5 => queue.yield_curr(),
            6..=8 => {
                let _ = queue.update_curr(rng.below(2 * SLICE));
            }
            _ => {
                // The running entity competes too, so the brute force sees
                // everything the pick will.
                let expected = brute_force_pick(&queue);
                let picked = queue.pick_next().copied();
                assert_eq!(
                    picked, expected,
                    "the tree picked differently from brute force"
                );
                picks += 1;
            }
        }
        check(&queue);
    }
    assert!(picks > 1000, "the test made only {picks} picks");
}

// ---------------------------------------------------------------------------
// The fairness bound, in simulation
// ---------------------------------------------------------------------------

/// Run a simulated CPU over `weights`, a slice at a time plus up to
/// `lateness` of timer overrun, for `decisions` scheduling decisions. At every
/// decision, measure each entity's lag in real service — its weighted share
/// of everything run so far, less what it has had — and return the largest
/// seen.
///
/// Measured from `sum_exec`, the real nanoseconds the queue was charged, not
/// from the queue's own lag field: the point is to check the virtual-time
/// arithmetic against what it is supposed to achieve, not against itself.
fn worst_real_lag(weights: &[u32], lateness: u64, decisions: u32, seed: u64) -> u64 {
    let mut rng = Rng(seed);
    let mut queue = queue();
    for (id, weight) in weights.iter().enumerate() {
        queue
            .enqueue(id as u64, id as u64, EntityState::new(*weight))
            .unwrap();
    }
    let total_weight: u128 = weights.iter().map(|weight| u128::from(*weight)).sum();

    let mut worst = 0u64;
    for _ in 0..decisions {
        let _ = queue.pick_next();
        let remaining = queue.remaining_ns().unwrap();
        let late = if lateness == 0 {
            0
        } else {
            rng.below(lateness + 1)
        };
        let _ = queue.update_curr(remaining + late);

        let mut service = Vec::new();
        queue.for_each(|view| service.push((view.weight, view.sum_exec)));
        let total: u128 = service.iter().map(|(_, ran)| u128::from(*ran)).sum();
        for (weight, ran) in service {
            let share = total * u128::from(weight) / total_weight;
            worst = worst.max(share.abs_diff(u128::from(ran)) as u64);
        }
    }
    check(&queue);
    worst
}

#[test]
fn equal_weights_stay_within_one_slice_of_their_share() {
    let worst = worst_real_lag(&[1024; 8], 0, 50_000, 1);
    assert!(
        worst <= SLICE,
        "an entity strayed {worst} ns from its share"
    );
}

#[test]
fn mixed_weights_stay_within_one_slice_of_their_share() {
    // Powers of two, so every conversion between real and virtual time is
    // exact and the bound can be checked to the nanosecond.
    let worst = worst_real_lag(&[256, 512, 1024, 1024, 2048, 4096, 8192], 0, 50_000, 2);
    assert!(
        worst <= SLICE,
        "an entity strayed {worst} ns from its share"
    );
}

#[test]
fn a_late_timer_widens_the_bound_by_exactly_its_lateness() {
    // The bound is the largest request actually served, and a request cut
    // late by up to half a millisecond is up to half a millisecond longer.
    let lateness = 500_000;
    let worst = worst_real_lag(&[512, 1024, 1024, 2048, 4096], lateness, 50_000, 3);
    assert!(
        worst <= SLICE + lateness,
        "an entity strayed {worst} ns from its share, past {} ns",
        SLICE + lateness
    );
}

#[test]
fn nice_weights_stay_within_one_slice_of_their_share() {
    // Weights that do not divide evenly, so every update rounds. The rounding
    // is at most a nanosecond of virtual time per update, which over this
    // many decisions is still far inside the tolerance allowed for it.
    let weights: Vec<u32> = [-5, -3, 0, 0, 2, 5, 10]
        .iter()
        .map(|nice| weight_of_nice(*nice).unwrap())
        .collect();
    let worst = worst_real_lag(&weights, 250_000, 50_000, 4);
    assert!(
        worst <= SLICE + 250_000 + 100_000,
        "an entity strayed {worst} ns from its share"
    );
}

#[test]
fn shares_come_out_in_proportion_to_weight() {
    let weights = [1024u32, 2048, 4096];
    let mut queue = queue();
    for (id, weight) in weights.iter().enumerate() {
        queue
            .enqueue(id as u64, id as u64, EntityState::new(*weight))
            .unwrap();
    }
    for _ in 0..30_000 {
        let _ = queue.pick_next();
        let remaining = queue.remaining_ns().unwrap();
        let _ = queue.update_curr(remaining);
    }
    let mut ran = [0u64; 3];
    queue.for_each(|view| ran[view.id as usize] = view.sum_exec);
    let total: u64 = ran.iter().sum();
    for (index, weight) in weights.iter().enumerate() {
        let expected = total / 7 * u64::from(*weight) / 1024;
        assert!(
            ran[index].abs_diff(expected) <= SLICE,
            "weight {weight} ran {} ns, owed {expected} ns",
            ran[index]
        );
    }
}

// ---------------------------------------------------------------------------
// Domains and modes
// ---------------------------------------------------------------------------

#[test]
fn throughput_is_fair_then_idle_and_steals() {
    assert_eq!(
        Mode::Throughput.classes(),
        Ok(&[Class::Fair, Class::Idle][..]),
        "the Throughput stack"
    );
    assert!(Mode::Throughput.steals_work(), "Throughput steals");
}

#[test]
fn the_real_time_modes_are_named_and_refused() {
    for mode in [Mode::SoftRt, Mode::HardRt] {
        assert_eq!(
            mode.classes(),
            Err(SchedError::ModeUnavailable(mode)),
            "{} is stage 14's",
            mode.name()
        );
        assert!(!mode.steals_work(), "{} does not steal", mode.name());
        assert_eq!(
            Domain::new(CpuSet::first(1).unwrap(), mode),
            Err(SchedError::ModeUnavailable(mode)),
            "and no domain can be built in it"
        );
    }
}

#[test]
fn a_cpu_set_holds_what_it_is_given_and_nothing_else() {
    let mut set = CpuSet::empty();
    assert!(set.is_empty(), "starts empty");
    for cpu in [0, 1, 63, 64, 200, MAX_CPUS - 1] {
        set.insert(cpu).unwrap();
    }
    assert_eq!(set.len(), 6, "six in");
    assert_eq!(
        set.iter().collect::<Vec<_>>(),
        [0, 1, 63, 64, 200, MAX_CPUS - 1],
        "lowest first"
    );
    assert!(!set.contains(2), "not given");
    assert_eq!(
        set.insert(MAX_CPUS),
        Err(SchedError::NoSuchCpu(MAX_CPUS)),
        "past the end"
    );
    assert!(!set.contains(MAX_CPUS), "and not there");
}

#[test]
fn an_empty_domain_is_refused() {
    assert_eq!(
        Domain::new(CpuSet::empty(), Mode::Throughput),
        Err(SchedError::EmptyDomain),
        "no CPUs"
    );
}

#[test]
fn a_partition_must_hold_every_cpu_exactly_once() {
    let everything = Domain::new(CpuSet::first(4).unwrap(), Mode::Throughput).unwrap();
    assert_eq!(
        check_partition(&[everything], 4),
        Ok(()),
        "one domain, all of them"
    );

    let low = Domain::new(CpuSet::first(2).unwrap(), Mode::Throughput).unwrap();
    assert_eq!(
        check_partition(&[low], 4),
        Err(SchedError::Uncovered(2)),
        "two CPUs in no domain"
    );
    assert_eq!(
        check_partition(&[low, everything], 4),
        Err(SchedError::Overlap(0)),
        "two domains claiming CPU 0"
    );
    assert_eq!(
        check_partition(&[everything], 3),
        Err(SchedError::NoSuchCpu(3)),
        "a domain naming a CPU the machine lacks"
    );

    let mut high = CpuSet::empty();
    high.insert(2).unwrap();
    high.insert(3).unwrap();
    let high = Domain::new(high, Mode::Throughput).unwrap();
    assert_eq!(check_partition(&[low, high], 4), Ok(()), "two halves");
}
