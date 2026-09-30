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
        doc /* src/kernel/src/sched/mod.rs: a wake that moves the task it wakes
             * onto the waker's processor, and the states that decide whether
             * it may. */

        requirement <'L.sched.3'> aMovedWakeLosesNothing : ItemLowLevel {
            attribute :>> statement = "wake_with shall move a blocked task onto
                the waker's processor only when, under both processors' queue
                locks, it is neither its home's running task nor queued, its home's
                sleeper set has given it up and it holds both its slots, and
                nothing is queued on the waker's processor behind the waker; and
                otherwise shall make it runnable where it is. Every task so woken
                shall run.";
            attribute :>> criterion = "One traveller per processor (at most four),
                each relayed by sync wakes between anchors pinned one per
                processor, makes 1000 hops and its anchors 1000 between them,
                exactly, within 30 seconds; at least one wake moved a task and the
                travellers ran on at least two processors. Hand-overs found by the
                5 ms recheck rather than a wake are counted, not failed (the `sync`
                line).";
            attribute :>> parent = "H.SCHED.1";
            attribute :>> unit = ("sched::wake_with", "sched::wake_onto",
                "sched::asleep_at_home", "sched::has_room",
                "sched::wait::WaitQueue::wake_all_with");
        }
        requirement <'L.sched.4'> aMovedWakeKeepsAffinity : ItemLowLevel {
            attribute :>> statement = "wake_with shall never move a task onto a
                processor outside its affinity or its scheduling domain.";
            attribute :>> criterion = "A task confined to processor 1, woken by 4
                sync wakes from processor 0 each once it is all the way asleep, is
                moved by none of them and runs on processor 1 alone; a task free to
                run anywhere, woken the same way, is moved by at least one.";
            attribute :>> parent = "H.SCHED.1";
            attribute :>> unit = "sched::may_place";
        }
    }

    package Idle {
        doc /* src/kernel/src/sched/mod.rs: what an idle processor does before it
             * halts, and how a kick reaches it meanwhile. */

        requirement <'L.sched.5'> aPollingProcessorIsKickedByItsFlag : ItemLowLevel {
            attribute :>> statement = "Where arch::polls_before_halt says so, an idle
                processor shall poll its reschedule flag for at most HALT_POLL_NANOS
                (50 us) of its idle task's time before it halts, advertising that it
                polls; kick shall then set the flag and send no interrupt; and the
                poll shall end, and its processor look for work again rather than
                halt, when an interrupt is taken during it.";
            attribute :>> criterion = "A task confined to processor 1, woken 8 times
                from processor 0 each once it is asleep and processor 1 is seen
                polling, runs every time, and at least one of the kicks found the
                processor polling and sent no interrupt (the `poll` line).";
            attribute :>> parent = "H.SCHED.1";
            attribute :>> unit = ("sched::halt_poll", "sched::kick",
                "sched::preempt_on_irq_exit");
        }
    }
}
