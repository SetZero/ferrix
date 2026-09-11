Report below, grouped by your numbered items. All paths absolute.

1. /home/sebastian/Documents/projects/os/ferrix/docs/ROADMAP.md
Top matter — two rules, quoted (lines 3–20). Opens with a one-sentence relationship to the other doc: "docs/ARCHITECTURE.md says what is being built. This says in what order, and how each stage knows it is finished." Then:

Two rules govern the ordering:

Every stage ends in something that runs. Not "the VM subsystem compiles" — a QEMU boot that demonstrates the new capability and stays in CI forever after. A stage with no observable exit criterion is a stage nobody can tell is broken.
Nothing is stubbed that a later stage has to unpick. A fixed-size process table, an in-memory-only filesystem or a cooperative scheduler would each save a week now and cost a rewrite later, because the goal at the end needs the real version of all three. (ROADMAP.md:6–15)
Then sizing (:17–20): "Sizes are order-of-magnitude, in the sense of 'a weekend / a week / a month / longer'… Nobody should read the table as a schedule."

Status block (:22–26) — newly added in the working tree (see item 7): "Where it stands: stages 0–3 are done and in the boot test on both architectures, and the boot marker reads FERRIX-BOOT-OK stages 1-3. Stage 4 is next."

Heading forms. Two shapes, and the one you quoted is stale in HEAD but changed in the working tree:

Done: ## Stage 3 — Traps, interrupts, time ✅ (:144); likewise ## Stage 0 — Foundation ✅ (:30), ## Stage 1 — Boot, both architectures ✅ (:41), ## Stage 2 — Physical and virtual memory ✅ (:73).
Not started: ## Stage 4 — SMP · *week* (:245) — em dash for the title, then two spaces, ·, two spaces, italic size. Sizes used: *week*, *month*, *longer*, and two specials: ## Stage 16 — rustc · *the goal* (:385) and ## Stage 17 — Self-hosting (:394, no size at all). Stages are separated by --- on its own line; note stage 3→4 is the one place with no --- (:244–245).
The uncommitted diff shows the older idiom was ## Stage 3 — Traps, interrupts, time · *exit criterion met* and it was just promoted to ✅.
Exit-criterion wording, stages 1–3. Escalating forms, never boilerplate:

Stage 0: **Exit:** cargo xtask check runs fmt, clippy on all four targets… (:35)
Stage 1: **Exit, met:** cargo xtask test-boot --arch both boots firmware → loader → kernel on each architecture, and the kernel verifies four things before it reports success: followed by a four-bullet list of the checks (:51–64), then a closing paragraph "Two bugs this stage found…" (:66–69).
Stage 2: **Exit criterion met, and in the boot test on both architectures:** 4096 blocks… (:95) and again at :129 for the virtual half.
Stage 3: **Exit criterion, half met, and in the boot test on both architectures.** (:169) for the trap half, and **Exit criterion met, and in the boot test on both architectures.** (:207) for the interrupt half.
Future stages use the bare **Exit:** a boot test that … one-liner (:253, :265, :275, :287, :298, :310, :320, :336), with variants **Exit, and it is a strict one:** (:344) and **Exit:** rustc hello.rs && ./hello on Ferrix, in CI. (:390).
Within a done stage the body is segmented by bolded run-in headings: **Done.** (:78), **Done — the virtual half, and the tidying the physical half was owed.** (:101), **Done — the trap half.** (:152), **Done — the interrupt and time halves.** (:186).

Deferred / "still to do" formatting. Two forms:

One-liner: "Deferred to stage 4, because it needs a second CPU to mean anything: per-CPU frame caches." (ROADMAP.md:139–140)
Paragraph + bullets, each bullet arguing why the gap is safe (:226–243): "Deferred, and none of it is reachable from a machine Ferrix boots on today. The first two are hardware variants rather than gaps in the stage: the facade above them does not change, and neither is on any later stage's path. The third is stage 4's by definition." Bullets: GICv3 ("QEMU's virt gives a GICv2 unless asked otherwise, so this needs a second boot-test configuration as much as it needs code."), TSC-deadline, "Per-CPU anything." The working-tree diff shows this heading was renamed from **Still to do, and neither is reachable from a machine Ferrix boots on today.** — i.e. the current idiom for a shipped stage's gaps is Deferred.
"Written ahead of their stage" (:401–451). Prose first (the libs/ rule and its consequence, plus which crates are the exception), then the table:


| Crate | Waiting for | Tests |
|---|---|---|
| `libs/acpi` | 3, 10 — RSDP, XSDT/RSDT, MADT, FADT fixed fields, GTDT, HPET. No AML, and there will be none. | 58 |
| `libs/fdt` | 3, 10 — flattened device tree reader; on AArch64 the only description of the machine there is. | 49 |
(:424–433; column 2 is "stage numbers — what it does", with an editorial clause; column 3 is a host test count.) Followed by a totals sentence ("448 host unit tests, all passing, plus 15 doc-tests and the 24 of xtask itself.", :435–437) and a self-critical section **The gap this opens, stated rather than hidden.** (:439–451) about owed fuzz targets and Miri jobs.

"Continuously, from stage 1" (:455–460) — three bullets, verbatim:

Every stage's exit criterion joins the CI boot test and stays there.
The assembly allow-list is not added to without an argument in the diff.
Anything expressible as a pure function of bytes goes to libs/ and gets a fuzz target and a Miri run — before it is called from the kernel, not after.
Does any stage mention real hardware / boards / DT consumption / a third architecture? No. grep -niE "device tree|fdt|board|hardware|armv7|arm32|cortex-a7|32-bit" over docs/ + README.md returns only six hits total (listed under item 2/3 below). Concretely:

No stage anywhere mentions a physical board, a third architecture, or 32-bit. The only "bare metal" is ROADMAP.md:201 about the virtual timer.
Device tree appears exactly twice in ROADMAP: stage 10 and the libs/fdt table row. So libs/fdt is written, fuzz-shaped-ish, and not consumed by the kernel today — AArch64 gets its interrupt controller and timer from ACPI MADT/GTDT, not FDT (:194–201).
Stage 4 heading + first paragraph (:245–253), verbatim:

Stage 4 — SMP · week
x86-64 AP bring-up via INIT–SIPI–SIPI and a real-mode trampoline (the one place in the project with 16-bit assembly). AArch64 via PSCI CPU_ON. Per-CPU data areas, IPIs, TLB shootdown, and the RCU-like grace period the scheduler mode switch later depends on.

Exit: a boot test that brings every QEMU vCPU online, runs a contended counter across all of them, and gets the right total.

No ACPI/DT mention — AP enumeration source is left implicit (PSCI named for AArch64; on ARM32 PSCI and/or the DT cpus node would be the analogue).

Stage 10 heading + first paragraph (:314–322), verbatim:

Stage 10 — Userspace drivers · month
ACPI and device-tree enumeration in the kernel; IOMMU domains (VT-d, AMD-Vi, SMMUv3); devmgr; the shared-ring block protocol; and the first driver — virtio-blk — as a user process.

Exit: a boot test that reads a sector from a virtio disk through a driver running in ring 3, with the IOMMU on and a deliberate out-of-domain DMA attempt faulting.

So device-tree enumeration is stage 10's by the current plan; an ARM32 port that must consume an FDT at stage 1 (no ACPI on Cortex-A7 / STM32MP157) pulls that forward and needs stating explicitly.

2. /home/sebastian/Documents/projects/os/ferrix/docs/ARCHITECTURE.md
Section 4 "Memory" — virtual layout (:135–146). The heading is ## 4. Memory, and the layout is a fenced code block, not a table:

Virtual. Four-level paging on both architectures, 48-bit addresses, identical layout constants on each:


0x0000_0000_0000_0000 .. 0x0000_7FFF_FFFF_FFFF   user
0xFFFF_8000_0000_0000 .. 0xFFFF_FEFF_FFFF_FFFF   direct map of all physical RAM
0xFFFF_FF00_0000_0000 .. 0xFFFF_FFEF_FFFF_FFFF   kernel vmap (MMIO, guard-paged stacks)
0xFFFF_FFFF_8000_0000 .. 0xFFFF_FFFF_FFFF_FFFF   the kernel image
The deliberate-sharing sentence is ARCHITECTURE.md:144–146:

x86-64 puts the image in the top -2 GiB because that is what the "kernel" code model addresses; AArch64 does not need to, and does anyway, because one layout is one set of bugs instead of two.

This is the single most load-bearing sentence for an ARM32 plan: a 32-bit arch has a 4 GiB space and cannot hold any of those constants — the "one layout" claim has to become "one layout for the 64-bit pair, and a stated reason the 32-bit one differs."

The arch facade + libs/ vs kernel/ rule — section 9 (:295–314). The table:


| Layer | Lives in | Reachable by tests |
| Byte-level logic: ELF, cpio, btrfs item parsing, seccomp BPF, page-table arithmetic, allocators | `libs/` | `cargo test`, Miri, fuzzers |
| Loader | `boot/` | QEMU boot test |
| Kernel | `kernel/` | QEMU boot test, in-kernel test harness |
| Host tooling | `xtask/` | `cargo test` |
then (:304–308): "The split is not cosmetic. Nothing in kernel/ can be run by cargo test, Miri cannot interpret a privileged instruction, and a fuzzer cannot drive a page fault handler. So anything expressible as a pure function of bytes is written as one, in libs/… scripts/check-crate-layering.sh keeps that from eroding."

The facade rule, verbatim (:310–314):

Architecture-specific code lives under arch/ and is reached through one facade. Generic code never names x86_64 or aarch64, and no #[cfg(target_arch)] appears outside those directories — both enforced by the same script, because a facade maintained by convention is a facade for about six weeks.

Note the rule is written as an enumeration of two arch names; a third requires either rewording or checking scripts/check-crate-layering.sh's pattern list. ROADMAP.md:150 and :165–167 state the kernel-side facade concretely: "Both behind one facade: irq::register, timer::after, trap::Frame" and "reached only through the architecture facade — TrapFrame, classify, report_trap — so generic code still never names a CPU."

Buses / ACPI / device tree, section 7 (:227–239). Line 229 is the sentence you were after:

The kernel enumerates buses, because that needs ACPI (x86-64) and device tree (AArch64) and privileged access. It does not drive devices.

Also relevant to an ARM32/board plan in the same section: "IOMMU is not optional here. … VT-d/AMD-Vi on x86-64, SMMUv3 on AArch64. Where no IOMMU exists, drivers run in a degraded trusted mode and the kernel says so loudly at boot." (:253–258) — the STM32MP157 has no IOMMU, so that escape hatch already exists in the doc.

Other places the doc hard-codes "two architectures": the title/intro (:3, "for x86-64 and AArch64"), section 10 "Assembly" (:320, "There is none at boot on either architecture: UEFI calls a Rust efi_main in 64-bit mode").

3. /home/sebastian/Documents/projects/os/ferrix/docs/BOOT-LOG.md
Important correction to the premise: this file does not document the format of Ferrix's own boot-log lines. Its title is # Reading the boot log and it is a forensic file about firmware noise that is not ours, per arch. Structure:

Opening claim + why the file exists (:1–7): "The AArch64 boot log opens with nine lines that look like failures. They are not ours, and this file is the evidence, so that the next person to read a boot log does not spend an afternoon on them the way the first one did." and "Everything below was checked by experiment, not inferred."
One ## section per puzzling message, each with: a fenced verbatim quote of the lines, a bolded verdict paragraph, then an italic *How this was established:* paragraph with the exact reproducing QEMU command or experiment. Sections: ## The nine firmware lines (:9), ## `ConvertPages: failed to find range 140000000 - 14000BFFF` (:45), ## `UEFI Non-Block Boot Device` (:84).
A PE-header fact table at :63–68, and an x86-64-vs-AArch64 side-by-side comparison block at :88–91.
Each section ends with a "and it should not be silenced" judgement, e.g. :80–82: "It could be silenced by passing /base:0x40000000 to the linker, and it should not be: that address is right for one QEMU machine and wrong for every real one. A cosmetic message beats a fixed load address that is a lie on hardware."
## The rule (:117–122): "None of the above is filtered out of the boot log, and none of it should be. xtask shows the guest's serial output verbatim, because the first time a real failure appears there, it will look exactly like these do…"
The actual per-line log format lives in the README sample block and in /home/sebastian/Documents/projects/os/ferrix/kernel/src/main.rs: two leading spaces, an 8-column left label, then the facts — main.rs:84 println!("  stage 1  loader hand-off verified");, main.rs:93 "  traps    vectors installed", main.rs:401 the clock line, main.rs:405 the irqs line. The success marker is const SUCCESS_MARKER: &str = "FERRIX-BOOT-OK"; (kernel/src/main.rs:39), matched by xtask/src/qemu.rs:20; the panic counterpart is FERRIX-PANIC (main.rs:81). So a third arch's expected log should be presented in README-style fenced form, and BOOT-LOG.md would gain a new ## section only for ARM32 firmware/U-Boot noise, in the same evidence-first idiom.

4. /home/sebastian/Documents/projects/os/ferrix/docs/RELIABILITY.md
## The boot test (:98–102) is the whole per-arch claim, verbatim: "The gate that answers the question the others cannot. Everything else checks the source; this one boots it — firmware to loader to kernel — on both architectures, and fails the build if the machine does not come up. An OS that compiles and does not boot is not a passing build."
## What the tests can actually reach (:67–83) ends with the rule the roadmap echoes: "What is left in kernel/ is the part that genuinely needs a CPU, and it is covered by the QEMU boot test instead: every roadmap stage's exit criterion becomes a boot test and stays in CI forever after."
No CI matrix is described in the doc — it says "both architectures" in prose only. The matrix itself is in /home/sebastian/Documents/projects/os/ferrix/.github/workflows/ci.yml: arch: [x86_64, aarch64] appears twice (ci.yml:182 build, ci.yml:220 boot), the toolchain targets: list is spelled out four-wide in three places (ci.yml:123, 189, 227 — x86_64-unknown-none, aarch64-unknown-none-softfloat, x86_64-unknown-uefi, aarch64-unknown-uefi), clippy is run once per freestanding target (ci.yml:140–154), and firmware packages are installed at ci.yml:240 (qemu-system-x86 qemu-system-arm ovmf qemu-efi-aarch64). The README and xtask check phrase this as "clippy on all four targets" (ROADMAP.md:35) — a third arch makes that "six", in two docs plus xtask/src/check.rs:76 (for arch in [Arch::X86_64, Arch::AArch64]).
Other per-arch coverage claims worth knowing: the overflow-checks section (:55–65) and the unsafe-audit table (:16–23) are arch-neutral and survive a port unchanged.
5. /home/sebastian/Documents/projects/os/ferrix/README.md — claims that break
Subtitle, :3–4: "An operating system written in Rust for x86-64 and AArch64, whose acceptance test is that it compiles Rust." (Same sentence at ARCHITECTURE.md:3.)
Sample output, :13–33: cargo xtask test-boot --arch both with exactly two trailing lines, x86_64: boot ok / aarch64: boot ok. "both" as the spelling for "all" becomes wrong the moment there are three (xtask/src/args.rs:77 maps "both" | "all" to the two-element vector).
"What exists today", :38–39: "Stages 1, 2 and 3 of docs/ROADMAP.md. Both architectures boot from firmware to a Rust kernel…" — and :57–59, "What stage 3 still owes is hardware Ferrix cannot currently be booted on to test: GICv3, and the local APIC's TSC-deadline mode."
## No assembly at boot, :69–71 — the most exposed claims: "Both architectures boot via UEFI, so firmware calls a Rust efi_main with the CPU already in 64-bit mode. There is no bootstrap assembly on either machine, which is unusual and is a direct consequence of choosing UEFI over Multiboot." An ARMv7-A port breaks all three clauses: QEMU virt for ARM32 can do UEFI, but the STM32MP157 boots via U-Boot/TF-A, the CPU is never in 64-bit mode, and the SVC-mode/MMU-off entry almost certainly costs bootstrap assembly. Mirrored at ARCHITECTURE.md:320–322 and ASSEMBLY.md:22–27 (## Boot: none).
:73–75: "The assembly that does exist — 107 lines, 99.56% Rust — is confined to constructs the machine defines before a Rust function could run…" Both numbers move, and ASSEMBLY.md:62–69 says the cap is deliberate: "max_total_lines in the allow-list is an absolute cap, not a percentage… The list above is meant to be finished once both architectures boot: a scheduler, a filesystem or a driver must add nothing to it. Raising the cap is a commit whose message explains what the machine made unavoidable." That sentence is the sanctioned route for an ARM32 vector table / EL-equivalent entry, and ASSEMBLY.md would need a third ### ARMv7-A subsection under ## The list (it currently has ### Both architectures / ### x86-64 / ### AArch64, :40–61).
## Getting started, :86–97: the four xtask commands use --arch both / --arch x86_64; the prerequisites sentence names qemu-system-x86, qemu-system-arm, ovmf, qemu-efi-aarch64 — qemu-system-arm is already listed (it is what runs aarch64 on Debian? no — it is the 32-bit binary, so this line is coincidentally already half-right), but 32-bit ARM UEFI firmware (qemu-efi-arm) is not.
## Layout table, :101–108, and ## Quality gates, :112–116 ("clippy at -D warnings on all four targets").
6. Conventions — no CLAUDE.md, no AGENTS.md, no CONTRIBUTING
None exist anywhere in the tree (find -maxdepth 2 for all three: no hits; ~/.claude/CLAUDE.md absent too). The written conventions live in three other places:

/home/sebastian/Documents/projects/os/ferrix/.githooks/README.md — the commit rule, verbatim: "## commit-msg — no Co-authored-by. Ferrix commits name one author. A Co-authored-by: trailer is refused at the moment it is written, which is where the error is cheap and the fix is to edit the message." Enforced twice (commit-msg and pre-push) and requires git config core.hooksPath .githooks per clone. This directly contradicts the attribution instruction in my system context — do not add a Co-Authored-By: trailer to commits in this repo; the hook rejects it. /home/sebastian/Documents/projects/os/ferrix/.githooks/commit-msg:30 is the grep that fires.
/home/sebastian/Documents/projects/os/ferrix/docs/RELIABILITY.md:37–47 — comment/exemption conventions: exemptions are #[expect(clippy::indexing_slicing, reason = "AUDIT: len checked at :212")], with "1. The reason begins AUDIT:, so it reads as an argument in the diff that adds it rather than as a way past the lint. 2. Exemptions use #[expect], never #[allow]." Plus the unsafe rules table (:16–23): a // SAFETY: comment per block, one unsafe op per block, # Safety on every unsafe fn.
/home/sebastian/Documents/projects/os/ferrix/docs/ASSEMBLY.md:5, 36–38 — "Entries are added to scripts/asm-allowlist.json in the same commit as the code", and the admission test: "The construct is defined by what the machine does to registers around it, before or after the first instruction of a Rust function could run." Plus "'Convenient in assembly' is not a reason. 'Faster in assembly' is not a reason either — measure it, then argue it in the allow-list entry."
Prose idiom, observable across all docs: em dashes, no emoji except the ✅ stage marker, bolded run-in headings, every claim paired with the reason or the experiment, and a deliberate "stated rather than hidden" habit for known gaps (ROADMAP.md:439).
7. Git history and branching
git log --oneline -30 returns only 8 commits (shallow history — the tree starts at "Initial commit: Ferrix through stage 3's trap half"):


7ca5ac1 Finish stage 2: a vmap arena, guard-paged stacks, and memory the kernel gets back
a6b21b1 Finish stage 3: interrupt controllers, a clock, and a timer that is measured
0def45f Stop tracking fuzz/target, which the last commit swept in by accident
0ae6b93 Queue the fairness test's threads before measuring, and fix the ELF fuzz target
76a492e Make CI boot the kernel it builds, and stop a fairness test measuring the scheduler
159652a Fix the carriage return that stopped CI parsing the workflow
7492ae7 Relicense to MIT, and make `cargo xtask check` pass
e629f09 Initial commit: Ferrix through stage 3's trap half
Stage-finishing title pattern: Finish stage N: <the two or three things, lowercase, comma/and list>. Note the ordering oddity — 7ca5ac1 (stage 2) sits on top of a6b21b1 (stage 3), i.e. the stage-2 tidy-up landed after stage 3's second half.
Non-stage commits are imperative-mood sentences naming cause and consequence, not conventional-commit prefixes ("Stop tracking…, which the last commit swept in by accident").
Bodies are long and argumentative — 2–6 paragraphs, hard-wrapped ~79 cols, no bullet lists, no trailers. They restate the why and the ordering constraints (e.g. stage 2's body: "Order matters in one place: the sweep cannot pass while the identity map is live, because the loader mapped itself writable and executable and was right to…"), name the specific bug a test guards ("a handler that acknowledges the controller without disarming is re-entered forever"), and close with small collateral renames ("Leaf::len is Leaf::bytes now."). Note stage 3's body uses -- rather than em dashes while stage 2's uses —; both appear.
Branches: main, and the current checkout is stage-3-interrupts-and-time (one commit ahead conceptually; origin/HEAD -> origin/main). So yes, the convention is a per-stage topic branch named stage-N-<slug>; an ARM32 effort would follow as e.g. stage-…-armv7a or an arch-named branch. Working tree currently has one uncommitted modification: docs/ROADMAP.md (the "Where it stands" block, the ✅ promotion of stage 3, "Still to do" → "Deferred", per-arch reclaim figures, and the Hz range widened to a measured band).
Two cross-cutting facts worth folding into the plan: (a) libs/fdt already exists with 49 tests but is unreachable from the kernel — an ARM32 port is the first consumer, which the "Written ahead of their stage" section explicitly anticipates ("What they buy is that the stage in question begins with its byte-handling already fuzz-shaped", ROADMAP.md:420–422); (b) xtask hard-codes the arch pair in xtask/src/paths.rs:10–67 (enum, parse, host, name, kernel target, UEFI target, BOOTX64.EFI/BOOTAA64.EFI) and xtask/src/args.rs:77 ("both" | "all"), plus xtask/src/check.rs:76 — so the "both" vocabulary is a real code change, not just a docs edit.

I'll verify the key facts the plan relies on: availability of 32-bit ARM UEFI firmware for QEMU, U-Boot's EFI loader expectations on ARM32, and what's already installed on the host.
Bash Check installed ARM32 firmware, QEMU, and Rust targets
IN
ls /usr/share/AAVMF /usr/share/qemu-efi-arm 2>&1 | head; which qemu-system-arm && qemu-system-arm --version | head -1; qemu-system-arm -M virt -cpu help 2>/dev/null | grep -i "cortex-a7\b" ; rustup target list 2>/dev/null | grep -E "armv7a-none|thumbv7a|arm.*uefi"; apt-cache policy qemu-efi-arm 2>/dev/null | head -3

OUT
ls: cannot access '/usr/share/AAVMF': No such file or directory
ls: cannot access '/usr/share/qemu-efi-arm': No such file or directory
/usr/local/bin/qemu-system-arm
QEMU emulator version 9.2.4 (v9.2.4-dirty)
  cortex-a7
armv7a-none-eabi
armv7a-none-eabihf
qemu-efi-arm:
  Installed: (none)
  Candidate: (none)
Web Searchqemu-efi-arm AAVMF32_CODE.fd ArmVirtQemu 32-bit ARM UEFI firmware QEMU virt cortex-a15 ACPI not supported AArch32 device tree only
OUT
Web search results for query: "qemu-efi-arm AAVMF32_CODE.fd ArmVirtQemu 32-bit ARM UEFI firmware QEMU virt cortex-a15 ACPI not supported AArch32 device tree only"

Links: [{"title":"AAVMF32_CODE.fd as UEFI bianry in Debian · Issue #174 · virt-manager/virt-manager","url":"https://github.com/virt-manager/virt-manager/issues/174"},{"title":"[Concept] [PATCH 3/6] test: hooks: Add QEMU configs for EFI app boards - Concept - U-Boot List","url":"https://lists.u-boot.org/archives/list/concept@u-boot.org/message/FOPGOH6HK2VI6TDQTFJAJPO2XF7LXPSJ/"},{"title":"#857858 - qemu-efi: Missing support for AArch32 (32bit ARM) EFI - Debian Bug report logs","url":"https://bugs.debian.org/857858"},{"title":"Bug#1003480: Using AAVMF32_VARS.fd causes data exception and failure to boot","url":"https://www.mail-archive.com/debian-bugs-dist@lists.debian.org/msg1838343.html"},{"title":"#1003480 - Using AAVMF32_VARS.fd causes data exception and failure to boot - Debian Bug report logs","url":"https://bugs.debian.org/cgi-bin/bugreport.cgi?bug=1003480"},{"title":"🚀/usr/share/AAVMF - Linux Jangnan","url":"https://linux.jangnan.org/usr/share/AAVMF/z_AAVMF.html"},{"title":"Emulating ARM on QEMU, with UEFI - Kya's Blog","url":"https://kya8.github.io/p/emulating-arm-on-qemu-with-uefi/"}]

Based on the search results, here's what I found about your query regarding 32-bit ARM UEFI firmware for QEMU:

## AAVMF32_CODE.fd Overview

AAVMF32_CODE.fd is UEFI firmware for 32-bit ARM virtual machines, distributed as part of the `qemu-efi-arm` package. Debian includes AAVMF32_CODE.fd as its UEFI binary for armv7l.

## ArmVirtQemu and Configuration Details

The virt board is being emulated with support for virtio devices, and the CPU capability is set to cortex-a15. The AAVMF32_CODE.fd and AAVMF32_VARS.fd binaries are used for 32-bit ARM UEFI emulation.

## Important Usage Notes

When using AAVMF32_VARS.fd, you should typically make a copy per-guest and omit the readonly=on bit for that file on the QEMU command line, as the VARS file needs to be writeable.

The search results primarily document the practical usage of AAVMF32_CODE.fd with QEMU's virt machine type and cortex-a15 CPU emulation, though specific documentation about ACPI support limitations or device tree-only configuration for AArch32 was not found in these results.


REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.
Web Searchsystemd elf2efi.py convert PIE ELF to PE/COFF .reloc R_ARM_RELATIVE arm32 IMAGE_FILE_MACHINE_ARMNT U-Boot efi_loader bootarm.efi
OUT
Web search results for query: "systemd elf2efi.py convert PIE ELF to PE/COFF .reloc R_ARM_RELATIVE arm32 IMAGE_FILE_MACHINE_ARMNT U-Boot efi_loader bootarm.efi"

Links: [{"title":"RFC: boot: Build EFI binaries natively by medhefgo · Pull Request #26097 · systemd/systemd","url":"https://github.com/systemd/systemd/pull/26097"},{"title":"systemd: tools/elf2efi.py | Fossies","url":"https://fossies.org/linux/systemd/tools/elf2efi.py"},{"title":"systemd/tools/elf2efi.py at main · systemd/systemd","url":"https://github.com/systemd/systemd/blob/main/tools/elf2efi.py"},{"title":"The EFI Boot Stub — The Linux Kernel documentation","url":"https://kernel.org/doc/html/v5.4/admin-guide/efi-stub.html"},{"title":"UEFI on U-Boot — Das U-Boot unknown version documentation","url":"https://docs.u-boot.org/en/latest/develop/uefi/uefi.html"},{"title":"elf2efi/README.md at main · davmac314/elf2efi","url":"https://github.com/davmac314/elf2efi/blob/main/README.md"},{"title":"u-boot/doc/develop/uefi/uefi.rst at master · u-boot/u-boot","url":"https://github.com/u-boot/u-boot/blob/master/doc/develop/uefi/uefi.rst"},{"title":"================= The EFI Boot Stub =================","url":"https://www.kernel.org/doc/Documentation/efi-stub.txt"}]

I found relevant search results about systemd's elf2efi.py tool and related ELF-to-PE/COFF conversion topics. Here's what the search revealed:

## elf2efi.py Tool Overview

elf2efi.py converts ELF static PIE to PE/EFI images by translating ELF relocations to PE relocations so that the EFI loader/firmware can properly load the binary to any address at runtime.

## Relocation Handling Strategy

If the ELF binary is built as a shared object instead of a static binary, the conversion only needs to handle R_*_RELATIVE relocation types that are straightforward to convert to PE relocations. The conversion tool is fairly small at 550 lines.

## Architecture Support

The results indicate that PE/COFF image support exists across multiple platforms. On ARM platforms, kernel images can masquerade as PE/COFF images for EFI firmware loaders, with the ARM EFI stub implemented in arch/arm/boot/compressed/efi-header.S and arch/arm/boot/compressed/efi-stub.c.

## U-Boot EFI Support

U-Boot's bootefi command uses the device path and file name of the most recently loaded PE-COFF file when setting up the loaded image protocol. This indicates U-Boot can work with PE-COFF formatted EFI applications like bootarm.efi.

The search results provide foundational information about how elf2efi.py handles the conversion process and how various systems support PE/COFF EFI binaries, though they don't contain detailed technical specifications about specific relocation types like R_ARM_RELATIVE or IMAGE_FILE_MACHINE_ARMNT constants.


REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.
Agent "Docs conventions and roadmap format" finished
The docs agent found that the repo's .githooks block Co-Authored-By trailers, and that libs/fdt has 49 tests but no kernel consumer yet—ARM32 would be first. While waiting on the other two agents, I'll verify one firmware fact.
Web Fetchhttps://raw.githubusercontent.com/tianocore/edk2/master/ArmVirtPkg/ArmVirtQemu.dsc
Fetched from https://raw.githubusercontent.com/tianocore/edk2/master/ArmVirtPkg/ArmVirtQemu.dsc
32-bit ARM UEFI firmware for QEMU isn't reliably available, so I'll fall back to U-Boot as the firmware, matching the board's real boot chain. Let me verify it's packaged and check its EFI loader's ARM32 expectations.
Bash Check distro, U-Boot QEMU package, firmware files, cross tools
IN
cat /etc/os-release | head -3; apt-cache policy u-boot-qemu u-boot-tools qemu-efi-aarch64 2>/dev/null | grep -A1 -E "^[a-z]"; dpkg -L u-boot-qemu 2>/dev/null | grep -i "qemu_arm" ; ls /usr/share/qemu/ 2>/dev/null | grep -i -E "aavmf|edk2-arm|u-boot" ; ls /usr/share/AAVMF /usr/share/edk2* 2>/dev/null | head; which arm-none-eabi-gcc arm-linux-gnueabihf-gcc mkimage 2>&1 | grep -v "not found"

OUT
PRETTY_NAME="Ubuntu 26.04 LTS"
NAME="Ubuntu"
VERSION_ID="26.04"
u-boot-qemu:
  Installed: (none)
--
u-boot-tools:
  Installed: (none)
--
qemu-efi-aarch64:
  Installed: (none)
u-boot-sam460-20100605.bin
u-boot-sam460.bin
u-boot.e500
(eval):1: no matches found: /usr/share/edk2*
Web SearchDebian "u-boot-qemu" package qemu_arm u-boot.bin 32-bit "qemu-system-arm -M virt" -bios bootefi BOOTARM.EFI
OUT
Web search results for query: "Debian "u-boot-qemu" package qemu_arm u-boot.bin 32-bit "qemu-system-arm -M virt" -bios bootefi BOOTARM.EFI"

Links: [{"title":"Debian -- Details of package u-boot-qemu in sid","url":"https://packages.debian.org/sid/u-boot-qemu"},{"title":"QEMU ARM SBSA — Das U-Boot unknown version documentation","url":"https://docs.u-boot.org/en/v2025.01/board/emulation/qemu-sbsa.html"},{"title":"note/arm_emulation/u-boot_for_arm_on_qemu.md at master · jasonblog/note","url":"https://github.com/jasonblog/note/blob/master/arm_emulation/u-boot_for_arm_on_qemu.md"},{"title":"u-boot/doc/board/emulation/qemu-arm.rst at master · u-boot/u-boot","url":"https://github.com/u-boot/u-boot/blob/master/doc/board/emulation/qemu-arm.rst"},{"title":"QEMU ARM — Das U-Boot unknown version documentation","url":"https://docs.u-boot.org/en/stable/board/emulation/qemu-arm.html"},{"title":"Arm64Qemu - Debian Wiki","url":"https://wiki.debian.org/Arm64Qemu"},{"title":"Debian on ARM via QEMU - Life, the Universe and Fishing","url":"http://phwl.org/2021/qemu-armhf-debian/"},{"title":"Emulating ARM with QEMU on Debian/Ubuntu · GitHub","url":"https://gist.github.com/luk6xff/9f8d2520530a823944355e59343eadc1"},{"title":"qemu arm","url":"https://github.com/u-boot/u-boot/blob/v2020.07/doc/board/emulation/qemu-arm.rst"}]

Based on the search results, here's what I found about the Debian u-boot-qemu package and related information:

## Debian u-boot-qemu Package

The Debian u-boot-qemu package includes boot loaders for qemu/kvm, with supported platforms including qemu_arm and qemu_arm64.

## 32-bit ARM QEMU with U-Boot

QEMU for ARM supports a special 'virt' machine designed for emulation and virtualization purposes, with both 32-bit ARM and AArch64 supported.

The minimal QEMU command line to get U-Boot up and running for ARM is: `qemu-system-arm -machine virt -bios u-boot.bin`

## Building U-Boot for QEMU ARM

On successful build 'u-boot.bin' should be created, which can then be used with the QEMU command.

The search results show that the qemu_arm platform is included in the Debian u-boot-qemu package, and the basic command structure you're looking at follows the documented pattern for running U-Boot under QEMU's ARM virtual machine, though the specific `-bios bootefi` and `BOOTARM.EFI` references in your query don't appear in these standard documentation sources.


REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.
Web Searchedk2 ArmVirtPkg remove 32-bit ARM support ArmVirtQemu ARM dropped 2024 qemu-efi-arm package removed
OUT
Web search results for query: "edk2 ArmVirtPkg remove 32-bit ARM support ArmVirtQemu ARM dropped 2024 qemu-efi-arm package removed"

Links: [{"title":"Change log : edk2 package : Debian","url":"https://launchpad.net/debian/+source/edk2/+changelog"},{"title":"Unofficial EDK2 nightly build | EDK2 Nightly Build","url":"https://retrage.github.io/edk2-nightly/"},{"title":"[PULL 04/13] edk2: drop 32-bit build config (ia32 and arm), update aarch64 vars","url":"http://www.mail-archive.com/qemu-devel@nongnu.org/msg1221764.html"},{"title":"[tianocore/edk2] caac25: ArmVirtPkg: Drop bogus reference to MPCore related...","url":"https://www.mail-archive.com/edk2-commits@lists.sourceforge.net/msg12719.html"},{"title":"TianoCore EDK2: ArmVirtPkg/ArmVirt.dsc.inc Source File","url":"https://bsdio.com/edk2/docs/master/_arm_virt_8dsc_8inc_source.html"},{"title":"EDK2 - OSDev Wiki","url":"https://wiki.osdev.org/EDK2"},{"title":"[PATCH 00/11] x86: 32-bit cleanups","url":"https://lkml.iu.edu/hypermail/linux/kernel/2412.0/04434.html"}]

Based on the search results, here's what I found about 32-bit ARM support removal from EDK2:

ARM support for ArmVirtPkg has been removed, and upstream edk2 has dropped support for 32-bit architectures. This was part of a broader effort to remove 32-bit architecture support from the EDK2 firmware project.

The removal was required because QEMU is removing 64-on-32 support, which meant that maintaining 32-bit ARM builds in EDK2 no longer made practical sense for QEMU's use cases.

The removal also affected the qemu-efi-arm package and related Debian builds, as distributions updated their EDK2-based firmware packages to align with these upstream changes. The aarch64 config was updated as aarch64 used to share the 'vars' image with arm, and references to the 32-bit builds were dropped in a number of places.


REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.
I have everything. Here is the map.

Per-architecture build/boot path in Ferrix
Key structural fact up front: the Arch enum does not live in xtask/src/args.rs — it is in xtask/src/paths.rs:10-79. args.rs only parses --arch and expands it.

1. xtask/src/args.rs + the Arch enum (actually paths.rs)
xtask/src/args.rs (169 lines) — hand-rolled parser, no clap. Args fields at args.rs:13-32: command, arch: Option<String>, release, gdb, fast, help, smp, memory, timeout. Defaults smp: 4, memory: 512, timeout: 120 (args.rs:39-41).

Arch expansion — args.rs:74-80:


None => Ok(vec![Arch::host()]),
Some("both" | "all") => Ok(vec![Arch::X86_64, Arch::AArch64]),
Some(name) => Ok(vec![Arch::parse(name)?]),
single_arch() at args.rs:83-89 errors with "this command needs exactly one --arch" when more than one. Tests at args.rs:141-149 assert exactly vec![X86_64, AArch64] for --arch both.

Every method on Arch (xtask/src/paths.rs):

Method	Line	x86_64	aarch64
parse(name)	19-27	"x86_64" | "x86-64" | "amd64"	"aarch64" | "arm64"
host()	31-37	fallback	cfg!(target_arch = "aarch64")
name()	40-45	"x86_64"	"aarch64"
kernel_target()	48-53	x86_64-unknown-none	aarch64-unknown-none-softfloat
loader_target()	56-61	x86_64-unknown-uefi	aarch64-unknown-uefi
removable_boot_name()	65-70	BOOTX64.EFI	BOOTAA64.EFI
qemu_binary()	73-78	qemu-system-x86_64	qemu-system-aarch64
Plus impl Display (paths.rs:81-85) delegating to name(). Arch::parse error message hardcodes "expected x86_64, aarch64 or both" (paths.rs:24). Tests at paths.rs:262-281 pin every spelling and every target triple, and explicitly assert Arch::parse("riscv64").is_err().

2. xtask/src/qemu.rs — full QEMU invocation
Markers, qemu.rs:20-27:


pub(crate) const SUCCESS_MARKER: &str = "FERRIX-BOOT-OK";
pub(crate) const PANIC_MARKER: &str = "FERRIX-PANIC";
const DEBUG_EXIT_SUCCESS: i32 = 33;   // (0x10 << 1) | 1
Common args (qemu.rs:156-180), both arches: -m <memory>, -smp <smp>, -no-reboot, -display none, -monitor none, -serial stdio, -net none (the -net none comment at qemu.rs:171-177 explains the "Image type X64 can't be loaded on AARCH64 UEFI system" noise).

x86_64 (qemu.rs:183-197): -machine q35, -cpu qemu64,+pdpe1gb,+smep,+smap, -device isa-debug-exit,iobase=0xf4,iosize=0x04, disk as -drive format=raw,file=<image>.

AArch64 (qemu.rs:198-209): -machine virt, -cpu cortex-a72, -drive format=raw,file=<image>,if=none,id=disk, -device virtio-blk-device,drive=disk. (Note x86 attaches the image as a plain drive; aarch64 goes through virtio-blk.)

Firmware — both arches then get two pflash drives (qemu.rs:212-222):


-drive if=pflash,format=raw,readonly=on,file=<firmware.code>
-drive if=pflash,format=raw,file=<build/<arch>/uefi-vars.fd>
No -bios / -kernel path at all. display() at qemu.rs:229-231 converts backslashes.

prepare_vars (qemu.rs:239-271): always rewrites build/<arch>/uefi-vars.fd, never reuses (long comment at 244-254 about stale boot entries dropping to the EFI shell). AArch64-specific: contents.resize(code_size, 0) at qemu.rs:264-267 — virt requires both pflash images to be exactly the same size.

Firmware location — paths::find_firmware (paths.rs:125-181):

FERRIX_OVMF_CODE / FERRIX_OVMF_VARS env override first (paths.rs:126-131).
x86_64 code names: edk2-x86_64-code.fd, OVMF_CODE.fd, OVMF_CODE_4M.fd, OVMF.fd, ovmf-x86_64-code.bin; vars: edk2-i386-vars.fd, OVMF_VARS.fd, OVMF_VARS_4M.fd.
AArch64 code: edk2-aarch64-code.fd, AAVMF_CODE.fd, QEMU_EFI.fd, QEMU_EFI-pflash.raw; vars: AAVMF_VARS.fd, QEMU_VARS.fd, edk2-aarch64-vars.fd.
Directly relevant to ARM32: paths.rs:151-157 explicitly blacklists edk2-arm-vars.fd — "That is the 32-bit Arm build's variable store, and QEMU ships it at exactly the same 64 MiB as the AArch64 firmware — so it loads, and EDK2 then finds boot entries belonging to another architecture in it."
Search path firmware_search_path() (paths.rs:184-215): QEMU-binary-relative share/, then /usr/share/qemu, /usr/share/OVMF, /usr/share/ovmf, /usr/share/AAVMF, /usr/share/qemu-efi-aarch64, /usr/share/edk2/ovmf, /usr/share/edk2/aarch64, /usr/share/edk2-ovmf, /usr/local/share/qemu, /opt/homebrew/share/qemu, two Windows paths. There is no /usr/share/AAVMF32 or edk2/arm entry.
Boot-marker detection (qemu.rs:41-121): stdout piped, reader thread → mpsc channel so the timeout applies to the boot as a whole; each line is echoed     | {line} and appended to build/<arch>/serial.log (qemu.rs:68). line.contains(SUCCESS_MARKER) → Ok; line.contains(PANIC_MARKER) → immediate failure. finish() (qemu.rs:124-138) gives the guest 5s to power off after a verdict, then kills. On success, a QEMU exit code that is neither 0 nor 33 is still a failure (qemu.rs:104-109).

--gdb adds -s -S (qemu.rs:32-35).

3. xtask/src/fat.rs + xtask/src/fat/tests.rs — ESP image
A hand-written FAT32 writer (492 lines), no mkfs.vfat/mtools. Constants at fat.rs:19-46: SECTOR = 512, RESERVED_SECTORS = 32, NUM_FATS = 2, ROOT_CLUSTER = 2, IMAGE_BYTES = 64 MiB, MIN_FAT32_CLUSTERS = 65_525, frozen timestamp (FIXED_DATE = 2026-01-01) for byte-reproducibility. Volume label FERRIX.

Image layout — write_image (fat.rs:462-489), exactly two files:


fs.add_file(&format!("EFI/BOOT/{}", arch.removable_boot_name()), &loader_bytes)?;
fs.add_file("FERRIX/KERNEL.ELF", &kernel_bytes)?;
So: EFI/BOOT/BOOTX64.EFI or EFI/BOOT/BOOTAA64.EFI, and FERRIX/KERNEL.ELF. No startup.nsh, no partition table, no GPT — the raw 64 MiB image is the FAT32 filesystem, handed to QEMU as format=raw. Output path build/<arch>/ferrix.img (fat.rs:477-480).

8.3 names only (short_name, fat.rs:418-443) — a name over 8.3 is a hard error, no LFN support. The boot sector is written twice (sector 0 and backup at sector 6, fat.rs:345-351) with a EB 58 90 jump stub and no boot code.

Tests (xtask/src/fat/tests.rs, 362 lines) parse the produced image back with an independent reader and check EFI/BOOT/BOOTX64.EFI, EFI/BOOT/BOOTAA64.EFI (line 188 — both can coexist), FERRIX/KERNEL.ELF, ./.. entries, and geometry.

ARM32 note: adding a third arch needs a BOOTARM.EFI name in removable_boot_name(); the FAT writer itself needs no change.

4. xtask/src/check.rs and xtask/src/main.rs
check::run (check.rs:15-87), cheapest-first:

cargo fmt --all -- --check (18-24)
python scripts/check-line-endings.py (26)
python scripts/check-asm-budget.py (28-30)
python scripts/check-unsafe-audit.py (32)
python scripts/check-panic-audit.py (33)
bash scripts/check-crate-layering.sh (35-41)
clippy host: --workspace --exclude ferrix-kernel --exclude ferrix-boot --all-targets (43-52)
cargo test with the same excludes (54-66)
--fast returns here (68-71)
The per-target loop — this is the target list (check.rs:76-83):

for arch in [Arch::X86_64, Arch::AArch64] {
    step(..., || clippy(&["-p", "ferrix-kernel", "--target", arch.kernel_target()]))?;
    step(..., || clippy(&["-p", "ferrix-boot",   "--target", arch.loader_target()]))?;
}
That array literal at check.rs:76 is the only place the cross-target list appears in xtask. No miri and no test-boot in check — miri is CI-only.

clippy() always appends -- -D warnings (check.rs:96-104). python() probes python3, python, py by running --version (check.rs:112-133, with the Windows App-Execution-Alias rationale).

xtask/src/main.rs: modules at 24-29; USAGE string at 67-88 (documents --arch <x86_64|aarch64|both> at line 80); dispatch at main.rs:113-135 — build and test-boot loop over args.arches()?, run uses single_arch(). build_image at main.rs:139-143 = build loader → build kernel → fat::write_image.

5. xtask/src/cargo.rs and xtask/src/paths.rs — build invocation
cargo.rs:26-43 — plain cargo build --package <pkg> --target <triple> (+ --release). No -Zbuild-std, no -Zbuild-std-features anywhere in the tree (verified by grep). rust-src is listed as a toolchain component (rust-toolchain.toml:9) but nothing consumes it — all four targets are tier-2 prebuilt. This is the single biggest ARM32 change: armv7a-none-eabi has prebuilt std-core, but there is no armv7a-*-uefi target at all, so the loader would need either a custom JSON target + build-std, or a non-UEFI boot path.

refuse_inherited_rustflags() (cargo.rs:54-69) hard-fails if RUSTFLAGS/CARGO_ENCODED_RUSTFLAGS are non-empty, because they replace [target.*].rustflags and silently drop the linker script.

Artifact paths (cargo.rs:14-23, 73-83): target/<triple>/<debug|release>/ferrix-boot.efi and .../ferrix-kernel. Note the loader artifact name is ferrix-boot.efi — that .efi extension comes from the *-uefi target; a non-UEFI ARM32 loader target would produce a bare ferrix-boot.

paths.rs: workspace_root() from env!("CARGO_MANIFEST_DIR")/.. (88-96), build_dir(arch) = <root>/build/<arch.name()> (99-101), target_dir() honours CARGO_TARGET_DIR (104-107), which() (231-255) with Windows .exe suffix and two hardcoded QEMU install dirs.

6. .cargo/config.toml, rust-toolchain.toml
.cargo/config.toml — alias xtask = "run --quiet --package xtask --" (line 2), then two rustflags blocks, one per kernel target:

[target.x86_64-unknown-none] (11-19): -C link-arg=-Tkernel/linker/kernel.ld, -C link-arg=-zmax-page-size=4096, -C relocation-model=static.
[target.aarch64-unknown-none-softfloat] (21-30): identical three flags; the comment at 23-27 notes max-page-size is required on AArch64 because lld defaults to 64 KiB.
No flags for the two -uefi targets (they link as PE via lld defaults). An ARM32 kernel target (armv7a-none-eabi) would need a third block here.

rust-toolchain.toml: channel = "1.97.1" (pinned, line 8), components = ["rustfmt", "clippy", "llvm-tools", "rust-src"], targets = [x86_64-unknown-none, aarch64-unknown-none-softfloat, x86_64-unknown-uefi, aarch64-unknown-uefi] (10-18).

7. .github/workflows/ci.yml (only workflow file)
Jobs and their arch coupling:

Job	Line	Arch/matrix
assembly	31-53	none; runs check-asm-budget.py, also posts the Rust share to $GITHUB_STEP_SUMMARY
audits	59-78	none; line-endings, unsafe-audit, panic-audit
layering	85-98	none; scripts/check-crate-layering.sh
test	109-171	matrix os: [ubuntu-latest, windows-latest]; 5 hardcoded clippy steps at lines 136-154
build	176-204	matrix arch: [x86_64, aarch64] (182); cargo xtask build --arch ${{matrix.arch}} --release
boot	214-251	matrix arch: [x86_64, aarch64] (220); both arches are boot-tested
deny	257-266	cargo-deny
miri	279-300	cargo +nightly miri test -p ferrix-elf --lib and -p ferrix-bootinfo --lib
fuzz	315-370	nightly cargo-fuzz, corpus replay then 30s search per target
The target triple list is repeated verbatim three times — ci.yml:123, :189, :227:


targets: x86_64-unknown-none, aarch64-unknown-none-softfloat, x86_64-unknown-uefi, aarch64-unknown-uefi
apt packages for the boot job (ci.yml:238-240): qemu-system-x86 qemu-system-arm ovmf qemu-efi-aarch64. Note qemu-system-arm is already installed (it provides qemu-system-arm for ARM32 as well as being the source package for aarch64); for ARM32 UEFI you'd additionally want qemu-efi-arm (/usr/share/AAVMF/AAVMF32_CODE.fd).

env: block at ci.yml:16-24 deliberately sets no RUSTFLAGS, with a long comment on why.

The clippy steps at 140-154 are hand-written per (crate × arch) — adding ARM32 means two more steps there plus the three target lists.

8. The loader (boot/)
Own bindings, not the uefi crate. boot/Cargo.toml deps are only ferrix-bootinfo, ferrix-elf, ferrix-paging. boot/src/uefi/mod.rs:1-14 states the rationale; every struct is repr(C) with all fields in spec order including unused ones (declared as usize placeholders).

efi_main — boot/src/main.rs:54-55:


#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(image: Handle, system_table: *mut SystemTable) -> Status
efiapi is what makes this per-arch-transparent today; on ARM32 the UEFI calling convention is AAPCS and extern "efiapi" has no ARM32 lowering in rustc.

Flow — boot() (main.rs:83-155):

Services::new(image, system_table) (86), arch::prepare_cpu() (87)
stage_kernel (89) → load::read_kernel_file(services, KERNEL_PATH) where KERNEL_PATH = "/FERRIX/KERNEL.ELF" (main.rs:41); path is widened to UCS-2 with / → \ by widen() (services.rs:490-510)
LoaderMemory::new (90) — 2 MiB page-table pool, TABLE_POOL_BYTES at load.rs:19
Three allocations (92-106): boot stack (BOOT_STACK_SIZE = 64 KiB), boot info (BOOT_INFO_BYTES = 64 KiB, main.rs:46), memory-map buffer
highest_ram_address (111, 197-203)
load::build_address_space (118)
write_boot_info (126)
console::shutdown() (129), leave_firmware (130, 256-265 — the fetch-map/exit/retry-once dance)
record_memory_map (131, 271-297) — copies descriptors into the info area at REGIONS_OFFSET = PAGE_SIZE and sorts by base
finish_boot_info (132, 300-311) — patches regions to the virtual PHYSMAP_BASE + phys + REGIONS_OFFSET
Four arch::clean_dcache calls (136-139): table pool, kernel image, info area, stack
arch::enter_kernel(Handoff { ... }) (144-154)
Memory map → MemRegion — load::describe (load.rs:290-317), a numeric match on UEFI types: 7 → Usable, 3|4 → Usable (boot services code/data), 1|2 → Loader, 9 → AcpiReclaim, 10 → AcpiNvs, 11|12 → Mmio, 8 → Defective, 0x8000_0000..=0x8000_0004 → Kernel/PageTables/BootStack/BootInfo/Initrd. The 0x8000_* types are the loader's own, defined at boot/src/uefi/tables.rs:41-53. Walking uses firmware's descriptor_size stride, never size_of::<MemoryDescriptor>() (services.rs:94-107, warning at tables.rs:56-59).

BootInfo fill — main.rs:206-250. Config-table GUIDs looked up (main.rs:237-241), all declared in boot/src/uefi/tables.rs:

ACPI_20_GUID = 8868e871-e4f1-11d3-bc22-0080c73c8881 (tables.rs:80-85)
ACPI_10_GUID = eb9d2d30-2d88-11d3-9a16-0090273fc14d (tables.rs:88-93) — fallback only
DEVICE_TREE_GUID = b1b621d5-f19c-41a5-830b-d9152c69aae0 (tables.rs:96-101) → dtb rsdp = ACPI 2.0 or else 1.0 or else 0; dtb = DTB table or 0. ttbr0_phys is set only when arch::SEPARATE_IDENTITY_TABLE (main.rs:227-231). initrd_phys/len and cmdline are currently 0/null.
Protocols used (4, all in boot/src/uefi/protocols.rs):

Protocol	GUID	Line	Used for
SimpleTextOutput (con_out, from the system table, no GUID lookup)	—	17-30	loader console → serial under -display none
LoadedImage	5b1b31a1-9562-11d2-8e3f-00a0c969723b	37-42	device_handle = the volume the loader came from
SimpleFileSystem	964e5b22-6459-11d2-8e39-00a0c969723b	68-73	open_volume
FileProtocol	(returned, no GUID)	90-114	open/close/read/get_position/set_position
GraphicsOutput	9042a9de-23dc-4a38-96fb-7aded080516a	126-131	optional framebuffer
Boot services called: allocate_pages, get_memory_map, handle_protocol, locate_protocol, exit_boot_services (declared tables.rs:113-196; free_pages, allocate_pool, free_pool, stall are declared but unused). File size is obtained by seek-to-FILE_POSITION_END + get_position rather than FileInfo (services.rs:369-387).

Address space — load::build_address_space (load.rs:180-222). PageEncoding is selected in boot/src/arch/mod.rs:24-28:


#[cfg(target_arch = "x86_64")]   type PageEncoding = ferrix_paging::x86_64::X86_64;
#[cfg(target_arch = "aarch64")]  type PageEncoding = ferrix_paging::aarch64::AArch64;
Three mappings, no more:

Identity map map_identity (load.rs:232-253): VA 0 → PA 0 for ram bytes, RWX (the only RWX mapping in the tree), non-global, into identity_root.
Direct/physmap: VirtAddr(PHYSMAP_BASE) → PhysAddr(0) for ram bytes with MapFlags::KERNEL_DATA (never executable) — load.rs:202-212. ram is rounded up to Level2::SPAN = 2 MiB (load.rs:199, 225-229). Physmap size = all of RAM, i.e. highest_ram_address(); PHYSMAP_BASE = 0xFFFF_8000_0000_0000 (libs/bootinfo/src/lib.rs:48).
Kernel segments map_segment (load.rs:256-287): per-PHDR flags from PF_W/PF_X, global: true.
SEPARATE_IDENTITY_TABLE (boot/src/arch/mod.rs:36-40): false on x86-64 (one PML4), true on AArch64 (TTBR0 vs TTBR1).

Handoff — boot/src/arch/mod.rs:47-60: root_table, identity_table, entry, stack_top, boot_info (all u64). Filled at main.rs:145-153 with stack_top = PHYSMAP_BASE + stack.address + stack.len and boot_info = PHYSMAP_BASE + info_area.address (both virtual, via the direct map).

Per-arch enter_kernel:

x86-64 (boot/src/arch/x86_64.rs:104-135): debug_assert_eq!(identity_table, root_table); asm cli; mov cr3, {root}; mov rsp, {stack}; xor rbp, rbp; jmp {entry} with in("rdi") handoff.boot_info (SysV arg 0), options(noreturn).
AArch64 (boot/src/arch/aarch64.rs:150-220): msr daifset, #0xf; dsb sy; isb → clear SCTLR_EL1.{M,C,I} → msr mair_el1/tcr_el1/ttbr0_el1/ttbr1_el1 → tlbi vmalle1; dsb nsh; isb → set SCTLR bits back → mov sp, x5; mov x29,xzr; mov x30,xzr; br x6, with in("x0") handoff.boot_info (AAPCS64 arg 0). Every operand bound to a named register because options(noreturn) forbids outputs; x7 claimed as scratch.
prepare_cpu:

x86-64 (x86_64.rs:28-48): set IA32_EFER.NXE (bit 11) and CR4.PGE (bit 7), before CR3 — comment at 24-27 explains bit 63 is reserved until NXE.
AArch64 (aarch64.rs:67-73): only checks CurrentEL; 1 => Ok, 2 => Err("firmware handed off at EL2; the loader only supports EL1"). tcr_el1() built field-by-field at aarch64.rs:31-49 (T0SZ/T1SZ = 16 → 48-bit, TG0=0b00, TG1=0b10, IPS from ID_AA64MMFR0_EL1.PARange capped at 5).
clean_dcache: x86-64 is a const fn no-op (x86_64.rs:52); AArch64 (aarch64.rs:81-112) reads CTR_EL0 DminLine, loops dc civac, then dsb sy; isb.

Console: boot/src/console.rs (132 lines) buffers 256 chars, widens to UCS-2, unsafe impl Sync justified by UEFI single-threadedness.

9. boot/Cargo.toml, kernel/Cargo.toml
Both have [lints] workspace = true (enforced by rule 5 of check-crate-layering.sh:141-160). No build.rs anywhere in the repo (verified). No external crates.io dependencies in either — boot: ferrix-bootinfo, ferrix-elf, ferrix-paging; kernel: ferrix-acpi, ferrix-bootinfo, ferrix-frame, ferrix-heap, ferrix-paging, ferrix-sync, ferrix-vma. Both declare [[bin]] explicitly. xtask/Cargo.toml has a deliberately empty [dependencies] with a comment saying so.

Workspace Cargo.toml: resolver = "3", edition 2024, rust-version = "1.97", default-members = ["xtask"] (line 34), workspace-wide [workspace.lints.rust]/[workspace.lints.clippy] starting line 65.

ARM32-relevant lib facts: libs/elf is an ELF64-only reader (libs/elf/src/lib.rs:1, 39, 97-117 — ELFCLASS64, EHDR_SIZE = 64, NotElf64 error); constants are EM_X86_64 = 62, EM_AARCH64 = 183 (lines 50-52) — EM_ARM = 40 is absent. libs/paging assumes ENTRIES = 512, VIRT_BITS = 48, u64 descriptors, 4-level walk (libs/paging/src/lib.rs:46-52, Encoding trait at 281-323) — ARM32 LPAE is 3-level with 64-bit descriptors (workable), short-descriptor is 32-bit (not). libs/bootinfo Arch is #[repr(u32)] { X86_64 = 0, AArch64 = 1 } with elf_machine() at lines 118-142; KernelEntry = extern "C" fn(*const BootInfo) -> ! at line 566; PHYSMAP_BASE/KERNEL_VIRT_BASE/KERNEL_VMAP_BASE are all 64-bit-canonical addresses with const_assert-style checks at lines 81-93.

10. kernel/linker/kernel.ld (full, 73 lines) and _start
Full content is quoted in my read above; the load-bearing parts:


ENTRY(_start)
KERNEL_VIRT_BASE = 0xffffffff80000000;   /* must equal libs/bootinfo */
PHDRS { text PT_LOAD FLAGS(5); rodata PT_LOAD FLAGS(4); data PT_LOAD FLAGS(6); }
SECTIONS {
    . = KERNEL_VIRT_BASE;  __kernel_start = .;
    .text : ALIGN(4096) { *(.text.entry) *(.text .text.*) } :text
    .rodata : ALIGN(4096) { *(.rodata .rodata.*) *(.srodata .srodata.*) } :rodata
    .data : ALIGN(4096) { *(.data .data.*) *(.sdata .sdata.*) *(.got) *(.got.plt) } :data
    .bss (NOLOAD) : ALIGN(4096) { __bss_start = .; ... __bss_end = .; } :data
    __kernel_end = .;
    /DISCARD/ : { *(.eh_frame*) *(.note*) *(.comment) *(.interp)
                  *(.dynamic) *(.dynsym) *(.dynstr) *(.hash) *(.gnu.hash)
                  *(.rela) *(.rela.*) *(.gcc_except_table*) }
}
One script for both architectures (header comment lines 1-8) — the address space layout is deliberately identical. The 0xffffffff80000000 base is impossible on 32-bit ARM: this file, KERNEL_VIRT_BASE in libs/bootinfo/src/lib.rs:67, and the loader's hard check if low != KERNEL_VIRT_BASE { return Err("the kernel is not linked at the address the loader maps it to") } (boot/src/load.rs:116-120) are all coupled.

_start — kernel/src/main.rs:46-48:


#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
extern "C" fn _start(boot_info: *const BootInfo) -> !
.text.entry is placed first in .text by the script, so the entry is at __kernel_start. The signature is pinned by ferrix_bootinfo::KernelEntry (libs/bootinfo/src/lib.rs:566) so a mismatch is a type error. Registers carrying it: rdi on x86-64 (boot/src/arch/x86_64.rs:131), x0 on AArch64 (boot/src/arch/aarch64.rs:208); the stack arrives in rsp/sp from Handoff::stack_top and the frame pointer (rbp/x29,x30) is zeroed to terminate backtraces. First thing _start does is info.validate() (main.rs:55-61) before touching anything; failure → arch::halt() with no console.

kernel/src/arch/mod.rs (27 lines) is the facade: two #[cfg(target_arch)] mod declarations and two pub(crate) use lists of 24 identical symbols each (lines 15-27) — that re-export list is the exact contract a third architecture must satisfy.

11. scripts/ — all seven
Script	Gates
check-asm-budget.py (253)	Assembly only in allow-listed files, each under its max_lines, no stale entries, tree total under max_total_lines, ratio under max_ratio.
check-unsafe-audit.py (210)	// SAFETY: on every unsafe {} and unsafe impl; /// # Safety on every unsafe fn (except trait-method impls). Prints unsafe-block count per crate.
check-panic-audit.py (152)	Every #[expect] of a panic lint carries a reason starting AUDIT:; #[allow] is forbidden for these lints.
check-crate-layering.sh (165)	5 rules: boot↮kernel, libs/* depends on neither, no arch::x86_64::/arch::aarch64:: outside kernel/src/arch/, no cfg(target_arch) outside {kernel,boot}/src/arch/, every member inherits [lints] workspace = true.
check-line-endings.py (160)	No CRLF, and no lone CR, in text files. --fix rewrites CRLF only.
seed-fuzz-corpus.py (142)	Not a gate — regenerates fuzz/corpus/elf_parse/ seeds.
asm-allowlist.json (70)	Data for check-asm-budget.py.
asm-allowlist.json entry format (scripts/asm-allowlist.json:39-43):


{
  "file": "boot/src/arch/x86_64.rs",
  "max_lines": 30,
  "reason": "The loader's last instructions. Writing CR3 replaces the address space..."
}
Top-level knobs: max_total_lines: 800 (line 17), max_ratio: 0.06 (24), target_ratio: 0.001 (31). Keys beginning // are prose comments (the JSON has no comment syntax). Current six entries: boot/src/arch/x86_64.rs (30), boot/src/arch/aarch64.rs (60), kernel/src/arch/x86_64/cpu.rs (100), kernel/src/arch/aarch64/cpu.rs (100), kernel/src/arch/x86_64/trap.rs (90), kernel/src/arch/aarch64/trap.rs (90).

Budget mechanism (check-asm-budget.py):

ROOTS = ("kernel", "boot", "libs", "xtask") (line 46) — same four roots for the unsafe and panic audits (check-unsafe-audit.py:41, check-panic-audit.py:52). Note xtask is in scope for all three; fuzz/ and scripts/ are not.
ASM_MACRO regex (line 50) matches global_asm|naked_asm|asm with optional core::arch:: prefix; balanced() (61-86) finds the macro body; a body line counts only if it contains a " (ASM_TEXT, line 53) — so operand lines like in(reg) x, are free.
rust_lines (99-101) excludes blank/comment/attribute lines via RUST_SKIP (line 56), so the ratio isn't flattered by comment density.
.s/.S/.asm files are counted wholesale by standalone_lines (104-110) — there are none today.
Four rules in check_allowlist (143-167) + rule 4 and the ratio backstop in main (189-203).
Scopes: check-unsafe-audit.py scans **/*.rs under the four roots, skipping target. check-panic-audit.py scans the same but also skips any path with tests in its parts (check-panic-audit.py:122-125) — integration tests are their own crates.
12. docs/ASSEMBLY.md and the line-count claim
Structure of an entry (docs/ASSEMBLY.md:35-61) — prose sections, then Markdown tables grouped by architecture:

## The test for admission (7-19): the blockquote rule — "The construct is defined by what the machine does to registers around it, before or after the first instruction of a Rust function could run." Plus: "'Convenient in assembly' is not a reason."
## Boot: none (21-33): the claim that UEFI removes bootstrap assembly on both arches.
## The list (35-61): ### Both architectures (40-45), ### x86-64 (47-53), ### AArch64 (55-60) — each a two-column | Site | Why | table. A new ### ARMv7-A section with its own table would slot in after line 60; the matching JSON entries go in the same commit (stated at line 37-38).
## The budget (62-74): explains max_total_lines is absolute, and why the percentage is a trend not a gate.
The count claim. README.md:73 says:

The assembly that does exist — 107 lines, 99.56% Rust — is confined to constructs the machine defines…

This is hand-written prose in the README and currently stale. Nothing regenerates it. The live number comes only from running the script; I ran it read-only:


asm-budget: 115 lines of assembly (800 allowed), 25448 lines of Rust
            99.5501% Rust  (backstop 94.0000%, goal 99.9000%)
       40     /60  boot/src/arch/aarch64.rs
       28    /100  kernel/src/arch/aarch64/cpu.rs
       25    /100  kernel/src/arch/x86_64/cpu.rs
       16     /30  boot/src/arch/x86_64.rs
        3     /90  kernel/src/arch/aarch64/trap.rs
        3     /90  kernel/src/arch/x86_64/trap.rs
So it is 115 lines / 99.5501% today, not 107 / 99.56%. The only automated surfacing is CI's assembly job (ci.yml:44-53), which pipes python3 scripts/check-asm-budget.py into $GITHUB_STEP_SUMMARY. The formatting of the printed line is at check-asm-budget.py:222-235, and --json (205-220) gives a machine-readable form if you ever want to generate the README figure.

Summary of everything that must change for ARM32
Enumerations to extend: Arch in xtask/src/paths.rs:10 (+7 methods +2 tests), Arch in libs/bootinfo/src/lib.rs:118-142 (#[repr(u32)], needs Arm = 2 and elf_machine() = 40), the arch match in xtask/src/qemu.rs:182-210, the firmware name/path tables in paths.rs:133-215.

Hardcoded target lists (5 places): rust-toolchain.toml:10-18; xtask/src/check.rs:76; .github/workflows/ci.yml:123, :189, :227 (plus 4 more clippy steps at 140-154 and two matrix.arch lists at 182/220).

Blocked on the missing UEFI target: extern "efiapi" (boot/src/main.rs:55), cargo.rs's expectation of a .efi artifact, Arch::removable_boot_name(), and Arch::loader_target(). A armv7a-none-eabi loader would need either a custom JSON target with is-like-windows/PE output, or a U-Boot/DTB handoff path that bypasses boot/src/uefi/ entirely — in which case boot/src/services.rs (510 lines of UEFI) would need a sibling.

64-bit assumptions: KERNEL_VIRT_BASE = 0xffffffff80000000 in kernel/linker/kernel.ld:14 + libs/bootinfo/src/lib.rs:67 + the loader's hard equality check at boot/src/load.rs:116; PHYSMAP_BASE; libs/elf being ELF64-only; libs/paging's VIRT_BITS = 48 / 4-level / u64 descriptors.

Gates to feed: a new ### ARMv7-A section in docs/ASSEMBLY.md, matching entries in scripts/asm-allowlist.json (ARM32 will need real bootstrap assembly — SVC-mode setup, LPAE TTBR0/TTBR1 + TTBCR, cache maintenance — so the 800-line cap and the "Boot: none" claim in both ASSEMBLY.md:21-33 and README.md:69-71 become false), plus the 24-symbol facade re-export list in kernel/src/arch/mod.rs:15-27.