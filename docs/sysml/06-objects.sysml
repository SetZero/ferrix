/*
 * Ferrix — SysML v2 model, part 6: kernel objects and the two ABIs.
 *
 * docs/ARCHITECTURE.md §2 and §3. The constants for the Linux half are in
 * libs/linux-abi and for the native half in libs/native-abi; handle tables,
 * channels and VMO handles exist in kernel/src/object, the rest is planned.
 */
package FerrixObjects {
    private import ScalarValues::*;
    private import FerrixLifecycle::*;
    private import FerrixMemory::*;
    private import FerrixScheduling::Task;

    // -----------------------------------------------------------------------
    // The nine kernel objects, stage 9 (Task and Process earlier)
    // -----------------------------------------------------------------------

    abstract #planned part def KernelObject {
        @stage { number = 9; }
        doc /* Typed, reference-counted, reached through per-process handle
             * tables. The native ABI is built on these. */
        attribute refcount : Natural;
    }

    // Vmo and ProcessAddressSpace are defined in FerrixMemory; they are two of
    // the nine.
    #planned part def VmoObject :> KernelObject, Vmo;
    #planned part def AddressSpaceObject :> KernelObject, ProcessAddressSpace;

    #implemented part def Channel :> KernelObject {
        doc /* Bidirectional datagram pipe carrying bytes and handles. The
             * basis of driver IPC. */
        action write;
        action read;
    }

    #implemented part def Port :> KernelObject {
        doc /* An event queue a thread waits on; how one driver thread
             * services many sources. */
        action wait;
        action queue;
    }

    #implemented part def Interrupt :> KernelObject {
        doc /* A bindable hardware interrupt. A userspace driver waits on it
             * through a Port. */
        attribute irq : Natural;
        ref part boundPort : Port[0..1];
        action bindToPort;
        action ack;
    }

    #implemented part def IoMapping :> KernelObject {
        doc /* An MMIO aperture, mappable into a driver's address space, with
             * its IOMMU domain. Nothing outside the aperture. */
        attribute phys : Natural;
        attribute len : Natural;
        ref part domain : FerrixDrivers::IommuDomain[0..1];
    }

    #planned part def TaskObject :> KernelObject, Task {
        @stage { number = 5; }
    }

    #planned part def Process :> KernelObject {
        @stage { number = 6; }
        doc /* A group of tasks sharing an address space, fd table, fs context
             * and signal dispositions. Precisely a particular sharing
             * arrangement of independently shareable objects, composed by
             * clone flags exactly as Linux does. */
        part tasks : TaskObject[1..*];
        ref part space : AddressSpaceObject;
        ref part fdTable : FdTable;
        ref part fsContext : FsContext;
        ref part sigHandlers : SignalDispositions;
        ref part namespaces : FerrixIsolation::NsSet;
        part handles : HandleTable;
        attribute credentials : FerrixIsolation::Credentials;
        ref part job : Job;
        attribute pid : Natural {
            doc /* Meaningless without saying in which pid namespace. */
        }
    }

    #implemented part def Job :> KernelObject {
        doc /* A container of processes, where resource limits and kill
             * authority live. A wedged userspace driver has to be killable as
             * a unit together with anything it spawned. */
        part processes : Process[0..*];
        part children : Job[0..*];
        action kill;
    }

    #implemented part def HandleTable {
        @stage { number = 9; }
        attribute handles : Handle[0..*];
    }

    attribute def Handle {
        attribute index : Natural;
        attribute rights : String;
    }

    part def FdTable {
        doc /* File descriptors and their sharing rules, stage 8. */
    }
    part def FsContext { doc /* cwd, root, umask. */ }
    part def SignalDispositions;

    // -----------------------------------------------------------------------
    // Clone flags: what a process shares
    // -----------------------------------------------------------------------

    enum def Shareable {
        AddressSpace;
        FdTable;
        FsContext;
        SignalHandlers;
        NamespaceSet;
        ThreadGroup;
    }

    #planned action def Clone {
        @stage { number = 7; }
        doc /* clone: each Shareable is shared or copied independently.
             * CLONE_THREAD|CLONE_VM|CLONE_SETTLS is a thread; none of them is
             * fork, which marks both address spaces copy-on-write. */
        in share : Shareable[0..*];
        out child : Process;
    }

    // -----------------------------------------------------------------------
    // The Linux ABI, stage 7
    // -----------------------------------------------------------------------

    enum def SyscallGroup {
        Memory;      // mmap, mprotect, munmap, brk, madvise
        Files;       // openat, read, write, pread64, getdents64, statx, renameat2 ...
        Process;     // clone, execve, wait4, exit_group, getpid ...
        Threads;     // set_tid_address, futex, robust lists, sched_*
        Signals;     // rt_sigaction, rt_sigprocmask, sigaltstack, rt_sigreturn
        Time;        // clock_gettime, nanosleep
        Identity;    // getuid, uname ...
        Native;      // 0x1000 and up
    }

    #inProgress part def LinuxSyscallLayer {
        @stage { number = 7; }
        doc /* The entry path on every architecture, the dispatch table, and
             * the ~150-call surface rustc needs. libs/linux-abi holds the
             * numbers for x86-64, AArch64 and the ARM EABI table, the errnos,
             * and the repr(C) layouts (statx, dirent64, sigaction, ...).
             *
             * kernel/src/syscall, reached through arch::decode_syscall, which
             * is the only place in the kernel that knows which of the three
             * number tables this build uses. Answers the calls a static musl
             * binary makes at startup -- memory, the console descriptors,
             * clocks, identity, uname, and signal dispositions -- which is
             * enough for busybox sh to run a script on all three
             * architectures. Everything else is ENOSYS, Linux's own answer. */
        attribute groups : SyscallGroup[1..*];
        #implemented action syscallEntry {
            doc /* The assembly trampoline; then Rust. Stage 6's, and it fills
                 * a SyscallArgs from the trap frame and applies the Outcome
                 * that comes back, so no register convention reaches the
                 * dispatch layer and no ABI knowledge reaches the vector. */
        }
        #implemented action dispatch {
            doc /* One function. Returns Outcome::Return for a value, or
                 * Outcome::Enter for the calls that resume on a frame which
                 * was constructed rather than returned into -- execve, and a
                 * clone child. Self-checked in the boot test on all three
                 * architectures. */
        }
        action seccompCheck {
            @stage { number = 13; }
            doc /* The filter runs on entry, before dispatch. */
        }
    }

    #inProgress part def Signals {
        @stage { number = 7; }
        doc /* The table exists: kernel/src/syscall/signal.rs records each
             * disposition, the blocked mask and the alternate stack, and
             * answers rt_sigaction, rt_sigprocmask and sigaltstack from it in
             * Linux's order. Delivery does not: a frame pushed on the user
             * stack or the sigaltstack, rt_sigreturn to unwind it. SIGSEGV
             * from the fault path is what rustc's stack-overflow guard
             * needs. */
        action deliver;
        action sigreturn;
    }

    #planned part def PosixIpc {
        @stage { number = 15; }
        doc /* Pipes, ttys and job control: what an interactive shell needs. */
    }

    // -----------------------------------------------------------------------
    // The native ABI, stage 9
    // -----------------------------------------------------------------------

    #inProgress part def NativeAbi {
        @stage { number = 9; }
        doc /* Syscall numbers from 0x1000. Handle-table operations, channel
             * send/receive with handle passing, port wait, interrupt bind,
             * VMO create/map, job create/kill. What devmgr and drivers speak;
             * a process may use both ABIs. */
        attribute firstNumber : String = "0x1000";
        action handleClose;
        action handleDuplicate;
        action channelCreate;
        action channelWrite;
        action channelRead;
        action portCreate;
        action portWait;
        action interruptBind;
        action vmoCreate;
        action vmoMap;
        action ioMappingMap;
        action jobCreate;
        action jobKill;
    }
}
