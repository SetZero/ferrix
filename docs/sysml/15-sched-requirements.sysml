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

    package Preemption {
        doc /* src/kernel/src/sched/preempt.rs and src/kernel/src/sched/mod.rs:
             * the preemption count every crate::sync::SpinLock raises, kept in
             * each processor's record without a locked operation
             * (docs/OPAQUE-KERNEL.md §9.8, 2b; the consultant's OK IF of
             * 2026-10-02, condition 2). */

        requirement <'L.sched.20'> theCountStaysWithItsProcessor : ItemLowLevel {
            attribute :>> statement = "A preemption-disabling lock, and a raise by
                hand, shall raise and lower the preemption count and, for a lock,
                the locks held of the processor its holder runs on, in that
                processor's record and without a locked operation, so that no
                update is lost to an interrupt and none lands on another
                processor's record: on x86-64 in one GS-relative instruction, with
                every entry that can reach it running on the kernel's GS base; on
                AArch64 and ARMv7-A with every exception that can take a lock
                masked; and a report reading another processor's count shall read
                each of the count and the locks held whole.";
            attribute :>> criterion = "At two processors or more, two tasks per
                processor, preempted by the timer and moving between processors,
                take and let go of a lock 100000 times each with raises by hand
                among them and move at least once (the `preempt` line). The
                stress provokes a lost or misplaced update; what detects one is
                FX-0503's test at the release, which stops the boot when the locks
                held or the count it lowers is already zero, and, behind it, every
                processor's count and locks held read zero there by a task that
                holds nothing. The three atomicity controls (x86-64's record found
                before the add, the Arm mask removed on aarch64 and on armv7a at
                two processors) each stop the boot on FX-0503's held-zero
                message.";
            attribute :>> parent = ("H.SCHED.1", "H.SCHED.2");
            attribute :>> unit = ("sched::preempt::raise", "sched::preempt::lower",
                "sched::preempt::Preempt::disable", "sched::preempt::preempt_disable",
                "sched::preempt::PreemptState::note", "sched::preempt::preempt_count",
                "sched::preempt::locks_held", "smp::record", "arch::percpu::this_cpu_add",
                "arch::x86_64::cpu::gs_xadd");
        }
        requirement <'L.sched.21'> aCountThatCannotBeKeptStops : ItemLowLevel {
            attribute :>> statement = "The scheduler shall stop the machine
                (FX-0503), naming the site that last raised the count, when it is
                asked to switch with its processor's count raised, and when a
                release finds the count, or for a lock the locks held, already at
                zero, having put the word back first; a release that brings the
                count to zero shall make the decision an interrupt deferred, if
                interrupts are on.";
            attribute :>> criterion = "The release's cover test refuses a word
                whose count, or for a lock whose locks held, is zero, and passes one
                that covers the release (the stage-5 checks). That held-zero test is
                the detector L.sched.20's stress relies on; the negative controls of
                a task that sleeps holding a lock and of a release with nothing
                raised each stop the boot on FX-0503's message.";
            attribute :>> parent = ("H.SCHED.1", "H.SCHED.2");
            attribute :>> unit = ("sched::preempt::preempt_enable",
                "sched::preempt::Preempt::enable", "sched::preempt::enable_from",
                "sched::preempt::unmatched", "sched::preempt::covers",
                "sched::preempt::decide_deferred", "sched::schedule_from");
        }
        requirement <'L.sched.22'> mayBlockReadsItsOwnCount : ItemLowLevel {
            attribute :>> statement = "may_block shall answer true only when the
                scheduler runs, interrupts are on, a task is running, and the count
                of the processor it runs on, read so that the processor and its
                count are one processor's (one instruction on x86-64, interrupts
                masked on Arm), is zero.";
            attribute :>> criterion = "A task holding nothing may block; holding a
                preemption-disabling lock, or with the count raised by hand, it may
                not; once it lets go it may again (the stage-5 checks).";
            attribute :>> parent = "H.SCHED.1";
            attribute :>> unit = ("sched::may_block", "sched::preempt::count_here",
                "arch::percpu::this_cpu_read", "arch::x86_64::cpu::read_gs_at");
        }
        requirement <'L.sched.23'> aFailedTryLeavesTheCount : ItemLowLevel {
            attribute :>> statement = "A try_lock of a preemption-disabling lock
                that fails shall leave its processor's count and locks held as it
                found them, and with interrupts masked shall not switch; one that
                succeeds shall raise both by one until its guard drops; and
                ferrix_sync::SpinLock::try_lock_manually shall take no ticket when
                it fails.";
            attribute :>> criterion = "With a lock held, a try_lock of it with
                interrupts on and with them masked leaves the count and the locks
                held unchanged and, masked, the task's switches unchanged; a
                try_lock_manually of a held plain lock fails and changes neither;
                a successful try_lock raises both by one until it drops (the
                stage-5 checks, and ferrix-sync's host test of try_lock_manually).";
            attribute :>> parent = "H.SCHED.1";
            attribute :>> unit = ("sched::preempt::Preempt::disable",
                "sched::preempt::Preempt::enable");
        }
    }
}
