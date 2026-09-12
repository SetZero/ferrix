//! More than one CPU.
//!
//! Stage 4 of `docs/ROADMAP.md`. It begins by finding out how many processors
//! the machine has and which of them is running this code, before any of the
//! others is started — every later step needs the answer, and the tables it
//! comes from are reclaimed at the end of boot.
//!
//! # Logical and hardware numbers
//!
//! A processor has a *hardware* identifier, which is what the machine calls
//! it: an APIC ID on x86-64, the affinity fields of `MPIDR_EL1` on `AArch64`.
//! Neither is dense — a two-socket machine can number its cores 0..16 and
//! 32..48 — so the kernel also gives each one a *logical* number, its index
//! in [`Topology`], and the boot CPU is always logical CPU zero.
//!
//! # Per-CPU records
//!
//! Every processor has a [`PerCpu`] record, allocated once and never freed,
//! and a register pointing at it: `GS`'s base on x86-64, `TPIDR_EL1` on
//! `AArch64`. [`this_cpu`] reads that register, which is how code running on a
//! processor finds out which one it is without being told — an interrupt
//! handler in particular, which is told nothing at all.

pub(crate) mod check;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ferrix_bootinfo::BootView;
use ferrix_sync::{IrqControl, Once, SpinLock};

use crate::arch;

/// The processors firmware described, as the architecture found them.
///
/// Built by `arch::describe_cpus` and checked by [`discover`], so that the
/// rules about what a sane list looks like are written once rather than once
/// per architecture.
#[derive(Debug)]
pub(crate) struct Described {
    /// What this architecture calls a hardware identifier, for the boot log.
    pub(crate) id_name: &'static str,
    /// The hardware identifier of the processor running this code.
    pub(crate) boot: u64,
    /// Every processor firmware says can be started, in table order, the boot
    /// processor included.
    pub(crate) ids: Vec<u64>,
}

/// One processor's own state.
///
/// `repr(C)` because of the first field, which is not optional: x86-64
/// reaches this record through `GS`, and the one thing `gs:0` can hand back
/// is a word stored at offset zero. Making that word the record's own address
/// turns a segment-relative load into an ordinary pointer.
#[repr(C)]
#[derive(Debug)]
pub(crate) struct PerCpu {
    /// This record's own address.
    this: u64,
    /// The kernel stack a system call from user mode lands on.
    ///
    /// Read by x86-64's `SYSCALL` trampoline *from assembly*, at a fixed
    /// offset from `GS`, because `SYSCALL` does not switch stacks: the first
    /// thing the kernel does on entry is still standing on the user's stack,
    /// and the only thing it can reach without one is this record. Set by
    /// `arch::restore_user_state` whenever the scheduler switches to a task
    /// that runs user code, to the top of that task's own kernel stack.
    ///
    /// The two Arm architectures need neither this nor the field below --
    /// `SP_EL1` and the banked SVC stack pointer are the same idea in
    /// hardware -- but the record is shared, so they simply leave them zero.
    pub(crate) kernel_stack: AtomicU64,
    /// Where the trampoline parks the user stack pointer while it switches.
    ///
    /// One word of scratch, also reached from assembly. It cannot be a
    /// register: every register at that moment either holds a system call
    /// argument or is `rcx`/`r11`, which `SYSCALL` has already overwritten
    /// with the return address and flags.
    pub(crate) user_stack: u64,
    /// Logical number: this record's index in [`Topology`].
    pub(crate) logical: usize,
    /// What the machine calls this processor.
    pub(crate) hardware_id: u64,
    /// Set by the processor itself once it is running kernel code with its
    /// own record installed. Never cleared: nothing takes a CPU offline.
    online: AtomicBool,
    /// The generation of the last piece of work this processor finished.
    job_done: AtomicU64,
    /// Inter-processor interrupts this processor has taken.
    ipis: AtomicU64,
    /// The last TLB shootdown this processor has flushed for.
    tlb_seen: AtomicU64,
    /// The last grace period this processor has been outside a read-side
    /// section for.
    gp_seen: AtomicU64,
}

impl PerCpu {
    /// Whether this processor has said it is running.
    pub(crate) fn is_online(&self) -> bool {
        self.online.load(Ordering::Acquire)
    }

    /// How many inter-processor interrupts this processor has taken.
    pub(crate) fn ipis_taken(&self) -> u64 {
        self.ipis.load(Ordering::Relaxed)
    }
}

/// The machine's processors.
#[derive(Debug)]
pub(crate) struct Topology {
    /// What this architecture calls a hardware identifier.
    id_name: &'static str,
    /// One record per processor, indexed by logical number. Index zero is the
    /// boot processor.
    cpus: &'static [PerCpu],
}

impl Topology {
    /// Check what the architecture found and build a record per processor.
    fn from_described(described: Described) -> Result<Topology, &'static str> {
        let Described { id_name, boot, ids } = described;

        if ids.is_empty() {
            return Err("firmware describes no processor that can be started");
        }

        // Two processors with one identifier cannot both be addressed, and
        // the second start request would go to the first one — which is
        // already running, and would be reset.
        for (index, id) in ids.iter().enumerate() {
            if ids.iter().skip(index + 1).any(|other| other == id) {
                return Err("firmware describes two processors with the same identifier");
            }
        }

        // The processor running this is already started. If it is not in the
        // list, the list is describing some other machine — or the identifier
        // was read in a form the table does not use, which is the more likely
        // bug and the one this is here to catch.
        if !ids.contains(&boot) {
            return Err("the processor running this code is not among those firmware describes");
        }

        let mut ordered = Vec::with_capacity(ids.len());
        ordered.push(boot);
        ordered.extend(ids.iter().copied().filter(|id| *id != boot));

        let records: Vec<PerCpu> = ordered
            .into_iter()
            .enumerate()
            .map(|(logical, hardware_id)| PerCpu {
                this: 0,
                kernel_stack: AtomicU64::new(0),
                user_stack: 0,
                logical,
                hardware_id,
                online: AtomicBool::new(logical == 0),
                job_done: AtomicU64::new(0),
                ipis: AtomicU64::new(0),
                tlb_seen: AtomicU64::new(0),
                gp_seen: AtomicU64::new(0),
            })
            .collect();

        // Leaked, deliberately: a register on every processor is about to
        // hold an address into this, for as long as the machine runs.
        let cpus: &'static mut [PerCpu] = Box::leak(records.into_boxed_slice());
        for cpu in cpus.iter_mut() {
            cpu.this = (&raw const *cpu) as u64;
        }
        Ok(Topology { id_name, cpus })
    }

    /// How many processors there are, the boot processor included.
    pub(crate) fn count(&self) -> usize {
        self.cpus.len()
    }

    /// Every processor's record, by logical number.
    pub(crate) const fn cpus(&self) -> &'static [PerCpu] {
        self.cpus
    }

    /// How many processors have said they are running.
    pub(crate) fn online(&self) -> usize {
        self.cpus.iter().filter(|cpu| cpu.is_online()).count()
    }

    /// The boot processor's hardware identifier.
    pub(crate) fn boot_id(&self) -> u64 {
        self.cpus.first().map_or(0, |cpu| cpu.hardware_id)
    }

    /// What this architecture calls a hardware identifier.
    pub(crate) const fn id_name(&self) -> &'static str {
        self.id_name
    }
}

/// The processors, once [`discover`] has run.
static TOPOLOGY: Once<Topology> = Once::new();

/// Set once the boot processor's record is installed.
///
/// Guards [`this_cpu`] against the one moment it would read garbage: before
/// the register has been written at all, when it holds whatever firmware left
/// there. A secondary processor installs its record before it runs any code
/// that could ask, so this only ever needs to cover the boot processor.
static LOCAL_READY: AtomicBool = AtomicBool::new(false);

/// Set by a panic and never cleared: a processor that sees it stops.
///
/// Looked at where every processor arrives whether or not it has work — the
/// inter-processor interrupt, and the wait that answers other processors'
/// shootdowns — and not in the scheduler, whose locks the panicking processor
/// may be holding.
static STOPPING: AtomicBool = AtomicBool::new(false);

/// Find every processor firmware describes, and install the boot processor's
/// record.
///
/// Must run after `arch::init_interrupts`, because x86-64 learns which
/// processor it is by reading its own local APIC, which that call maps; and
/// before `mm::reclaim_boot_memory`, which gives the tables this reads back
/// to the frame allocator.
///
/// # Errors
///
/// If the tables cannot be read, describe a list no machine could have, or
/// the boot processor's record does not read back as its own.
pub(crate) fn discover(view: &BootView<'_>) -> Result<&'static Topology, &'static str> {
    let topology = Topology::from_described(arch::describe_cpus(view)?)?;
    let topology = TOPOLOGY.call_once(|| topology);

    let boot = topology
        .cpus
        .first()
        .ok_or("there is no record for the boot processor")?;
    // SAFETY: `boot` is the boot processor's own record, this is the boot
    // processor, and the record lives in a slice leaked for the life of the
    // system.
    unsafe { arch::set_cpu_local(boot.this) };
    LOCAL_READY.store(true, Ordering::Release);

    check_this_cpu(boot)?;
    Ok(topology)
}

/// How long a secondary has to report in before bring-up gives up on it.
///
/// Generous by orders of magnitude on hardware and by one under an emulator,
/// and short enough that a core which never answers costs the boot test a
/// second rather than its whole timeout.
const START_TIMEOUT_NANOS: u64 = 1_000_000_000;

/// Start every processor other than this one, one at a time.
///
/// One at a time for two reasons. The architecture's start sequence reads its
/// parameters from one shared block, which cannot be rewritten until the core
/// reading it is done; and a core that fails to come up should be named, not
/// lost in a crowd.
///
/// # Errors
///
/// If a processor cannot be started, or is started and does not report in.
pub(crate) fn start_secondaries(view: &BootView<'_>) -> Result<(), &'static str> {
    let topology = TOPOLOGY
        .get()
        .ok_or("the processors have not been discovered")?;

    // Before any secondary can unmask interrupts: an IPI arriving at a line
    // with nothing registered is counted as unclaimed and otherwise lost.
    crate::irq::register(arch::ipi_irq(), on_ipi)
        .map_err(|_| "the inter-processor interrupt's line is already taken")?;

    let mut starter = arch::CpuStarter::new(view)?;

    for cpu in topology.cpus.iter().skip(1) {
        // Never freed: nothing takes a processor offline, and this is the
        // stack it runs on for the rest of its life.
        let stack = crate::vmap::allocate_stack()
            .map_err(|_| "no kernel stack for a secondary processor")?;
        starter.start(cpu.hardware_id, stack.top, cpu.this)?;
        wait_until_online(cpu)?;
    }

    // Only now, and deliberately not on the failure paths above: a core that
    // has not reported in may still be about to read the start block, and
    // freeing it under that core would turn a missing processor into a
    // corrupted one.
    starter.finish()
}

/// Wait for `cpu` to say it is running, for at most [`START_TIMEOUT_NANOS`].
fn wait_until_online(cpu: &PerCpu) -> Result<(), &'static str> {
    let deadline = crate::timer::now_nanos().saturating_add(START_TIMEOUT_NANOS);
    while !cpu.is_online() {
        if crate::timer::now_nanos() > deadline {
            return Err("a secondary processor was started and never reported in");
        }
        spin_loop();
    }
    Ok(())
}

/// Where every secondary processor arrives from its architecture's start
/// path: in the upper half, on its own stack, with its trap vectors and its
/// interrupt controller up and every interrupt masked.
///
/// `record` is the address [`start_secondaries`] handed the architecture for
/// this processor, and is checked rather than trusted: it crossed a start
/// sequence written in assembly to get here.
pub(crate) fn secondary_main(record: u64) -> ! {
    let expected = TOPOLOGY
        .get()
        .and_then(|topology| topology.cpus.iter().find(|cpu| cpu.this == record));
    let Some(expected) = expected else {
        crate::panic::fatal!(
            crate::panic::catalog::SECONDARY_NO_RECORD,
            "a secondary processor arrived with no record of its own"
        );
    };

    // SAFETY: `expected` is a record in the leaked slice, and it is this
    // processor's own: `start_secondaries` passed its address to the start of
    // exactly this processor and no other.
    unsafe { arch::set_cpu_local(expected.this) };
    if let Err(problem) = check_this_cpu(expected) {
        crate::panic::fatal!(
            crate::panic::catalog::SECONDARY_RECORD_MISMATCH,
            "secondary processor {}: {problem}",
            expected.logical
        );
    }
    // Work handed out before this processor existed is not its to do.
    expected
        .job_done
        .store(JOB_GENERATION.load(Ordering::SeqCst), Ordering::Relaxed);

    // Online first, then look at the shootdown generation, then flush — in
    // that order, and sequentially consistent, against a shootdown's
    // "advance the generation, then look at who is online". Of any pair,
    // at least one sees the other: either the shootdown sees this processor
    // online and waits for it, or this processor sees the shootdown's
    // generation and the flush below covers it. A processor that came online
    // in between with a stale translation is the case neither would catch.
    expected.online.store(true, Ordering::SeqCst);
    let generation = TLB_GENERATION.load(Ordering::SeqCst);
    arch::flush_tlb();
    let _ = expected.tlb_seen.fetch_max(generation, Ordering::SeqCst);

    // The same handshake for grace periods, and a simpler one: a processor
    // that has only just come online is inside no read-side section, so it
    // can answer for every grace period requested so far.
    let _ = expected
        .gp_seen
        .fetch_max(GRACE_GENERATION.load(Ordering::SeqCst), Ordering::SeqCst);

    loop {
        // Masked while looking, so that the look and the wait are one step:
        // an interrupt sent after the look wakes the wait rather than being
        // taken, and forgotten, just before it.
        arch::disable_interrupts();

        // **Where this processor stops being stage 4's and becomes stage 5's.**
        // Until the scheduler exists this loop is all there is to do; once it
        // does, this processor's job is to run tasks, and `enter_idle` never
        // returns. Checked inside the masked region so that the handover
        // cannot happen between looking for work and waiting for it.
        //
        // Nothing after this point hands work out: `run_everywhere` belongs to
        // stage 4's checks, which have finished by the time `sched::init`
        // runs. What still reaches every processor — TLB shootdown, grace
        // periods — arrives as an interrupt, and an idle processor takes those
        // exactly as this loop did.
        if crate::sched::started() {
            crate::sched::enter_idle();
        }

        match next_job(expected) {
            Some((generation, work)) => {
                arch::enable_interrupts();
                work(expected);
                expected.job_done.store(generation, Ordering::Release);
            }
            None => arch::wait_for_work(),
        }
    }
}

/// Work to run on every processor, handed that processor's own record.
///
/// A function rather than a closure: the same code runs on every processor at
/// once, so anything it shares lives somewhere every processor can reach —
/// a static — rather than on the stack of the one that handed it out.
pub(crate) type Work = fn(&'static PerCpu);

/// The work most recently handed out.
static JOB: SpinLock<Option<Work>> = SpinLock::new(None);

/// How many pieces of work have been handed out. A processor whose `job_done`
/// is behind this has work to do.
static JOB_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Held by whoever is handing out work, so there is one piece at a time.
static HANDING_OUT: SpinLock<()> = SpinLock::new(());

/// How long a processor has to finish its share of a piece of work before the
/// caller gives up on it.
///
/// Long, because the work is the stage's tests and some of them are meant to
/// take a while; finite, because a processor that never finishes should be
/// named in the boot log rather than discovered by the test's timeout.
const WORK_TIMEOUT_NANOS: u64 = 30_000_000_000;

/// Run `work` on every online processor, this one included, and return once
/// all of them have finished.
///
/// # Errors
///
/// If the other processors cannot be interrupted, or one does not finish
/// within [`WORK_TIMEOUT_NANOS`].
pub(crate) fn run_everywhere(work: Work) -> Result<(), &'static str> {
    let _one_at_a_time = HANDING_OUT.lock();
    let topology = TOPOLOGY
        .get()
        .ok_or("the processors have not been discovered")?;
    let me = this_cpu().ok_or("the per-CPU register is not installed")?;

    *JOB.lock() = Some(work);
    let generation = JOB_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    arch::send_ipi_to_others()?;

    work(me);
    me.job_done.store(generation, Ordering::Release);

    let deadline = crate::timer::now_nanos().saturating_add(WORK_TIMEOUT_NANOS);
    for cpu in topology.cpus.iter().filter(|cpu| cpu.is_online()) {
        while cpu.job_done.load(Ordering::Acquire) < generation {
            if crate::timer::now_nanos() > deadline {
                return Err("a processor did not finish its share of the work");
            }
            // The work may be waiting on this processor's TLB.
            service_tlb(me);
            spin_loop();
        }
    }
    Ok(())
}

/// The work `me` has not done yet, if there is any, with its generation.
fn next_job(me: &PerCpu) -> Option<(u64, Work)> {
    let generation = JOB_GENERATION.load(Ordering::SeqCst);
    if generation <= me.job_done.load(Ordering::Relaxed) {
        return None;
    }
    // Written before the generation was advanced, and not rewritten until
    // every online processor has finished it — so this is the right work.
    let work = (*JOB.lock())?;
    Some((generation, work))
}

/// What an inter-processor interrupt does.
///
/// Every reason one is sent is answered here, whichever of them it was sent
/// for: a flush if a shootdown is outstanding, the answer to a grace period,
/// and — by returning — a wake-up for a processor waiting in
/// [`secondary_main`], which looks for work next. Answering all of them every
/// time is what lets one interrupt stand for several, which is what two sent
/// before the first is taken become.
fn on_ipi(_irq: u32) {
    halt_if_stopping();
    if let Some(me) = this_cpu() {
        let _ = me.ipis.fetch_add(1, Ordering::Relaxed);
        service_tlb(me);
        // Being here is the answer: an interrupt is never taken inside a
        // read-side section, so this processor is outside one, and was when
        // every grace period requested so far began waiting.
        let _ = me
            .gp_seen
            .fetch_max(GRACE_GENERATION.load(Ordering::SeqCst), Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// TLB shootdown
// ---------------------------------------------------------------------------

/// Shootdowns requested so far. A processor whose `tlb_seen` is behind this
/// may still hold translations it has been told to drop.
static TLB_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Held by the processor running a shootdown, so there is one at a time.
static SHOOTING: SpinLock<()> = SpinLock::new(());

/// Shootdowns run, for the boot log.
static SHOOTDOWNS: AtomicU64 = AtomicU64::new(0);

/// How long a shootdown waits for every processor before calling it fatal.
const SHOOTDOWN_TIMEOUT_NANOS: u64 = 1_000_000_000;

/// How often a processor waiting on all the others re-sends its interrupt.
///
/// Belt and braces. The handshake in [`secondary_main`] already covers the
/// processor that comes online in the middle of a wait, and an interrupt is
/// not otherwise lost — but a lost one would cost the whole timeout and the
/// machine, and a second one costs a flush.
const KICK_NANOS: u64 = 10_000_000;

/// Drop stale translations from every processor's TLB, and return once every
/// processor has.
///
/// What an unmap or a narrowed permission has to call before it frees the
/// memory or relies on the permission. Where the architecture's own
/// invalidation is broadcast that is all this does; elsewhere it interrupts
/// every other processor and waits for each to flush.
///
/// **Must not be called holding a lock another processor might be spinning
/// on with interrupts masked.** That processor could not take the interrupt,
/// and this would wait for it until the timeout. The waiting itself services
/// other processors' shootdowns, so two running at once do not deadlock on
/// each other.
pub(crate) fn flush_tlb_everywhere() {
    arch::flush_tlb();
    if arch::TLB_FLUSH_IS_BROADCAST {
        return;
    }
    // Before discovery, or with nobody else running, there is nobody to tell.
    let (Some(topology), Some(me)) = (TOPOLOGY.get(), this_cpu()) else {
        return;
    };
    if topology.online() <= 1 {
        return;
    }

    let _turn = loop {
        if let Some(turn) = SHOOTING.try_lock() {
            break turn;
        }
        service_tlb(me);
        spin_loop();
    };

    // This processor flushed above, after every change the caller made.
    let generation = TLB_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let _ = me.tlb_seen.fetch_max(generation, Ordering::SeqCst);
    let _ = arch::send_ipi_to_others();

    wait_for_everyone(
        topology,
        me,
        SHOOTDOWN_TIMEOUT_NANOS,
        "flushed its TLB for a shootdown",
        &crate::panic::catalog::SHOOTDOWN_TIMEOUT,
        |cpu| cpu.tlb_seen.load(Ordering::SeqCst) >= generation,
    );
    let _ = SHOOTDOWNS.fetch_add(1, Ordering::Relaxed);
}

/// Wait until `done` holds for every online processor.
///
/// Shared by the two things that interrupt every other processor and wait for
/// each to answer. While it waits it answers other processors' shootdowns —
/// two processors each waiting for the other would otherwise wait forever —
/// and re-sends its own interrupt every [`KICK_NANOS`]. After `timeout` it
/// gives up on the machine: a processor that never answers is one whose TLB
/// or whose read-side section nothing can vouch for any more, and carrying on
/// would be carrying on regardless.
fn wait_for_everyone(
    topology: &Topology,
    me: &PerCpu,
    timeout: u64,
    what: &str,
    entry: &'static crate::panic::catalog::Explanation,
    done: impl Fn(&PerCpu) -> bool,
) {
    let started = crate::timer::now_nanos();
    let mut kicked = started;
    for cpu in topology.cpus.iter().filter(|cpu| cpu.is_online()) {
        while !done(cpu) {
            halt_if_stopping();
            service_tlb(me);
            let now = crate::timer::now_nanos();
            if now.saturating_sub(started) > timeout {
                crate::panic::fatal!(*entry, "processor {} never {what}", cpu.logical);
            }
            if now.saturating_sub(kicked) > KICK_NANOS {
                let _ = arch::send_ipi_to_others();
                kicked = now;
            }
            spin_loop();
        }
    }
}

/// Flush this processor's TLB if a shootdown is waiting for it to.
///
/// One flush answers every shootdown requested so far: each asks for the whole
/// TLB, and a flush drops whatever any of them wanted dropped.
fn service_tlb(me: &PerCpu) {
    let wanted = TLB_GENERATION.load(Ordering::SeqCst);
    if me.tlb_seen.load(Ordering::SeqCst) < wanted {
        arch::flush_tlb();
        // A maximum rather than a store: an interrupt taken between the load
        // above and here may have recorded a later generation already, and
        // a store would take it back.
        let _ = me.tlb_seen.fetch_max(wanted, Ordering::SeqCst);
    }
}

/// How many shootdowns have run.
pub(crate) fn shootdowns() -> u64 {
    SHOOTDOWNS.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Grace periods
// ---------------------------------------------------------------------------

/// Grace periods requested so far. A processor whose `gp_seen` is behind this
/// may still be inside a read-side section the latest one has to wait for.
static GRACE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Grace periods completed, for the boot log.
static GRACE_PERIODS: AtomicU64 = AtomicU64::new(0);

/// How long a grace period may take before it is called fatal.
///
/// Much longer than a shootdown's, because a shootdown waits for a flush and
/// this waits for other code to finish. But a read-side section that lasts
/// five seconds is a bug in its own right, and one worth a line in the log.
const GRACE_TIMEOUT_NANOS: u64 = 5_000_000_000;

/// Run `body` as a read-side section.
///
/// Inside one, a pointer loaded from shared data stays valid until the
/// section ends, even if a writer unpublishes the object meanwhile: the
/// writer frees it only after [`synchronize`], which waits for every section
/// that could have loaded it.
///
/// A section masks interrupts, and that is the whole mechanism.
/// [`synchronize`] interrupts every other processor and waits for each to
/// take the interrupt, which none can do inside a section — so once every one
/// has, every section that was running when it started has ended. It costs a
/// reader two instructions; what it asks of one is to be short and never to
/// wait for anything, since a processor inside a section answers nobody.
///
/// An interrupt handler is a read-side section already, for the same reason,
/// which is what will let a handler be unregistered safely: take it out of the
/// table, [`synchronize`], and nothing is still running it.
pub(crate) fn read_section<T>(body: impl FnOnce() -> T) -> T {
    let saved = <arch::Irq as IrqControl>::disable();
    let result = body();
    <arch::Irq as IrqControl>::restore(saved);
    result
}

/// Wait for a grace period: return once every read-side section that was
/// running anywhere when this was called has ended.
///
/// What a writer calls between unpublishing an object and freeing it — and
/// what the scheduler's domain-mode switch will call between retiring one set
/// of scheduling classes and installing another.
///
/// Must not be called from inside a [`read_section`], which would wait for
/// itself; nor holding a lock another processor may be spinning on with
/// interrupts masked, for the reason [`flush_tlb_everywhere`] gives.
pub(crate) fn synchronize() {
    let (Some(topology), Some(me)) = (TOPOLOGY.get(), this_cpu()) else {
        return;
    };

    let generation = GRACE_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    // This processor is outside any section: that is this function's
    // contract, and why it may answer for itself.
    let _ = me.gp_seen.fetch_max(generation, Ordering::SeqCst);

    if topology.online() > 1 {
        let _ = arch::send_ipi_to_others();
        wait_for_everyone(
            topology,
            me,
            GRACE_TIMEOUT_NANOS,
            "left a read-side section for a grace period",
            &crate::panic::catalog::GRACE_PERIOD_TIMEOUT,
            |cpu| cpu.gp_seen.load(Ordering::SeqCst) >= generation,
        );
    }
    let _ = GRACE_PERIODS.fetch_add(1, Ordering::Relaxed);
}

/// How many grace periods have completed.
pub(crate) fn grace_periods() -> u64 {
    GRACE_PERIODS.load(Ordering::Relaxed)
}

/// The record of the processor this is running on, or `None` before the
/// boot processor has installed its own.
pub(crate) fn this_cpu() -> Option<&'static PerCpu> {
    if !LOCAL_READY.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: the boot processor installed its record before the flag above
    // was set, and a secondary installs its own before running anything that
    // could reach here.
    let at = unsafe { arch::cpu_local() };
    // SAFETY: every processor's register holds the address of its own record
    // in the slice `Topology::from_described` leaked, which lives forever and
    // is only ever reached through shared references.
    Some(unsafe { &*(at as *const PerCpu) })
}

/// Ask every other processor to stop, for a panic, and return how many were
/// asked.
///
/// None before the secondaries are started: until then there is nobody to
/// ask, and on x86-64 the local APIC the interrupt would be sent through may
/// not be mapped yet. A processor running with interrupts masked outside the
/// shootdown wait does not stop until it next looks; this is the best a
/// panic can do without a non-maskable interrupt.
pub(crate) fn stop_others() -> usize {
    STOPPING.store(true, Ordering::Release);
    let Some(topology) = TOPOLOGY.get() else {
        return 0;
    };
    let others = topology.online().saturating_sub(1);
    if others > 0 {
        let _ = arch::send_ipi_to_others();
    }
    others
}

/// Stop this processor if a panic has asked every processor to.
fn halt_if_stopping() {
    if STOPPING.load(Ordering::Acquire) {
        arch::halt()
    }
}

/// Which processor this is, for a failure report.
///
/// [`this_cpu`] follows the per-CPU register, which on a processor that has
/// not installed its record yet points wherever firmware left it — exactly the
/// processor most likely to be reporting a failure. This compares the
/// register's value against the address of every record instead, and believes
/// it only if it is one of them. The error says what can be said instead.
pub(crate) fn this_cpu_for_report() -> Result<&'static PerCpu, &'static str> {
    let Some(topology) = TOPOLOGY.get() else {
        return Err("the boot processor, before any other was started");
    };
    let register = arch::cpu_local_register();
    topology
        .cpus
        .iter()
        .find(|cpu| cpu.this == register)
        .ok_or("a processor that has not installed its per-CPU record")
}

/// Require that this processor's register leads back to `expected`, and that
/// the record names the processor that is actually reading it.
///
/// Two different mistakes, and each has a symptom that appears somewhere
/// else: a register pointing at another CPU's record makes two processors
/// share per-CPU state, and a record naming the wrong hardware sends every
/// interrupt meant for this processor to another one.
fn check_this_cpu(expected: &PerCpu) -> Result<(), &'static str> {
    let me = this_cpu().ok_or("the per-CPU register is not installed")?;
    if !core::ptr::eq(me, expected) {
        return Err("this processor's per-CPU register points at another record");
    }
    if me.this != (&raw const *me) as u64 {
        return Err("a per-CPU record does not hold its own address");
    }
    let indexed = TOPOLOGY
        .get()
        .and_then(|topology| topology.cpus.get(me.logical));
    if !indexed.is_some_and(|record| core::ptr::eq(record, me)) {
        return Err("a per-CPU record's logical number does not lead back to it");
    }
    if me.hardware_id != arch::hardware_id() {
        return Err("this processor's per-CPU record names a different processor");
    }
    Ok(())
}
