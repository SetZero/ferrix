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

    package Instrumentation {
        doc /* src/kernel/src/sched/trip.rs: the seam's trip trace, whose stamp
             * points and counts sit on the scheduler's switch path, its waits
             * and the interrupt paths. Inert unless stage 10's hop check is
             * running its traced reads, so that those paths do no work for it
             * otherwise; H.SCHED.2's bound is measured with the trace down. */

        requirement <'L.sched.3'> theTripTraceIsInertOutsideTheHopCheck : ItemLowLevel {
            attribute :>> statement = "sched::trip::disarm shall lower the seam's trip
                trace, after which no stamp point or count of sched::trip shall record
                anything, and the trace shall be down whenever stage 10's hop check is
                not running its traced reads.";
            attribute :>> criterion = "The trace reads down right after the hop
                check's traced run and again just before the boot's success marker;
                with the hop check's disarm skipped, the boot stops at the first of
                those with that check's own failure.";
            attribute :>> parent = "H.SCHED.2";
            attribute :>> unit = ("sched::trip::disarm", "sched::trip::armed");
        }
    }
}
