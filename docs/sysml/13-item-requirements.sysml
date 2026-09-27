/*
 * Ferrix — SysML v2 model, part 13: the certified item's requirements.
 *
 * The high-level requirements of the certified item (the `core` and `item`
 * rings of docs/certification/ITEM.md): what each subsystem promises at its
 * interface, decomposed from the Security Target's objectives
 * (docs/certification/SECURITY-TARGET.md §4.1 and §8.2) and the safety
 * manual's assumed safety requirements (SAFETY-MANUAL.md §2). The low-level
 * requirements, one per unit of code, go in one file per subsystem after this
 * one. docs/certification/IMPLEMENTATION.md W-8 is the design, and
 * scripts/check/check-traceability.py the gate that reads these and writes
 * docs/certification/TRACEABILITY.md.
 */
package FerrixItemRequirements {
    private import ScalarValues::*;
    private import FerrixStructure::Kernel;

    requirement def ItemRequirement {
        doc /* A requirement of the certified item. Unlike the goal and the
             * design rules of part 1, which say why the system is shaped as it
             * is, each of these says what the item shall do in terms a test
             * can pass or fail, and names the requirement above it that it
             * serves. The gate refuses one without all three. */
        subject item : Kernel;
        attribute statement : String {
            doc /* One sentence with "shall", about behaviour a program, a
                 * device or the boot log can observe. */
        }
        attribute criterion : String {
            doc /* The pass/fail condition a check tests, counted where there
                 * is something to count. */
        }
        attribute parent : String[1..*] {
            doc /* The ids of the level above: for a high-level requirement a
                 * Security Target objective (O.*), an assumed safety
                 * requirement (ASR-*) or a goal requirement (G.*); for a
                 * low-level one, a high-level requirement (H.*). */
        }
    }

    requirement def ItemHighLevel :> ItemRequirement {
        doc /* What one subsystem of the item promises at its interface, with
             * an id `H.<AREA>.<n>`. The areas follow the families of
             * functional requirement the item implements. */
    }

    requirement def ItemLowLevel :> ItemRequirement {
        doc /* What one unit of the item's code does, with an id
             * `L.<module>.<n>`, and the unit named so that a renamed function
             * breaks the trace loudly. */
        attribute unit : String[1..*] {
            doc /* `path::function` or `path::Type::method`, relative to
                 * kernel/src: `mm::zero_frame`. */
        }
    }

    // -----------------------------------------------------------------------
    // The high level, one package per area. The areas follow the families of
    // SECURITY-TARGET.md §8.2 the item implements, and the safety manual's
    // ASR-1 to ASR-8; a requirement names every objective it serves.
    // -----------------------------------------------------------------------

    package Memory {
        doc /* Address spaces, the frames under them and the mappings that
             * reach them: O.ISOLATE, O.WXN and O.SCRUB, ASR-1, ASR-2 and
             * ASR-5 (FDP_IFC.1, FDP_IFF.1, FDP_RIP.2). */

        requirement <'H.MEM.1'> spacesAreSeparate : ItemHighLevel {
            attribute :>> statement = "A user address space shall translate a user
                virtual address only to a frame its own mappings name, so that one
                address in two address spaces reaches two frames unless a VMO both
                map shares one.";
            attribute :>> criterion = "Of N reads of one user address, made
                alternately in two address spaces that each wrote a different value
                there, all N return the reading space's own value and none the
                other's (the boot's `spaces` line, N = 128).";
            attribute :>> parent = ("O.ISOLATE", "ASR-1");
        }
        requirement <'H.MEM.2'> kernelMemoryIsNotUsers : ItemHighLevel {
            attribute :>> statement = "No mapping of kernel memory shall be
                accessible from user mode: a user-mode read, write or instruction
                fetch at a kernel address shall end in a fault delivered to the
                thread, never in the access.";
            attribute :>> criterion = "A program that reads, writes and jumps to a
                kernel-half address is ended with SIGSEGV each time, 3 of 3, on every
                architecture, and the kernel's memory at that address is unchanged.";
            attribute :>> parent = ("O.ISOLATE", "ASR-1");
        }
        requirement <'H.MEM.3'> kernelKeptOutOfUserPages : ItemHighLevel {
            attribute :>> statement = "On every processor the kernel shall run with
                its own access to user pages refused outside the window a user copy
                opens: SMEP and SMAP on x86-64, PAN on AArch64 where the processor
                implements it. ARMv7-A has no such control (AoU-6).";
            attribute :>> criterion = "The boot reports SMEP on and SMAP on (x86-64),
                or PAN on where offered (AArch64), for each of the N online
                processors; a kernel read of a user page outside the window faults.";
            attribute :>> parent = ("O.ISOLATE", "ASR-1");
        }
        requirement <'H.MEM.4'> writableOrExecutable : ItemHighLevel {
            attribute :>> statement = "No mapping the kernel installs in any page
                table shall be both writable and executable.";
            attribute :>> criterion = "The boot's sweep of every leaf mapping of every
                page table counts N mappings, E > 0 of them executable, and 0 both
                writable and executable (the `w^x` line).";
            attribute :>> parent = ("O.WXN", "ASR-2");
        }
        requirement <'H.MEM.5'> imageIsSealed : ItemHighLevel {
            attribute :>> statement = "No mapping of the frames that hold the
                kernel's text and read-only data, the direct map's alias of them
                included, shall be writable.";
            attribute :>> criterion = "The boot's sweep of every mapping of those
                frames finds 0 writable (the `sealed` line), and a write to the text
                through the direct map faults on every architecture (FX-9001).";
            attribute :>> parent = ("O.WXN", "ASR-2");
        }
        requirement <'H.MEM.6'> framesZeroedBeforeUse : ItemHighLevel {
            attribute :>> statement = "A physical frame shall hold only zeros when it
                is committed to a VMO, mapped into a user address space for the first
                time, or made a page table.";
            attribute :>> criterion = "A frame written with a pattern, freed and
                committed again reads 0 non-zero bytes of its 4096 before the new
                owner's first write, for a VMO page and for a page table alike.";
            attribute :>> parent = ("O.SCRUB", "ASR-5");
        }
        requirement <'H.MEM.7'> nothingFreedBeforeItsShootdown : ItemHighLevel {
            attribute :>> statement = "A frame or page-table frame removed from a
                translation shall not return to the frame allocator until the TLB
                invalidation that covers it has completed on every processor that
                may hold the translation.";
            attribute :>> criterion = "A page table an unmap empties is still held,
                not freed, until the shootdown covering it completes: 0 tables freed
                before it; and each of N remaps is seen by every online processor
                (the `tlb` line).";
            attribute :>> parent = ("O.ISOLATE", "ASR-1");
        }
        requirement <'H.MEM.8'> privateCopiesStayPrivate : ItemHighLevel {
            attribute :>> statement = "After a fork, a write by the parent or the
                child to a private page shall not be visible to the other, and a
                write to a shared mapping shall be.";
            attribute :>> criterion = "The `cow` program, in which each side writes a
                page both shared, exits 61 (neither saw the other's write); the
                `shared` program exits 62 (its MAP_SHARED write reached the parent
                and its MAP_PRIVATE write did not).";
            attribute :>> parent = ("O.ISOLATE", "ASR-1");
        }
        requirement <'H.MEM.9'> framesComeBack : ItemHighLevel {
            attribute :>> statement = "Every frame an address space, a VMO or a
                process holds shall return to the frame allocator when the last
                reference to it goes.";
            attribute :>> criterion = "After each memory and program check, the free
                frame count is what it was before: 0 frames leaked, per check (the
                `objects`, `uaccess` and `exits` lines).";
            attribute :>> parent = "O.QUOTA";
        }
        requirement <'H.MEM.10'> imageRangeRefused : ItemHighLevel {
            attribute :>> statement = "Every interface that maps a physical range a
                caller names shall refuse a range that touches the kernel image.";
            attribute :>> criterion = "Requests naming the image's first page, its
                last page and a range straddling its end are each refused, 0
                accepted, at stages 1, 2 and 6.";
            attribute :>> parent = ("O.ISOLATE", "O.WXN");
        }
        requirement <'H.MEM.11'> allocationFailureIsAnswered : ItemHighLevel {
            attribute :>> statement = "An allocation the item makes after bring-up
                shall report failure to its caller, which shall answer it as running
                out of memory and keep nothing half made.";
            attribute :>> criterion = "With every allocation of the swept memory
                operations and native calls failed in turn, each failure is absorbed
                or answered NO_MEMORY or ENOMEM, and 0 frames or objects are left
                behind (the `sweep` and `no-mem` lines).";
            attribute :>> parent = "O.QUOTA";
        }
    }

    package Objects {
        doc /* Handles, rights and the objects they name: O.CAPABILITY and
             * ASR-3 (FDP_ACC.1, FDP_ACF.1, FMT_MSA.1, FMT_MSA.3). */

        requirement <'H.OBJ.1'> onlyThroughAHandle : ItemHighLevel {
            attribute :>> statement = "A native call shall act on an object only
                through a handle in the calling process's own handle table, and shall
                refuse a value naming no live handle there with BAD_HANDLE.";
            attribute :>> criterion = "Every native call given a value never issued,
                a value from another process's table and a value closed earlier is
                refused BAD_HANDLE: 0 of them act on an object.";
            attribute :>> parent = ("O.CAPABILITY", "ASR-3");
        }
        requirement <'H.OBJ.2'> rightsAreChecked : ItemHighLevel {
            attribute :>> statement = "A native call shall be refused with
                WRONG_TYPE when its handle names an object of another type, and with
                ACCESS_DENIED unless its handle carries every right the operation
                requires.";
            attribute :>> criterion = "For each native operation that requires a
                right, a call through a handle lacking it is refused ACCESS_DENIED,
                and a call through a handle of the wrong type WRONG_TYPE; 0 are
                performed.";
            attribute :>> parent = ("O.CAPABILITY", "ASR-3");
        }
        requirement <'H.OBJ.3'> duplicatesNeverRaise : ItemHighLevel {
            attribute :>> statement = "Duplicating a handle shall need the DUPLICATE
                right and yield rights that are a subset of the source's; asking for
                a right the source lacks shall be refused with ACCESS_DENIED.";
            attribute :>> criterion = "A duplicate of a handle without DUPLICATE is
                refused ACCESS_DENIED; one asking for a right the source lacks is
                refused ACCESS_DENIED and the source keeps its rights; a narrower
                copy is made with the rights asked for.";
            attribute :>> parent = ("O.CAPABILITY", "ASR-3");
        }
        requirement <'H.OBJ.4'> staleValuesRefused : ItemHighLevel {
            attribute :>> statement = "A handle value shall stop naming its object
                when the handle is closed or transferred, and shall not name the
                object its slot holds next.";
            attribute :>> criterion = "A value whose handle was closed is refused
                BAD_HANDLE, on a get and on a second close, after its slot is
                reused, and the reused slot issues a new value.";
            attribute :>> parent = ("O.CAPABILITY", "ASR-3");
        }
        requirement <'H.OBJ.5'> handlesMoveOnlyInMessages : ItemHighLevel {
            attribute :>> statement = "A handle shall pass from one process to another
                only inside a channel message, leaving the sender's table in the same
                step, and a write that is refused shall leave every handle it carried
                with the sender.";
            attribute :>> criterion = "After a write carrying handles each is refused
                BAD_HANDLE in the sender and works in the reader; writes refused
                PEER_CLOSED, TOO_BIG and SHOULD_WAIT leave every carried handle
                usable by the sender; a handle without TRANSFER is refused
                ACCESS_DENIED.";
            attribute :>> parent = ("O.CAPABILITY", "ASR-3");
        }
        requirement <'H.OBJ.6'> newProcessHoldsNothing : ItemHighLevel {
            attribute :>> statement = "A process shall start holding no handle but
                the bootstrap handle its creator explicitly passes, and a fork shall
                not copy the native handle table.";
            attribute :>> criterion = "A created process holds 0 handles before it
                starts and exactly 1 after a start that passed a bootstrap; a forked
                child's native handle table is empty.";
            attribute :>> parent = ("O.CAPABILITY", "ASR-3");
        }
        requirement <'H.OBJ.7'> tableIsBounded : ItemHighLevel {
            attribute :>> statement = "A process's handle table shall hold at most
                4096 handles, refuse an insertion past that with NO_HANDLES, and lose
                nothing it could not deliver.";
            attribute :>> criterion = "Duplicating until refused ends in NO_HANDLES
                with at most 4096 held; a channel read and a VMO create into the full
                table are refused NO_HANDLES and the message stays queued (the
                `refusals` line).";
            attribute :>> parent = ("O.CAPABILITY", "O.QUOTA");
        }
        requirement <'H.OBJ.8'> objectsEndWithTheirLastHandle : ItemHighLevel {
            attribute :>> statement = "An object shall be destroyed when the last
                handle to it closes.";
            attribute :>> criterion = "A program made and never started ends when
                its last handle closes, and one whose last handle rode unread in a
                channel ends when the channel is closed: 2 of 2.";
            attribute :>> parent = ("O.CAPABILITY", "O.QUOTA");
        }
        requirement <'H.OBJ.9'> killingAJobEndsItsTree : ItemHighLevel {
            attribute :>> statement = "Killing a job shall end every process in it
                and in every job beneath it.";
            attribute :>> criterion = "4 processes in a tree of three jobs are all
                ended by two kills, and each wait on them is woken (the `jobs`
                line).";
            attribute :>> parent = ("O.CAPABILITY", "O.QUOTA");
        }

        // H.OBJ.10 to H.OBJ.17 were added by the low-level pilot on object/
        // (IMPLEMENTATION.md W-8, step 3). H.OBJ.10, 12 and 13 are behaviour
        // object/ implements that no requirement said; H.OBJ.14 to 17 are the
        // halves split out of H.OBJ.3, 4, 8 and 11, so that one check can prove
        // each whole, since a `Verifies:` tag credits the whole requirement.

        requirement <'H.OBJ.10'> queuesAreBounded : ItemHighLevel {
            attribute :>> statement = "Every queue a program can fill in the kernel
                shall be bounded -- a channel end's unread messages at 256, a port's
                user packets at 1024, an object's port registrations at 64 -- and an
                addition past the bound shall be refused without being queued.";
            attribute :>> criterion = "Writing to a channel nobody reads is refused
                SHOULD_WAIT at its bound, and queueing on a port nobody takes from is
                refused SHOULD_WAIT at exactly 1024 packets, 0 queued past either
                bound, and draining the channel makes room again; the 65th
                registration on one object is refused.";
            attribute :>> parent = "O.QUOTA";
        }
        requirement <'H.OBJ.11'> waitsEndAtTheirSignal : ItemHighLevel {
            attribute :>> statement = "A wait on an object shall end when a signal
                it waits for is asserted, at once if one already is, and shall not
                end before its deadline while none is.";
            attribute :>> criterion = "A wait on an empty channel end with a zero
                deadline times out; one with a two-minute deadline is ended by a
                message written 20 ms later, not before it, and reports READABLE; a
                wait for PEER_CLOSED on an end whose peer has closed returns at
                once.";
            attribute :>> parent = ("G.7", "O.CAPABILITY");
        }
        requirement <'H.OBJ.12'> jobsAreCgroups : ItemHighLevel {
            attribute :>> statement = "The job tree shall be the cgroup v2
                hierarchy: every process in exactly one job, a job populated exactly
                while it or a job beneath it has a member not yet released, and a
                job refusing a process or a child that its killed or removed state,
                its limits or the no-internal-process rule forbid.";
            attribute :>> criterion = "A new process is in the root job and a fork's
                child in its parent's; a job empties at its last member's release
                and wakes its waiters; a move a job refuses leaves the process where
                it was; a removed job takes neither a process nor a child; a child
                past cgroup.max.depth or cgroup.max.descendants is refused.";
            attribute :>> parent = ("G.8", "O.QUOTA");
        }
        requirement <'H.OBJ.13'> pidsNameOneProcess : ItemHighLevel {
            attribute :>> statement = "A pid shall name at most one process at a
                time, and a pid given back shall not be handed out again before
                allocation has come round the whole range past it.";
            attribute :>> criterion = "Allocating through the whole range wraps to
                300 and passes over a pid still held: 0 pids handed out twice and
                none below 300 after the wrap.";
            attribute :>> parent = ("G.4", "O.ISOLATE");
        }
        requirement <'H.OBJ.14'> replacesNeverRaise : ItemHighLevel {
            attribute :>> statement = "Replacing a handle shall close the source and
                yield rights that are a subset of its; asking for a right the source
                lacks shall be refused with ACCESS_DENIED and leave the source as it
                was.";
            attribute :>> criterion = "A replace asking for a right not held is
                refused ACCESS_DENIED and the source keeps its rights; a narrower
                replace closes the source and leaves one handle, with the rights
                asked for.";
            attribute :>> parent = ("O.CAPABILITY", "ASR-3");
        }
        requirement <'H.OBJ.15'> usedUpSlotsRetire : ItemHighLevel {
            attribute :>> statement = "A slot whose generations are all used shall
                be retired, so that no handle value is ever issued twice.";
            attribute :>> criterion = "After every generation of one slot has been
                issued and closed, the next handle comes from another slot, and 0 of
                the values issued before it are issued again or resolve.";
            attribute :>> parent = ("O.CAPABILITY", "ASR-3");
        }
        requirement <'H.OBJ.16'> thePeerSeesTheClose : ItemHighLevel {
            attribute :>> statement = "The peer of a destroyed channel end shall
                observe PEER_CLOSED from the moment the close returns, and what the
                destroyed end held shall be freed.";
            attribute :>> criterion = "After one end of a channel holding a queued
                VMO is closed, a read and a write on the survivor are refused
                PEER_CLOSED, 2 of 2, and 0 frames are leaked.";
            attribute :>> parent = ("O.CAPABILITY", "O.QUOTA");
        }
        requirement <'H.OBJ.17'> registrationsFireOnce : ItemHighLevel {
            attribute :>> statement = "A port registration shall queue exactly one
                packet, with its key and the asserted signals it asked for, when one
                of them is first asserted, at once if one already is.";
            attribute :>> criterion = "A registration does not fire before its
                signal, fires once on the message that asserts it and not on a
                second, fires at once for a state already true, and fires when the
                peer closes for PEER_CLOSED, each packet with its key.";
            attribute :>> parent = ("G.7", "O.CAPABILITY");
        }
    }

    package Scheduling {
        doc /* Processor time: the fair class, placement and sleep. ASR-8
             * (partially met: the item has no admission control for real-time
             * work, AoU-4, so none is required here) and O.QUOTA's share of a
             * processor (FRU_RSA.1's refinement). */

        requirement <'H.SCHED.1'> runnableTasksRun : ItemHighLevel {
            attribute :>> statement = "Every runnable task shall run: no task shall
                stay runnable without being scheduled while processors are online.";
            attribute :>> criterion = "1000 threads spawned across the online
                processors all run to completion within the check's deadline (the
                `tasks` line).";
            attribute :>> parent = ("G.1", "ASR-8");
        }
        requirement <'H.SCHED.2'> lagIsBounded : ItemHighLevel {
            attribute :>> statement = "Among processor-bound tasks of equal weight on
                one processor, no task's service shall lag its fair share by more
                than one scheduling slice plus the timer overruns served in the
                window.";
            attribute :>> criterion = "With 12 spinners on every processor, each
                processor's worst lag is at or below that bound (the `fair` line).";
            attribute :>> parent = ("ASR-8", "O.QUOTA");
        }
        requirement <'H.SCHED.3'> jobsShareByWeight : ItemHighLevel {
            attribute :>> statement = "Under contention a job shall receive processor
                time in proportion to its weight, whatever the number of tasks in
                it.";
            attribute :>> criterion = "One task alone in a job keeps half of a
                processor, within the check's stated tolerance, against eight tasks
                in a sibling job of equal weight (the `quota` line).";
            attribute :>> parent = ("O.QUOTA", "ASR-8");
        }
        requirement <'H.SCHED.4'> affinityIsObeyed : ItemHighLevel {
            attribute :>> statement = "A task shall run only on the processors its
                affinity mask allows.";
            attribute :>> criterion = "Spinners confined to processors 0 and 1, twice
                as many as there are online processors, are seen running on no other
                processor: 0 violations.";
            attribute :>> parent = "G.1";
        }
        requirement <'H.SCHED.5'> sleepIsNotCutShort : ItemHighLevel {
            attribute :>> statement = "A task that sleeps for a duration shall not be
                woken before the duration has passed, and shall be woken within
                twenty times it.";
            attribute :>> criterion = "A 20 ms sleep returns after at least 20 ms and
                at most 400 ms (the `sleep` line).";
            attribute :>> parent = ("G.1", "ASR-8");
        }
    }

    package Interrupts {
        doc /* Interrupt lines given to ring-3 drivers as objects: O.CAPABILITY
             * and ASR-3 for who receives one, O.QUOTA for what an unacknowledged
             * or unclaimed line can cost. */

        requirement <'H.IRQ.1'> deliveredToTheHolder : ItemHighLevel {
            attribute :>> statement = "An interrupt on a line bound to an Interrupt
                object shall be delivered only to that object, waking the task
                waiting on it.";
            attribute :>> criterion = "16 of 16 deliveries end their wait by waking
                it (the `wake` line), and no other waiter is woken.";
            attribute :>> parent = ("O.CAPABILITY", "ASR-3");
        }
        requirement <'H.IRQ.2'> maskedUntilAcknowledged : ItemHighLevel {
            attribute :>> statement = "A line bound to an Interrupt object shall stay
                masked from its delivery until the holder acknowledges it, and shall
                be masked when the object is destroyed.";
            attribute :>> criterion = "A line held from delivery to acknowledgement
                is delivered once and 0 times more before the acknowledgement; after
                the last handle to its object closes, the line is masked.";
            attribute :>> parent = ("O.CAPABILITY", "O.QUOTA");
        }
        requirement <'H.IRQ.3'> unclaimedLinesMasked : ItemHighLevel {
            attribute :>> statement = "An interrupt on a line no object and no kernel
                handler holds shall be masked and counted, and delivered to nobody.";
            attribute :>> criterion = "A delivery provoked on a line nobody holds
                leaves the line masked and the unclaimed count one higher, with 0
                deliveries (the `edges` line).";
            attribute :>> parent = "O.QUOTA";
        }
    }

    package DirectMemoryAccess {
        doc /* Devices driven from ring 3 through apertures and IOMMU domains:
             * O.DMA and ASR-4 (FDP_ACF.1, FDP_IFF.1). ARMv7-A's reference
             * board has no IOMMU, so these hold on x86-64 and AArch64 only
             * (AoU-6). */

        requirement <'H.DMA.1'> everyFunctionBehindAUnit : ItemHighLevel {
            attribute :>> statement = "Where a translating IOMMU is present, every
                PCI function the kernel enumerates shall be placed behind a unit
                before its driver starts, and a function no unit covers shall be
                reported.";
            attribute :>> criterion = "The boot counts N functions behind units, 0
                bypassing and 0 unresolved (the `iommu` line), with VT-d on x86-64
                and SMMUv3 on AArch64.";
            attribute :>> parent = ("O.DMA", "ASR-4");
        }
        requirement <'H.DMA.2'> outOfDomainWritesRefused : ItemHighLevel {
            attribute :>> statement = "A device write to an address outside its
                domain shall be refused by the unit, reach no memory and be recorded
                as a fault against the device.";
            attribute :>> criterion = "A device made to write a page outside its
                domain leaves the page unchanged and a fault recorded against its
                stream, and 0 faults are recorded that no check provoked.";
            attribute :>> parent = ("O.DMA", "ASR-4");
        }
        requirement <'H.DMA.3'> onlyPinnedFramesReachable : ItemHighLevel {
            attribute :>> statement = "A frame shall be reachable by a device only
                while pinned into that device's domain through a handle its driver
                holds, and unpinning shall complete the unit's invalidation before
                the frame or an emptied table is reused.";
            attribute :>> criterion = "Pages pinned through a translated domain are
                found at their device addresses while pinned and not after; invalid
                pins are refused as specified (the `iommu` pin line); a domain's
                emptied tables are freed only after its invalidation completes.";
            attribute :>> parent = ("O.DMA", "ASR-4");
        }
        requirement <'H.DMA.4'> deadDriversPinsQuarantined : ItemHighLevel {
            attribute :>> statement = "Frames a driver pinned shall not be reused
                after the driver dies until its replacement announces itself, and the
                frames so held shall be bounded.";
            attribute :>> criterion = "A dead driver's pin is quarantined, a pin past
                the quarantine's cap is refused and taken again once the quarantine
                is released, and a live driver's pin is given back (the `iommu`
                quarantine line).";
            attribute :>> parent = ("O.DMA", "O.QUOTA");
        }
        requirement <'H.DMA.5'> registersOnlyThroughApertures : ItemHighLevel {
            attribute :>> statement = "A driver shall reach device registers only
                through an I/O mapping of an aperture it holds, and an aperture that
                is not whole pages or that covers an MSI-X table shall be withheld.";
            attribute :>> criterion = "The `devices` line counts the apertures
                withheld for partial pages and for MSI-X ranges; a mapping of a
                withheld range is refused; a program reaches only the aperture it
                was given (the `handles` line).";
            attribute :>> parent = ("O.DMA", "O.CAPABILITY", "ASR-4");
        }
    }

    package Traps {
        doc /* The system call boundary and the exceptions a program raises:
             * O.VALIDATE and ASR-7 (FPT_TDC.1), with O.ISOLATE for what a
             * program may leave in the registers the kernel is entered with. */

        requirement <'H.TRAP.1'> userRangesChecked : ItemHighLevel {
            attribute :>> statement = "Every pointer and length a system call is
                given shall be checked to lie wholly within the user half before any
                access through it, and a range that does not shall be refused with
                EFAULT.";
            attribute :>> criterion = "Each system call given a kernel-half pointer,
                a range that wraps past the user half's end and a null pointer is
                refused EFAULT, with 0 accesses made.";
            attribute :>> parent = ("O.VALIDATE", "ASR-7");
        }
        requirement <'H.TRAP.2'> unmappedUserMemoryIsEfault : ItemHighLevel {
            attribute :>> statement = "An access to user memory on a system call's
                behalf that meets an unmapped or protected page shall return EFAULT
                to the caller and shall not fault the kernel.";
            attribute :>> criterion = "Copies to an unmapped and to a read-only user
                page, and from an unmapped one, each return EFAULT, and the boot goes
                on to FERRIX-BOOT-OK.";
            attribute :>> parent = ("O.VALIDATE", "ASR-7");
        }
        requirement <'H.TRAP.3'> indicesClampedUnderSpeculation : ItemHighLevel {
            attribute :>> statement = "Every table index a program chooses shall be
                clamped at the system call boundary so that it cannot be used out of
                bounds under speculation, and each processor's speculation controls
                shall be applied and read back.";
            attribute :>> criterion = "The boot reads the speculation defences back
                on every one of the N online processors (the `cpu` line), and the
                `--mitigations off` build is the only one without the clamps.";
            attribute :>> parent = ("O.ISOLATE", "O.VALIDATE");
        }
        requirement <'H.TRAP.4'> userFaultsBecomeSignals : ItemHighLevel {
            attribute :>> statement = "A processor exception a user-mode instruction
                raises shall end in a signal to the faulting thread, the one Linux
                sends for it, and shall not stop the kernel.";
            attribute :>> criterion = "A divide error, an invalid opcode, an unmapped
                read, a privileged instruction, a floating-point exception and a read
                past a mapped file's end end their programs with SIGFPE, SIGILL,
                SIGSEGV, SIGSEGV, SIGFPE and SIGBUS, 6 of 6 (the `fault` line).";
            attribute :>> parent = ("O.ISOLATE", "ASR-1");
        }
        requirement <'H.TRAP.5'> signalReturnCannotRaisePrivilege : ItemHighLevel {
            attribute :>> statement = "Restoring a signal frame a program supplies
                shall not raise its privilege: the restored mode, segment selectors,
                interrupt masks and I/O privilege shall be the user's whatever the
                frame holds, and a return address outside the user half shall be
                refused.";
            attribute :>> criterion = "A sigreturn through frames asking for kernel
                mode, masked interrupts, I/O privilege and a kernel-half return
                address resumes the program in user mode with user flags, or ends it
                with SIGSEGV, on each architecture: 0 in kernel mode.";
            attribute :>> parent = ("O.VALIDATE", "O.ISOLATE", "ASR-7");
        }
        requirement <'H.TRAP.6'> entryTrustsNoUserState : ItemHighLevel {
            attribute :>> statement = "On every entry from user mode the kernel shall
                establish its own stack and per-processor state before using either,
                whatever the program left in its registers, including for an
                exception taken on the entry path itself.";
            attribute :>> criterion = "Breakpoints on the system call entry and on
                its return, and an NMI taken with interrupts masked, each run on the
                kernel's own stack with its own per-processor base, and the program
                exits 42 (the `debug` and `nmi` lines, x86-64).";
            attribute :>> parent = ("O.ISOLATE", "O.FAILSAFE");
        }
        requirement <'H.TRAP.7'> onlyUserSelectors : ItemHighLevel {
            attribute :>> statement = "A program shall load only the segment
                selectors meant for user mode: one naming a kernel descriptor, the
                TSS, the LDT or a forged thread-local descriptor shall be refused.
                x86-64 only.";
            attribute :>> criterion = "7 user selectors load at RPL 3 and 18 others
                are refused (the `gdt` line).";
            attribute :>> parent = ("O.ISOLATE", "O.VALIDATE");
        }
    }

    package Boot {
        doc /* What the kernel does with what it is handed before anything runs
             * above it, and what a boot claims: O.FAILSAFE and ASR-6 (FPT_FLS.1),
             * and O.ISOLATE for the layout. */

        requirement <'H.BOOT.1'> handOffVerified : ItemHighLevel {
            attribute :>> statement = "Before using what the loader hands over, the
                kernel shall verify the memory map, the page tables and its own
                placement, and shall halt with a diagnostic if any check fails.";
            attribute :>> criterion = "Stage 1 reports the hand-off verified on
                every architecture; a hand-off with a field made inconsistent ends in
                the catalogued panic, not in a boot.";
            attribute :>> parent = ("O.FAILSAFE", "ASR-6");
        }
        requirement <'H.BOOT.2'> aBootSaysWhetherItChecked : ItemHighLevel {
            attribute :>> statement = "Every boot shall run every stage's self-checks
                unless told not to, and shall print FERRIX-BOOT-OK only when all of
                them passed; a boot that skipped them shall end with
                FERRIX-BOOT-UNCHECKED instead.";
            attribute :>> criterion = "`cargo xtask test-boot` finds FERRIX-BOOT-OK
                on each architecture; a boot with `ferrix.checks=skip` ends with
                FERRIX-BOOT-UNCHECKED and never prints FERRIX-BOOT-OK.";
            attribute :>> parent = ("O.FAILSAFE", "ASR-6");
        }
        requirement <'H.BOOT.3'> layoutIsRandomised : ItemHighLevel {
            attribute :>> statement = "The kernel shall run with its image, direct
                map and vmap arena at the offsets the loader drew from the firmware's
                random-number protocol, and shall verify the move at stage 1.";
            attribute :>> criterion = "Two boots of one image report two different
                layouts (`cargo xtask test-kaslr`), and stage 1 verifies the slide
                each reports.";
            attribute :>> parent = "O.ISOLATE";
        }
        requirement <'H.BOOT.4'> firmwareTablesBounded : ItemHighLevel {
            attribute :>> statement = "The kernel shall refuse an ACPI table or a
                device tree whose declared length is shorter than its header or
                reaches beyond the memory mapped for it, rather than read past it.";
            attribute :>> criterion = "The ACPI and device-tree parsers' host tests
                feed truncated, oversized and misaligned tables and each is refused
                with its error, 0 read out of bounds; the kernel refuses a table
                outside the direct map.";
            attribute :>> parent = ("O.FAILSAFE", "ASR-6");
        }
    }

    package Quotas {
        doc /* What a job, with every job beneath it, may hold at once:
             * O.QUOTA (FRU_RSA.1), against T.EXHAUST. */

        requirement <'H.QUOTA.1'> taskLimit : ItemHighLevel {
            attribute :>> statement = "Creating a task in a job that, with the jobs
                beneath it, holds its task limit shall be refused: a fork with
                EAGAIN, a native process_create with SHOULD_WAIT.";
            attribute :>> criterion = "A fork loop in a job limited to 8 tasks is
                refused at the limit, 0 tasks beyond it (the `quota` line).";
            attribute :>> parent = "O.QUOTA";
        }
        requirement <'H.QUOTA.2'> memoryLimit : ItemHighLevel {
            attribute :>> statement = "A job's programs shall not hold more memory
                than the job's limit, counting the frames of their address spaces
                and their page tables together with every job beneath it; a sibling
                job shall be unaffected.";
            attribute :>> criterion = "Faults in a job limited to 48 pages' worth
                are refused when the next page would not fit, and the refusal is
                counted, while a sibling job faults 48 pages in (the `quota`
                line).";
            attribute :>> parent = "O.QUOTA";
        }
        requirement <'H.QUOTA.3'> objectLimit : ItemHighLevel {
            attribute :>> statement = "Creating a VMO, a channel, a port, a job or a
                pin in a job at its object limit shall be refused with NO_MEMORY.";
            attribute :>> criterion = "Object creation in a job limited to 5 objects
                is refused at the limit, 0 objects beyond it (the `quota` line).";
            attribute :>> parent = "O.QUOTA";
        }
        requirement <'H.QUOTA.4'> chargesComeBack : ItemHighLevel {
            attribute :>> statement = "Every charge against a job shall be returned
                when the resource it paid for is freed, and a job's quota slot shall
                be given back when the job goes.";
            attribute :>> criterion = "After the quota checks every counter of their
                jobs is back to zero, and as many quota slots are in use as before
                them (the `quota` line).";
            attribute :>> parent = "O.QUOTA";
        }
        requirement <'H.QUOTA.5'> limitsAreHierarchical : ItemHighLevel {
            attribute :>> statement = "A limit set on a job shall bound it together
                with every job beneath it, and a charge refused at any level shall
                leave no level charged.";
            attribute :>> criterion = "A charge that would pass a grandparent's limit
                is refused although the child's own limit allows it, and each level's
                counter reads as before the attempt.";
            attribute :>> parent = "O.QUOTA";
        }
        // Added by the low-level pilot on object/ (W-8 step 3), like H.OBJ.10-13.
        requirement <'H.QUOTA.6'> oomKillIsScoped : ItemHighLevel {
            attribute :>> statement = "A user page fault refused by a job's memory
                limit shall end a process in the job whose limit was full or beneath
                it, never one outside it, count the kill in that job and every job
                above it, and retry the fault.";
            attribute :>> criterion = "A program writing 8 MiB in a job limited to 1
                MiB ends by SIGKILL while a larger process in a sibling job lives;
                exactly 1 process is killed; memory.events counts one OOM kill in the
                job and its parent and none in the sibling.";
            attribute :>> parent = ("G.8", "O.QUOTA");
        }
        // Split out of H.QUOTA.2 and H.QUOTA.4 by the pilot, for one check each.
        requirement <'H.QUOTA.7'> kernelHeapIsCharged : ItemHighLevel {
            attribute :>> statement = "The kernel heap the Linux personality keeps
                for a job's programs shall be charged against the job's memory
                limit, and every byte of it returned when it is freed; a sibling job
                shall be unaffected.";
            attribute :>> criterion = "At a 32 KiB limit each kind of personality
                object is refused one past its count with ENOMEM (ENOLCK for a lock)
                while a sibling makes one, and with them gone every byte charged has
                come back (the `kmem` line).";
            attribute :>> parent = "O.QUOTA";
        }
    }

    package Failure {
        doc /* The safe state and what brings the kernel to it: O.FAILSAFE and
             * ASR-6 (FPT_FLS.1). SAFETY-MANUAL.md §3 defines the safe state. */

        requirement <'H.FAIL.1'> haltOnInconsistency : ItemHighLevel {
            attribute :>> statement = "On a failed internal consistency check the
                kernel shall stop every other processor, print a diagnostic naming the
                failure and its catalogued explanation, and halt without returning to
                user mode.";
            attribute :>> criterion = "A boot told to panic at its end
                (`ferrix.onexit=panic`) ends with the FX-1501 report and no program
                output after it; every panic site in the item has a catalogue entry
                (`check-panic-audit.py`).";
            attribute :>> parent = ("O.FAILSAFE", "ASR-6");
        }
        requirement <'H.FAIL.2'> stacksAreGuarded : ItemHighLevel {
            attribute :>> statement = "Every kernel stack shall have an unmapped
                guard page directly below and above it, so that an overflow faults
                rather than overwrites memory.";
            attribute :>> criterion = "For every kernel stack, the page below and the
                page above do not translate, 0 exceptions; on x86-64 an overflow is
                taken as a double fault on its own stack (FX-9004).";
            attribute :>> parent = ("O.FAILSAFE", "ASR-6");
        }
    }
}
