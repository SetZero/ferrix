# Stage 5 — Tasks and the scheduler ✅

`Task`, kernel stacks, context switch, per-CPU runqueues, the class stack, and
the EEVDF fair class. Scheduling domains exist from the start with one mode
(`Throughput`) implemented; the other two are stage 14, but the domain
abstraction is not retrofitted.

**Done.**

* **The deciding is `src/lib/kernel/sched`**, host-tested, because a scheduler that is
  wrong is wrong in a way nothing on the machine can print. The EEVDF tree,
  the weights, the lag arithmetic and the domain partition are all reachable
  from `cargo test`; what is in `src/kernel/` is the part that needs a machine.
* **Deciding and switching are one operation.** The run queue's lock is taken
  before the decision and released *after* the switch, by whichever context
  ends up running. That is not an optimisation: it is what stops another
  processor picking up the outgoing task in the window between it going back
  on the queue and its registers being saved. `SpinLock::lock_manually` exists
  to say so where the type system cannot.
* **Preemption happens only on the way out of an interrupt.** The timer sets a
  flag and returns; the decision is made once the controller has been told the
  interrupt is done. Switching inside the handler would leave an interrupt in
  service for as long as the next task ran, and a controller still servicing
  one delivers nothing further.
* **The context switch** is one of the two sites in `docs/ASSEMBLY.md`'s
  *Every architecture* table: a function that returns onto a
  different stack from the one it was called on, which Rust cannot express.

**Exit criterion met, and in the boot test on all three architectures.** A
thousand kernel threads, all spawned on one processor so that the only way the
others get any is by taking them, run bounded work to completion and give every
stack back — about 28,000 context switches and 1,000 steals in the runs
recorded when this landed. Then twelve spinners, three on each processor and
one of each three at a different weight, run inside a measured window, and each
task's service is required to stay within EEVDF's own bound of its weighted
share. The bound is not a constant: it is a slice plus the worst overrun the
scheduler actually served, and both numbers are printed, because a bound that
moves is only honest beside what it bounded.

**Three bugs it found, each invisible to the check before it:**

* **An idle processor is never told.** Work appearing on another processor's
  queue after an idle one has halted is invisible to it forever, so a thousand
  tasks ran on one processor with three asleep. Placing a task now wakes the
  idle, and `should_preempt` is not the only reason to: it compares an arrival
  against the fair queue, which the idle task is deliberately not in, so it
  answers false however urgent the arrival.
* **`vmap::free` freed the address before it unmapped the pages**, so another
  processor could be handed an address that was still mapped and have a
  perfectly ordinary allocation refused. The unmapping cannot happen under the
  arena lock — it waits for processors that cannot answer while spinning for
  that lock — so the two steps are separate now, with the address reserved
  across the gap.
* **Every private interrupt the boot core enables is off on every other core**,
  the timer included, because those enable bits are banked. A core whose timer
  is masked in the controller runs, takes inter-processor interrupts, and is
  never preempted: whatever it picks first, it runs forever. The GICv2 driver
  records what was enabled and gives each core the same set, which fixes the
  class rather than the instance.

Two of the three needed more than one processor and work that outlives a
timeslice, which is to say they needed this stage's own test to exist.

**Deferred:** a task that is not a kernel thread, which is stage 6; and the
other two domain modes, which are stage 14.

## Closing the distance to Linux

Stage 5 left the scheduler fair on each processor and naive across them, which
is enough to pass its own exit criterion and not enough to be called a
scheduler. Five things were added afterwards, all of them arithmetic in
`src/lib/kernel/sched` with the kernel supplying the numbers, and each with a check in
the boot test that fails without it.

* **Load tracking.** A decaying average with a 33-millisecond half-life, in the
  shape of Linux's PELT. It measures *weighted demand* rather than occupancy,
  which is the distinction the balancer lives on: a processor is either running
  something or not, so "busy" saturates at one task and says nothing after
  that. Folding in a long stretch costs no more than a short one: past 512
  periods, whatever came before is gone, and the average is set to the level
  held rather than walked there one period at a time. The walk had been done
  under the run queue lock with interrupts masked, three and a half million
  steps for a processor idle for an hour.
* **Placement.** A new task goes where it should rather than where it was
  created — the processor it prefers if that one is idle, any idle processor
  otherwise, the least loaded if none is.
* **Affinity.** `pinned: bool` became a `CpuSet` per task, which is the field
  `sched_setaffinity` wants and is cheaper now than retrofitted. Stealing and
  balancing both honour it. Stage 7's `sched_setaffinity` validates a mask
  and accepts it, but does not yet write it into the task's `CpuSet`.
* **Periodic balancing**, for the case stealing structurally cannot reach:
  every processor busy, one of them much busier.
* **Slice scaling.** The slice is a share of a target latency rather than a
  constant, floored at a minimum granularity. A fixed slice is also a latency
  bound per task, and at a thousand runnable tasks that bound was seconds.

**Four bugs, three of which only a running machine could show.**

* The load average never reached its own fixed point. Two truncating integer
  divisions per step settled a permanently busy processor at 978 of 1024, so
  every processor was compared against a ceiling none could reach.
* Pulling work is useless on a tickless kernel. `arm_timer` deliberately leaves
  a processor alone when nothing is waiting, so an *under*-loaded processor is
  never interrupted and never reaches the balancer to pull anything towards
  itself. The overloaded one is interrupted constantly, precisely because it
  has tasks to switch between — so it is the only one awake to notice, and it
  has to push. Six thousand balance attempts moved nothing before this.
* **A task queued behind a running one did not re-arm its processor's timer.**
  A remote enqueue where `should_preempt` says no left a processor running one
  task forever with others waiting. A hang, not a fairness problem, and the
  cause of an intermittent failure that had been putting the `AArch64` boot
  test down about one run in three.
* Balancing thrashed: the load average is deliberately slow, so moving one task
  does not change it for tens of milliseconds and the balancer kept moving
  more. Eight movable tasks were observed moving 1,262 times. The queue length,
  which updates instantly, is now a brake on the decision the average makes.

Measured on the same boot test: worst-case fairness lag fell from 2,882 to
about 1,000 microseconds, and balance thrash from 1,262 moves to 7.

**Withdrawn, and worth saying why.** Choosing a processor for a task at the
moment it *wakes* — which is what Linux does, and better than placing only at
creation — was implemented and reverted, along with detaching a woken task from
its processor's sleeper set. Each made an already-flaky machine reliably worse;
the second wedged every run. The reason both are harder than they look is the
same: a blocked task is not an unattached one, and "blocked" covers several
states this code does not distinguish. A task can be on a wait queue, in a
sleeper set, part-way into `block` and in neither yet, or in both. A waker that
reasons about one of them moves a task something else still believes it owns.
Naming those states and giving them an order is a change of its own, and the
balancer covers the same ground less promptly in the meantime.

**Found by the review of stages 1–7 on 2026-09-13**, each fixed with a boot
check shown to fail without the fix:

* **The check that a dead task has left its queue could not fail.** It looked
  at a run queue's `previous` under the queue lock, but `finish_switch` empties
  `previous` before it releases that lock. `finish_switch` now asks the
  question itself, at the one moment a dead task leaves its processor for
  good, and records the answer for the invariant check. A kernel broken on
  purpose to leave dead tasks marked queued now ends stage 5 with "a dead task
  is still queued".
* **A dead task could be put back on the run queue.** A task woken after
  marking itself blocked, but before it reached `block`, kept its sleep
  deadline, because only a switch away took it. When the task later exited,
  that switch filed the dead task as a sleeper, and the timer made it runnable
  on a stack the reaper was freeing. `wake` and `exit` now clear the deadline,
  `choose_next` never files a dead task, and `wake_sleepers` wakes only tasks
  still blocked. The check wakes a task in exactly that window and requires
  that it not be filed as a sleeper after it exits.
* **A timer interrupt part-way into a wait lost the task.** `wait_until_deadline`
  marked the task blocked before it set its deadline or joined the waiter
  list, with interrupts on. A switch in between took the task off its run
  queue with no deadline to file it under and no waker able to find it, and
  the machine hung silently. The comment that excused it said every holder of
  the waiter lock masks interrupts; none does. The deadline and the waiter
  entry now come first and `BLOCKED` last, and the way out is the reverse.
  No boot check can hit the window on demand, so it was shown with a
  two-millisecond spin added inside it, locally: the old order hangs stage 5,
  and the new order boots clean with the same spin.
* **A task spawned or woken onto the caller's own processor waited for an
  unrelated interrupt.** The same class as the timer re-arm above, recorded
  as fixed for a spawn or wake onto *another* processor. Onto the caller's
  own, it only set the reschedule flag, which is read on the way out of an
  interrupt, and a system call or kernel thread returns through none. On a
  processor whose timer was stopped, the new task waited until the caller
  blocked or something else happened to interrupt it. The flag now comes
  with the timer armed for the shortest interval, and from inside an
  interrupt the exit re-arms it for the real decision first. The check
  spawns, then wakes, a task onto its own processor while it spins, and
  requires the task to run.
* **A task pulled by periodic balancing was not scheduled.** A pull added a
  task behind a processor's lone running task and asked nothing of it. Its
  timer was stopped, and later wake-ups saw two tasks and did not kick, so
  the pulled task waited for an unrelated interrupt. This was the
  intermittent "a balancing task never started": an anchor-only processor
  took a placement's broadcast, pulled a movable spinner, and never ran it.
  A pull now asks the puller to decide again. The check pulls a task from
  behind a spinner running with interrupts masked, then spins, and requires
  the pulled task to run.

**Four intermittent failures, found a day later and each a real bug.** Stage
5's checks had been failing one boot in three to six on a loaded host, and
had been counted as noise for a day. Probes printed from the worker tasks,
not the checker, and a per-pick trace found them:

* *"a task's stack was never given back"* — the idle loop reaped the whole
  zombie list at once and could be switched out mid-batch when the checker
  was woken onto its processor; the checker then yielded forever waiting for
  stacks the idle task held. The idle task then freed one stack at a time and
  was not switched out while holding one; since 2026-09-19 it frees a batch
  of up to sixteen under one shootdown, still unswitchable while it holds
  them.
* *The thousand-task check taking 20–50 s* — the wait queue's lock was a
  plain ticket lock taken with interrupts on. A worker preempted inside the
  few instructions it is held started a convoy: every finishing worker spun
  its slice away holding a ticket, and each hand-off cost a full round of the
  queue. Two hundred thousand switches to run a thousand tasks. A holder with
  interrupts masked cannot be preempted, so that lock now masks them. Any
  plain spin lock taken from a task with interrupts on is exposed to the same
  thing once contended.
* *"every thread ran on one processor"* and *"a balancing task never
  started"* — a task spawned onto the spawner's own tickless processor, or
  pulled there by the balancer, waited for an interrupt that never came.
* *"a task's service strayed further from its share than EEVDF allows"* —
  not the scheduler. Lag carried *into* the window: a host stall while the
  first spinner ran alone was charged to it as service, EEVDF repaid its
  siblings inside the window, and the check read the repayment as a
  violation. The window now levels every lag when it opens, and its bound is
  a slice plus the sum of the overruns that processor served inside it.

**Preemption is disabled under every task-context spin lock.** The convoy
above was one lock; the kernel had forty more plain ticket locks taken from
tasks with interrupts on, each exposed to the same thing once contended.
Masking interrupts for all of them is the wrong tool, so the scheduler keeps
a per-processor preemption count, raised and lowered by the kernel's
`sync::SpinLock` (a `ferrix_sync::PreemptSpinLock`) for as long as it is
held and while it spins for its ticket, and read on the way out of every
interrupt: a pending reschedule waits until the count is zero, then is made.
A holder must not block, and the scheduler enforces it -- a switch with the
count raised stops the machine (FX-0503) -- so a holder that sleeps is found
by the first boot rather than by a convoy on a loaded host. The run queues'
own locks stay plain, being taken with interrupts masked and handed across a
switch.

**And no shootdown is asked for under one.** A shootdown waits for every
other processor to answer an interrupt, and a holder that asks for one keeps
its lock, contended, with preemption off, for the round trip; a holder with
interrupts masked cannot be answered at all. An audit of every lock in the
kernel on 2026-09-13 found no holder that blocks, and three that shot down:
a shrinking `brk` under the process's state lock, the alarm clock's spawn
under its running flag, and the migration check's spawns under the turn
itself, whose failure path would have waited for the turn it held. Each now
lets go first. The rule enforces itself: the scheduler counts, beside the
preemption count, how much of it *locks* raised, and both flushes assert
that count zero before they take the turn, naming the lock's site when it is
not, and interrupts on wherever they are about to wait for another
processor. A flush that waits for nobody else is exempt on purpose: stage 6's
checks fault with interrupts masked to keep a space installed, and a
copy-on-write fault there retires a page through a shootdown whose set names
only that processor; the first row of this landing found exactly that. The
same row found a real one: a secondary processor enters the idle loop with
the interrupts its hand-over masked, and its first reap frees a stack, a
global flush that waits for every processor; the idle loop now enables
interrupts once at entry. The reaper's own by-hand raise around freeing a
stack, which is a shootdown by design, is not a lock and passes.

**A queue insert charges the running task first.** `CpuQueue::insert`
placed a newcomer before charging the task already running, so a task alone
on a tickless processor -- charged only at its next decision, with an
`exec_start` a hundred milliseconds old -- had all of that billed after the
newcomer was counted, and the newcomer came out owed half of it, past the
placement clamp. Stage 7's first spinner arrived owed 59 ms and ran its
whole loop before the checker could start the second. `insert` and `release`
now charge first, as Linux's `enqueue_entity` calls `update_curr` before it
places; found by stage 7's session with a trace ring, 598 of 600 looped
iterations under KVM, and 2 of 12 whole boots under KVM on the tree before
the charge against 0 of 12 after it. And the check that caught it measures preemption now
-- each program switched out still runnable at the exit of an interrupt that
arrived in its user code, twice -- rather than being switched to twice,
which a program never preempted shows too, or switched away at all, which a
lock released inside its one write with a reschedule pending also does; it
gets three attempts, since a host stall charged to one program as service
lets the other run its whole loop, and prints what each attempt saw.

**Stage 4's contended count judges its overlap over five rounds.** The count
requires two processors' increments to overlap, and an emulator whose host
deschedules whole virtual processors can run the shares one after another
for a round. Each round is judged for the lock's correctness, the first
round that overlaps ends the check, a round that did not is printed with its
shares, and only five rounds without overlap fail it -- the same shape as
stage 7's three-attempt pair check.

**Still missing against Linux**, none of it on stage 6's path: group scheduling
and bandwidth control, which are stage 13; the real-time classes, which are
stage 14; and NUMA and capacity awareness, which need a topology this kernel
does not yet parse.

---

