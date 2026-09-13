/*
 * Ferrix — SysML v2 model, part 10: the roadmap.
 *
 * docs/ROADMAP.md as requirements: one per stage, each with its exit
 * criterion, its status, the boot test that verifies it, and the part of
 * the system that satisfies or will satisfy it. Stages 0 to 5 and the
 * ARMv7-A port are done; stage 6 is in progress.
 */
package FerrixRoadmap {
    private import ScalarValues::*;
    private import VerificationCases::*;
    private import FerrixLifecycle::*;
    private import FerrixStructure::*;
    private import FerrixRequirements::*;

    requirement def Stage {
        doc /* Two rules govern the ordering: every stage ends in something
             * that runs, and nothing is stubbed that a later stage has to
             * unpick. Sizes are order-of-magnitude and not a schedule. */
        attribute number : Natural;
        attribute status : StageStatus;
        attribute size : String;
        subject os : Ferrix;
        requirement exitCriterion {
            doc /* How the stage knows it is finished: a QEMU boot that
                 * demonstrates the capability and stays in CI. */
        }
    }

    verification def BootTest {
        doc /* cargo xtask test-boot --arch <a>: boot firmware, loader and
             * kernel under QEMU and require FERRIX-BOOT-OK within the
             * timeout. Every stage's exit criterion is a self-check in kmain
             * and a line in this log. Under tcg by default; --accel auto uses
             * the host's MMU and TLB, which is the only way a missing
             * invalidation is reachable. */
        subject os : Ferrix;
        attribute arch : IsaFamily;
        attribute marker : String = "FERRIX-BOOT-OK stages 1-9";
    }

    // -----------------------------------------------------------------------
    // Done
    // -----------------------------------------------------------------------

    #implemented requirement <'S0'> stage0Foundation : Stage {
        attribute :>> number = 0;
        attribute :>> status = StageStatus::Done;
        attribute :>> size = "weekend";
        doc /* Workspace, the quality gates ported from Starling, CI, the first
             * host-testable libraries. Exit: cargo xtask check passes on an
             * empty tree. */
    }

    #implemented requirement <'S1'> stage1Boot : Stage {
        attribute :>> number = 1;
        attribute :>> status = StageStatus::Done;
        attribute :>> size = "week";
        doc /* UEFI loader in Rust to a kernel Rust entry point, zero
             * bootstrap assembly. Exit, met on all three: the hand-off's
             * magic, version and layout agree; the memory map is sorted,
             * non-overlapping, has usable RAM and describes the loader's own
             * allocations; the direct map aliases physical memory; the kernel
             * can walk and extend the loader's tables. */
    }

    #implemented requirement <'S2'> stage2Memory : Stage {
        attribute :>> number = 2;
        attribute :>> status = StageStatus::Done;
        attribute :>> size = "week";
        doc /* Buddy allocator, heap, vmap arena with guard pages and stacks,
             * identity map dropped, W^X sweep, boot memory reclaimed, empty
             * slab pages returned. Exit, met: 4096 blocks allocated and freed
             * with the free count returning exactly; Box, Vec, BTreeMap;
             * arena and stack checks; address zero translates to nothing; the
             * sweep finds no writable-and-executable leaf. */
        requirement perCpuCaches {
            @deferred { reason = "Needs a workload that can measure them; the per-CPU area exists since stage 4."; }
        }
    }

    #implemented requirement <'S3'> stage3TrapsInterruptsTime : Stage {
        attribute :>> number = 3;
        attribute :>> status = StageStatus::Done;
        attribute :>> size = "week";
        doc /* Vectors on every architecture, one dispatch above them; LAPIC,
             * I/O APIC, HPET (TSC/PIT fallback); GICv2 and the architected
             * virtual timer; the facade's irq::register, timer::after and
             * trap::Frame. Exit, met: two breakpoints with a register canary,
             * four page faults with an exact frame bound, a one-shot that
             * fires once, a thousand ticks measured against the counter at
             * 998 to 999 Hz for a requested 1000. */
        requirement gicv3 {
            @deferred { reason = "Refused rather than guessed; needs a second boot-test configuration."; }
        }
        requirement tscDeadline {
            @deferred { reason = "Calibration exists; waits for a tickless scheduler."; }
        }
    }

    #implemented requirement <'S4'> stage4Smp : Stage {
        attribute :>> number = 4;
        attribute :>> status = StageStatus::Done;
        attribute :>> size = "week";
        doc /* Every processor online: INIT-SIPI-SIPI and a real-mode
             * trampoline, PSCI CPU_ON through an identity map of the entry;
             * a record per processor; every "one CPU" global now a lock; IPIs;
             * TLB shootdown where hardware does not broadcast; grace periods.
             * Exit, met: four processors increment one counter under one
             * ticket lock 25,000 times each to exactly 100,000 with their
             * shares overlapping in time, while an unlocked count beside it
             * loses updates. */
        requirement x2apic { @deferred { reason = "APIC IDs above 255 refused; QEMU's are 0 to 3."; } }
        requirement psciParking { @deferred { reason = "Refused, not guessed at."; } }
        requirement cpuOffline { @deferred { reason = "Nothing takes a processor offline."; } }
    }

    #implemented requirement <'SA'> armv7aPort : Stage {
        attribute :>> number = 4;
        attribute :>> status = StageStatus::Done;
        attribute :>> size = "month";
        doc /* The third architecture, joined after stage 3 and brought through
             * stage 4: the same loader converted ELF to PE32, a 32-bit layout
             * argued rather than shrunk, LPAE as a paging geometry, device
             * tree only, traps without mode stacks. Exit, met: the same
             * self-checks and the same marker under U-Boot on QEMU virt. */
        requirement ramAbove2GiB { @deferred { reason = "The board has it; QEMU cannot place it."; } }
        requirement boardUart { @deferred { reason = "The STM32 USART driver and the flash/watch-serial/deploy path exist; nothing has run on hardware yet."; } }
        requirement ed1Ev1Boards { @deferred { reason = "1 GiB boards put RAM's identity range on the direct map; plan_identity_map refuses, so they need a trampoline page."; } }
        requirement hardwareBootTest { @deferred { reason = "Automated hardware boot testing beyond watch-serial."; } }
    }

    // -----------------------------------------------------------------------
    // Ahead
    // -----------------------------------------------------------------------

    #implemented requirement <'S5'> stage5Scheduler : Stage {
        attribute :>> number = 5;
        attribute :>> status = StageStatus::Done;
        attribute :>> size = "week";
        doc /* Task, kernel stacks, context switch, per-CPU runqueues, the
             * class stack, EEVDF in libs/sched. Scheduling domains from the
             * start with Throughput alone implemented. Exit, met on all three:
             * a thousand kernel threads spawned on one processor run bounded
             * work to completion and give every stack back (about 28,000
             * switches and 1,000 steals when it landed); then twelve spinners
             * at two weights, each required to stay within EEVDF's own bound
             * of its weighted share, the bound printed beside the lag. Three
             * bugs it found: an idle processor never told of work, vmap::free
             * releasing the address before unmapping, and banked private
             * interrupt enables leaving every other core's timer off. */
        requirement loadBalancing { @deferred { reason = "Nothing beyond work stealing on an idle processor."; } }
    }

    #implemented requirement <'S6'> stage6UserMode : Stage {
        attribute :>> number = 6;
        attribute :>> status = StageStatus::Done;
        attribute :>> size = "week";
        doc /* AddressSpace, VMOs, the VMA tree as a process map, demand
             * paging, copy-on-write, the ELF loader, the ring-3/EL0/USR
             * transition. Exit, met on all three: a program at user privilege
             * writes to fd 1 and exits with 42, with a page fault serviced
             * along the way -- counted either side of the program and required
             * to be non-zero. Down to user mode by sysretq on x86-64, eret to
             * EL0 on AArch64 and rfeia to USR on ARMv7-A, each onto a dedicated
             * entry stack. Also landed: fork with copy-on-write, TLB
             * invalidation where a live mapping changes, tasks carrying an
             * address space with the root swapped in choose_next, and a read of
             * an inaccessible region refused. Left: scoping the user TLB
             * shootdown, the missing invalidation in protect, and reading the
             * console on Arm. */
    }

    #implemented requirement <'S7'> stage7LinuxAbi : Stage {
        attribute :>> number = 7;
        attribute :>> status = StageStatus::Done;
        attribute :>> size = "month";
        doc /* Syscall entry on every architecture, the dispatch table, the
             * core surface: memory, files, process, threads and futex,
             * signals with sigaltstack and rt_sigreturn, time, identity.
             * Exit, met on all three with the script given to sh -c: Alpine's
             * static musl busybox runs a builtins-only script and exits with
             * its status, required line by line by cargo xtask test-shell,
             * which is outside the boot test because it needs a binary the
             * repository does not carry.
             *
             * Built: the three number tables in libs/linux-abi, the startup
             * stack in libs/ustack, dispatch in kernel/src/syscall, the copy
             * layer, the ELF loader, and the calls a static binary makes --
             * mmap, mmap2, munmap, mprotect, brk, set_tid_address, read and
             * write/writev on the console, the clocks, getrandom, uname, the
             * identity calls, and rt_sigaction, rt_sigprocmask and
             * sigaltstack recorded without delivery; exit_group, arch_prctl
             * and set_tls in each architecture's trap path. Running foreign
             * binaries found a Thumb entry point entered in ARM state and the
             * FPU closed to user mode on both Arm kernels. Since the exit:
             * programs are scheduled tasks, preempted in user mode, with their
             * thread pointer and FPU state switched per task, load/start/kill
             * on Process, and two programs taking turns in the boot test;
             * then fork, vfork and clone without threads, execve, wait4 and
             * waitid, and the process-group and session calls, with a forking
             * and an exec'ing program in the boot test; futex waits, wakes and
             * requeues, and clone3 through the same path as clone;
             * descriptors closed when a process ends; and the edge calls
             * busybox makes -- prctl, limits, priorities, credentials, sleeps,
             * clock setting, host names, sysinfo, syslog, reboot, and
             * sockets refused; mremap, execveat, unshare and setns; and
             * /proc/self/exe as the resolved file actually loaded.
             *
             * Signals delivered on every return to user mode, on Linux's
             * frames, with rt_sigreturn, EINTR, stop and continue, SIGPIPE,
             * SIGALRM and faults as signals.
             *
             * The console a terminal: termios honoured by a line discipline,
             * the terminal and job-control requests, select and pselect6, and
             * Ctrl-C raised on the foreground group.
             *
             * Left: SA_RESTART, threads, and the stand-ins for a tty, a clock
             * chip and an entropy source. */
    }

    #implemented requirement <'S8'> stage8Vfs : Stage {
        attribute :>> number = 8;
        attribute :>> status = StageStatus::Done;
        attribute :>> size = "month";
        doc /* Inode and dentry caches, the mount table, fd sharing rules,
             * tmpfs, devfs, procfs, cpio initramfs. Exit: busybox ls -R /proc,
             * cat /proc/self/maps, a script manipulating files under tmpfs. */
    }

    #implemented requirement <'S9'> stage9NativeAbi : Stage {
        attribute :>> number = 9;
        attribute :>> status = StageStatus::Done;
        attribute :>> size = "week";
        doc /* Handle tables, Channel with handle passing, Port, Interrupt,
             * IoMapping, Job; the 0x1000 syscalls. Exit: two processes
             * exchange messages and a handle over a channel, and a Job kill
             * takes down a process tree.
             *
             * Built: the byte-level half, in libs/native-abi and libs/objects,
             * host-tested, fuzzed and under Miri; and in kernel/src/object and
             * syscall/native, handle tables on every Process, channels carrying
             * handles, VMOs, signals and object_wait_one, Job, ports with
             * object_wait_async, and IoMapping and Interrupt minted from stage
             * 10's device nodes, with interrupts delivered to ports. The exit
             * criterion runs in the boot test: two user-mode programs exchange
             * a message and a VMO handle over a channel, and a job kill ends
             * every program in the job and beneath it. Left for later: an
             * interrupt-safe wake, vmo_map, sub-page apertures, and process
             * creation in the native ABI. Vmo::hold, which keeps a page a DMA pin
             * holds on its frame, is in. */
    }

    #inProgress requirement <'S10'> stage10UserspaceDrivers : Stage {
        attribute :>> number = 10;
        attribute :>> status = StageStatus::InProgress;
        attribute :>> size = "month";
        doc /* Enumeration, IOMMU domains, devmgr, the shared-ring block
             * protocol, virtio-blk as a user process. Exit: a sector read
             * through a ring-3 driver with the IOMMU on, and a deliberate
             * out-of-domain DMA attempt faulting.
             *
             * Built: PCI configuration space in libs/pci, host-tested, fuzzed
             * and under Miri; kernel enumeration from the MCFG and the device
             * tree, in the boot test on all three architectures against a
             * virtio-rng-pci device; device nodes whose apertures and vectors
             * are tokens only device.rs mints; a virtio-rng device driven by DMA
             * from the boot check, 64 bytes on every architecture; MSI-X tables and pending bits withheld
             * from apertures; the DMAR and IORT parsed; the ten
             * defects a review found fixed, apertures screened against
             * everything the kernel owns and each other; the
             * virtio-rng completion delivered by MSI-X on every architecture,
             * through arch::msi_allocate (local APIC vectors, GICv2m SPIs);
             * PCI vectors minted per MSI-X entry and masked at the entry,
             * which stage 9's Interrupt uses; every IOMMU found and each PCI
             * function placed behind one (DMAR, IORT, device tree), virtio's
             * DMA sent through it on x86-64 and AArch64, and VT-d and stage-2
             * table encodings in libs/paging; an iommu::Domain every device
             * node has, which the entropy check's DMA goes through; VT-d
             * programmed at boot on x86-64 and the SMMUv3 under ACPI on
             * AArch64, with translated domains. VMO_PIN pinning VMO pages into
             * a device's domain. The block ring's protocol as a host-tested
             * library, ferrix-blkring, used by nothing yet. Next, in the order
             * stage 11 needs: devmgr, the ring's kernel side and virtio-blk in
             * ring 3, the ring-3 driver reading sectors on x86-64 and AArch64
             * (ARMv7-A in degraded trusted mode); trusting decoding-off BARs
             * last. */
    }

    #inProgress requirement <'S11'> stage11BtrfsRead : Stage {
        attribute :>> number = 11;
        attribute :>> status = StageStatus::InProgress;
        attribute :>> size = "month";
        doc /* Block core, then btrfs stage A. Exit: an image made by real
             * mkfs.btrfs is mounted and a file tree read out byte-for-byte
             * matching what the host wrote.
             *
             * Built, host-side: the btrfs read path in libs/btrfs (mount
             * bootstrap, lookup, readdir, read, zlib/LZO/zstd), reading four
             * real mkfs.btrfs images back exactly; the mount over stage 8's
             * traits in libs/btrfs-vfs; the block queue in libs/block. Next:
             * the kernel mount, which needs stage 10's block driver. */
    }

    #planned requirement <'SN'> stageNetworking : Stage {
        attribute :>> number = 11;
        attribute :>> status = StageStatus::Planned;
        attribute :>> size = "month";
        doc /* Placed after stage 11 without a number of its own, as the
             * ARMv7-A port sits after stage 4. The net core: AF_UNIX, AF_INET
             * and AF_INET6 sockets, the AF_NETLINK route family, interfaces,
             * routes and loopback; virtio-net as a userspace driver on stage
             * 10's device objects; /proc/net; parsers and the TCP state
             * machine in libs/, fuzzed. Exit: under QEMU user networking,
             * busybox configures eth0 with ip, route and netstat report
             * through /proc/net, wget fetches a file byte-for-byte, and nc
             * carries a stream over loopback and an AF_UNIX socket. */
    }

    #planned requirement <'S12'> stage12BtrfsWrite : Stage {
        attribute :>> number = 12;
        attribute :>> status = StageStatus::Planned;
        attribute :>> size = "longer";
        doc /* btrfs stage B. Exit, strict: Ferrix writes a tree and host
             * btrfs check finds nothing; then the power-fail test — kill QEMU
             * at a random point inside a transaction, remount, replay, check
             * again — over hundreds of seeds. */
    }

    #planned requirement <'S13'> stage13Isolation : Stage {
        attribute :>> number = 13;
        attribute :>> status = StageStatus::Planned;
        attribute :>> size = "month";
        doc /* All eight namespaces, the unified cgroup hierarchy with cpu,
             * memory, io and pids, cgroupfs, classic-BPF seccomp with the
             * interpreter in libs/. Exit: an unprivileged user namespace runs
             * pid 1 under a memory limit that triggers scoped reclaim and a
             * scoped OOM kill, with a seccomp filter blocking a syscall. */
    }

    #planned requirement <'S14'> stage14RealTime : Stage {
        attribute :>> number = 14;
        attribute :>> status = StageStatus::Planned;
        attribute :>> size = "month";
        doc /* SoftRt and HardRt: FIFO/RR, threaded interrupts, PI mutexes,
             * EDF with CBS admission, the runtime mode switch with its
             * quiescence protocol. Exit: a cyclictest-shaped boot test on a
             * HardRt domain while a Throughput domain is saturated, maximum
             * wake-up latency inside the stated bound; plus an admission test
             * that refuses an unschedulable set. */
    }

    #planned requirement <'S15'> stage15Userland : Stage {
        attribute :>> number = 15;
        attribute :>> status = StageStatus::Planned;
        attribute :>> size = "week";
        doc /* Static musl busybox as /bin, a working init, job control, ttys,
             * pipes. Exit: an interactive shell over serial a person can use. */
    }

    #planned requirement <'S16'> stage16Rustc : Stage {
        attribute :>> number = 16;
        attribute :>> status = StageStatus::Planned;
        attribute :>> size = "the goal";
        doc /* The remaining syscall surface, the memory scale, the spawn path
             * for rust-lld, a sysroot on btrfs. Exit: rustc hello.rs &&
             * ./hello on Ferrix, in CI. */
    }

    #planned requirement <'S17'> stage17SelfHosting : Stage {
        attribute :>> number = 17;
        attribute :>> status = StageStatus::Planned;
        attribute :>> size = "longer";
        doc /* Build Ferrix on Ferrix; the image the hosted compiler produces
             * boots and passes every test above. */
    }

    // -----------------------------------------------------------------------
    // Ordering and the debts the roadmap states
    // -----------------------------------------------------------------------

    dependency from stage1Boot to stage0Foundation;
    dependency from stage2Memory to stage1Boot;
    dependency from stage3TrapsInterruptsTime to stage2Memory;
    dependency from stage4Smp to stage3TrapsInterruptsTime;
    dependency from stage5Scheduler to stage4Smp;
    dependency from stage6UserMode to stage5Scheduler, stage3TrapsInterruptsTime;
    dependency from stage7LinuxAbi to stage6UserMode;
    dependency from stage8Vfs to stage7LinuxAbi;
    dependency from stage9NativeAbi to stage8Vfs;
    dependency from stage10UserspaceDrivers to stage9NativeAbi;
    dependency from stage11BtrfsRead to stage10UserspaceDrivers;
    dependency from stageNetworking to stage11BtrfsRead, stage10UserspaceDrivers;
    dependency from stage12BtrfsWrite to stage11BtrfsRead;
    dependency from stage13Isolation to stage12BtrfsWrite;
    dependency from stage14RealTime to stage13Isolation, stage4Smp;
    dependency from stage15Userland to stage14RealTime;
    dependency from stage16Rustc to stage15Userland;
    dependency from stage17SelfHosting to stage16Rustc;

    #planned requirement <'D.fuzz'> fuzzTargetsOwed : Principles::DesignRule {
        doc /* fuzz/ has two targets, elf_parse and frame_alloc. Every
             * written-ahead crate parses bytes from outside the system and is
             * owed a target before its consuming stage starts. */
    }
    #planned requirement <'D.miri'> miriOwed : Principles::DesignRule {
        doc /* CI runs Miri over libs/elf and libs/bootinfo only; frame, heap
             * and paging run in the kernel today and are owed a step ahead of
             * every crate in the written-ahead table. */
    }

    // -----------------------------------------------------------------------
    // What satisfies what
    // -----------------------------------------------------------------------

    satisfy stage1Boot by ferrix.loader;
    satisfy stage2Memory by ferrix.kernel.mm;
    satisfy stage2Memory by ferrix.kernel.vmap;
    satisfy stage3TrapsInterruptsTime by ferrix.kernel.trap;
    satisfy stage3TrapsInterruptsTime by ferrix.kernel.irq;
    satisfy stage3TrapsInterruptsTime by ferrix.kernel.timer;
    satisfy stage4Smp by ferrix.kernel.smp;
    satisfy armv7aPort by ferrixArmv7a;
    satisfy stage5Scheduler by ferrix.kernel.sched;
    satisfy stage5Scheduler by ferrix.kernel.tasks;

    allocate stage6UserMode to ferrix.kernel.vm;
    allocate stage7LinuxAbi to ferrix.kernel.syscalls;
    allocate stage7LinuxAbi to ferrix.kernel.signals;
    allocate stage7LinuxAbi to ferrix.kernel.futex;
    allocate stage8Vfs to ferrix.kernel.vfs;
    allocate stage8Vfs to ferrix.kernel.filesystems;
    allocate stage9NativeAbi to ferrix.kernel.native;
    allocate stage10UserspaceDrivers to ferrix.kernel.devices;
    allocate stage10UserspaceDrivers to ferrix.kernel.iommu;
    allocate stage11BtrfsRead to ferrix.kernel.blockCore;
    allocate stageNetworking to ferrix.kernel.netCore;
    allocate stage13Isolation to ferrix.kernel.namespaces;
    allocate stage13Isolation to ferrix.kernel.cgroups;
    allocate stage13Isolation to ferrix.kernel.seccomp;
    allocate stage14RealTime to ferrix.kernel.sched;
    allocate stage15Userland to ferrix.userland;
    allocate stage16Rustc to ferrix.userland.rustc;

    // Which goal requirements each stage discharges.
    dependency from stage5Scheduler to hostsRustc::kernelThreads;
    dependency from stage6UserMode to hostsRustc::addressSpaceScale;
    dependency from stage7LinuxAbi to hostsRustc::signalDelivery, hostsRustc::processSpawn;
    dependency from stage8Vfs to hostsRustc::syscallSurface, hostsRustc::procfs;
    dependency from stage12BtrfsWrite to hostsRustc::durableFilesystem;
    dependency from stage13Isolation to hostsRustc::memoryPressure;

    // -----------------------------------------------------------------------
    // The boot tests that verify the done stages, one per architecture
    // -----------------------------------------------------------------------

    verification bootX86 : BootTest {
        attribute :>> arch = IsaFamily::X86_64;
        objective {
            verify stage1Boot;
            verify stage2Memory;
            verify stage3TrapsInterruptsTime;
            verify stage4Smp;
            verify stage5Scheduler;
        }
    }
    verification bootAArch64 : BootTest {
        attribute :>> arch = IsaFamily::AArch64;
        objective {
            verify stage1Boot;
            verify stage2Memory;
            verify stage3TrapsInterruptsTime;
            verify stage4Smp;
            verify stage5Scheduler;
        }
    }
    verification bootArmv7a : BootTest {
        attribute :>> arch = IsaFamily::Armv7a;
        objective {
            verify stage1Boot;
            verify stage2Memory;
            verify stage3TrapsInterruptsTime;
            verify stage4Smp;
            verify stage5Scheduler;
            verify armv7aPort;
        }
    }

    verification def BoardBoot {
        doc /* cargo xtask deploy --arch armv7a: flash two files onto the
             * card's bootfs, watch the ST-LINK serial port for the marker.
             * Manual today; the automated form is deferred above. */
        subject os : Ferrix;
        objective { verify armv7aPort; }
    }

    #planned verification def RustcTest {
        doc /* The acceptance test itself: rustc hello.rs && ./hello inside
             * the boot test. */
        subject os : Ferrix;
        objective { verify hostsRustc; }
    }
}
