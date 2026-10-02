/*
 * Ferrix — SysML v2 model, part 15: the low-level requirements of sched/.
 *
 * What each unit of src/kernel/src/sched/ does, as `ItemLowLevel` requirements
 * (part 13 defines the format, part 14's pilot settled how they are cut).
 * The scheduling decision itself is `src/lib/kernel/sched`'s `RunQueue`, an
 * EEVDF queue the kernel's per-processor queue wraps; its host tests are the
 * checks, and each requirement names the kernel function that carries the
 * behaviour into the item as its `unit`.
 *
 * The slice starts with the two requirements FX-0502's fix created. The
 * rest of sched/ is check-writing work, in the baseline until then.
 *
 * Ids are `L.sched.<n>`, numbered through the subsystem.
 */
package FerrixSchedRequirements {
    private import FerrixItemRequirements::*;

    package Decisions {
        doc /* src/kernel/src/sched/queue.rs and src/kernel/src/sched/mod.rs: when a
             * processor next decides, and what a yield asks for. */

        requirement <'L.sched.1'> aDecisionWithinASlice : ItemLowLevel {
            attribute :>> statement = "While any task waits on a processor's
                queue, CpuQueue::arm_timer shall arm the next decision no more
                than one configured slice away, however long a request the
                running task holds.";
            attribute :>> criterion = "With an entity queued that is not
                eligible and the running one having yielded 600 times, so that
                its remaining request exceeds 100 slices, the queue's next
                decision is at most one slice away; with nothing queued there
                is none.";
            attribute :>> parent = "H.SCHED.2";
            attribute :>> unit = "sched::queue::CpuQueue::arm_timer";
        }
        requirement <'L.sched.2'> aLoneYieldAsksNothing : ItemLowLevel {
            attribute :>> statement = "A yield by the only task on a processor's
                queue shall leave its request unchanged.";
            attribute :>> criterion = "After 600 yields with nothing else
                queued, the running entity's remaining slice is what it was
                before the first.";
            attribute :>> parent = "H.SCHED.2";
            attribute :>> unit = "sched::yield_now";
        }
    }

    package Wakes {
        doc /* src/kernel/src/timer.rs and src/kernel/src/sched/mod.rs: the
             * timer's skipped arm, the clock, and the decisions a channel
             * round trip's wakes defer (docs/OPAQUE-KERNEL.md §9.4, items 1,
             * 2, 3 and 6; the consultant's conditions of 2026-10-02). */

        requirement <'L.sched.5'> aSkippedArmIsNeverLate : ItemLowLevel {
            attribute :>> statement = "timer::after shall keep, for each
                processor, the clock read after the hardware was armed plus the
                delay asked for, and shall leave a one-shot armed only when that is
                no later than the new deadline; otherwise it shall arm for the new
                one, so that no interrupt asked for is served later than the
                hardware's own one-tick rounding of a delay shorter than a tick.";
            attribute :>> criterion = "On the boot processor, a 2 ms one-shot asked
                for while a 1 s one is armed, and one asked for before a 1 s one,
                each fire within 200 ms of the 2 ms deadline (the `arm` line).";
            attribute :>> parent = "H.SCHED.5";
            attribute :>> unit = ("timer::after", "timer::stop", "timer::on_tick",
                "timer::armed_slot");
        }
        requirement <'L.sched.6'> theClockIsTheWideFormula : ItemLowLevel {
            attribute :>> statement = "now_nanos shall convert the counter to
                nanoseconds as ticks times 10^9 over the counter's rate, rounded
                down, exactly, and u64::MAX past what 64 bits hold.";
            attribute :>> criterion = "At 12 counter rates from 1 Hz to u64::MAX
                and 10 tick values at each, from 0 to u64::MAX across a second's
                boundaries and the saturation point, the two-division conversion
                equals the 128-bit formula (ferrix-vdso's host test).";
            attribute :>> parent = "H.SCHED.9";
            attribute :>> unit = ("timer::now_nanos", "timer::ticks_to_nanos");
        }
        requirement <'L.sched.7'> aDeferredDecisionIsBounded : ItemLowLevel {
            attribute :>> statement = "A wake made inside a system call shall
                leave its decision to the call's end (sched::call_left), the next
                preempt_enable that brings the count to zero, or the next interrupt
                exit, whichever comes first; and a Sync wake shall arm the waker's
                processor's timer for its next decision, so that a waker that does
                not block leaves the woken task waiting at most one slice.";
            attribute :>> criterion = "Argued in MEMORY-AND-TIMING §2.2c: no check
                times the deferral, which sits inside H.SCHED.2's bound of one slice
                plus the timer overruns served.";
            attribute :>> parent = "H.SCHED.2";
            attribute :>> unit = ("sched::resched_here", "sched::call_entered",
                "sched::call_left", "sched::carry_in_call", "sched::kick_after_wake",
                "sched::wake_at_home");
        }
        requirement <'L.sched.8'> aSyncWakeMovesOnlyWhereAllowed : ItemLowLevel {
            attribute :>> statement = "A Sync wake shall move the task it wakes
                onto the waker's processor only when the task is asleep at its
                home and in nothing else, its affinity allows the waker's
                processor, and nothing else is queued there, deciding under both
                processors' queue locks, and shall otherwise wake it where it is.";
            attribute :>> criterion = "At two processors or more, a reader blocked
                in channel_write_read and free to move, woken by a Sync write from
                another processor, answers every time and is moved in at least one
                of 8 rounds; a reader pinned to processor 1 woken from processor 0
                answers on processor 1 in each of 8 rounds (the `sync` line).";
            attribute :>> parent = ("H.SCHED.4", "H.SCHED.1");
            attribute :>> unit = ("sched::wake_with", "sched::wake_onto",
                "sched::may_place", "sched::asleep_at_home",
                "sched::task::Task::holds_slots");
        }
    }
}
