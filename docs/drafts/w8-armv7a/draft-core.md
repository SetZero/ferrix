# Area C: ARMv7-A CPU primitives, context switch, secondaries, timer

Draft for `docs/sysml/24-armv7a-requirements.sysml`. Worktree read at
b9b7d26e. 26 requirements, `L.armv7a.C1` to `L.armv7a.C26`. All 66 units in
area-core.txt are covered (the unit check is at the end).

## Requirements

```sysml
    package Context {
        doc /* kernel/src/arch/armv7a/switch.rs and the thread-pointer and FPU
             * wrappers in cpu.rs: moving between SVC stacks, and the state a
             * trap does not save (TPIDRURO, USR mode's banked sp and lr, FPSCR
             * and d0 to d31), carried across a switch, a fork, an execve and a
             * signal frame. */

        requirement <'L.armv7a.C1'> stacksSwitch : ItemLowLevel {
            attribute :>> statement = "prepare_stack shall lay out a stack so that
                the first switch_to it calls entry with argument, and switch_to
                shall save one context's r4 to r11, link register and SVC stack
                pointer and resume another's.";
            attribute :>> criterion = "1000 threads spawned on one processor all
                run to completion, each switched to at least once (the `tasks`
                line).";
            attribute :>> parent = "H.SCHED.1";
            attribute :>> unit = ("arch::armv7a::switch::switch_to",
                "arch::armv7a::switch::prepare_stack");
        }
        requirement <'L.armv7a.C2'> userStateFollowsTheTask : ItemLowLevel {
            attribute :>> statement = "save_user_state and restore_user_state
                shall carry a user task's TPIDRURO, USR mode's banked stack
                pointer and link register, FPSCR and d0 to d15, or d0 to d31 on a
                core with 32, across a switch, so that each task runs in USR mode
                with its own.";
            attribute :>> criterion = "Two programs pinned to one processor, each
                with its own set_tls pointer, stack pointer and d-register and
                FPSCR values, read back their own across N sched_yields: 0
                mismatches. Not written: every musl program's thread-local
                accesses would break without it, but no check asserts it.";
            attribute :>> parent = "H.SCHED.1";
            attribute :>> unit = ("arch::armv7a::switch::save_user_state",
                "arch::armv7a::switch::restore_user_state",
                "arch::armv7a::cpu::read_tpidruro", "arch::armv7a::cpu::write_tpidruro");
        }
        requirement <'L.armv7a.C3'> forkChildInheritsUserState : ItemLowLevel {
            attribute :>> statement = "UserState::capture shall copy the calling
                task's TPIDRURO, banked USR stack pointer and link register, FPSCR
                and double registers, for a fork child to start with.";
            attribute :>> criterion = "A fork child reads its parent's thread
                pointer, FPSCR and a d-register value set before the fork. Not
                written.";
            attribute :>> parent = "H.SCHED.1";
            attribute :>> unit = "arch::armv7a::switch::UserState::capture";
        }
        requirement <'L.armv7a.C4'> execveStartsFromTheResetState : ItemLowLevel {
            attribute :>> statement = "reset_user_state shall give an execve'd
                program no thread pointer, FPSCR 0 and every double register zero,
                and set_user_stack shall load its new stack into USR mode's banked
                stack pointer, with the banked link register 0.";
            attribute :>> criterion = "A program that set a thread pointer, a
                non-default FPSCR and a d-register and then calls execve finds, in
                the new image, TPIDRURO 0, FPSCR 0 and the register 0. Not
                written.";
            attribute :>> parent = "H.SCHED.1";
            attribute :>> unit = ("arch::armv7a::switch::reset_user_state",
                "arch::armv7a::switch::set_user_stack");
        }
        requirement <'L.armv7a.C5'> signalFramesReachTheUnsavedState : ItemLowLevel {
            attribute :>> statement = "user_banked and set_user_banked shall read
                and write USR mode's banked stack pointer and link register, and
                set_fp with load_user_fpu shall load the FPSCR and double registers
                a signal frame holds, so that a handler's frame carries them and
                sigreturn puts them back.";
            attribute :>> criterion = "A handler that changes its frame's VFP
                record (FPSCR and a d-register) and its saved sp and lr returns to
                find all four changed. Not written: the trap check's no-restorer
                program exits from inside its handler and never returns.";
            attribute :>> parent = "H.TRAP.9";
            attribute :>> unit = ("arch::armv7a::switch::user_banked",
                "arch::armv7a::switch::set_user_banked",
                "arch::armv7a::switch::load_user_fpu",
                "arch::armv7a::switch::UserState::set_fp");
        }
        requirement <'L.armv7a.C6'> programsMayUseTheFpu : ItemLowLevel {
            attribute :>> statement = "enable_user_fpu shall, on each core, grant
                USR mode coprocessors 10 and 11 and set FPEXC.EN where CPACR keeps
                the grant, touch FPEXC nowhere else, and record MVFR0 and MVFR1 and
                whether a program's state has 16 or 32 double registers, or none.";
            attribute :>> criterion = "A hard-float program's VFP instruction runs
                on every core rather than raising SIGILL, and the recorded count is
                32 on QEMU's cortex-a15 and the DK1's Cortex-A7. Not written as a
                check; every hard-float program the images carry needs it.";
            attribute :>> parent = "H.SCHED.1";
            attribute :>> unit = ("arch::armv7a::cpu::enable_user_fpu",
                "arch::armv7a::switch::fpu_enable", "arch::armv7a::switch::fpu_features",
                "arch::armv7a::switch::fpu_features1");
        }
        requirement <'L.armv7a.C7'> programsSeeTheCoresFeatures : ItemLowLevel {
            attribute :>> statement = "read_id_isar0 and read_id_mmfr0 shall give
                user_hwcaps ID_ISAR0 and ID_MMFR0, which with MVFR0 and MVFR1 are
                what AT_HWCAP is computed from.";
            attribute :>> criterion = "The mapping is ferrix-linux-abi's hwcap,
                host-tested against Linux's bits; that the registers read here
                reach it is not checked on the machine.";
            attribute :>> parent = "H.TRAP.4";
            attribute :>> unit = ("arch::armv7a::cpu::read_id_isar0",
                "arch::armv7a::cpu::read_id_mmfr0");
        }
    }

    package InterruptMask {
        doc /* The interrupt-mask half of cpu.rs: CPSR's A, I and F bits,
             * waiting for an interrupt, and the barrier before an IPI. */

        requirement <'L.armv7a.C8'> interruptMaskFollowsTheCaller : ItemLowLevel {
            attribute :>> statement = "disable_interrupts shall mask asynchronous
                aborts, IRQs and FIQs, enable_interrupts shall unmask IRQs alone,
                restore_interrupt_mask shall put back exactly the A, I and F bits a
                read_cpsr saw and no other bit, and wait_then_enable_interrupts
                shall wait with IRQs masked and then unmask them, so that an
                interrupt pending before the wait ends it.";
            attribute :>> criterion = "A nested disable and restore leaves CPSR's
                A, I and F bits as they were at each level, and a wait entered with
                an interrupt already pending returns. Not written: every lock that
                masks interrupts relies on it.";
            attribute :>> parent = "H.IRQ.1";
            attribute :>> unit = ("arch::armv7a::cpu::disable_interrupts",
                "arch::armv7a::cpu::enable_interrupts", "arch::armv7a::cpu::read_cpsr",
                "arch::armv7a::cpu::restore_interrupt_mask",
                "arch::armv7a::cpu::wait_then_enable_interrupts", "arch::armv7a::cpu::wfi");
        }
        requirement <'L.armv7a.C9'> ipisFollowTheSendersStores : ItemLowLevel {
            attribute :>> statement = "dsb_ishst shall complete every store before
                it, as the inner shareable domain sees them, before the
                distributor write that sends a software-generated interrupt.";
            attribute :>> criterion = "A processor woken by an IPI reads what the
                sender stored before sending it. QEMU does not reorder, so no gate
                can fail without it; the DK1's two Cortex-A7s are where it shows.
                Not written.";
            attribute :>> parent = "H.MEM.7";
            attribute :>> unit = "arch::armv7a::cpu::dsb_ishst";
        }
    }

    package Timer {
        doc /* kernel/src/arch/armv7a/timer.rs and its coprocessor 15 wrappers:
             * the generic timer's virtual counter and comparator. */

        requirement <'L.armv7a.C10'> theTimerWakesASleeper : ItemLowLevel {
            attribute :>> statement = "timer::arm shall set the virtual timer's
                comparator to the counter, read after an isb, plus nanos in
                counter ticks, at least one, and enable it, so that a timed sleep
                is woken.";
            attribute :>> criterion = "A 20 ms sleep returns after at least 20 ms
                and at most 400 ms (the `sleep` line).";
            attribute :>> parent = "H.SCHED.5";
            attribute :>> unit = ("arch::armv7a::timer::arm",
                "arch::armv7a::cpu::write_cntv_cval", "arch::armv7a::cpu::write_cntv_ctl",
                "arch::armv7a::cpu::read_cntvct", "arch::armv7a::cpu::read_cntfrq");
        }
        requirement <'L.armv7a.C11'> theTimerIsDescribed : ItemLowLevel {
            attribute :>> statement = "timer::init shall take the timer's interrupt
                from the device tree's virtual timer entry when it is a private
                one, 16 to 31, and 27 otherwise, shall refuse a CNTFRQ that reads
                zero, and shall leave the timer disarmed.";
            attribute :>> criterion = "Timer nodes built for a check -- one
                naming PPI 11 (interrupt 27), one naming a shared interrupt, one
                with none -- give 27, 27 and 27, and one naming PPI 14 gives 30.
                Not written; every boot takes its ticks on 27, and a zero CNTFRQ
                cannot be made on QEMU.";
            attribute :>> parent = "H.SCHED.5";
            attribute :>> unit = "arch::armv7a::timer::init";
        }
    }

    package Translation {
        doc /* The translation and TLB half of cpu.rs: TTBR0 and TTBCR.EPD0,
             * which carry a program's tables and the loader's identity map, and
             * the TLB and branch-predictor invalidations. */

        requirement <'L.armv7a.C12'> userRootIsTranslated : ItemLowLevel {
            attribute :>> statement = "write_ttbr0 shall make the processor
                translate user addresses through the root it is given, writing
                TTBR0 before clearing TTBCR.EPD0, and flush_user_tlb shall drop the
                non-global translations the previous address space left, which
                share its ASID 0.";
            attribute :>> criterion = "Two tasks pinned to one processor, each in
                its own address space, read one user address 64 times each across
                yields: 128 reads see their own space's value, 0 the other's (the
                `spaces` line).";
            attribute :>> parent = "H.MEM.1";
            attribute :>> unit = ("arch::armv7a::cpu::write_ttbr0",
                "arch::armv7a::cpu::flush_user_tlb");
        }
        requirement <'L.armv7a.C13'> theLowerHalfIsSwitchedOff : ItemLowLevel {
            attribute :>> statement = "disable_ttbr0 shall stop the lower half
                translating on this processor -- TTBCR.EPD0 set, TTBR0 zeroed, this
                processor's TLB invalidated -- and read_ttbcr and read_ttbr0 shall
                read TTBCR and the 64-bit TTBR0 back, so that identity_map_live can
                say whether the loader's regime still translates.";
            attribute :>> criterion = "Stage 1 requires identity_map_live false
                after drop_identity_map; it is in main.rs, product code. A check
                that a kernel thread's lower-half read faults is not written.";
            attribute :>> parent = ("H.BOOT.1", "H.MEM.1");
            attribute :>> unit = ("arch::armv7a::cpu::disable_ttbr0",
                "arch::armv7a::cpu::read_ttbcr", "arch::armv7a::cpu::read_ttbr0");
        }
        requirement <'L.armv7a.C14'> aPageIsDroppedEverywhere : ItemLowLevel {
            attribute :>> statement = "flush_tlb_page shall make every table write
                before it visible to the walker and invalidate the page's
                translation for every ASID, and the branch predictor, on every
                processor in the inner shareable domain (TLBIMVAAIS, BPIALLIS).";
            attribute :>> criterion = "20 remaps of a kernel page to a new frame
                are each seen by every online processor: 0 stale reads (the `tlb`
                line, invalidated by broadcast).";
            attribute :>> parent = ("H.MEM.7", "H.MEM.12");
            attribute :>> unit = "arch::armv7a::cpu::flush_tlb_page";
        }
        requirement <'L.armv7a.C15'> theWholeTlbIsDroppedEverywhere : ItemLowLevel {
            attribute :>> statement = "flush_tlb shall make every table write before
                it visible to the walker and invalidate every translation, global
                ones included, and the branch predictor, on every processor in the
                inner shareable domain (TLBIALLIS, BPIALLIS).";
            attribute :>> criterion = "A global kernel page remapped and flushed
                with flush_tlb is seen at its new frame by every online processor.
                Not written: the shootdown check's remaps go through
                flush_tlb_page, and the migrating check is skipped where the flush
                is broadcast.";
            attribute :>> parent = "H.MEM.7";
            attribute :>> unit = "arch::armv7a::cpu::flush_tlb";
        }
    }

    package Caches {
        doc /* The cache-maintenance and barrier half of cpu.rs: memory a core
             * with its caches off or a device that does not snoop reads, code
             * written through the data side, and the order a DMA master sees. */

        requirement <'L.armv7a.C16'> flushedBuffersKeepTheirBytes : ItemLowLevel {
            attribute :>> statement = "clean_invalidate_to_poc shall operate on
                every data cache line covering a range, by the smallest line CTR
                gives, the partial first and last lines included, and leave every
                byte of the range as it was.";
            attribute :>> criterion = "A 1000-byte patterned buffer flushed from
                byte 3 for 990 bytes, so that the first and last lines are
                partial, holds its pattern afterwards, and nothing faults (the
                `machine` line). On QEMU, which models no caches, only the bounds
                can fail; the DK1 is where the contents can.";
            attribute :>> parent = "H.MEM.6";
            attribute :>> unit = ("arch::armv7a::cpu::clean_invalidate_to_poc",
                "arch::armv7a::cpu::data_line");
        }
        requirement <'L.armv7a.C17'> cachesReachThePointOfCoherency : ItemLowLevel {
            attribute :>> statement = "clean_to_poc shall write back every data
                cache line covering a range to the point of coherency and complete
                it with a dsb, and sync_instructions shall then invalidate every
                instruction cache and branch predictor in the inner shareable
                domain, so that a core with its caches off, a device that does not
                snoop, and every core's instruction fetch see what was written.";
            attribute :>> criterion = "A secondary that starts with its caches off
                reads the start block the boot core wrote, and a program's page of
                code runs as written; on QEMU, which models no caches, neither can
                fail, and a real core's evidence is the DK1's boot. Not a check.";
            attribute :>> parent = "H.MEM.6";
            attribute :>> unit = ("arch::armv7a::cpu::clean_to_poc",
                "arch::armv7a::cpu::sync_instructions");
        }
        requirement <'L.armv7a.C18'> dmaAccessesKeepTheirOrder : ItemLowLevel {
            attribute :>> statement = "dma_barrier shall order every memory access
                before it against every access after it, a following write to a
                device register included, as a DMA master in the outer shareable
                domain observes them (dmb osh).";
            attribute :>> criterion = "A virtqueue's descriptors are seen by the
                device before the index that publishes them, and the used index
                before the entry it counts. No emulator reorders, so no gate fails
                without it (F-44); a run on the DK1 is the evidence. Not written.";
            attribute :>> parent = "H.DMA.2";
            attribute :>> unit = "arch::armv7a::cpu::dma_barrier";
        }
    }

    package Processors {
        doc /* kernel/src/arch/armv7a/smp.rs and the processor half of cpu.rs:
             * which processors the device tree says can be started, starting
             * them through PSCI with the loader's regime copied, the per-CPU
             * register, and what each core says of its coherency. */

        requirement <'L.armv7a.C19'> processorsAreDescribed : ItemLowLevel {
            attribute :>> statement = "describe_cpus shall name, by MPIDR affinity,
                this processor as the boot processor and every other /cpus node
                whose enable-method is psci, or has none where the tree describes
                PSCI, and this processor alone under nosmp from the loader's
                command line or /chosen/bootargs.";
            attribute :>> criterion = "Trees built for a check -- nodes with and
                without enable-method, one naming spin-table, with and without a
                PSCI node, with nosmp -- are each given the processors listed. Not
                written; test-boot's `cpus` line counts 2 of 2 at --smp 2.";
            attribute :>> parent = "H.BOOT.5";
            attribute :>> unit = "arch::armv7a::smp::describe_cpus";
        }
        requirement <'L.armv7a.C20'> secondariesStart : ItemLowLevel {
            attribute :>> statement = "CpuStarter shall start each processor
                describe_cpus listed through PSCI CPU_ON at the entry sequence,
                identity mapped by install_identity, with a start block of this
                core's MAIR0, MAIR1, TTBCR, TTBR1 and SCTLR cleaned to the point
                of coherency, and secondary_start shall bring it into the kernel,
                so that it runs kernel work under its own per-processor record.";
            attribute :>> criterion = "Each of the N processors listed runs its
                share of the work in each of 100 rounds, 0 of it on a processor
                whose MPIDR affinity is not its record's (the `smp` line).";
            attribute :>> parent = "H.BOOT.5";
            attribute :>> unit = ("arch::armv7a::smp::CpuStarter::new",
                "arch::armv7a::smp::CpuStarter::start", "arch::armv7a::smp::secondary_start",
                "arch::armv7a::smp::install_identity", "arch::armv7a::smp::psci",
                "arch::armv7a::cpu::psci_call", "arch::armv7a::cpu::read_mpidr",
                "arch::armv7a::cpu::read_mair0", "arch::armv7a::cpu::read_mair1",
                "arch::armv7a::cpu::read_sctlr");
        }
        requirement <'L.armv7a.C21'> theStartupMapIsTakenDown : ItemLowLevel {
            attribute :>> statement = "CpuStarter::finish shall unmap the entry
                sequence's identity mapping from whichever tree install_identity
                put it in, give back its own root table where it had one, and give
                back the start block's frame.";
            attribute :>> criterion = "After finish, the entry sequence's physical
                address translates in no tree and the free frame count is what it
                was before CpuStarter::new. Not written.";
            attribute :>> parent = ("H.MEM.9", "H.MEM.4");
            attribute :>> unit = "arch::armv7a::smp::CpuStarter::finish";
        }
        requirement <'L.armv7a.C22'> coherencyIsReported : ItemLowLevel {
            attribute :>> statement = "Each processor shall read its ACTLR.SMP
                once in Rust, unless noactlr is given, and report_coherency shall
                say, once every core is up, whether it was not read, set on the one
                processor, set on all, clear on all, or set on only some, naming
                the last as incoherent.";
            attribute :>> criterion = "QEMU's cortex-a15 reads ACTLR as zero, so
                every --smp 2 boot prints the clear-on-all line and a noactlr boot
                the not-read line; judged by reading the log, not a check.";
            attribute :>> parent = "H.BOOT.5";
            attribute :>> unit = ("arch::armv7a::smp::note_coherency",
                "arch::armv7a::smp::report_coherency", "arch::armv7a::smp::yes_no",
                "arch::armv7a::cpu::read_actlr");
        }
        requirement <'L.armv7a.C23'> perCpuRecordIsTheProcessors : ItemLowLevel {
            attribute :>> statement = "write_tpidrprw shall point TPIDRPRW, which
                USR mode can neither read nor write, at this processor's own
                per-CPU record, and read_tpidrprw shall read it back.";
            attribute :>> criterion = "In 100 rounds of work run on every online
                processor at once, the record each processor's register gives it
                names its own hardware identifier every time: 0 misplaced runs
                (the `smp` line).";
            attribute :>> parent = "H.SCHED.6";
            attribute :>> unit = ("arch::armv7a::cpu::write_tpidrprw",
                "arch::armv7a::cpu::read_tpidrprw");
        }
    }

    package Machine {
        doc /* What remains of cpu.rs: the vector base, the frame pointer a
             * backtrace starts from, and PSCI's whole-system calls. */

        requirement <'L.armv7a.C24'> vectorsAreInstalled : ItemLowLevel {
            attribute :>> statement = "install_vectors shall point VBAR at the
                table and clear SCTLR.V and SCTLR.TE, so that this core's
                exceptions enter that table in ARM state.";
            attribute :>> criterion = "Four programs whose own udf, bkpt and
                misaligned ldm each end with 128 plus SIGILL, SIGTRAP and SIGBUS,
                and whose restorer-less handler exits 55, are run from the trap
                check.";
            attribute :>> parent = "H.TRAP.4";
            attribute :>> unit = "arch::armv7a::cpu::install_vectors";
        }
        requirement <'L.armv7a.C25'> backtracesStartAtTheFramePointer : ItemLowLevel {
            attribute :>> statement = "frame_pointer shall give r11, the address of
                the calling function's frame record, where a panic's backtrace
                starts.";
            attribute :>> criterion = "A panic's report lists the functions that
                called it; no boot check provokes one, and a deliberate crash is
                the failure path's evidence.";
            attribute :>> parent = "H.FAIL.1";
            attribute :>> unit = "arch::armv7a::cpu::frame_pointer";
        }
        requirement <'L.armv7a.C26'> theMachineStops : ItemLowLevel {
            attribute :>> statement = "psci_system shall make PSCI SYSTEM_OFF or
                SYSTEM_RESET, in the 32-bit convention, through the conduit the
                device tree names, and return if firmware does.";
            attribute :>> criterion = "Every ARMv7-A gate's QEMU exits when the
                boot powers off; a reset on this architecture is not asserted.
                Judged by test-boot, not a check.";
            attribute :>> parent = ("H.BOOT.6", "H.BOOT.7");
            attribute :>> unit = "arch::armv7a::cpu::psci_system";
        }
    }
```

## Verification

| id | verifying check | why |
|---|---|---|
| C1 | `sched::check::many_tasks` | Already `Verifies: L.x86_64.17` with this criterion; generic, runs on ARMv7-A, and fails if a switch or a prepared stack does not start/resume a task. |
| C2 | baseline | No check asserts TLS/USR sp/VFP per task on ARMv7-A (no armv7a counterpart of x86's pinned-programs checks). |
| C3 | baseline | No fork check reads TPIDRURO/FPSCR/d-registers in the child. |
| C4 | baseline | No execve check reads the reset thread pointer or VFP state. |
| C5 | baseline | trap/check.rs's no-restorer handler exits in the handler; nothing returns through sigreturn with a changed VFP record or sp/lr. |
| C6 | baseline | Exercised by every hard-float program, but no check asserts VFP access or the recorded count. |
| C7 | baseline | hwcap mapping is host-tested in ferrix-linux-abi; the register reads are not checked on the machine (same as 19's L.38). |
| C8 | baseline | No check of nested mask/restore on ARMv7-A (x86's L.x86_64.92 is untagged too). |
| C9 | baseline | Ordering is invisible under QEMU; `smp::check::everywhere` asserts IPIs arrive, not that stores precede them. |
| C10 | `sched::check::sleeping` | Already `Verifies: L.x86_64.91` with this criterion; on ARMv7-A the wake comes from timer::arm's comparator. |
| C11 | baseline | No check builds timer nodes; a zero CNTFRQ cannot be made on QEMU. |
| C12 | `user::check::read_own_space` | Already `Verifies: H.MEM.1, L.user.56, L.x86_64.110`; on ARMv7-A every space is ASID 0, so a missing flush_user_tlb or a wrong TTBR0/EPD0 order shows as the other space's value or a fault. |
| C13 | baseline | Stage 1's identity_map_live test is in main.rs, product code. |
| C14 | `smp::check::shootdown` | Already `Verifies: H.MEM.12, L.mm.28, L.smp.14, L.x86_64.108`; QEMU honours the IS broadcast, so a local-only invalidate gives stale reads. Please confirm that on ARMv7-A the unmap reaches `arch::flush_tlb_page` (smp.rs:897/1192/1300), not the whole flush. |
| C15 | baseline | No check tests the whole-TLB flush on ARMv7-A; `migrating_shootdown` returns early where `TLB_FLUSH_IS_BROADCAST`. |
| C16 | `arch::armv7a::check::check_cache_maintenance` | It calls `flush_for_device` = `cpu::clean_invalidate_to_poc` over an odd start and length and asserts the bytes survive. That is the whole criterion, and the criterion claims only that much. |
| C17 | baseline | QEMU models no caches; the evidence is the DK1 boot. |
| C18 | baseline | F-44: no emulator reorders; the commit says the DK1/crosvm run is still owed. |
| C19 | baseline | Listing rules (enable-method default, spin-table excluded, nosmp) are not asserted; a check building /cpus trees like check_chosen's could. |
| C20 | `smp::check::everywhere` | Already `Verifies: H.SCHED.6, L.smp.3, L.smp.11, L.x86_64.18 ...`; `runs` is sized by `topology.count()`, so a listed core that never started has 0 runs and fails it, and `hardware_id` reads MPIDR. |
| C21 | baseline | No check that the identity map is gone or the frames returned (x86's L.x86_64.20 is also untagged). |
| C22 | baseline | Log line only; QEMU does not model ACTLR. |
| C23 | `smp::check::everywhere` | Its MISPLACED count is exactly this (record through TPIDRPRW vs MPIDR); it already verifies H.SCHED.6. |
| C24 | `arch::armv7a::trap::check::run` | All four programs require the vector table to be entered. Better merged into the trap area's requirement for `trap::init` (see Notes). |
| C25 | baseline | No boot check panics. |
| C26 | baseline | Power-off is judged by test-boot's QEMU exit, not a check. |

Proposed verified: 8 requirements (C1, C10, C12, C14, C16, C20, C23 and C24)
by 7 checks, because C20 and C23 share `everywhere`. Baseline: 18.

## Check code in product files

None of my units is check code. `speculation::check` is the facade area's, and
`gicv2::idle_line_for_check` is arm_common's.

## Notes

1. **Units named twice or shared with other areas.** Each of my units is named
   in exactly one requirement. Some other areas' units should join these
   requirements when you merge:
   - facade `Irq::disable`/`Irq::restore` (and the unlisted
     `enable_interrupts`/`disable_interrupts`/`wait_for_work`/`halt`) → C8;
   - facade `send_ipi_to_others` → C9, or a separate "IPI reaches every
     secondary" requirement that `everywhere` verifies (x86's L.x86_64.19);
   - facade `install_user_root`/`uninstall_user_root` → C12/C13;
   - facade `drop_identity_map`/`identity_map_live` → C13;
   - facade `flush_tlb_page` → C14;
   - facade `shutdown`/`reset` → C26;
   - facade `user_hwcaps` → C7;
   - trap `trap::init` → C24. As in 19's L.aarch64.3, `install_vectors` could
     go into the trap area's "programs fault to their signal" requirement
     instead of standing alone.
   - `disable_ttbr0` does three jobs: drop the identity map, uninstall a user
     root, and let a secondary leave its start map. It sits in C13 only, but
     you could also name it in C12.
   - `data_line` serves both C16 and C17 and is named only in C16.
2. **Parents.**
   - C2–C4 use H.SCHED.1 to match 19's userStateFollowsTheTask. H.SCHED.8
     ("user-mode state a trap does not save ... inherited by a fork, reset by
     an execve") is the real match, but its wording is x86-only ("FS and GS
     bases and the x87 and SSE"). If H.SCHED.8 were made architecture-neutral,
     C2–C6 should move to it.
   - C16/C17 use H.MEM.6 because 19 did, but H.MEM.6 is about zeroing frames
     and fits poorly.
   - C18 on H.DMA.2 is a guess: the only callers are pci/virtio.rs's Rings,
     which the IOMMU check drives. No H.* says "a device sees the ring in
     order".
   - C21: H.MEM.9/H.MEM.4, after x86's L.x86_64.20 (H.MEM.9); the leftover map
     is executable.
3. **Possible bugs and mismatches** (reported, not fixed):
   - `cpu::psci_system` does not declare `r12` clobbered, but `cpu::psci_call`
     does, with a comment that the convention lets the callee corrupt
     `r0`–`r3` and `r12`. If that is right, `psci_system` gives the compiler a
     wrong clobber list in the case where firmware returns: `reset` falls
     through to `shutdown`, and a failed SYSTEM_OFF falls to `halt`. The two
     must agree.
   - `timer::init`'s doc says "check the counter runs", but it only refuses
     `CNTFRQ == 0`. AArch64's init also refuses a counter that does not
     advance (19's L.aarch64.25). Either the doc overclaims, or the ARMv7-A
     check is missing.
   - The switch.rs module doc says "a ninth register is pushed with them as
     padding". The code pushes r4–r11 and lr, which is the nine callee-saved
     words, and pads with `sub sp, sp, #4`, as FRAME_BYTES's doc says. The
     module doc is the one that is wrong.
   - `ferrix_user_banked_save`/`_restore` run a few instructions in System
     mode, with IRQs in whatever state the caller had. They look safe, because
     the trap stubs store to the SVC stack whatever the mode and a switch
     saves and restores the banked pair. But `UserContext::store_trap` calls
     `set_user_banked` after `enable_interrupts()` in the sigreturn path, and
     a preemption between its `ldr sp` and `ldr lr` would be saved and
     restored consistently only because save_user_state reads the same
     registers. Worth one sentence in a SAFETY comment; I see no bug.
   - `CpuStarter::start` maps PSCI's -2/-4/-5/-9 to named errors. No check
     covers them, and C20's statement leaves them out. If they need a
     requirement it would be baseline: PSCI cannot be scripted, because
     `psci_call` is real `hvc`/`smc`.
4. **Accessors not in my list.** These are one-line wrappers the gate treats
   as accessors: `psci_system_off`, `psci_system_reset`, `user_fpu_doubles`,
   `user_fpu_features`, `smp::hardware_id`, `smp::register`, `timer::irq`,
   `counter_now`, `counter_hz`, `disarm`, `UserState::new`,
   `set_thread_pointer`, `set_thread_area`, `UserState::set_user_stack`, and
   `fp`. `UserState::new` defines the execve reset values that C4's criterion
   checks, so name it in C4 if the gate allows.

Unit check: all 66 units in area-core.txt appear once. That is 41 from cpu.rs,
9 from smp.rs, 14 from switch.rs and 2 from timer.rs.
