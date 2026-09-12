/*
 * Ferrix — SysML v2 model, part 2: structure.
 *
 * The shape of the system (docs/ARCHITECTURE.md §1), the workspace it is
 * built from (§9), the architecture facade, and the three architectures
 * behind it. Subsystem internals live in the packages that follow; this one
 * says what exists and how it is connected.
 */
package FerrixStructure {
    private import ScalarValues::*;
    private import FerrixLifecycle::*;
    private import FerrixBoot::*;
    private import FerrixMemory::*;
    private import FerrixScheduling::*;
    private import FerrixObjects::*;
    private import FerrixIsolation::*;
    private import FerrixDrivers::*;
    private import FerrixStorage::*;

    // -----------------------------------------------------------------------
    // Context: the machine firmware hands us
    // -----------------------------------------------------------------------

    enum def IsaFamily {
        X86_64;
        AArch64;
        Armv7a;
    }

    enum def FirmwareKind {
        doc /* Every architecture boots via UEFI, so firmware calls a Rust
             * efi_main with a stack set up and the MMU on. Which firmware
             * differs: EDK2 on the 64-bit pair, U-Boot's bootefi on ARMv7-A
             * (upstream EDK2 dropped its 32-bit Arm build, and U-Boot is what
             * the STM32MP157 runs anyway). */
        Edk2;
        UBoot;
    }

    enum def MachineDescription {
        doc /* How the kernel learns what the machine contains: ACPI tables on
             * x86-64 and on AArch64 under EDK2, a flattened device tree on
             * ARMv7-A and on AArch64 firmware that offers one. */
        Acpi;
        DeviceTree;
    }

    part def Cpu {
        attribute isa : IsaFamily;
        attribute hardwareId : Natural {
            doc /* What the machine calls it: an APIC ID, or the affinity
                 * fields of MPIDR. Not dense, so the kernel also assigns a
                 * logical index; the boot CPU is always logical zero. */
        }
    }

    part def Iommu {
        doc /* VT-d or AMD-Vi on x86-64, SMMUv3 on AArch64. Absent on QEMU's
             * default machines, which is why the degraded trusted mode exists. */
    }

    part def Device;

    part def Machine {
        doc /* The system context. Ferrix targets QEMU's q35 and virt
             * machines in CI and the STM32MP157 on the ARMv7-A side. */
        attribute firmware : FirmwareKind;
        attribute description : MachineDescription;
        part cpus : Cpu[1..*];
        attribute ramBytes : Natural;
        part devices : Device[0..*];
        part iommu : Iommu[0..1];
        port efi : EfiHandoffPort;
    }

    // -----------------------------------------------------------------------
    // Interfaces between the big pieces
    // -----------------------------------------------------------------------

    item def EfiSystemTable {
        doc /* Firmware's table: boot services, con_out, the configuration
             * tables that carry the RSDP or the device tree. */
    }

    port def EfiHandoffPort {
        doc /* extern "efiapi" fn efi_main(image, system_table). On ARMv7-A the
             * calling convention lowers to AAPCS on the musleabi target, so the
             * signature is the same on all three. */
        out item systemTable : EfiSystemTable;
    }

    port def BootHandoffPort {
        doc /* The loader and the kernel are two programs linked for different
             * targets that meet at exactly one struct, declared once in
             * libs/bootinfo. A mismatch is a type error rather than a triple
             * fault. */
        in item info : BootInfo;
    }

    item def SyscallRequest {
        attribute number : Natural;
        attribute args : Integer[0..6] ordered;
    }

    item def SyscallResult {
        doc /* A value, or a negative errno, exactly as Linux returns it. */
        attribute value : Integer;
    }

    port def LinuxSyscallPort {
        doc /* Syscall numbers 0.., each architecture's table as Linux defines
             * it (the EABI table on ARMv7-A). A compatibility obligation: no
             * opinions live here. libs/linux-abi holds the numbers, errnos and
             * repr(C) layouts. */
        in item request : SyscallRequest;
        out item result : SyscallResult;
    }

    port def NativeSyscallPort {
        doc /* Syscall numbers from 0x1000, capability-handle based. Where the
             * design opinions live; what devmgr and drivers speak. A process
             * may use both ports. */
        in item request : SyscallRequest;
        out item result : SyscallResult;
    }

    port def SerialConsolePort {
        doc /* Early boot and panic output. The boot test reads this stream
             * and waits for the marker FERRIX-BOOT-OK. */
        out item line : String;
    }

    // -----------------------------------------------------------------------
    // The system of interest
    // -----------------------------------------------------------------------

    part def Ferrix {
        doc /* An operating system written in Rust for x86-64, AArch64 and
             * ARMv7-A, whose acceptance test is that it compiles Rust. */

        part loader : Loader;
        part kernel : Kernel;
        #planned part initramfs : Initramfs;
        #planned part userland : Userland;

        connect loader.handoff to kernel.handoff;
        connect userland.posix to kernel.linuxAbi;
        connect userland.native to kernel.nativeAbi;
    }

    part def Deployment {
        doc /* Ferrix on a machine. CI instantiates three of these under QEMU;
             * the STM32MP157-DK board is the fourth, reached by
             * `cargo xtask deploy --arch armv7a`, which copies two files onto
             * a vendor-provisioned card rather than writing a whole image,
             * because the board's boot chain lives in partitions imaging would
             * destroy, then watches the ST-LINK serial port for the marker.
             * Nothing in that path has run on hardware yet. */
        part machine : Machine;
        part ferrix : Ferrix;
        connect machine.efi to ferrix.loader.efi;
    }

    part ferrix : Ferrix {
        doc /* The reference instance the roadmap and assurance packages point
             * at. */
    }

    // -----------------------------------------------------------------------
    // The loader
    // -----------------------------------------------------------------------

    #implemented part def Loader {
        @stage { number = 1; }
        doc /* boot/: the UEFI loader. Reads the kernel from the volume it was
             * booted from, copies it to its link address, builds the address
             * space, takes the memory map, leaves boot services, installs the
             * new tables and jumps. Only the last step is assembly, because
             * the return address of a Rust call would be in the address space
             * just replaced. One program for three architectures: boot/src/arch
             * has one file per machine and no second loader. */

        port efi : ~EfiHandoffPort;
        port handoff : ~BootHandoffPort;

        attribute kernelPath : String = "/FERRIX/KERNEL.ELF";
        attribute bootInfoBytes : Natural = 65536;
        attribute bootStackBytes : Natural = 65536;

        part uefi : UefiBindings {
            doc /* boot/src/uefi: the handful of protocols and tables used —
                 * simple file system, loaded image, graphics output, and the
                 * ACPI and device-tree configuration tables. Hand-written,
                 * no crates.io dependency. */
        }
        part services : Services {
            doc /* boot/src/services.rs: allocation, memory map, and the one
                 * call after which firmware is gone: ExitBootServices. */
        }
        part load : ImageLoading {
            doc /* boot/src/load.rs: ELF parse via libs/elf, placement, page
                 * tables via libs/paging, the identity plan for the switch. */
        }
        part arch : LoaderArch;

        perform action boot : LoaderSequence;
    }

    part def UefiBindings;
    part def Services;
    part def ImageLoading;

    abstract part def LoaderArch {
        doc /* prepare_cpu, clean_dcache, enter_kernel: what each machine needs
             * between ExitBootServices and the kernel's first instruction. */
        action prepareCpu;
        action cleanDcache;
        action enterKernel {
            doc /* The one assembly site in the loader. x86-64: EFER.NXE,
                 * CR4.PGE, write CR3, jump. AArch64: MMU off, MAIR/TCR/TTBR0/1,
                 * SCTLR, MMU on, jump — TCR cannot be reprogrammed under a
                 * live regime. ARMv7-A: the same through cp15, refusing to
                 * start in HYP mode rather than leaving it. */
        }
    }

    // -----------------------------------------------------------------------
    // The kernel
    // -----------------------------------------------------------------------

    part def Kernel {
        doc /* kernel/: monolithic core, capability seams, userspace device
             * drivers. Entered from the loader with the MMU on, three
             * mappings in place and nothing else: no vectors, no allocator,
             * no other CPU running. */

        port handoff : BootHandoffPort;
        port console : SerialConsolePort;
        #inProgress port linuxAbi : LinuxSyscallPort;
        #planned port nativeAbi : NativeSyscallPort;

        // -- Runs today, stages 1 to 4 --------------------------------------
        part arch : ArchLayer;
        #implemented part printer : Console {
            doc /* kernel/src/console.rs: println over the arch console, behind
                 * a lock that a panicking CPU waits a bounded time for, so a
                 * fault while printing still produces its FERRIX-PANIC line.
                 * It keeps its last 4 KiB in a lock-free ring, which a panic
                 * draws on the firmware framebuffer when there is one
                 * (kernel/src/panic/screen.rs): the second in-kernel output
                 * device, write-only and used for nothing else. */
        }
        #implemented part early : EarlyMemory;
        #implemented part trap : TrapDispatch;
        #implemented part mm : PhysicalMemory;
        #implemented part vmap : VmapArena;
        #implemented part mmio : MmioWindows;
        #implemented part irq : IrqTable;
        #implemented part timer : Timer;
        #implemented part smp : Smp;
        #implemented part acpi : AcpiAccess;
        #implemented part fdt : FdtAccess;
        #implemented part tasks : Tasks;
        #implemented part sched : Scheduler;

        // -- Ahead: stages 6 to 14 -------------------------------------------
        #inProgress part vm : VirtualMemory;
        #inProgress part syscalls : LinuxSyscallLayer;
        #planned part native : NativeAbi;
        #planned part futex : Futex;
        #inProgress part signals : Signals;
        #planned part ipc : PosixIpc;
        #planned part vfs : Vfs;
        #planned part pageCache : PageCache;
        #planned part filesystems : Filesystems;
        #planned part blockCore : BlockCore;
        #planned part netCore : NetCore;
        #planned part namespaces : Namespaces;
        #planned part cgroups : Cgroups;
        #planned part seccomp : Seccomp;
        #planned part devices : DeviceEnumeration;
        #planned part iommu : IommuDomains;

        perform action bringUp : KernelBringUp;

        // The lock order stages 2 and 4 wrote down, as a constraint on the
        // parts that hold them: tables before frames before heap.
        assert constraint lockOrder {
            doc /* mm.rs: TABLES serialises every walk and change of the
                 * kernel page tables and is taken before FRAMES (a mapping
                 * may need a frame for a table) and before HEAP (an unmap
                 * records what it released). Never the other way round.
                 * Stage 5 adds a run queue's lock outside the heap's and the
                 * frame allocator's, with nothing ever taken outside it. */
        }
    }

    part def Console;

    // -----------------------------------------------------------------------
    // Userland, from stage 6
    // -----------------------------------------------------------------------

    #planned part def Initramfs {
        @stage { number = 8; }
        doc /* A cpio "newc" archive the loader places in RAM, holding devmgr,
             * the virtio-blk driver and init. What breaks the cycle "the root
             * filesystem needs a block driver that lives on the root
             * filesystem". */
        part devmgr : UserBinary;
        part virtioBlk : UserBinary;
        part init : UserBinary;
    }

    part def UserBinary {
        attribute staticMusl : Boolean = true;
    }

    #planned part def UserProcess {
        @stage { number = 6; }
        doc /* A running static musl binary: a Process (see FerrixObjects)
             * speaking the Linux ABI, and optionally the native one. */
        attribute usesNativeAbi : Boolean = false;
    }

    #planned part def Userland {
        doc /* Static musl binaries speaking the Linux ABI; devmgr and drivers
             * additionally speak the native ABI. */
        port posix : ~LinuxSyscallPort;
        port native : ~NativeSyscallPort;

        part init : UserProcess;
        part shell : UserProcess {
            doc /* busybox sh, the first foreign binary to run on Ferrix:
                 * Alpine's static musl build runs a script at stage 7, given
                 * with sh -c and embedded in the kernel by FERRIX_INIT. */
        }
        part devmgr : UserProcess;
        part drivers : DriverProcess[0..*];
        part rustc : UserProcess {
            doc /* The goal, stage 16. */
        }
    }

    // -----------------------------------------------------------------------
    // The architecture facade
    // -----------------------------------------------------------------------

    abstract part def ArchFacade {
        doc /* kernel/src/arch/mod.rs: the one module generic code reaches the
             * CPU through. Each architecture exports the same set of names;
             * the facade re-exports whichever one the build is for. Adding an
             * architecture is a third cfg branch, not a second facade. */

        attribute NAME : String;
        attribute TLB_FLUSH_IS_BROADCAST : Boolean {
            doc /* True where the hardware invalidates every core's TLB in one
                 * instruction (tlbi ...is on both Arm architectures); false
                 * where the kernel has to send a shootdown IPI (x86-64). */
        }

        item def TrapFrame {
            doc /* The interrupted context as the entry stub saved it. Shape
                 * differs per machine; classify turns it into a Trap. */
        }
        part encoding : PageTableEncoding {
            doc /* PageEncoding: which libs/paging Encoding this machine's
                 * descriptors use. */
        }

        // Console and traps
        action initConsole;
        action initTraps;
        action breakpoint;
        action advancePastBreakpoint;
        action classify;
        action reportTrap;

        // Interrupts and time
        action initInterrupts;
        action enableInterrupts;
        action disableInterrupts;
        action serviceInterrupts;
        action timerArm;
        action timerDisarm;
        attribute timerIrq : Natural;
        attribute ipiIrq : Natural;
        action counterNow;
        action counterHz;
        action waitForInterrupt;
        action waitForWork;

        // Memory
        action flushTlb;
        action dropIdentityMap;
        action identityRoot;
        action prepareUserRoot {
            @stage { number = 6; }
            doc /* Make a freshly allocated user root usable. x86-64 keeps both
                 * halves in one root, so it shares the kernel's top-level
                 * slots into it — shared, not copied, so a later kernel
                 * mapping appears in every space without walking any. The
                 * two Arm architectures do nothing: the kernel is in TTBR1. */
        }
        action installUserRoot {
            @stage { number = 6; }
            doc /* Translate this processor's user half through a given root.
                 * x86-64 is one CR3 write, whose own side effect is to drop
                 * every non-global entry while the kernel's global ones — set
                 * because the loader enables CR4.PGE — survive. The two Arm
                 * architectures write TTBR0, clear EPD0 (the lower half is
                 * disabled rather than dismantled, so an install re-enables a
                 * regime), and then invalidate by ASID, in that order: the
                 * other order leaves the regime live with the register still
                 * naming the previous space's tables. */
        }
        action uninstallUserRoot {
            @stage { number = 6; }
            doc /* Stop translating user addresses, which is the state a kernel
                 * thread runs in. x86-64 goes back to the kernel's own root;
                 * the Arm pair set EPD0 and invalidate, because EPD0 governs
                 * walks and not the TLB. Not lazy TLB: keeping the outgoing
                 * root installed is faster and has to keep an address space
                 * alive under a thread that holds no reference to it. */
        }

        // Processors
        action describeCpus;
        action hardwareId;
        action cpuLocal;
        action setCpuLocal;
        action sendIpiToOthers;
        part cpuStarter : CpuStarter;

        // Power
        action halt;
        action shutdown;

        // Tasks (stage 5)
        action prepareStack {
            doc /* Lay out a fresh task's stack so that the first switch into
                 * it "returns" into its entry function. */
        }
        action switchTo {
            doc /* The context switch, arch/<machine>/switch.rs: saves the
                 * callee-saved set and the stack pointer, and returns onto a
                 * stack that belongs to another task, which Rust cannot say.
                 * The last entry the allow-list's every-architecture half was
                 * meant to gain. */
        }

        // Owed by later stages, and the only things a stage may add.
        #implemented action enterUser {
            @stage { number = 6; }
            doc /* run_user: the ring-3 / EL0 / USR transition, and leave_user
                 * back from it. Callee-saved registers saved, the stack pointer
                 * parked in the per-CPU record, a dedicated entry stack for
                 * every exception from user mode -- at EL1 sp is SP_EL1, and on
                 * ARMv7-A exceptions are stored on the SVC stack, so leaving
                 * either where the parked registers are lets the first fault
                 * overwrite them -- every general register zeroed, then
                 * sysretq, eret or rfeia. Interrupts masked in user mode while a
                 * program is a guest of the boot task. */
        }
        #implemented action systemCall {
            @stage { number = 6; }
            doc /* The Arm half of system call entry: svc arrives through the
                 * trap vector as Trap::SystemCall, so the architecture reads the
                 * number and arguments from the saved registers, handles exit
                 * and exit_group with leave_user before dispatch, and writes the
                 * result back. x86-64's body refuses, because SYSCALL has an
                 * entry of its own. */
        }
        #planned action syscallEntry {
            @stage { number = 7; }
            doc /* x86-64: SYSCALL leaves the return address in rcx and does
                 * not switch the stack, so entry swaps to the kernel stack
                 * through swapgs before anything can be pushed. */
        }
        #planned part iommu : IommuDriver {
            @stage { number = 10; }
        }
    }

    part def PageTableEncoding;
    part def CpuStarter {
        doc /* How a secondary processor is brought from "off" to running the
             * kernel's secondary_main with the MMU on: INIT-SIPI-SIPI and a
             * real-mode trampoline, or PSCI CPU_ON through an identity map of
             * the entry sequence. */
    }
    part def IommuDriver;

    #implemented part def X86_64Arch :> ArchFacade {
        attribute :>> NAME = "x86_64";
        attribute :>> TLB_FLUSH_IS_BROADCAST = false;
        doc /* kernel/src/arch/x86_64. Target x86_64-unknown-none, kernel code
             * model, no red zone, soft float. Machine description: ACPI. */

        part gdt : Gdt {
            doc /* Per processor, with its TSS: a TSS cannot be shared because
                 * loading one marks its descriptor busy. */
        }
        part idt : Idt {
            doc /* 256 gates, every one naming the kernel code selector; the
                 * double-fault gate on an IST stack of its own. Entry stubs are
                 * an assembler .rept, ten of which push an error code. */
        }
        part lapic : LocalApic {
            doc /* Mapped from the MADT, enabled, task priority dropped. Its
                 * timer is calibrated against the HPET. One per processor. */
        }
        part ioapics : IoApic[1..*] {
            doc /* Every I/O APIC firmware described is mapped and every input
                 * masked: quiesced, not configured, until stage 10. */
        }
        part clock : X86Clock {
            doc /* HPET main counter, whose period firmware states in
                 * femtoseconds. Falls back to the TSC calibrated against the
                 * PIT on a machine with no HPET table. */
        }
        part serial : Serial16550 {
            doc /* COM1 through port I/O. The one in-kernel device. */
        }
        part trampoline : ApTrampoline {
            doc /* Real mode to long mode in one step, on a root table below
                 * 1 MiB sharing the kernel's upper half. The only 16-bit code
                 * in the project. */
        }
        part shootdown : TlbShootdown;

        part tscDeadline : TscDeadlineTimer {
            @deferred { reason = "Replaces the LAPIC countdown with a comparator against the TSC; the calibration exists. Waits for a tickless scheduler to want it."; }
        }
        part x2apic : X2ApicMode {
            @deferred { reason = "APIC IDs above 255 are refused with a message; QEMU's are 0 to 3."; }
        }
        #planned part vtd : IommuDriver {
            @stage { number = 10; }
        }
    }

    part def Gdt; part def Idt; part def LocalApic; part def IoApic;
    part def X86Clock; part def Serial16550; part def ApTrampoline;
    part def TlbShootdown; part def TscDeadlineTimer; part def X2ApicMode;

    #implemented part def AArch64Arch :> ArchFacade {
        attribute :>> NAME = "aarch64";
        attribute :>> TLB_FLUSH_IS_BROADCAST = true;
        doc /* kernel/src/arch/aarch64. Target aarch64-unknown-none-softfloat.
             * Machine description: ACPI (MADT, FADT for the PSCI conduit,
             * GTDT for the timer interrupt). */

        part vectors : VbarEl1Table {
            doc /* Sixteen entries at fixed 128-byte offsets; the layout is the
                 * interface. Each entry stashes two registers, records which
                 * entry it is and branches to the common save path. */
        }
        part gic : Gicv2Front {
            doc /* aarch64/gic.rs: the MADT walk alone — distributor and CPU
                 * interface addresses, the check that every core's interface
                 * is the same banked address, the version check — then a call
                 * into the shared gicv2 driver. */
        }
        part timer : GenericTimer {
            doc /* The architected virtual timer, CNTV_CVAL_EL0, because a
                 * kernel at EL1 is below any hypervisor present. Compares
                 * against an absolute instant; there is no periodic mode. */
        }
        part serial : Pl011Console {
            doc /* Still aarch64's own copy with a hard-coded address; moving
                 * it onto the shared pl011 driver is the open half of
                 * docs/arm32.md decision 6. */
        }
        part psci : PsciCpuOn {
            doc /* Secondaries start with the MMU off at a physical address and
                 * enter through an identity map of the entry sequence alone,
                 * in a tree of their own, loading every parameter before the
                 * MMU goes on. */
        }
        part el2Drop : El2ToEl1 {
            doc /* Firmware may hand off at EL2; lowering is an eret into a
                 * constructed context. */
        }

        part gicv3 : Gicv3 {
            @deferred { reason = "gic::init refuses anything that is not a GICv2; QEMU virt gives GICv2 unless asked, so this needs a second boot-test configuration as much as code."; }
        }
        part parking : PsciParkingProtocol {
            @deferred { reason = "For firmware without PSCI. Refused, not guessed at."; }
        }
        #planned part smmu : IommuDriver {
            @stage { number = 10; }
        }
    }

    part def VbarEl1Table; part def Gicv2Front; part def GenericTimer;
    part def Pl011Console; part def PsciCpuOn; part def El2ToEl1;
    part def Gicv3; part def PsciParkingProtocol;

    #implemented part def Armv7aArch :> ArchFacade {
        attribute :>> NAME = "armv7a";
        attribute :>> TLB_FLUSH_IS_BROADCAST = true;
        doc /* kernel/src/arch/armv7a: the Cortex-A7 of the STM32MP157, run on
             * QEMU virt under U-Boot. Target armv7a-none-eabi with LPAE.
             * Machine description: device tree only. Joined after stage 3
             * without a second loader, a second facade or a line of bootstrap
             * assembly; docs/arm32.md is the plan it followed. */

        part vectors : Armv7aVectorTable {
            doc /* Eight one-instruction entries at VBAR. Each corrects the
                 * banked lr, stores it and the status on the SVC stack with
                 * srsdb and switches to SVC with cps; return is rfeia. No
                 * processor mode but SVC is ever given a stack. */
        }
        part gic : Gicv2FromFdt {
            doc /* Distributor and CPU interface from the device tree, then the
                 * shared gicv2 driver. */
        }
        part timer : GenericTimerCp15 {
            doc /* The same counter as AArch64's, reached through cp15; the
                 * interrupt number comes from the device tree. */
        }
        part serial : ConsoleChoice {
            doc /* armv7a/console.rs chooses between the ports the device tree
                 * describes, in priority order: `console=pl011|stm32` from
                 * /chosen/bootargs (U-Boot sets it without reflashing, for a
                 * board whose tree is exactly what is in question), then
                 * stdout-path, then the first enabled port. A name matching
                 * no drivable node falls through rather than failing. The
                 * PL011 on QEMU virt, the STM32 USART on the board. */
        }
        part coherency : ActlrReport {
            doc /* Every core reads ACTLR.SMP once it is in Rust and the boot
                 * log says how many had it set. An observation, never a
                 * failure: QEMU does not implement the register, so
                 * "clear on all" is ambiguous there. `noactlr` on the command
                 * line suppresses the read, for silicon that turns it into an
                 * undefined instruction. */
        }
        part psci : PsciCpuOn {
            doc /* Conduit from the device tree: hvc on QEMU, not the smc the
                 * plan first guessed. Secondaries found in /cpus. Entry above
                 * the 2 GiB TTBR0 covers is refused. */
        }

        part boardDeferred : Ed1Ev1Boards {
            @deferred { reason = "The ED1 and EV1 have 1 GiB, whose identity range lands on the direct map; Layout::plan_identity_map refuses rather than guesses, so they need a trampoline page not yet written."; }
        }
        part highRam : RamAbove2GiB {
            @deferred { reason = "The board has RAM above 2 GiB physical and beyond the 1.25 GiB direct map; QEMU cannot place it."; }
        }
        part thumb2 : Thumb2 {
            @deferred { reason = "ARM code generation only, as docs/arm32.md argues."; }
        }
        part vfp : Vfp {
            @deferred { reason = "Soft float; a UEFI application may not assume firmware enabled the VFP."; }
        }
    }

    part def Armv7aVectorTable; part def Gicv2FromFdt; part def GenericTimerCp15;
    part def ConsoleChoice; part def ActlrReport; part def Ed1Ev1Boards;
    part def RamAbove2GiB; part def Thumb2; part def Vfp;

    variation part def ArchLayer :> ArchFacade {
        doc /* Exactly one is compiled in, chosen by the build target. */
        variant part x86 : X86_64Arch;
        variant part arm64 : AArch64Arch;
        variant part arm32 : Armv7aArch;
    }

    #implemented part def SharedArmDrivers {
        doc /* Register-level drivers under arch/ shared by the two Arm
             * architectures, gated by cfg(any(aarch64, arm)) inside the arch
             * directory where the layering check permits it. */
        part gicv2 : Gicv2 {
            doc /* kernel/src/arch/gicv2.rs: distributor (machine-wide) and CPU
                 * interface (per core). 0..16 software-generated, 16..32
                 * private peripheral, 32.. shared. IPIs through GICD_SGIR.
                 * Private interrupts' enable bits are banked per core, so the
                 * driver records what the boot core enabled and init_this_cpu
                 * replays the whole set on every other core — the timer
                 * included, which is what stage 5 found missing. */
        }
        part pl011 : Pl011 {
            doc /* kernel/src/arch/pl011.rs. Used by ARMv7-A today. */
        }
        part stm32Usart : Stm32Usart {
            doc /* kernel/src/arch/stm32_usart.rs: the board's own UART. */
        }
    }
    part def Gicv2; part def Pl011; part def Stm32Usart;

    // Configurations, one per CI boot.
    part ferrixX86 : Ferrix {
        part :>> kernel { part :>> arch = ArchLayer::x86; }
    }
    part ferrixAArch64 : Ferrix {
        part :>> kernel { part :>> arch = ArchLayer::arm64; }
    }
    part ferrixArmv7a : Ferrix {
        part :>> kernel { part :>> arch = ArchLayer::arm32; }
    }

    // -----------------------------------------------------------------------
    // The workspace: crates and the layering between them
    // -----------------------------------------------------------------------

    package Workspace {
        doc /* docs/ARCHITECTURE.md §9. libs/ is host-testable by design and
             * is the only code cargo test, Miri and the fuzzers can reach;
             * boot/ and kernel/ are reached by the QEMU boot test; xtask/ by
             * cargo test. scripts/check-crate-layering.sh keeps the arrows
             * below pointing the right way. */

        enum def CrateKind {
            Library;
            Loader;
            Kernel;
            HostTool;
            FuzzTargets;
        }

        part def Crate {
            attribute path : String;
            attribute kind : CrateKind;
            attribute hostTestable : Boolean;
            attribute forbidsUnsafe : Boolean;
            attribute hostTests : Natural[0..1];
        }

        part def LibraryCrate :> Crate {
            attribute :>> kind = CrateKind::Library;
            attribute :>> hostTestable = true;
        }

        // The five the boot path was built on.
        #implemented part bootinfo : LibraryCrate {
            attribute :>> path = "libs/bootinfo";
            attribute :>> forbidsUnsafe = false;
            doc /* The hand-off ABI, both address layouts, checked at compile
                 * time on every build. */
        }
        #implemented part elf : LibraryCrate {
            attribute :>> path = "libs/elf";
            attribute :>> forbidsUnsafe = true;
            doc /* ELF64 and, since the ARMv7-A port, ELF32. Used by the
                 * loader, by xtask's PE converter, and fuzzed. */
        }
        #implemented part frameCrate : LibraryCrate {
            attribute :>> path = "libs/frame";
            attribute :>> forbidsUnsafe = true;
        }
        #implemented part heap : LibraryCrate {
            attribute :>> path = "libs/heap";
            attribute :>> forbidsUnsafe = false;
            doc /* The body has no unsafe; the crate cannot forbid it because
                 * declaring Backing as an unsafe trait is the point. */
        }
        #implemented part paging : LibraryCrate {
            attribute :>> path = "libs/paging";
            attribute :>> forbidsUnsafe = false;
        }

        // Written ahead, and reached early.
        #implemented part acpi : LibraryCrate {
            attribute :>> path = "libs/acpi";
            attribute :>> forbidsUnsafe = true;
            attribute :>> hostTests = 58;
            doc /* Reached at stage 3: the MADT walk the interrupt controller
                 * needed. Stage 10 uses the rest. */
        }
        #implemented part fdt : LibraryCrate {
            attribute :>> path = "libs/fdt";
            attribute :>> forbidsUnsafe = true;
            attribute :>> hostTests = 61;
            doc /* Reached at stage 1 on ARMv7-A: console, GIC, timer
                 * interrupt and PSCI conduit come from it there. */
        }
        #implemented part sync : LibraryCrate {
            attribute :>> path = "libs/sync";
            attribute :>> forbidsUnsafe = false;
            attribute :>> hostTests = 19;
            doc /* Reached at stage 4: SpinLock and IrqSpinLock guard every
                 * shared kernel structure. RwSpinLock still waits. */
        }
        #implemented part sched : LibraryCrate {
            attribute :>> path = "libs/sched";
            attribute :>> forbidsUnsafe = true;
            attribute :>> hostTests = 34;
            @stage { number = 5; }
            doc /* The half of the scheduler that is arithmetic: the EEVDF
                 * tree, weights, lag, the domain partition. cargo test drives
                 * a run queue through hundreds of thousands of decisions. */
        }
        #implemented part vma : LibraryCrate {
            attribute :>> path = "libs/vma";
            attribute :>> forbidsUnsafe = true;
            attribute :>> hostTests = 65;
            doc /* Written for stage 6; reached at stage 2 as the vmap arena's
                 * range map. */
        }

        // Written ahead, not yet reached.
        #writtenAhead part linuxAbi : LibraryCrate {
            attribute :>> path = "libs/linux-abi";
            attribute :>> forbidsUnsafe = true;
            attribute :>> hostTests = 52;
            @stage { number = 7; }
            doc /* Three system call number tables, not two: x86-64's own,
                 * AArch64's generic one, and ARMv7-A's EABI one. The 32-bit
                 * architecture needs its own Syscall variants as well as its
                 * own numbers -- mmap2 counts its offset in pages, _llseek
                 * returns through a pointer, fstat64 writes a different
                 * structure -- so they are separate calls rather than the
                 * same calls renumbered. */
        }
        #writtenAhead part ustack : LibraryCrate {
            attribute :>> path = "libs/ustack";
            attribute :>> forbidsUnsafe = true;
            attribute :>> hostTests = 22;
            @stage { number = 7; }
            doc /* The initial process stack image: argc, argv, envp and the
                 * auxiliary vector, laid out as a program's _start reads them,
                 * at both pointer widths. Carries a reader as well as a
                 * writer, because the writer is checked by walking the bytes
                 * back out with nothing but a stack pointer -- which is also
                 * what the fuzz target does. */
        }
        #writtenAhead part cpio : LibraryCrate {
            attribute :>> path = "libs/cpio";
            attribute :>> forbidsUnsafe = true;
            attribute :>> hostTests = 45;
            @stage { number = 8; }
        }
        #writtenAhead part virtio : LibraryCrate {
            attribute :>> path = "libs/virtio";
            attribute :>> forbidsUnsafe = false;
            attribute :>> hostTests = 50;
            @stage { number = 10; }
        }
        #writtenAhead part nativeAbi : LibraryCrate {
            attribute :>> path = "libs/native-abi";
            attribute :>> forbidsUnsafe = true;
            attribute :>> hostTests = 13;
            @stage { number = 9; }
            doc /* The native ABI's numbers, handle values, rights, signals,
                 * errno names and repr(C) layouts. One table on every
                 * architecture, in 0x1000..=0x1FFF, tested clear of all three
                 * Linux tables. No argument wider than a register. */
        }
        #writtenAhead part objects : LibraryCrate {
            attribute :>> path = "libs/objects";
            attribute :>> forbidsUnsafe = true;
            attribute :>> hostTests = 15;
            @stage { number = 9; }
            doc /* The handle table and the channel message queue, generic
                 * over what a handle names. A handle is a slot and a
                 * generation, and a slot retires rather than wrap, so a closed
                 * handle never resolves again -- which the handle_table fuzz
                 * target checks against a model. */
        }
        #writtenAhead part btrfs : LibraryCrate {
            attribute :>> path = "libs/btrfs";
            attribute :>> forbidsUnsafe = true;
            attribute :>> hostTests = 38;
            @stage { number = 11; }
        }

        // Owed by later stages.
        #planned part seccompBpf : LibraryCrate {
            attribute :>> path = "libs/seccomp";
            attribute :>> forbidsUnsafe = true;
            @stage { number = 13; }
            doc /* The classic-BPF interpreter as a pure function over bytes. */
        }

        #implemented part bootCrate : Crate {
            attribute :>> path = "boot";
            attribute :>> kind = CrateKind::Loader;
            attribute :>> hostTestable = false;
            attribute :>> forbidsUnsafe = false;
        }
        #implemented part kernelCrate : Crate {
            attribute :>> path = "kernel";
            attribute :>> kind = CrateKind::Kernel;
            attribute :>> hostTestable = false;
            attribute :>> forbidsUnsafe = false;
        }
        #implemented part xtask : Crate {
            attribute :>> path = "xtask";
            attribute :>> kind = CrateKind::HostTool;
            attribute :>> hostTestable = true;
            attribute :>> forbidsUnsafe = true;
            attribute :>> hostTests = 41;
            doc /* Cross-compiles both halves, writes the FAT32 image with its
                 * own writer, converts the ARMv7-A loader ELF to PE32, drives
                 * QEMU. Nothing from crates.io. */
            attribute commands : String[7] = ("build", "run", "test-boot", "check", "flash", "watch-serial", "deploy");
        }
        #implemented part fuzz : Crate {
            attribute :>> path = "fuzz";
            attribute :>> kind = CrateKind::FuzzTargets;
            attribute :>> hostTestable = true;
            attribute :>> forbidsUnsafe = false;
        }

        // Dependency edges, as Cargo.toml has them.
        dependency from kernelCrate to acpi, bootinfo, fdt, frameCrate, heap, paging, sched, sync, vma;
        dependency from bootCrate to bootinfo, elf, paging;
        dependency from xtask to elf;
        dependency from fuzz to elf, frameCrate;

        // Build targets.
        attribute kernelTargets : String[3] = ("x86_64-unknown-none", "aarch64-unknown-none-softfloat", "armv7a-none-eabi");
        attribute loaderTargets : String[3] = ("x86_64-unknown-uefi", "aarch64-unknown-uefi", "armv7-unknown-linux-musleabi");
        attribute toolchain : String = "1.97.1";
    }
}
