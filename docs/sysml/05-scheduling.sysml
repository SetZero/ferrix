/*
 * Ferrix — SysML v2 model, part 5: processors, time, and scheduling.
 *
 * Stages 3 to 5 run today: interrupts, a clock, every processor online,
 * IPIs, TLB shootdown, grace periods, fair locks, and tasks scheduled by
 * EEVDF in one Throughput domain. Stage 14's real-time domains are designed
 * here (docs/ARCHITECTURE.md §5) and not yet written.
 */
package FerrixScheduling {
    private import ScalarValues::*;
    private import FerrixLifecycle::*;

    // -----------------------------------------------------------------------
    // Synchronisation (libs/sync)
    // -----------------------------------------------------------------------

    #implemented part def TicketSpinLock {
        @stage { number = 4; }
        doc /* Fair by construction: arrival order is acquisition order, so the
             * worst-case wait is bounded by the CPUs ahead rather than by
             * luck. An unfair lock on a starved core is a stage-14 latency
             * bug nobody will find. Rules the callers keep: never a plain
             * SpinLock from an interrupt handler, never twice on one CPU, one
             * global order, never sleep inside. lock_manually / force_unlock
             * exist for the one case a guard cannot express: a run queue lock
             * handed from the outgoing context to the incoming one across a
             * context switch. */
        action lock;
        action lockManually;
        action forceUnlock;
    }
    #implemented part def IrqSpinLock :> TicketSpinLock {
        doc /* The same, with interrupts masked for the duration through an
             * IrqControl implemented over each architecture's mask. */
    }
    #implemented part def Once;
    #writtenAhead part def RwSpinLock {
        doc /* Many readers or one writer, writer-preferring. Not yet reached. */
    }

    // -----------------------------------------------------------------------
    // Interrupts and time (kernel/src/irq.rs, kernel/src/timer.rs)
    // -----------------------------------------------------------------------

    #implemented part def IrqTable {
        @stage { number = 3; }
        doc /* A table of 1024 slots behind an interrupt-masking lock; dispatch
             * copies the handler out and runs it after release, so two CPUs
             * taking interrupts contend for a load, not for each other's
             * handlers. The acknowledge protocol stays in arch; what arrives
             * here is the number. Unregistering waits for stage 10 and for a
             * grace period: a running handler is a read-side section. */
        attribute slots : Natural = 1024;
        attribute delivered : Natural;
        attribute unclaimed : Natural;
        action register;
        action dispatch;
    }

    #implemented part def Timer {
        @stage { number = 3; }
        doc /* Two things deliberately kept apart. The counter answers "how
             * long since boot" and is read, never waited on. The timer is an
             * interrupt scheduled for a future instant. One-shot is the
             * primitive, because the Arm timers compare against an absolute
             * instant and a tickless scheduler wants one-shot anyway. */
        action afterNanos { doc /* One-shot, in nanoseconds. */ }
        action every {
            doc /* Periodic as a schedule: tick n is due at start + n * interval
                 * and the instant tick n-1 arrived has no say in it, so a late
                 * tick is absorbed rather than propagated. Past sixteen
                 * intervals behind, the debt is written off. */
        }
        action stop;
        action ticks;
        action nowNanos;
        action counterHz;
        attribute catchUpLimit : Natural = 16;
    }

    // -----------------------------------------------------------------------
    // Processors (kernel/src/smp.rs)
    // -----------------------------------------------------------------------

    #implemented part def PerCpu {
        @stage { number = 4; }
        doc /* One record per processor, allocated once and never freed, with
             * a register pointing at it: GS base, TPIDR_EL1, or TPIDRPRW. It
             * holds its own address first so gs:0 is a pointer. Every
             * processor checks its record against what its hardware says it
             * is. */
        attribute logicalIndex : Natural;
        attribute hardwareId : Natural;
        attribute online : Boolean;
        attribute ipisTaken : Natural;
        ref part stack : KernelStack;
        #implemented part runqueue : Runqueue { @stage { number = 5; } }
        attribute needResched : Boolean;
    }

    part def KernelStack;

    #implemented part def Smp {
        @stage { number = 4; }
        doc /* Discover, start, and coordinate every processor. */
        part topology : PerCpu[1..*] ordered;
        action discover {
            doc /* From the MADT or the device tree; duplicates refused; the
                 * reading processor must be among them. */
        }
        action startSecondaries;
        action secondaryMain {
            doc /* Bring up this core's interrupt controller interface, mark
                 * online, then sleep in sti;hlt or wfi between pieces of work,
                 * woken by a broadcast IPI. */
        }
        action runEverywhere;
        action flushTlbEverywhere {
            doc /* A shootdown IPI where the hardware does not broadcast
                 * invalidation (x86-64, which invalidates global entries by
                 * toggling CR4.PGE); nothing extra where it does. */
        }
        action readSection {
            doc /* A read-side critical section: interrupts masked. */
        }
        action synchronize {
            doc /* The grace period: interrupt every other processor and wait
                 * for each to take it, which none can inside a section. What
                 * stage 14's mode switch and stage 10's handler removal wait
                 * on. */
        }
        attribute shootdowns : Natural;
        attribute gracePeriods : Natural;

        attribute offlining : Boolean {
            @deferred { reason = "Nothing takes a processor offline; records and stacks live for the life of the machine."; }
        }
    }

    // -----------------------------------------------------------------------
    // Tasks and the scheduler, stage 5
    // -----------------------------------------------------------------------

    enum def TaskState { Runnable; Blocked; Dead; }

    enum def SchedClass {
        doc /* Highest first. libs/sched names Fair and Idle today; the
             * real-time classes are stage 14's. */
        Edf;
        Fifo;
        RoundRobin;
        Eevdf;
        Idle;
    }

    enum def LinuxPolicy { SchedOther; SchedBatch; SchedIdle; SchedFifo; SchedRr; SchedDeadline; }

    #implemented part def Task {
        @stage { number = 5; }
        doc /* kernel/src/sched/task.rs: a kernel thread — a guard-paged stack,
             * a saved stack pointer, and the bookkeeping that says where it
             * is. Everything about choosing between tasks is libs/sched's.
             * The saved stack pointer is touched only by the CPU that holds
             * the owning run queue's lock at that moment. From stage 6 also a
             * user thread, 1:1, forced by the ABI; one of the nine kernel
             * objects. */
        attribute taskState : TaskState;
        attribute weight : Natural { doc /* From nice: nice 0 is 1024. */ }
        attribute pinned : Boolean;
        attribute runtimeNanos : Natural;
        attribute switches : Natural;
        attribute sleepDeadline : Natural[0..1];
        part stack : KernelStack;
        ref part cpu : PerCpu;
        #planned attribute schedClass : SchedClass { @stage { number = 14; } }
        #planned attribute policy : LinuxPolicy { @stage { number = 7; } }
        #planned attribute priority : Natural { @stage { number = 14; } doc /* 1 to 99 for FIFO/RR. */ }
        #planned attribute bandwidth : Natural { @stage { number = 14; } doc /* CBS reservation for EDF. */ }
    }

    #implemented part def Tasks {
        @stage { number = 5; }
        doc /* kernel/src/sched/mod.rs: spawn, spawn_on, exit, yield, sleep,
             * wake, reap. Preemption happens only on the way out of an
             * interrupt: the timer sets need_resched and returns, and the trap
             * path decides once the controller has been told the interrupt is
             * done, because switching inside the handler would leave it in
             * service for as long as the next task ran. */
        part tasks : Task[0..*];
        action spawnKernelThread;
        action switchTo { doc /* Deciding and switching are one operation under the queue lock. */ }
        action exitTask;
        action yieldNow;
        action sleepUntil;
        action wake { doc /* Places the task and wakes an idle processor, which is otherwise never told. */ }
        action preemptOnIrqExit;
        action reap;
        part waitQueues : WaitQueue[0..*];
    }

    #implemented part def WaitQueue {
        @stage { number = 5; }
        doc /* kernel/src/sched/wait.rs. The lost wake-up is closed by order:
             * a waiter marks itself blocked and joins the queue before its
             * last look at the condition; a waker takes the same lock, so it
             * either sees the waiter or made the condition true before that
             * look. */
        action waitUntilDeadline;
        action wakeAll;
    }

    #implemented part def Runqueue {
        @stage { number = 5; }
        doc /* kernel/src/sched/queue.rs: one per CPU, one plain SpinLock taken
             * with interrupts masked and handed across the switch. Tickless:
             * the timer is armed for the end of the running task's slice or
             * the first sleeper's wake-up, whichever is first, and not at all
             * for one task with nothing behind it. Work stealing by an idle
             * processor in Throughput; none in HardRt, because partitioned
             * scheduling is what makes the admission test valid. */
        attribute sliceNanos : Natural = 3000000;
        part fair : EevdfRunQueue;
        ref part current : Task[0..1];
        ref part idle : Task;
        ref part sleepers : Task[0..*] ordered;
        action pickNext;
        action account;
        action armTimer;
        action stealCandidate;
        action shouldPreempt;
        action checkInvariants;
    }

    #implemented part def EevdfRunQueue {
        @stage { number = 5; }
        doc /* libs/sched: entities with weight and virtual runtime; the
             * queue's virtual time is the weight-average; an entity is
             * eligible when its virtual runtime is at or behind it; the pick
             * is the eligible entity with the earliest virtual deadline. The
             * tree is an AVL ordered by deadline, each subtree remembering its
             * minimum virtual runtime, so the pick is one O(log n) walk. Lag
             * stays within one request, which is what the boot test measures. */
        attribute nice0Weight : Natural = 1024;
        action insert;
        action remove;
        action pick;
        action release { doc /* For a steal: hand the entity's state to another queue. */ }
    }

    enum def DomainMode {
        Throughput;
        SoftRt;
        HardRt;
    }

    attribute def ModeSpec {
        attribute mode : DomainMode;
        attribute classes : SchedClass[1..*] ordered;
        attribute preemption : String;
        attribute interrupts : String;
    }

    #implemented part def SchedulingDomain {
        @stage { number = 5; }
        doc /* libs/sched/domain.rs. CPUs are partitioned into domains, each
             * in one of three modes, changeable at runtime. Per domain, not
             * global: a four-core machine runs a HardRt partition on one core
             * and Throughput on the other three, with rustc on the latter.
             * Stage 5 builds one domain holding every CPU (up to 256) in
             * Throughput; the mode is a property of the domain from the first
             * line, the class stack is looked up from the mode, and SoftRt
             * and HardRt are named and refused until stage 14. check_partition
             * requires the domains to cover the online CPUs exactly once. */
        ref part cpus : PerCpu[1..*];
        attribute mode : DomainMode = DomainMode::Throughput;
        attribute classStack : SchedClass[1..*] ordered;
        exhibit state lifecycle : DomainLifecycle;

        attribute throughput : ModeSpec = ModeSpec(mode = DomainMode::Throughput, classes = (SchedClass::Eevdf, SchedClass::Idle), preemption = "preemption points, deferred for cache locality", interrupts = "in context");
        attribute softRt : ModeSpec = ModeSpec(mode = DomainMode::SoftRt, classes = (SchedClass::Fifo, SchedClass::RoundRobin, SchedClass::Eevdf, SchedClass::Idle), preemption = "fully preemptible kernel, PI mutexes", interrupts = "threaded");
        attribute hardRt : ModeSpec = ModeSpec(mode = DomainMode::HardRt, classes = (SchedClass::Edf), preemption = "fully preemptible, bounded sections", interrupts = "threaded, bandwidth-reserved");
    }

    attribute def ModeSwitchRequest {
        attribute target : DomainMode;
    }
    attribute def GracePeriodElapsed;

    #planned state def DomainLifecycle {
        @stage { number = 14; }
        doc /* Switching modes takes the domain through a quiescent point.
             * Tasks the new mode cannot represent are demoted with an errno
             * the switching caller sees, never silently. */
        entry; then running;
        state running;
        transition running_to_draining
            first running
            accept request : ModeSwitchRequest
            then draining;
        state draining {
            doc /* Stop admitting. */
        }
        transition draining_to_migrating
            first draining
            accept elapsed : GracePeriodElapsed
            then migrating;
        state migrating {
            do action migrateTasks {
                doc /* Move tasks whose policy the new mode does not offer;
                     * demote with errno what cannot move. */
            }
        }
        transition migrating_to_swapping first migrating then swapping;
        state swapping {
            do action swapClassStack;
        }
        transition swapping_to_running first swapping then running;
    }

    #implemented part def Scheduler {
        @stage { number = 5; }
        doc /* Not one policy: a class stack per domain. EEVDF rather than CFS
             * for the fair class, because each task gets an actual eligible
             * time and a deadline, so latency has a bound and not only
             * fairness. Tickless: the timer is armed for the next decision. */
        part domains : SchedulingDomain[1..*];
        part fair : EevdfClass;
        part idle : IdleClass;
        part loadBalancing : LoadBalancing {
            @deferred { reason = "Nothing beyond work stealing by an idle processor; stage 5 left the rest."; }
        }
        #planned part fifoRr : FifoRrClass { @stage { number = 14; } }
        #planned part edf : EdfClass { @stage { number = 14; } }
        action pickNext;
        action tick;
        #planned action setScheduler {
            @stage { number = 7; }
            doc /* Linux sched_setscheduler: SCHED_FIFO/RR to the soft-RT
                 * classes, SCHED_DEADLINE to EDF, SCHED_OTHER/BATCH/IDLE to
                 * fair. */
        }
        #planned action switchDomainMode { @stage { number = 14; } }
    }

    part def LoadBalancing;

    part def EevdfClass {
        doc /* Eligible virtual deadline first. Weight and bandwidth arrive
             * from the cpu cgroup controller at stage 13. */
    }
    part def IdleClass;
    part def FifoRrClass {
        doc /* Priorities 1 to 99, with priority-inheritance mutexes and
             * threaded interrupts in SoftRt. */
    }
    part def EdfClass {
        doc /* Earliest deadline first with constant-bandwidth-server
             * admission control that refuses an unschedulable set instead of
             * missing deadlines. Preallocated pools on the RT path; no
             * dynamic allocation there. */
        action admit;
    }

    #planned part def Futex {
        @stage { number = 7; }
        doc /* The futex family over a hash of wait queues keyed by physical
             * page and offset, so a shared mapping's futex is one futex.
             * Robust lists and PI futexes with it. */
    }
}
