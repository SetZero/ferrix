/*
 * Ferrix — SysML v2 model, part 3: boot.
 *
 * The hand-off ABI, the two address layouts, the loader's sequence, the
 * kernel's bring-up through stage 4, and the trap path. All of this runs
 * today on all three architectures.
 */
package FerrixBoot {
    private import ScalarValues::*;
    private import FerrixLifecycle::*;

    // -----------------------------------------------------------------------
    // The hand-off structure (libs/bootinfo)
    // -----------------------------------------------------------------------

    enum def Arch {
        X86_64;
        AArch64;
        Armv7a;
    }

    enum def MemKind {
        doc /* Firmware's memory types folded into what the kernel cares
             * about. Usable is free at boot; Loader and AcpiReclaim are
             * reclaimable once nothing points into them; Reserved, Mmio and
             * Defective are not RAM. */
        Usable;
        Reserved;
        AcpiReclaim;
        AcpiNvs;
        Loader;
        Kernel;
        Initrd;
        PageTables;
        BootStack;
        Framebuffer;
        Mmio;
        Defective;
        BootInfo;
        DeviceTree;
    }

    attribute def MemRegion {
        attribute base : Natural;
        attribute len : Natural;
        attribute kind : MemKind;
    }

    enum def PixelFormat { Bgrx8888; Rgbx8888; Unknown; }

    attribute def Framebuffer {
        attribute phys : Natural;
        attribute size : Natural;
        attribute width : Natural;
        attribute height : Natural;
        attribute stride : Natural;
        attribute format : PixelFormat;
    }

    #implemented item def BootInfo {
        @stage { number = 1; }
        doc /* Version 3. Every type is repr(C); the kernel refuses to start if
             * the magic or version disagree. Addresses are u64 on every width
             * so the layout is one layout. The kernel validates it once
             * through BootInfo::validate into a BootView whose accessors are
             * safe. */
        attribute magic : String = "FERRIXBI";
        attribute version : Natural = 3;
        attribute arch : Arch;
        attribute regions : MemRegion[1..*] ordered {
            doc /* Sorted, non-overlapping, describing the loader's own
                 * allocations — without which the frame allocator would hand
                 * out the frames holding its own page tables. */
        }
        attribute physmapBase : Natural;
        attribute physmapPhys : Natural {
            doc /* The direct map begins at the lowest RAM address rather than
                 * at zero: a gibibyte in on QEMU's Arm machines. */
        }
        attribute physmapLen : Natural;
        attribute kernelPhys : Natural;
        attribute kernelVirt : Natural;
        attribute kernelLen : Natural;
        attribute rootTablePhys : Natural;
        attribute ttbr0Phys : Natural;
        attribute loaderAliasPhys : Natural;
        attribute loaderAliasLen : Natural;
        attribute bootStackTop : Natural;
        attribute bootStackSize : Natural = 65536;
        attribute framebuffer : Framebuffer;
        attribute initrdPhys : Natural;
        attribute initrdLen : Natural;
        attribute rsdp : Natural;
        attribute dtb : Natural;
        attribute dtbLen : Natural {
            doc /* The device tree is copied into memory of its own kind,
                 * DeviceTree, so it outlives the reclaim of firmware's copy;
                 * stage 10 enumerates devices from the same bytes. */
        }
        attribute uefiSystemTable : Natural;
        attribute cmdline : String {
            doc /* key=value options and bare flags, one grammar (option_in,
                 * flag_in) whether the loader filled this in or the kernel
                 * read /chosen/bootargs from the device tree, which is where
                 * the Arm boards carry it. Options today: console=pl011|stm32,
                 * nosmp (describe_cpus returns the boot CPU only), noactlr. */
        }
    }

    // -----------------------------------------------------------------------
    // Address layouts
    // -----------------------------------------------------------------------

    attribute def AddressRange {
        attribute base : String;
        attribute limit : String;
        attribute purpose : String;
    }

    part def VirtualLayout {
        doc /* libs/bootinfo::Layout. Both instances are checked for overlap
             * at compile time on every build, whichever one the build uses. */
        attribute addressBits : Natural;
        attribute user : AddressRange;
        attribute vmap : AddressRange;
        attribute physmap : AddressRange;
        attribute image : AddressRange;
        attribute vmapReserved : String {
            doc /* The front of the vmap area held back for fixed windows —
                 * the framebuffer and the demand-paging test window — before
                 * the arena begins. */
        }
    }

    #implemented part layout64 : VirtualLayout {
        doc /* Shared by x86-64 and AArch64: four-level tables over 48-bit
             * addresses. x86-64 needs the image in the top 2 GiB for the
             * kernel code model; AArch64 uses it anyway, because one layout is
             * one set of bugs instead of two. */
        attribute :>> addressBits = 64;
        attribute :>> user = AddressRange(base = "0x0000_0000_0000_0000", limit = "0x0000_7FFF_FFFF_FFFF", purpose = "user");
        attribute :>> physmap = AddressRange(base = "0xFFFF_8000_0000_0000", limit = "0xFFFF_FEFF_FFFF_FFFF", purpose = "direct map of all physical RAM");
        attribute :>> vmap = AddressRange(base = "0xFFFF_FF00_0000_0000", limit = "0xFFFF_FFEF_FFFF_FFFF", purpose = "kernel vmap: MMIO, guard-paged stacks");
        attribute :>> image = AddressRange(base = "0xFFFF_FFFF_8000_0000", limit = "0xFFFF_FFFF_FFFF_FFFF", purpose = "the kernel image");
        attribute :>> vmapReserved = "0x1_0000_0000";
    }

    #implemented part layout32 : VirtualLayout {
        doc /* ARMv7-A: a 2/2 split, TTBR0 translating the lower half and
             * TTBR1 the upper, three-level LPAE tables. The user half is the
             * larger because a 32-bit process wants it; the direct map gets
             * what the vmap area and the image leave, and its size is the
             * ceiling on RAM the kernel can use. */
        attribute :>> addressBits = 32;
        attribute :>> user = AddressRange(base = "0x0000_0000", limit = "0x7FFF_FFFF", purpose = "user");
        attribute :>> vmap = AddressRange(base = "0x8000_0000", limit = "0x9FFF_FFFF", purpose = "kernel vmap");
        attribute :>> physmap = AddressRange(base = "0xA000_0000", limit = "0xEFFF_FFFF", purpose = "direct map of RAM, 1.25 GiB");
        attribute :>> image = AddressRange(base = "0xF000_0000", limit = "0xFFFF_FFFF", purpose = "the kernel image");
        attribute :>> vmapReserved = "0x0400_0000";
    }

    attribute pageSize : Natural = 4096;
    attribute tableEntries : Natural = 512;

    // -----------------------------------------------------------------------
    // The loader's sequence (boot/src/main.rs)
    // -----------------------------------------------------------------------

    #implemented action def LoaderSequence {
        @stage { number = 1; }
        doc /* Firmware calls efi_main in 64-bit mode (SVC mode on ARMv7-A)
             * with a stack and the MMU on. Only enterKernel is assembly. */
        out handoff : BootInfo;

        first start;
        then action initFirmwareConsole {
            doc /* Firmware's con_out, until console::shutdown just before
                 * ExitBootServices. */
        }
        then action prepareCpu;
        then action stageKernel {
            doc /* Read /FERRIX/KERNEL.ELF from the volume the loader came from,
                 * parse it with libs/elf, copy the segments to their link
                 * address. Malformed segments are refused here rather than
                 * discovered by the MMU. */
        }
        then action allocateBootAreas {
            doc /* Boot stack (64 KiB), boot info (64 KiB, around 2700 regions
                 * of room), memory map buffer. */
        }
        then action buildAddressSpace {
            doc /* libs/paging Mapper: the image at its link address, the direct
                 * map from the lowest RAM address, and an identity plan for the
                 * loader's own code so the instruction after the switch is
                 * fetchable. The loader also maps itself where the kernel can
                 * reach it, so the kernel can drop the map rather than abandon
                 * a table. */
        }
        then action writeBootInfo;
        then action leaveFirmware {
            doc /* Fetch the memory map one last time and call ExitBootServices.
                 * Past this line firmware is gone: no allocation, no console,
                 * no protocols. */
        }
        then action recordMemoryMap {
            doc /* Translate UEFI descriptors into MemRegion[] behind the boot
                 * info, tagging the loader's own allocations. */
        }
        then action cleanDcache {
            doc /* The Arm architectures turn the MMU off in the middle of the
                 * switch, so anything dirty in a cache would vanish. No-op on
                 * x86-64. */
        }
        then action enterKernel {
            doc /* Install the new tables and jump to _start with the boot info
                 * pointer in the direct map. */
        }
        then done;
    }

    // -----------------------------------------------------------------------
    // The kernel's bring-up (kernel/src/main.rs)
    // -----------------------------------------------------------------------

    #implemented part def EarlyMemory {
        @stage { number = 1; }
        doc /* kernel/src/early.rs: a pool of sixteen page tables carved from
             * the kernel image, enough to map a console and a framebuffer
             * before there is an allocator. On AArch64 the console is an MMIO
             * register that has to be mapped as device memory before a byte
             * can go out. */
        attribute earlyTables : Natural = 16;
        action readPhysicalByte;
        action translate;
        action mapDevice;
    }

    #implemented action def KernelBringUp {
        doc /* kmain. Every stage's exit criterion runs here on every boot, and
             * each failure panics with its own message so the boot test
             * fails with a reason rather than a timeout. The marker at the end
             * reads FERRIX-BOOT-OK stages 1-6. */
        in info : BootInfo;

        first start;
        then action validateHandoff {
            doc /* Magic, version, arch and layout constants. No console yet, so
                 * a mismatch halts silently: there is no valid way to make one. */
        }
        then action initConsole;
        then action reportHandoff;
        then action stage1SelfCheck : Stage1SelfCheck;
        then action installTraps {
            doc /* Before anything can fault: until this runs the CPU still
                 * points at firmware's handlers, which stopped existing at
                 * ExitBootServices. */
        }
        then action initPhysicalMemory {
            doc /* mm::init: carve the per-frame array from the largest usable
                 * region, hand every usable frame to the buddy, start the heap. */
        }
        then action initVmapArena;
        then action stage2SelfCheck : Stage2SelfCheck;
        then action stage3TrapCheck : Stage3TrapCheck;
        then action initInterrupts {
            doc /* arch::init_interrupts: controller and clocks, reported as
                 * "clock" and "irqs" lines. */
        }
        then action initTimer;
        then action enableInterrupts;
        then action stage3TimerCheck : Stage3TimerCheck;
        then action stage4Processors : Stage4BringUp;
        then action stage5Scheduler : Stage5SchedulerCheck {
            doc /* After stage 4 because it needs every processor it will
                 * schedule on, before finishMemory because the task stacks it
                 * takes and gives back are mappings the sweep has to see
                 * settled. */
        }
        then action stage6MemoryObjects {
            @stage { number = 6; }
            doc /* Stage 6 so far: a reservation costs nothing until it is
                 * touched, and every frame an object was given comes back
                 * when it is dropped — the leak that would otherwise kill the
                 * machine an hour into a rustc build. */
        }
        then action finishMemory : FinishMemory;
        then action reportSuccess;
        then action shutdown;
        then done;
    }

    #implemented action def Stage1SelfCheck {
        @stage { number = 1; }
        doc /* Stage 1's exit criterion. */
        first start;
        then action mapIsSortedAndNonOverlapping;
        then action mapHasUsableRam;
        then action mapDescribesLoaderAllocations {
            doc /* Kernel, PageTables and BootInfo regions must all be present. */
        }
        then action directMapAliasesPhysical {
            doc /* Read the kernel's own first bytes through the image mapping
                 * and through the direct map; they must agree. */
        }
        then action canWalkAndExtendTables {
            doc /* Translating the kernel image agrees with the loader; a new
                 * device mapping (the framebuffer, where one exists) takes
                 * effect. */
        }
        then done;
    }

    #implemented action def Stage2SelfCheck {
        @stage { number = 2; }
        first start;
        then action frameHammer {
            doc /* 4096 blocks of orders 0..4 allocated and freed in an order
                 * that forces coalescing; the free count must return exactly
                 * to where it started. Uses no heap on purpose. */
        }
        then action heapCheck {
            doc /* A Box, a Vec grown through reallocations, a BTreeMap of two
                 * thousand entries; heap_allocated must return to zero. */
        }
        then action vmapCheck {
            doc /* Written through and read back; two allocations separated by
                 * guard pages; the pages either side translate to nothing; a
                 * kernel stack 16-byte aligned, writable at both ends, guarded
                 * beyond each; freeing returns every frame. */
        }
        then done;
    }

    #implemented action def Stage3TrapCheck {
        @stage { number = 3; }
        first start;
        then action breakpointTwice {
            doc /* A canary in a register the frame saves and restores must
                 * come back intact; that proves the path restores rather than
                 * merely arrives. */
        }
        then action demandPagingWindow {
            doc /* Three pages touched out of order in an unmapped window; each
                 * fault maps the address and the instruction retries. Pages
                 * read back what was written, are zeroed, and cost exactly one
                 * frame each plus one per table level for the first fault. */
        }
        then done;
    }

    #implemented action def Stage3TimerCheck {
        @stage { number = 3; }
        first start;
        then action oneShotFiresOnce {
            doc /* Arm once, wait for the tick, then require the count not to
                 * move for ten further intervals. Catches a level-triggered
                 * timer acknowledged but not disarmed, which re-enters forever. */
        }
        then action thousandTicksMeasured {
            doc /* Count 1000 interrupts and measure the elapsed time with the
                 * counter, not by multiplying ticks by the programmed rate.
                 * Against a requested 1000 Hz the three report 998 to 999. */
        }
        then done;
    }

    #implemented action def Stage4BringUp {
        @stage { number = 4; }
        first start;
        then action discoverProcessors {
            doc /* MADT local APIC / x2APIC / GIC CPU interface entries, or the
                 * device tree's /cpus; checked for duplicates and required to
                 * include the processor reading them. */
        }
        then action startSecondaries;
        then action everyDescribedProcessorOnline {
            doc /* Unless nosmp was given, in which case one is described. On
                 * ARMv7-A the coherency line reports how many cores had
                 * ACTLR.SMP set. */
        }
        then action smpChecks {
            doc /* On every processor at once: a hundred rounds of IPI-woken
                 * work; a page remapped twenty times with every processor
                 * required to read the new frame; a hundred grace periods
                 * against readers, with retired objects poisoned rather than
                 * freed; then the contended counter. */
        }
        then done;
    }

    #implemented action def Stage5SchedulerCheck {
        @stage { number = 5; }
        doc /* Four checks, the last the stage's own. */
        first start;
        then action initScheduler {
            doc /* One domain holding every CPU in Throughput mode, a queue and
                 * an idle task per processor, the boot thread adopted as a
                 * task. */
        }
        then action spawnRunReap {
            doc /* A task can be created, run, and cleaned up after. */
        }
        then action sleepIsASleep {
            doc /* The task is off the run queue and the processor is free,
                 * and it comes back when it said it would. */
        }
        then action thousandThreads {
            doc /* All spawned on one processor, so the others get any only by
                 * stealing; bounded work to completion; every stack back. */
        }
        then action fairnessBound {
            doc /* Twelve spinners, three per processor, one of each three at a
                 * different weight, inside a measured window. Each task's
                 * service, in nanoseconds of the stage 3 clock, must stay
                 * within EEVDF's bound of its weighted share: a slice plus the
                 * worst overrun the scheduler actually served, both printed. */
        }
        then done;
    }

    #implemented action def FinishMemory {
        @stage { number = 2; }
        doc /* The half of stage 2 that cannot run until the rest of boot has,
             * in an order that is forced: the W^X sweep must first be able to
             * see the loader's identity map as a violation, then the map is
             * dropped, then the sweep must pass, then the memory early boot
             * used goes back to the allocator. */
        first start;
        then action sweepMustSeeIdentityMap;
        then action dropIdentityMap;
        then action addressZeroTranslatesToNothing {
            doc /* A null dereference in kernel code must fault rather than
                 * find the first page of physical memory. */
        }
        then action wxSweep {
            doc /* Walk every live leaf through Mapper::for_each_leaf; none may
                 * be writable and executable. Reports what it swept, because a
                 * sweep that walks nothing also finds nothing. */
        }
        then action reclaimBootMemory {
            doc /* The loader's own memory and the ACPI-reclaim regions go to
                 * the buddy: 2 to 4 MiB on a 512 MiB QEMU machine. */
        }
        then done;
    }

    // -----------------------------------------------------------------------
    // Traps (kernel/src/trap.rs)
    // -----------------------------------------------------------------------

    enum def TrapKind {
        doc /* Why the kernel was entered, in terms all three architectures
             * share. Each arch module classifies its own frame into this and
             * the policy is written once. */
        PageFault;
        Breakpoint;
        IllegalInstruction;
        Interrupt;
        SystemCall;
        Fault;
    }

    attribute def PageFault {
        attribute address : Natural;
        attribute write : Boolean;
        attribute execute : Boolean;
        attribute user : Boolean;
        attribute present : Boolean {
            doc /* A mapping existed but denied the access, versus no mapping
                 * at all: protection violation versus not-yet-faulted-in. */
        }
        attribute inDemandWindow : Boolean;
    }

    attribute def Trap {
        attribute kind : TrapKind;
        attribute fault : PageFault[0..1];
        attribute irq : Natural[0..1];
    }

    #implemented part def TrapDispatch {
        @stage { number = 3; }
        doc /* One dispatch path above every architecture, reached only through
             * the facade: classify the frame, act, and on return let the
             * entry stub restore the interrupted context. */
        attribute breakpointsTaken : Natural;
        attribute faultsHandled : Natural;
        perform action dispatch : Dispatch;
    }

    #implemented action def Dispatch {
        in trap : Trap;
        action breakpoint {
            doc /* Count it and advance past the instruction. */
        }
        action pageFault : HandlePageFault;
        action interrupt {
            doc /* Acknowledge in arch, then irq::dispatch by number. */
        }
        action systemCall {
            @stage { number = 7; }
            doc /* Reserved: stage 7 gives this a body. */
        }
        action report {
            doc /* Anything else is a kernel bug at this stage: print the
                 * frame and halt with FERRIX-PANIC. */
        }
        first start;
        then decide;
            if trap.kind == TrapKind::Breakpoint then breakpoint;
            if trap.kind == TrapKind::PageFault then pageFault;
            if trap.kind == TrapKind::Interrupt then interrupt;
            if trap.kind == TrapKind::SystemCall then systemCall;
            else report;
    }

    #implemented action def HandlePageFault {
        doc /* Resolving a fault by mapping a page and retrying is demand
             * paging. It arrived at stage 3 because it is the only honest way
             * to show the fault path recovers; stage 6 inherits it rather than
             * writing it, replacing the fixed window with the VMA lookup. */
        in fault : PageFault;
        action mapDemandPage {
            doc /* Allocate a zeroed frame, map it at the faulting page,
                 * return so the instruction retries. */
        }
        action reportFault;
        first start;
        then decide;
            if fault.inDemandWindow then mapDemandPage;
            else reportFault;
        first mapDemandPage then done;
        first reportFault then done;
    }
}
