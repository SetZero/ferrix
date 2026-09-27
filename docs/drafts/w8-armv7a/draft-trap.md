# Draft: ARMv7-A trap path and signal frames (area T)

Source read: kernel/src/arch/armv7a/trap.rs, signal.rs, check.rs, trap/check.rs,
the ARMv7-A user programs in arch/armv7a/mod.rs, syscall/deliver.rs, trap.rs
(generic dispatch), syscall/check.rs (generic program checks), main.rs's stage 3
check, and 19-aarch64-requirements.sysml on e4-w8 (Traps, Signals).

12 requirements for 20 units (19 units in requirements, 1 as check code). 5 proposed
verified, 7 baseline.

## Requirements

```sysml
    package Traps {
        doc /* kernel/src/arch/armv7a/trap.rs: the vector table and its stubs,
             * the decoding of a vector entry and fault status into the
             * architecture-neutral trap, the signal a program's fault gets,
             * system calls, and entering and resuming USR mode. */

        requirement <'L.armv7a.T1'> vectorEntriesAreDecoded : ItemLowLevel {
            attribute :>> statement = "classify shall turn a trap frame into the
                trap its vector entry and fault status name: an undefined
                instruction as an illegal instruction, a supervisor call as a
                system call, a prefetch abort with the debug status as a
                breakpoint, a translation, access-flag or permission abort as a
                page fault with its address, access and origin, an alignment
                fault, any other abort and any other vector entry as a named
                fault, and an IRQ entry as an interrupt; and came_from_user shall
                answer from the saved CPSR's mode.";
            attribute :>> criterion = "The machine check's 15 frames -- one per
                vector entry, the abort statuses a boot or a program raises, an
                external abort of each kind, an entry no stub numbers (9) and an
                IRQ from USR and from SVC mode -- are each classified as the trap
                listed for them, and a frame from USR mode and one from SVC mode
                are told apart (the `machine` line).";
            attribute :>> parent = ("H.TRAP.4", "H.TRAP.6");
            attribute :>> unit = ("arch::armv7a::trap::classify", "arch::armv7a::trap::abort",
                "arch::armv7a::trap::kind_name",
                "arch::armv7a::trap::TrapFrame::came_from_user");
        }
        requirement <'L.armv7a.T2'> faultsGetTheirSignal : ItemLowLevel {
            attribute :>> statement = "fault_signal shall give a program's
                unresolved trap its signal, si_code and address: SIGSEGV with
                SEGV_ACCERR for a permission fault and SEGV_MAPERR for any other
                page fault, at the fault address; SIGTRAP with TRAP_BRKPT for a
                breakpoint, at the instruction; SIGBUS with BUS_ADRALN for an
                alignment fault and BUS_OBJERR for any other abort, at the fault
                address; and SIGILL with ILL_ILLOPC for anything else, at the
                instruction.";
            attribute :>> criterion = "Each of the machine check's 13 faulting
                frames is given the signal, si_code and address listed for it
                (the `machine` line).";
            attribute :>> parent = "H.TRAP.4";
            attribute :>> unit = "arch::armv7a::trap::fault_signal";
        }
        requirement <'L.armv7a.T3'> programsFaultToTheirSignal : ItemLowLevel {
            attribute :>> statement = "A program whose own instruction is an
                undefined instruction, a bkpt or a load-multiple from a misaligned
                address shall be ended by SIGILL, SIGTRAP and SIGBUS respectively,
                the kernel going on: the vector table init installs takes the
                exception from USR mode, its stub saves the frame on the SVC stack
                and hands it to ferrix_trap_entry.";
            attribute :>> criterion = "Three programs, each one of those
                instructions and then exit_group(97), are run from the trap check,
                and each ends with the status 128 plus its signal (the `fault`
                line).";
            attribute :>> parent = "H.TRAP.4";
            attribute :>> unit = ("arch::armv7a::trap::init", "arch::armv7a::trap::ferrix_trap_entry",
                "arch::armv7a::init_traps", "arch::armv7a::cpu::install_vectors");
        }
        requirement <'L.armv7a.T4'> systemCallsAreServed : ItemLowLevel {
            attribute :>> statement = "system_call shall refuse an svc taken from
                SVC mode; answer set_tls itself by writing TPIDRURO and returning
                0; answer rt_sigreturn and sigreturn itself by replacing the whole
                frame and the banked stack pointer and link register; and
                otherwise pass r7 and r0 to r5 to the dispatcher and write its
                result into r0, or begin the program an execve enters at its entry
                -- every register zero, Thumb state when the entry's bit 0 is set
                -- on its new stack.";
            attribute :>> criterion = "Every program the boot runs makes its calls
                through this path; a check that an svc from SVC mode is refused,
                that a call's six arguments and its number reach the dispatcher
                unchanged, that set_tls reads back from TPIDRURO, and that an
                execve'd image starts with its registers zero and in the state its
                entry names, is not written.";
            attribute :>> parent = ("H.TRAP.10", "H.TRAP.12", "H.TRAP.6", "H.SCHED.7");
            attribute :>> unit = ("arch::armv7a::trap::system_call",
                "arch::armv7a::trap::UserRegs::set_stack",
                "arch::armv7a::trap::UserRegs::stack_pointer");
        }
        requirement <'L.armv7a.T5'> forkChildrenResumeWithZero : ItemLowLevel {
            attribute :>> statement = "for_child shall give a fork child a copy
                of its parent's saved registers with r0 zero, and resume_user shall
                return to USR mode from such a frame, so that the child continues
                after the parent's svc with the call answering 0.";
            attribute :>> criterion = "The ARMv7-A fork program exits 24: its
                child took fork's zero branch after the svc and exited 23, and its
                parent's wait4 found that child and read its status.";
            attribute :>> parent = "H.TRAP.10";
            attribute :>> unit = ("arch::armv7a::trap::UserRegs::for_child",
                "arch::armv7a::trap::resume_user");
        }
        requirement <'L.armv7a.T6'> programsAreEnteredClean : ItemLowLevel {
            attribute :>> statement = "enter_user shall enter USR mode for the
                first time at the entry on the stack given, in Thumb state when
                the entry's bit 0 is set and ARM state otherwise, with IRQs open,
                the start argument's low half in r0 and r1 to r12 and lr zero.";
            attribute :>> criterion = "A program entered at an odd address runs
                its Thumb code, one started with an argument finds it in r0, and
                one that ORs r1 to r12 and lr on entry finds 0; the start-argument
                check proves r0 for an ARM-state entry, and the Thumb entry and
                the cleared registers are not checked.";
            attribute :>> parent = "H.TRAP.10";
            attribute :>> unit = "arch::armv7a::trap::enter_user";
        }
        requirement <'L.armv7a.T7'> kernelTrapsAreReported : ItemLowLevel {
            attribute :>> statement = "report_trap shall print the interrupted
                state -- the vector entry and its name, the fault status and
                address, pc, cpsr, lr, r0 to r12 and whether user mode
                took it -- without taking the console's log, for a trap that stops
                the kernel.";
            attribute :>> criterion = "A kernel trap's failure report carries the
                vector name, fsr, far, pc, cpsr and the 13 general registers; no
                boot check provokes one, and a deliberate crash is the failure
                path's evidence.";
            attribute :>> parent = "H.FAIL.1";
            attribute :>> unit = ("arch::armv7a::trap::report_trap",
                "arch::armv7a::trap::TrapFrame::instruction_pointer");
        }
    }

    package Signals {
        doc /* kernel/src/arch/armv7a/signal.rs: Linux's sigframe and
             * rt_sigframe from arch/arm, the restore that reads them back, the
             * VFP record, the return sequence a frame carries, and the rewind
             * of a system call a signal interrupted. */

        requirement <'L.armv7a.T8'> interruptedCallsAreRewound : ItemLowLevel {
            attribute :>> statement = "rewind_syscall shall make an interrupted
                svc run again when the program resumes: r0 gets its first argument
                back, the return address steps back over the svc -- four bytes in
                ARM state, two in Thumb -- and r7 keeps the program's number, or
                becomes restart_syscall's when the call's restart is
                restart_syscall's.";
            attribute :>> criterion = "A clock_nanosleep frame rewound both ways,
                in ARM and in Thumb state, has r0 its argument, the return address
                four and two bytes back, and r7 clock_nanosleep's number and
                restart_syscall's respectively; not written as a check on this
                architecture (AArch64's machine check has its equivalent).";
            attribute :>> parent = "H.TRAP.4";
            attribute :>> unit = "arch::armv7a::signal::UserContext::rewind_syscall";
        }
        requirement <'L.armv7a.T9'> handlersAreEnteredOnTheirFrame : ItemLowLevel {
            attribute :>> statement = "setup_signal_frame shall write an
                rt_sigframe for a handler installed with SA_SIGINFO and a sigframe
                otherwise below the stack pointer, with the interrupted registers
                in the ucontext's uc_mcontext (r4 at offset 48, as on Linux), and
                enter the handler with r0 the signal -- r1 the siginfo and r2 the
                ucontext for an rt_sigframe -- sp at the frame and lr the restorer;
                from_trap and store_trap shall carry USR mode's banked sp and lr
                between the processor and the context, so that the handler runs on
                its frame and returns into its restorer, and a register it changes
                through the ucontext is the one its sigreturn puts back.";
            attribute :>> criterion = "The ARMv7-A signal program exits 77: its
                SA_SIGINFO handler is entered with r0 10, r1 a siginfo whose
                si_signo is 10 and r2 the ucontext, its plain handler with r0 12
                and the ucontext at sp; each adds to the r4 its uc_mcontext holds
                and returns through lr into its restorer, and the program exits
                with both additions made (the `signals` line).";
            attribute :>> parent = "H.TRAP.9";
            attribute :>> unit = ("arch::armv7a::signal::setup_signal_frame",
                "arch::armv7a::signal::write_registers",
                "arch::armv7a::signal::UserContext::from_trap",
                "arch::armv7a::signal::UserContext::store_trap",
                "arch::armv7a::signal::UserContext::stack_pointer",
                "arch::armv7a::signal::UserContext::syscall_result",
                "arch::armv7a::signal::UserContext::set_syscall_result");
        }
        requirement <'L.armv7a.T10'> sigreturnKeepsPrivilege : ItemLowLevel {
            attribute :>> statement = "restore_signal_frame shall refuse a frame
                whose stack pointer is not eight-byte aligned, shall load r0 to r12,
                sp, lr, pc and the mask from it, and shall keep of its CPSR only
                the condition flags, the IT state, GE and the Thumb bit, forcing
                USR mode with asynchronous aborts and FIQs masked and IRQs open.";
            attribute :>> criterion = "A frame forged with SVC mode, IRQs masked
                and the E and J bits set in its CPSR returns to USR mode with IRQs
                open and E and J clear, and a frame at a stack pointer four bytes
                off alignment ends the program with SIGSEGV; the signal program
                proves the load of r4 and the mask, and the forged frames are not
                written as a check here.";
            attribute :>> parent = "H.TRAP.5";
            attribute :>> unit = "arch::armv7a::signal::restore_signal_frame";
        }
        requirement <'L.armv7a.T11'> vfpStateFollowsTheHandler : ItemLowLevel {
            attribute :>> statement = "On a core with a VFP, write_vfp shall put a
                record with VFP_MAGIC, its size of 288, d0 to d31 and FPSCR at the
                frame's uc_regspace, and restore_vfp shall refuse a frame whose
                record lacks the magic or the size and otherwise load d0 to d31 and
                FPSCR from it; on a core without one, neither touches the frame.";
            attribute :>> criterion = "A handler that changes d8 and FPSCR through
                its frame's VFP record returns to a program that reads the changed
                values, and one whose record's magic was overwritten ends with
                SIGSEGV; the signal program's frames pass restore_vfp's test, but
                no check compares a floating-point register across a handler.";
            attribute :>> parent = "H.TRAP.9";
            attribute :>> unit = ("arch::armv7a::signal::write_vfp",
                "arch::armv7a::signal::restore_vfp");
        }
        requirement <'L.armv7a.T12'> theFrameCarriesItsReturn : ItemLowLevel {
            attribute :>> statement = "write_retcode shall put in the frame's
                retcode the two ARM instructions mov r7, #number and svc, with
                sigreturn's number for a sigframe and rt_sigreturn's for an
                rt_sigframe, which a handler installed without SA_RESTORER returns
                into.";
            attribute :>> criterion = "A handler installed without SA_RESTORER,
                with and without SA_SIGINFO, returns and its program goes on to
                exit with its own status; the trap check's no-restorer program is
                entered from its frame but exits from the handler, so the retcode
                never runs in any check.";
            attribute :>> parent = "H.TRAP.9";
            attribute :>> unit = "arch::armv7a::signal::write_retcode";
        }
    }
```

## Verification

| id | verifying check | why |
|---|---|---|
| L.armv7a.T1 | `arch::armv7a::check::check_trap_decoding` | Asserts `classify` on all 15 frames (every vector entry, all abort-status kinds, entry 9, IRQ from USR and SVC mode) and `came_from_user` both ways. `kind_name` is reached for reset, hypervisor trap, FIQ and "exception"; the other five names reach only `report_trap`, which prints them, so they are not a behaviour a check can fail here. |
| L.armv7a.T2 | `arch::armv7a::check::check_trap_decoding` | Same loop asserts `fault_signal` on the 13 frames with `Some(signal)`: signal, si_code and address each. |
| L.armv7a.T3 | `arch::armv7a::trap::check::run` | Runs the udf, bkpt and misaligned-ldm programs from USR mode and requires 128+SIGILL/SIGTRAP/SIGBUS. `init` has to have installed the table for any of that to happen. `run` also runs the no-restorer program, which does not change this. |
| L.armv7a.T4 | baseline | No check refuses an svc from SVC mode, checks set_tls against TPIDRURO, checks argument order, or checks that execve's Enter branch clears registers and sets Thumb. Generic program checks (`check_execve_replaces_the_program`, every program check) go through this path, but their criteria are the generic layer's. |
| L.armv7a.T5 | `syscall::check::check_a_forked_child_is_waited_for` | Generic, but on ARMv7-A it runs `USER_FORK_PROGRAM`. The child's `cmp r0, #0` after the svc is exactly for_child's r0=0 plus resume_user's return to the instruction after the svc. Exit 24 needs both, and nothing else in the criterion is left untested. (It already carries `Verifies: L.x86_64.63`, so this adds a second id.) |
| L.armv7a.T6 | baseline | `syscall::check::check_a_program_is_handed_its_start_argument` proves only r0 = argument, for an ARM-state entry. No check enters at an odd (Thumb) address or looks at r1-r12 and lr on entry. |
| L.armv7a.T7 | baseline | No boot check provokes a kernel trap. This matches AArch64's L.aarch64.5. |
| L.armv7a.T8 | baseline | No ARMv7-A check rewinds a frame. `syscall::check::check_a_stop_stops_every_thread_and_a_continue_restarts_their_calls` nearly does: the ARMv7-A stopped program's futex wait must restart after a stop and continue, which needs r0 restored and pc back 4 in ARM state. It does not cover Thumb or restart_block (r7 → restart_syscall). The cheapest fix is a copy of aarch64/check.rs's rewind check. |
| L.armv7a.T9 | `syscall::check::check_a_handler_runs_and_returns` | Generic, but on ARMv7-A `USER_SIGNAL_PROGRAM` asserts every clause of the criterion: r0/r1/r2 at rt entry, r0 and ucontext-at-sp at plain entry, r4 at uc_mcontext+48 read and changed, return through lr to both restorers, and the change surviving both sigreturns. Exit codes 98/7/129+ separate the failures. It needs restore_signal_frame too (T10's unit); see Notes. |
| L.armv7a.T10 | baseline | The forged-frame and misaligned-sp halves are not checked on ARMv7-A. The round-trip half is proved by the T9 check. |
| L.armv7a.T11 | baseline | The magic/size test passes on every signal-program return (a mismatch would kill the program), but no check compares a d register or FPSCR across a handler, and none forges a bad record. |
| L.armv7a.T12 | baseline | `arch::armv7a::trap::check::run`'s no-restorer program exits 55 from inside the handler, so the retcode is written but never executed. See Notes: it probably cannot execute. |

## Check code in product files

| unit | reason |
|---|---|
| `arch::armv7a::trap::breakpoint` | Its only caller is main.rs's `check_breakpoint`, which raises `bkpt` twice for stage 3's check. This is the same classification scripts/data/traceability-units.json on e4-w8 gives `arch::x86_64::trap::breakpoint`. |

## Notes

1. **Possible bug: a handler without SA_RESTORER cannot return.** `setup_signal_frame` points lr at the retcode copy on the user stack, and its own comment says the copy "runs only if the stack is executable". But `syscall/exec.rs:305` maps the stack `VmaFlags::READ_WRITE`, and W^X (H.MEM.4) keeps it that way. ARMv7-A has no vDSO either (`vdso_spec()` is `None`), so `deliver::run_handler` never substitutes a trampoline. The result: a handler installed without SA_RESTORER takes a prefetch permission fault when it returns and dies with SIGSEGV, unless it runs on an altstack the program mapped PROT_EXEC. There is a second problem even if the stack were executable: the retcode is written with `copy_to_user` and no D-cache clean or I-cache invalidate follows. On the DK1's Cortex-A7 that can execute stale instructions, and QEMU would hide it. Linux avoids both problems with its sigpage. musl and glibc always set SA_RESTORER, which is why nothing has hit this. The trap check's no-restorer program exits from inside its handler, so it cannot see the bug. The brief's "signal frame's return sequence" is therefore not exercised. Suggested check: the same program, with a handler that returns.
2. **Divergence from Linux on alignment faults.** `fault_signal`, `check_trap_decoding`'s ALIGNMENT case and the trap check's misaligned `ldm` all say Linux gives SIGBUS/BUS_ADRALN. On ARMv7 Linux, `alignment_init` → `safe_usermode` forces UM_FIXUP (from memory of arch/arm/mm/alignment.c; worth checking against a Linux tree). With UM_FIXUP, a user misaligned `ldm` is emulated and the program goes on. The machine line's "the trap and signal Linux gives them" is then not true for that frame. For that reason I avoided "as Linux" in T2 and T3.
3. **"Exactly one requirement per unit" against the pilot.** `restore_signal_frame` carries two behaviours. The round trip is verified by the T9 check; the privilege clamp and alignment refusal are baseline. Under the brief's exactly-one rule I put it only in T10, and so T10 as a whole is baseline. T9's criterion still exercises it. If you allow a unit in two requirements (as the pilot's `object::Object::signals`), add `restore_signal_frame` to T9's unit and cut T10 to the clamp and the refusal. `enter_user` (T6) is the same case: split off "the start argument arrives in r0" and it is verified by `check_a_program_is_handed_its_start_argument`. `setup_signal_frame` is similar too: the no-restorer program proves only "a plain handler without a restorer is entered".
4. **Parent H.TRAP.9 says "A 64-bit program's signal handler".** ARMv7-A programs are 32-bit, so T9, T11 and T12 do not fit it literally. 19 uses H.TRAP.9 for AArch64. Either widen H.TRAP.9's wording or accept the stretch.
5. **Units named that are not in my list.** I named these because they are the behaviour's entry points or accessors on its path, as 19 does: `TrapFrame::came_from_user`, `TrapFrame::instruction_pointer`, `UserRegs::set_stack`, `UserRegs::stack_pointer`, `UserContext::stack_pointer/syscall_result/set_syscall_result`, `trap::ferrix_trap_entry`, `arch::armv7a::init_traps` and `cpu::install_vectors`. Drop them, or move them if another area owns them. `trap::init` also calls `cpu::enable_user_fpu`. That behaviour (CPACR grant, FPEXC) belongs to cpu's requirement, not T3.
6. **`advance_past_breakpoint` is uncovered.** It is not in my list: a one-statement accessor, `pc += 4`, which assumes the kernel is ARM code. The step over the kernel's own `bkpt` has no requirement once `breakpoint` is check code. AArch64 gave it L.aarch64.6. Its only evidence is main.rs's stage 3 check (product code).
7. **Minor divergences from Linux, not failures.**
   - The plain `sigframe` path writes and restores `uc_stack`. Linux's `sys_sigreturn` does not restore the altstack; only `rt_sigreturn` does.
   - `trap_no`, `error_code` and `fault_address` in uc_mcontext are left 0. Linux fills them.
   - `restore_signal_frame` does not refuse a kernel-half pc. That is safe: the fetch from USR mode faults and the program gets SIGSEGV, which H.TRAP.5's criterion allows.
   - A data-abort debug event (a watchpoint) would decode as a "data abort" fault and become SIGBUS/BUS_OBJERR, not SIGTRAP. Harmless while USR mode cannot program watchpoints.
8. **T4's execve branch.** It clears only the frame's `lr`, which is the SVC-mode one. USR's banked lr is cleared by `switch::set_user_stack`, which writes `[stack, 0]`, so the net behaviour is right. It is split across two files, though.
9. **Small report oddity.** `report_trap` prints four rows of four registers, `r0` to `r15`, from a 13-entry array. The rows show `r13`, `r14` and `r15` as `0x00000000` (`unwrap_or(0)`), not the banked sp, the lr or the pc. The pc and lr appear on the line above, but a reader could take the zeros as real values. This is cosmetic, but a kernel-trap report is failure evidence (H.FAIL.1).
