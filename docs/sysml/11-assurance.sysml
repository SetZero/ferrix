/*
 * Ferrix — SysML v2 model, part 11: assurance.
 *
 * docs/RELIABILITY.md and docs/ASSEMBLY.md: the quality gates, what each one
 * verifies, and what the tests can actually reach. All of it runs in CI
 * today except the two debts the roadmap states.
 */
package FerrixAssurance {
    private import ScalarValues::*;
    private import VerificationCases::*;
    private import FerrixLifecycle::*;
    private import FerrixStructure::*;
    private import FerrixRequirements::Principles::*;

    verification def Gate {
        doc /* A check that fails the build. Cheapest first in cargo xtask
             * check, so the gate most likely to fail on a work-in-progress
             * tree fails first. */
        subject os : Ferrix;
        attribute command : String;
    }

    #implemented verification hooksArmed : Gate {
        attribute :>> command = "python3 scripts/check-commit-authors.py --hooks";
        doc /* The first gate in cargo xtask check: fails when this clone has
             * not run `git config core.hooksPath .githooks`, so the missing
             * line reports itself rather than waiting to be noticed. */
        objective { verify aGateNeedsNoArming; }
    }
    #implemented verification commitAuthorship : Gate {
        attribute :>> command = "python3 scripts/check-commit-authors.py BASE HEAD";
        doc /* The One-author-per-commit CI job, over the range a push or pull
             * request adds and never the whole tree, so public history that
             * already carries a trailer cannot make the gate permanent red.
             * Matches a trailer at the start of a line only, so prose about
             * the rule passes. The commit-msg and pre-push hooks check the
             * same thing a second earlier, where the fix is a message edit
             * rather than a rebase; they are dormant until armed, and this
             * job is the one that is not. */
        objective { verify oneAuthorPerCommit; verify aGateNeedsNoArming; }
    }
    #implemented verification formatting : Gate {
        attribute :>> command = "cargo fmt --all -- --check";
    }
    #implemented verification lineEndings : Gate {
        attribute :>> command = "python3 scripts/check-line-endings.py";
        doc /* A CRLF in a shell script makes its shebang unparseable. */
    }
    #implemented verification assemblyAllowList : Gate {
        attribute :>> command = "python3 scripts/check-asm-budget.py";
        doc /* Fails on an assembly site not in scripts/asm-allowlist.json, on
             * a file over its budget, and on a stale entry. Reports the Rust
             * share on every run. */
        objective { verify assemblyOnlyWhereTheMachineDefinesIt; }
    }
    #implemented verification unsafeAudit : Gate {
        attribute :>> command = "python3 scripts/check-unsafe-audit.py";
        doc /* Every unsafe block has a SAFETY comment and one operation;
             * every unsafe fn a Safety section. Prints the per-crate count so
             * growth is noticed rather than happening. */
        objective { verify unsafeIsExpensive; }
    }
    #implemented verification panicAudit : Gate {
        attribute :>> command = "python3 scripts/check-panic-audit.py";
        doc /* Every panic-lint exemption is an #[expect] whose reason begins
             * AUDIT:. expect fails once the lint stops firing, so a site
             * refactored into safety loses its exemption. */
        objective { verify noReachablePanic; }
    }
    #implemented verification crateLayering : Gate {
        attribute :>> command = "scripts/check-crate-layering.sh";
        doc /* libs/ depend on nothing above them; generic kernel code names
             * no architecture; cfg(target_arch) only under arch/. */
        objective { verify pureFunctionsInLibs; verify oneArchFacade; }
    }
    #implemented verification clippy : Gate {
        attribute :>> command = "cargo clippy -- -D warnings";
        doc /* Seven runs: the host (libraries and xtask), and the kernel and
             * the loader for each of the three targets, because
             * cfg(target_arch) code is invisible to a lint pass for another
             * machine. The lint table denies unwrap, expect, panic,
             * unreachable, indexing and string slicing in production code. */
        attribute runs : Natural = 7;
    }
    #implemented verification hostTests : Gate {
        attribute :>> command = "cargo test --workspace --exclude ferrix-kernel --exclude ferrix-boot";
        doc /* Around 520 unit tests across libs/ (490 before libs/sched), plus
             * doc tests and xtask's 41. */
    }
    #implemented verification cargoDeny : Gate {
        attribute :>> command = "cargo deny check";
    }
    #implemented verification docsBuild : Gate {
        attribute :>> command = "cargo doc";
        doc /* Broken intra-doc links are denied. */
    }
    #implemented verification miri : Gate {
        attribute :>> command = "cargo miri test";
        doc /* libs/elf, bootinfo, ustack, objects, vfs, pci, block, blkring, native, virtio-blk, frame, heap and paging. cargo xtask check --miri runs the same list,
             * and an xtask test holds it to the workflow. A test too slow to
             * interpret runs shorter under cfg(miri) rather than being
             * ignored. */
    }
    #implemented verification fuzz : Gate {
        attribute :>> command = "cargo fuzz run elf_parse; cargo fuzz run frame_alloc; cargo fuzz run ustack_build";
        doc /* Replays the committed corpus first, searches second: an input
             * that crashed once fails again in seconds. ustack_build asserts a
             * round trip rather than the absence of a crash -- whatever the
             * builder accepts, a walk that knows only the stack pointer must
             * recover exactly what went in -- which is how it found that a
             * string containing a NUL built an image that read back short. */
    }
    #implemented verification buildImages : Gate {
        attribute :>> command = "cargo xtask build --arch all --release";
        doc /* One bootable FAT32 image per architecture, byte-for-byte
             * reproducible, uploaded as a CI artifact. xtask refuses to build
             * a kernel with RUSTFLAGS set, because cargo lets that variable
             * replace the per-target flags and silently drop the linker
             * script. */
    }
    #implemented verification bootTest : Gate {
        attribute :>> command = "cargo xtask test-boot --arch all";
        doc /* The gate that answers the question the others cannot. An OS
             * that compiles and does not boot is not a passing build. */
        objective { verify everyStageEndsInSomethingThatRuns; verify provedOnEveryBoot; }
    }

    // -----------------------------------------------------------------------
    // The assembly budget
    // -----------------------------------------------------------------------

    attribute def AssemblySite {
        attribute file : String;
        attribute maxLines : Natural;
    }

    #implemented part def AssemblyBudget {
        doc /* An absolute cap, because assembly here is a fixed cost that a
             * scheduler, a filesystem or a driver must add nothing to. The
             * ratio is a backstop and the target is the direction of travel:
             * reached by writing the operating system, not by shrinking the
             * vector table. 303 lines and about 99.2% Rust when this was
             * written, across fifteen sites. */
        attribute maxTotalLines : Natural = 800;
        attribute maxRatio : Real = 0.06;
        attribute targetRatio : Real = 0.001;
        attribute sites : AssemblySite[15] = (
            AssemblySite(file = "boot/src/arch/x86_64.rs", maxLines = 30),
            AssemblySite(file = "boot/src/arch/aarch64.rs", maxLines = 60),
            AssemblySite(file = "boot/src/arch/armv7a.rs", maxLines = 60),
            AssemblySite(file = "kernel/src/arch/x86_64/cpu.rs", maxLines = 100),
            AssemblySite(file = "kernel/src/arch/aarch64/cpu.rs", maxLines = 100),
            AssemblySite(file = "kernel/src/arch/armv7a/cpu.rs", maxLines = 100),
            AssemblySite(file = "kernel/src/arch/x86_64/trap.rs", maxLines = 90),
            AssemblySite(file = "kernel/src/arch/aarch64/trap.rs", maxLines = 90),
            AssemblySite(file = "kernel/src/arch/armv7a/trap.rs", maxLines = 90),
            AssemblySite(file = "kernel/src/arch/x86_64/smp.rs", maxLines = 40),
            AssemblySite(file = "kernel/src/arch/aarch64/smp.rs", maxLines = 30),
            AssemblySite(file = "kernel/src/arch/armv7a/smp.rs", maxLines = 40),
            AssemblySite(file = "kernel/src/arch/x86_64/switch.rs", maxLines = 30),
            AssemblySite(file = "kernel/src/arch/aarch64/switch.rs", maxLines = 30),
            AssemblySite(file = "kernel/src/arch/armv7a/switch.rs", maxLines = 30)
        );
        #planned attribute syscallEntrySites : AssemblySite[3] {
            @stage { number = 7; }
            doc /* The SYSCALL / SVC trampolines: the last entries the list is
                 * meant to gain. */
        }
    }

    // -----------------------------------------------------------------------
    // Reachability: what each layer's tests can drive
    // -----------------------------------------------------------------------

    enum def TestReach {
        CargoTest;
        Miri;
        Fuzzer;
        QemuBoot;
    }

    part def LayerReach {
        attribute layer : String;
        attribute reach : TestReach[1..*];
    }

    part libsReach : LayerReach {
        attribute :>> layer = "libs/";
        attribute :>> reach = (TestReach::CargoTest, TestReach::Miri, TestReach::Fuzzer);
    }
    part bootReach : LayerReach {
        attribute :>> layer = "boot/";
        attribute :>> reach = (TestReach::QemuBoot);
    }
    part kernelReach : LayerReach {
        attribute :>> layer = "kernel/";
        attribute :>> reach = (TestReach::QemuBoot);
        doc /* Miri cannot interpret a privileged instruction and a fuzzer
             * cannot drive a page-fault handler, which is the whole argument
             * for libs/. */
    }
    part xtaskReach : LayerReach {
        attribute :>> layer = "xtask/";
        attribute :>> reach = (TestReach::CargoTest);
    }

    // -----------------------------------------------------------------------
    // Verification the later stages owe, as the architecture states it
    // -----------------------------------------------------------------------

    #planned verification def BtrfsCheck {
        @stage { number = 12; }
        doc /* Every CI run builds an image with real mkfs.btrfs, mounts it
             * under Ferrix in QEMU, runs a workload, and hands the result to
             * host btrfs check. A filesystem only our own reader can read is
             * not a filesystem. */
        subject os : Ferrix;
    }
    #planned verification def PowerFailInjection {
        @stage { number = 12; }
        doc /* Kill QEMU at a random point inside a transaction, remount,
             * replay, btrfs check again, over hundreds of seeds. The test a
             * CoW filesystem actually has to pass. */
        subject os : Ferrix;
        attribute seeds : Natural;
    }
    #planned verification def CyclicTest {
        @stage { number = 14; }
        doc /* Wake-up latency on a HardRt domain while a Throughput domain on
             * other cores is saturated; the maximum must be inside the stated
             * bound. */
        subject os : Ferrix;
        attribute maxLatencyNanos : Natural;
    }
}
