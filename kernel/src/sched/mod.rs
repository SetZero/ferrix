//! Tasks, and the scheduler that decides which of them runs.
//!
//! Stage 5 of `docs/ROADMAP.md`. Until now the kernel has run one thread of
//! control per processor, and every "CPU" in the code has meant a processor.
//! From here a processor runs a *task*, chosen from a queue of its own, and
//! the choosing is `ferrix_sched`'s EEVDF fair class — written in `libs/` and
//! tested on the host, because a scheduler that is wrong is wrong in a way
//! nothing on the machine can print.
//!
//! # What a switch is, and who holds the lock across it
//!
//! Deciding and switching are one operation. The run queue's lock is taken
//! before the decision and released *after* the switch, by whichever context
//! ends up running — the one switched to, or this one when nothing changed.
//! That is not an optimisation: it is what stops another processor picking up
//! the outgoing task in the window between it being put back on the queue and
//! its registers being saved. Linux hands its own `rq` lock across
//! `context_switch` for exactly this reason, and `SpinLock::lock_manually`
//! exists to say so in the type system's absence.
//!
//! # Where preemption happens
//!
//! Nowhere except on the way out of an interrupt. A timer interrupt does not
//! switch: it sets this processor's `need_resched` flag and returns, and the
//! generic trap path calls [`preempt_on_irq_exit`] once the controller has
//! been acknowledged. Switching inside the handler would leave an interrupt
//! in service on the local APIC for as long as the next task ran, which is a
//! machine that takes one interrupt and then no more.
//!
//! Because every lock that an interrupt handler may take masks interrupts, a
//! task cannot be preempted while it holds one — so the scheduler never has
//! to reason about a task switched out mid-critical-section.
//!
//! # Per-processor state and preemption
//!
//! Anything read through the per-processor register is only valid while
//! preemption cannot happen, which means with interrupts masked. A task that
//! read `smp::this_cpu()` and then blocked could wake on another processor
//! holding another processor's record. Every use here is inside an
//! interrupts-masked region for that reason.

mod check;
mod queue;
mod task;
mod wait;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ferrix_sched::{CpuSet, Domain, Mode, NICE_0_WEIGHT, check_partition};
use ferrix_sync::{IrqControl, IrqSpinLock, Once, SpinLock};

use crate::arch;
use crate::smp::Topology;
use queue::CpuQueue;
use task::{DEAD, RUNNABLE, Task};

pub(crate) use check::run as run_checks;
pub(crate) use queue::SLICE_NS;
pub(crate) use task::TaskId;
pub(crate) use wait::WaitQueue;

/// One run queue per logical processor.
static QUEUES: Once<Vec<SpinLock<CpuQueue>>> = Once::new();

/// One flag per processor, set by an interrupt that wants a decision made on
/// the way out of it.
static NEED_RESCHED: Once<Vec<AtomicBool>> = Once::new();

/// The machine's one scheduling domain, until stage 14 makes more.
static DOMAIN: Once<Domain> = Once::new();

/// Whether [`init`] has run.
static STARTED: AtomicBool = AtomicBool::new(false);

/// How many processors have taken up their idle task.
///
/// Waited for by [`init`], because a check that measures work stealing is
/// measuring nothing until there is a second processor in the scheduler to
/// steal. Secondaries arrive under their own steam — they are woken by an
/// IPI and have to get from stage 4's job loop to `enter_idle` — so "the
/// scheduler is up" is not the same instant on every processor, and the
/// difference is milliseconds an emulated machine can easily stretch.
static IN_SCHEDULER: AtomicU64 = AtomicU64::new(0);

/// The next task identifier. Never reused.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Tasks that have exited, waiting for their stacks to be freed.
///
/// Not freed where they exit: a task cannot unmap the stack it is standing
/// on, and the context that switched away from it is holding a run queue lock
/// while `vmap::free_stack` needs to invalidate other processors' translations
/// and wait for them.
static ZOMBIES: IrqSpinLock<Vec<Arc<Task>>, arch::Irq> = IrqSpinLock::new(Vec::new());

/// Whether the scheduler is running.
pub(crate) fn started() -> bool {
    STARTED.load(Ordering::Acquire)
}

/// The next identifier to give a task.
fn next_id() -> TaskId {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// The run queue of logical processor `cpu`.
fn queue_of(cpu: usize) -> Option<&'static SpinLock<CpuQueue>> {
    QUEUES.get()?.get(cpu)
}

/// Which processor this is, or `None` before the per-processor register is
/// installed.
fn this_cpu() -> Option<usize> {
    crate::smp::this_cpu().map(|cpu| cpu.logical)
}

/// Say that `cpu` should pick again on the way out of the interrupt it is in.
fn mark_resched(cpu: usize) {
    if let Some(flag) = NEED_RESCHED.get().and_then(|flags| flags.get(cpu)) {
        flag.store(true, Ordering::Release);
    }
}

/// Interrupt another processor so it notices its flag.
///
/// Broadcast, because that is the only inter-processor interrupt the
/// architectures offer today: a processor with nothing to do wakes, finds
/// nothing, and goes back to waiting. Stage 10 wants a targeted one anyway,
/// for a device interrupt steered to one core.
fn kick(cpu: usize) {
    // The flag is set here, on the target's behalf, rather than by the target
    // inside its own interrupt handler. So the interrupt carries no meaning
    // of its own: it exists to make the target *reach* an interrupt exit,
    // where `preempt_on_irq_exit` reads the flag. That is why there is no
    // scheduler hook in the IPI handler, and why a processor woken by
    // somebody else's shootdown finds this flag and acts on it just as well.
    mark_resched(cpu);
    let _ = arch::send_ipi_to_others();
}

/// Bring up the scheduler, and make the context calling it a task.
///
/// Must run after `smp::discover` and `smp::start_secondaries`, because it
/// gives every processor a queue and expects the secondaries to be waiting
/// for work.
///
/// # Errors
///
/// If the machine has more processors than the domain can name, or there is
/// no memory for a queue or an idle task's stack.
pub(crate) fn init(topology: &'static Topology) -> Result<(), &'static str> {
    let online = topology.count();
    let cpus =
        CpuSet::first(online).map_err(|_| "more processors than a scheduling domain holds")?;
    let domain =
        Domain::new(cpus, Mode::Throughput).map_err(|_| "the Throughput domain was refused")?;
    check_partition(core::slice::from_ref(&domain), online)
        .map_err(|_| "the scheduling domains do not cover every processor exactly once")?;
    let _ = DOMAIN.call_once(|| domain);

    let mut queues = Vec::with_capacity(online);
    for _ in 0..online {
        queues.push(SpinLock::new(CpuQueue::new()?));
    }
    let _ = QUEUES.call_once(|| queues);
    let _ = NEED_RESCHED.call_once(|| (0..online).map(|_| AtomicBool::new(false)).collect());

    adopt_boot_task()?;
    joined(0);
    STARTED.store(true, Ordering::Release);

    // The secondaries are asleep in `smp::secondary_main`. Wake them, so each
    // takes up the idle loop that is its half of the scheduler.
    let _ = arch::send_ipi_to_others();
    wait_for_processors(online)
}

/// Wait until every processor is in the scheduler, or say which never came.
///
/// The boot processor counts itself, having just adopted its own context; the
/// rest arrive through [`enter_idle`]. Bounded, because a processor that never
/// arrives should be a sentence in the boot log rather than a wait that never
/// ends — and re-sending the interrupt each time round, because the first one
/// may have been sent while a processor was still on its way into the halt
/// that was meant to receive it.
fn wait_for_processors(online: usize) -> Result<(), &'static str> {
    /// How long to give them. Generous: an emulated processor may be a host
    /// thread that is not currently running.
    const PATIENCE_NANOS: u64 = 5_000_000_000;

    let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
    let all = if online >= 64 {
        u64::MAX
    } else {
        (1u64 << online) - 1
    };

    while IN_SCHEDULER.load(Ordering::Acquire) != all {
        if crate::timer::now_nanos() >= deadline {
            return Err("a processor never joined the scheduler");
        }
        let _ = arch::send_ipi_to_others();
        core::hint::spin_loop();
    }
    Ok(())
}

/// Record that `cpu` is now running tasks.
fn joined(cpu: usize) {
    if cpu < 64 {
        let _ = IN_SCHEDULER.fetch_or(1u64 << cpu, Ordering::AcqRel);
    }
}

/// Make the context that called [`init`] the boot processor's first task, and
/// give that processor an idle task to fall back to.
fn adopt_boot_task() -> Result<(), &'static str> {
    let boot = Arc::new(Task::adopt(next_id(), "kmain", NICE_0_WEIGHT, 0));
    let idle = new_idle_task(0)?;
    let lock = queue_of(0).ok_or("the boot processor has no run queue")?;

    let saved = <arch::Irq as IrqControl>::disable();
    {
        let mut queue = lock.lock();
        queue.idle = Some(idle);
        queue.insert(&boot);
        // Make it the running entity rather than one waiting to run: it is
        // already on the processor.
        let _ = queue.fair.pick_next();
        queue.exec_start = crate::timer::now_nanos();
        queue.current = Some(boot);
    }
    <arch::Irq as IrqControl>::restore(saved);
    Ok(())
}

/// An idle task with a stack of its own, for a processor whose own context is
/// busy being something else.
fn new_idle_task(cpu: usize) -> Result<Arc<Task>, &'static str> {
    let stack = crate::vmap::allocate_stack().map_err(|_| "no stack for an idle task")?;
    // SAFETY: the stack was allocated a moment ago, is mapped and writable,
    // and nothing else refers to it.
    let stack_pointer = unsafe { arch::prepare_stack(stack.top, task_start, 0) };
    Ok(Arc::new(Task::new(task::NewTask {
        id: next_id(),
        name: "idle",
        entry: |_| idle_loop(),
        argument: 0,
        stack,
        stack_pointer,
        weight: NICE_0_WEIGHT,
        cpu,
        pinned: true,
    })))
}

/// Where a secondary processor joins the scheduler: its bring-up context
/// becomes its idle task, and it never returns.
pub(crate) fn enter_idle() -> ! {
    let Some(cpu) = this_cpu() else { arch::halt() };
    let idle = Arc::new(Task::adopt(next_id(), "idle", NICE_0_WEIGHT, cpu));

    if let Some(lock) = queue_of(cpu) {
        let saved = <arch::Irq as IrqControl>::disable();
        {
            let mut queue = lock.lock();
            queue.idle = Some(Arc::clone(&idle));
            queue.current = Some(idle);
            queue.exec_start = crate::timer::now_nanos();
        }
        <arch::Irq as IrqControl>::restore(saved);
    }
    joined(cpu);
    idle_loop()
}

/// What a processor does with nothing to run: look for work to take from a
/// busier processor, tidy up after tasks that have exited, and otherwise
/// sleep until an interrupt says something has changed.
fn idle_loop() -> ! {
    loop {
        let _ = reap();
        if steal_work() {
            schedule();
            continue;
        }

        // Masked while looking, so that an interrupt arriving after the look
        // wakes the wait rather than being taken just before it.
        arch::disable_interrupts();
        if has_work() {
            arch::enable_interrupts();
            schedule();
        } else {
            arch::wait_for_work();
        }
    }
}

/// Whether this processor has anything but its idle task to run.
fn has_work() -> bool {
    let Some(lock) = this_cpu().and_then(queue_of) else {
        return false;
    };
    let queue = lock.lock();
    queue.has_work()
}

/// Start a task on this processor.
///
/// # Errors
///
/// If there is no stack for it, or the scheduler is not up.
pub(crate) fn spawn(
    name: &'static str,
    entry: fn(usize),
    argument: usize,
    weight: u32,
) -> Result<Arc<Task>, &'static str> {
    let cpu = this_cpu().ok_or("no processor to start a task on")?;
    spawn_on(name, entry, argument, weight, cpu, false)
}

/// Start a task on `cpu`, optionally pinned there.
///
/// # Errors
///
/// If there is no stack for it, or `cpu` has no run queue.
pub(crate) fn spawn_on(
    name: &'static str,
    entry: fn(usize),
    argument: usize,
    weight: u32,
    cpu: usize,
    pinned: bool,
) -> Result<Arc<Task>, &'static str> {
    let stack = crate::vmap::allocate_stack().map_err(|problem| {
        // The arena's own reason, because "no stack" has four of them and they
        // want four different fixes: no address space, no frames, the page
        // tables refusing, or the arena not being up at all.
        crate::console::println!("  tasks    no stack for {name}: {problem}");
        "no kernel stack for a new task"
    })?;
    // SAFETY: the stack was allocated a moment ago, is mapped and writable,
    // and nothing else refers to it.
    let stack_pointer = unsafe { arch::prepare_stack(stack.top, task_start, 0) };
    let task = Arc::new(Task::new(task::NewTask {
        id: next_id(),
        name,
        entry,
        argument,
        stack,
        stack_pointer,
        weight,
        cpu,
        pinned,
    }));

    let lock = queue_of(cpu).ok_or("no such processor")?;
    let saved = <arch::Irq as IrqControl>::disable();
    let (preempt, stealable) = {
        let mut queue = lock.lock();
        queue.insert(&task);
        // Two reasons to make the target reschedule, and the second is the one
        // that is easy to miss: either something better than what it is
        // running has arrived, or it is not running anything at all and has
        // to be woken to notice. A processor asleep in `wait_for_work` finds
        // out only when interrupted.
        let wake = queue.should_preempt() || queue.is_running_idle();
        // More than the one task running means there is something here for an
        // idle processor to take. Measured after the insert, so the first task
        // to make the queue worth stealing from is the one that says so.
        (wake, queue.len() > 1)
    };
    let here = this_cpu();
    <arch::Irq as IrqControl>::restore(saved);

    if preempt {
        if here == Some(cpu) {
            mark_resched(cpu);
        } else {
            kick(cpu);
        }
    }

    // **Tell the idle processors, or they will sleep through this.** An idle
    // processor looks for work to steal and then halts, and nothing wakes it
    // but an interrupt: a queue that fills up on another processor after it
    // halted is invisible to it forever. The first stage-5 run found exactly
    // that — a thousand tasks spawned on one processor, three processors
    // asleep, and stealing that moved nothing.
    //
    // Broadcast and unconditional rather than aimed at the processors that
    // are actually idle: an idle processor is only idle until it looks, so
    // any answer to "which ones" is stale before it is used. One woken
    // processor that finds nothing costs a halt and a wake; the alternative
    // costs the whole machine minus one.
    if stealable {
        wake_idle_processors();
    }
    Ok(task)
}

/// Wake every other processor so that anything idle looks for work to steal.
///
/// Separate from [`kick`], which is about a specific processor needing to
/// reschedule. This one carries no request at all: the interrupt exists only
/// to return an idle processor to the top of its loop, where it looks.
fn wake_idle_processors() {
    let _ = arch::send_ipi_to_others();
}

/// Where every task begins.
///
/// Reached from the architecture's trampoline, on a stack `prepare_stack`
/// laid out, with the run queue lock still held by the switch that got here.
extern "C" fn task_start(_argument: usize) -> ! {
    finish_switch();
    arch::enable_interrupts();

    if let Some(task) = current()
        && let Some((entry, argument)) = task.entry()
    {
        entry(argument);
    }
    exit()
}

/// End the running task.
pub(crate) fn exit() -> ! {
    if let Some(task) = current() {
        task.set_state(DEAD);
    }
    schedule();
    // A dead task is never picked again, so this is not reached. If it ever
    // were, stopping is the only answer that cannot corrupt anything.
    loop {
        arch::wait_for_interrupt();
    }
}

/// The task running on this processor.
pub(crate) fn current() -> Option<Arc<Task>> {
    let saved = <arch::Irq as IrqControl>::disable();
    let task = this_cpu()
        .and_then(queue_of)
        .and_then(|lock| lock.lock().current.clone());
    <arch::Irq as IrqControl>::restore(saved);
    task
}

/// Give up the rest of this task's slice.
pub(crate) fn yield_now() {
    let saved = <arch::Irq as IrqControl>::disable();
    if let Some(lock) = this_cpu().and_then(queue_of) {
        lock.lock().fair.yield_curr();
    }
    <arch::Irq as IrqControl>::restore(saved);
    schedule();
}

/// Block the running task until `deadline`, by the counter's reckoning.
pub(crate) fn sleep_until(deadline: u64) {
    let Some(task) = current() else {
        while crate::timer::now_nanos() < deadline {
            core::hint::spin_loop();
        }
        return;
    };
    task.set_sleep_deadline(deadline);
    task.set_state(task::BLOCKED);
    schedule();
}

/// Block the running task for `nanos`.
pub(crate) fn sleep_for(nanos: u64) {
    sleep_until(crate::timer::now_nanos().saturating_add(nanos));
}

/// Block the running task, which has already marked itself blocked.
fn block() {
    schedule();
}

/// Make `task` runnable, wherever it is.
pub(crate) fn wake(task: &Arc<Task>) {
    let saved = <arch::Irq as IrqControl>::disable();
    let mut kick_cpu = None;
    loop {
        let cpu = task.cpu();
        let Some(lock) = queue_of(cpu) else {
            break;
        };
        let mut queue = lock.lock();
        // It may have moved to another processor between the read and the
        // lock, in which case this is the wrong queue and the wrong lock.
        if task.cpu() != cpu {
            continue;
        }
        if task.state() != task::BLOCKED {
            break;
        }
        task.set_state(RUNNABLE);
        if !task.is_queued() {
            queue.insert(task);
        }
        // As `spawn_on`: an idle processor has to be told, because
        // `should_preempt` compares against a fair queue the idle task is not
        // in and so answers false however urgent the arrival.
        if queue.should_preempt() || queue.is_running_idle() {
            kick_cpu = Some(cpu);
        }
        break;
    }
    let here = this_cpu();
    <arch::Irq as IrqControl>::restore(saved);

    match kick_cpu {
        Some(cpu) if here == Some(cpu) => mark_resched(cpu),
        Some(cpu) => kick(cpu),
        None => {}
    }
}

/// What a timer interrupt does: ask for a decision on the way out.
pub(crate) fn timer_expired() {
    if let Some(cpu) = this_cpu() {
        mark_resched(cpu);
    }
}

/// Make the decision an interrupt asked for, on the way out of it.
pub(crate) fn preempt_on_irq_exit() {
    if !started() {
        return;
    }
    let Some(cpu) = this_cpu() else {
        return;
    };
    let asked = NEED_RESCHED
        .get()
        .and_then(|flags| flags.get(cpu))
        .is_some_and(|flag| flag.swap(false, Ordering::AcqRel));
    if asked {
        schedule();
    }
}

/// Give the processor to whatever should have it now.
fn schedule() {
    let saved = <arch::Irq as IrqControl>::disable();
    pick_and_switch();
    <arch::Irq as IrqControl>::restore(saved);
}

/// With interrupts masked: decide, and switch if the decision changed
/// anything. Returns possibly much later, and possibly on another processor.
fn pick_and_switch() {
    let Some(cpu) = this_cpu() else {
        return;
    };
    let Some(lock) = queue_of(cpu) else {
        return;
    };
    let Some((save, resume)) = choose_next(lock, cpu) else {
        return;
    };
    // SAFETY: `save` is this context's own slot and `resume` a stack pointer
    // this module prepared or saved; this processor holds the run queue's
    // lock, which keeps every other processor off both until `finish_switch`
    // releases it.
    unsafe { arch::switch_to(save, resume) };
    finish_switch();
}

/// Choose what runs next, leaving the queue's lock held and returning where
/// to save this context and what to resume — or releasing the lock and
/// returning `None` when nothing has to change.
fn choose_next(lock: &'static SpinLock<CpuQueue>, cpu: usize) -> Option<(*mut u64, u64)> {
    // SAFETY: released below when nothing is switched, and otherwise by the
    // context this switches to, in `finish_switch`.
    let queue = unsafe { lock.lock_manually() };

    let now = crate::timer::now_nanos();
    queue.account(now);
    queue.wake_sleepers(now);

    let previous = queue.current.clone();
    if let Some(previous) = previous.as_ref().filter(|task| task.state() != RUNNABLE) {
        queue.detach_current();
        if let Some(at) = previous.take_sleep_deadline() {
            let _ = queue
                .sleepers
                .insert((at, previous.id), Arc::clone(previous));
        }
    }

    let next = queue.pick_next();
    let switching = match (previous.as_ref(), next.as_ref()) {
        (Some(previous), Some(next)) => !Arc::ptr_eq(previous, next),
        (None, Some(_)) => true,
        _ => false,
    };

    if !switching {
        queue.arm_timer(now);
        // SAFETY: taken above, and nothing was switched, so this context is
        // still the holder.
        unsafe { lock.force_unlock() };
        return None;
    }

    let (previous, next) = (previous?, next?);
    queue.stats.switches += 1;
    queue.previous = Some(Arc::clone(&previous));
    queue.current = Some(Arc::clone(&next));
    queue.exec_start = now;
    queue.arm_timer(now);
    next.note_switch(cpu);

    // SAFETY: both tasks belong to this queue and this processor holds its
    // lock, so nothing else may read or write either saved stack pointer.
    let save = unsafe { previous.stack_pointer_slot() };
    // SAFETY: as above.
    let resume = unsafe { next.saved_stack_pointer() };
    Some((save, resume))
}

/// Release the lock the switch handed over, and dispose of what ran before.
fn finish_switch() {
    let Some(lock) = this_cpu().and_then(queue_of) else {
        return;
    };
    // SAFETY: this processor holds this lock — either it took it in
    // `choose_next` and switched to here, or the context that switched to
    // this one did and handed it over.
    let queue = unsafe { lock.locked_data() };
    let previous = queue.previous.take();
    // SAFETY: held as above, released exactly once, and the queue is not
    // touched afterwards.
    unsafe { lock.force_unlock() };

    if let Some(previous) = previous
        && previous.state() == DEAD
    {
        ZOMBIES.lock().push(previous);
    }
}

/// Take a task from the busiest other processor in this domain, if one will
/// come.
///
/// Only an idle processor steals, and only from a queue with something
/// waiting rather than merely something running: taking the task another
/// processor is running is not possible, and taking its last waiting one is
/// exactly the work it is about to do itself.
fn steal_work() -> bool {
    let Some(domain) = DOMAIN.get().filter(|domain| domain.mode().steals_work()) else {
        return false;
    };
    let Some(me) = this_cpu() else {
        return false;
    };
    domain
        .cpus()
        .iter()
        .filter(|cpu| *cpu != me)
        .any(|victim| steal_from(me, victim))
}

/// Move one task from `victim`'s queue to `me`'s.
fn steal_from(me: usize, victim: usize) -> bool {
    let (Some(mine), Some(theirs)) = (queue_of(me), queue_of(victim)) else {
        return false;
    };

    let saved = <arch::Irq as IrqControl>::disable();
    // Lowest processor number first, always, so that two processors stealing
    // from each other cannot each hold what the other wants.
    let (first, second) = if me < victim {
        (mine, theirs)
    } else {
        (theirs, mine)
    };
    let mut first_queue = first.lock();
    let mut second_queue = second.lock();
    let (mine_queue, theirs_queue) = if me < victim {
        (&mut *first_queue, &mut *second_queue)
    } else {
        (&mut *second_queue, &mut *first_queue)
    };

    let moved = match theirs_queue.steal_candidate() {
        Some(id) => match theirs_queue.release(id) {
            Some((task, state)) => {
                task.store_entity_state(state);
                task.set_cpu(me);
                mine_queue.insert(&task);
                theirs_queue.stats.stolen_out += 1;
                mine_queue.stats.stolen_in += 1;
                true
            }
            None => false,
        },
        None => false,
    };

    drop(second_queue);
    drop(first_queue);
    <arch::Irq as IrqControl>::restore(saved);
    moved
}

/// Free the stacks of tasks that have exited, and return how many.
pub(crate) fn reap() -> usize {
    let dead = core::mem::take(&mut *ZOMBIES.lock());
    let count = dead.len();
    for task in dead {
        if let Some(stack) = task.stack() {
            // SAFETY: the task is dead and on no queue, and the processor
            // that switched away from it has finished doing so — which is
            // what put it here. Nothing is running on this stack.
            let _ = unsafe { crate::vmap::free_stack(stack) };
        }
    }
    count
}

/// Start measuring how far `tasks` stray from their shares.
///
/// Only those tasks are counted, and each from its own service now: a task
/// that joins a queue while the window is open was never owed any of what was
/// handed out before it arrived, and counting it would report an arrival as a
/// violation.
pub(crate) fn open_window(tasks: &[Arc<Task>]) {
    for task in tasks {
        task.open_window();
    }
    set_measuring(true);
}

/// Stop measuring.
pub(crate) fn close_window(tasks: &[Arc<Task>]) {
    set_measuring(false);
    for task in tasks {
        task.close_window();
    }
}

/// Open or close the measurement window on every processor.
fn set_measuring(measuring: bool) {
    let Some(queues) = QUEUES.get() else {
        return;
    };
    let saved = <arch::Irq as IrqControl>::disable();
    for lock in queues {
        let mut queue = lock.lock();
        queue.stats.measuring = measuring;
        if measuring {
            queue.stats.worst_lag = 0;
            queue.stats.worst_overrun = 0;
        }
    }
    <arch::Irq as IrqControl>::restore(saved);
}

/// What every processor's scheduling has done.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Summary {
    /// Context switches.
    pub(crate) switches: u64,
    /// Tasks moved from one processor to another.
    pub(crate) steals: u64,
    /// The most any task ran past its deadline before being switched out.
    pub(crate) worst_overrun: u64,
    /// The most any task's service strayed from its share while a measurement
    /// window was open.
    pub(crate) worst_lag: u64,
    /// Tasks on the queues, running ones included.
    pub(crate) tasks: usize,
}

/// Add up what every processor's scheduling has done.
pub(crate) fn summary() -> Summary {
    let mut summary = Summary::default();
    let Some(queues) = QUEUES.get() else {
        return summary;
    };
    let saved = <arch::Irq as IrqControl>::disable();
    for lock in queues {
        let queue = lock.lock();
        summary.switches += queue.stats.switches;
        summary.steals += queue.stats.stolen_in;
        summary.worst_overrun = summary.worst_overrun.max(queue.stats.worst_overrun);
        summary.worst_lag = summary.worst_lag.max(queue.stats.worst_lag);
        summary.tasks += queue.len();
    }
    <arch::Irq as IrqControl>::restore(saved);
    summary
}

/// Check every run queue's own bookkeeping.
///
/// # Errors
///
/// The first queue that has broken an invariant, as a sentence.
pub(crate) fn check_invariants() -> Result<(), &'static str> {
    let Some(queues) = QUEUES.get() else {
        return Ok(());
    };
    let saved = <arch::Irq as IrqControl>::disable();
    let mut outcome = Ok(());
    for lock in queues {
        let queue = lock.lock();
        outcome = outcome.and(queue.check_invariants());
    }
    <arch::Irq as IrqControl>::restore(saved);
    outcome
}

/// Print what each processor's run queue holds, for a check that has failed.
pub(crate) fn report_queues() {
    let Some(queues) = QUEUES.get() else {
        return;
    };
    let saved = <arch::Irq as IrqControl>::disable();
    for (cpu, lock) in queues.iter().enumerate() {
        let queue = lock.lock();
        crate::console::println!(
            "  fair     cpu {} holds {} tasks, idle={}, resched={}",
            cpu,
            queue.len(),
            queue.is_running_idle(),
            NEED_RESCHED
                .get()
                .and_then(|flags| flags.get(cpu))
                .is_some_and(|flag| flag.load(Ordering::Relaxed)),
        );
    }
    <arch::Irq as IrqControl>::restore(saved);
}
