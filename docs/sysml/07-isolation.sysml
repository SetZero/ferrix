/*
 * Ferrix — SysML v2 model, part 7: isolation.
 *
 * docs/ARCHITECTURE.md §6: namespaces, cgroups v2, seccomp, credentials.
 * Stage 13, designed in from the start so that no global table has to be
 * found later.
 */
package FerrixIsolation {
    private import ScalarValues::*;
    private import FerrixLifecycle::*;

    enum def NamespaceKind {
        Pid; Mount; Uts; Ipc; Net; User; Cgroup; Time;
    }

    #planned part def Namespace {
        @stage { number = 13; }
        attribute kind : NamespaceKind;
        part parent : Namespace[0..1];
    }

    #planned part def NsSet {
        @stage { number = 13; }
        doc /* One namespace of each kind, held by every task. Every table
             * that would otherwise be global — pids, mounts, hostname, ... —
             * is reached through this, from the first line. */
        ref part namespaces : Namespace[8];
    }

    #planned part def Namespaces {
        @stage { number = 13; }
        part every : Namespace[8..*];
        action unshare;
        action setns;
    }

    enum def CgroupController {
        Cpu;      // weight and bandwidth handed to the scheduling classes
        Memory;   // where reclaim and the OOM killer are scoped
        Io;       // per-cgroup bandwidth in the I/O scheduler
        Pids;
    }

    #planned part def Cgroup {
        @stage { number = 13; }
        attribute controllers : CgroupController[0..*];
        part children : Cgroup[0..*];
        attribute memoryLimit : Natural[0..1];
        attribute cpuWeight : Natural[0..1];
        attribute pidsMax : Natural[0..1];
    }

    #planned part def Cgroups {
        @stage { number = 13; }
        doc /* One unified hierarchy (v2), exposed as cgroupfs. cpu is not a
             * separate mechanism: it is bandwidth and weight handed to the
             * scheduling classes. memory scopes reclaim and the OOM kill. */
        part root : Cgroup;
    }

    #planned part def Seccomp {
        @stage { number = 13; }
        doc /* Classic BPF filters evaluated on syscall entry. The interpreter
             * is a pure function over bytes in libs/, fuzzable and Miri-able. */
        part interpreter : ClassicBpfInterpreter;
        action installFilter;
        action evaluate;
    }

    #planned part def ClassicBpfInterpreter {
        @stage { number = 13; }
        doc /* libs/seccomp, owed before stage 13 starts. */
    }

    attribute def Credentials {
        doc /* Unix: uid, gid, supplementary groups, POSIX capability sets,
             * no-new-privs. They sit on top of the handle system rather than
             * beside it. */
        attribute uid : Natural;
        attribute gid : Natural;
        attribute groups : Natural[0..*];
        attribute capabilities : String;
        attribute noNewPrivs : Boolean;
    }
}
