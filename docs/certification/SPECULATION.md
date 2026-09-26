# Side-channel defences

The argument for F-31's side-channel half: which speculative-execution
defences the item carries, per architecture, why that set and not another, how
one build switch takes all of them out, and what they cost. The layout
randomisation half of F-31 (KASLR) is §6, and is not built.

The item's claim is isolation between mutually distrusting processes
([SECURITY-TARGET.md](SECURITY-TARGET.md) T.MEMORY). A processor that runs
ahead of a branch can read what the page tables forbid and leave the answer in
a cache, where a program can time it; no page table, SMEP or PAN stops that.
So the defences below belong to O.ISOLATE as much as the page tables do, and
[VULNERABILITY-ANALYSIS.md](VULNERABILITY-ANALYSIS.md) no longer lists side
channels as out of scope.

---

## 1. One switch, two settings

```
cargo xtask <anything that builds a kernel> --mitigations on    # the default
cargo xtask <anything that builds a kernel> --mitigations off
```

**`on` is the certified reference configuration and the default.** It is what a
plain `cargo build -p ferrix-kernel` makes, what every gate boots, and the only
setting [SAFETY-MANUAL.md](SAFETY-MANUAL.md) AoU-8 and AoU-11 cover. **`off`
is an explicit opt-out**, for measuring what the defences cost and for a
machine whose owner has decided it runs nothing that distrusts anything else.

`off` builds with `--cfg ferrix_mitigations_off`, and that one `cfg` is the
whole of the configuration space. It is not a Cargo feature, deliberately: the
reference configuration's claim of zero Cargo features in `kernel/` and `boot/`
stands, and a `cfg` that only `xtask` sets, with two values and one of them the
default, is a configuration an evaluator can enumerate in a sentence.

| Where | What the switch does |
|---|---|
| `xtask/src/cargo.rs` | `--mitigations off` adds `--config target.<triple>.rustflags=["--cfg","ferrix_mitigations_off"]` to every kernel build, and builds into `target/mitigations-off` so neither setting's cache is thrown away for the other's. A `--config` array is appended to `.cargo/config.toml`'s, where `RUSTFLAGS` would have replaced the linker flags the image needs. |
| `xtask/src/check.rs` | `cargo xtask check` runs clippy over the kernel for all three architectures in both settings, so code only one setting compiles cannot rot in the other. |
| `Cargo.toml` | declares the `cfg` to `unexpected_cfgs`, so a misspelt one is a warning. |
| `kernel/src/arch/speculation.rs` | `HARDENED`, the one constant every defence tests. |
| `libs/sync/src/nospec.rs` | the libraries' clamp, the identity when the `cfg` is set. |

**What `off` removes:** every clamp (they become the identity after the
ordinary bounds check), every write to a speculation control, every switch
barrier, and the extra instructions on the entry and exit paths, which the
assembler leaves out (`.if` on a constant). **What it keeps:** SMEP, SMAP and
PAN, which are memory protection rather than side-channel defences (F-32), and
every architectural bounds check. The boot log says which build it is:

```
  cpu      speculation defences off: built with --mitigations off
  cpu      speculation defences off on 4 processors, as built
```

---

## 2. What `on` does everywhere: bounded indices (Spectre v1)

A program chooses integers the kernel then indexes with. A processor that
predicts the bounds check passed loads past the table before the check
resolves. The defence is Linux's `array_index_nospec`: after the ordinary
check, clamp the index with arithmetic the processor does not predict, so the
mispredicted path reads slot zero.

| Site | Index | Clamp |
|---|---|---|
| `arch/*/mod.rs` `decode_syscall` | Linux system call number, into a `match` the compiler makes a jump table of | `arch::nospec_index`, to the table's end (`ferrix_linux_abi::nr::X86_64_END` and kin; a host test holds each end to its table) |
| `syscall/native.rs` `decode` | native call number | the same, to the native range |
| `libs/objects/src/table.rs` | handle slot, in `get`, `remove`, `replace` | `ferrix_sync::nospec::bounded` |
| `libs/vfs/src/fd.rs` | descriptor slot | the same |
| `syscall/uaccess.rs` `user_address` | a user address, before the software table walk that follows it into the direct map | `arch::nospec_below`, to the top of the user half |

The kernel's clamp is a line of assembly per architecture
(`arch/<arch>/speculation.rs`): `cmp`/`sbb` on x86-64, `cmp`/`csel`/`csdb` on
AArch64, `cmp`/`movhs`/`csdb` on ARMv7-A. It has to be assembly: written in
Rust, the compiler knows the index is in bounds inside the `if` and folds the
clamp away, and `csdb` — which forbids an Arm processor to use a *predicted*
value of the select — has no Rust spelling.

The libraries may contain neither assembly nor `cfg(target_arch)`, so their
clamp is Rust with both operands passed through `core::hint::black_box`, which
stops the fold. The kernel's build was disassembled to confirm what comes out:
in `syscall::fd::file`, `cmp`/`cmovae` on x86-64 and `cmp`/`csel` on AArch64,
with no branch between the clamp and the load. What it lacks is `csdb`: a core
that predicted the *value* of a conditional select would see through it. The
kernel's own sites carry it, and §9 lists the libraries' two as a residual.

This is not an exhaustive gadget audit. It covers every place a program's
integer becomes an index into kernel memory at the system call boundary —
number, handle, descriptor, address. Tables reached deeper inside a call
(signal numbers, `epoll` sets, `/proc` lookups) were not audited, and are listed
in §9.

---

## 3. x86-64

Decided once from `CPUID` and `IA32_ARCH_CAPABILITIES`, applied on every
processor, read back on each (`arch/x86_64/speculation.rs`).

| Hazard | `on` does | Condition | Vendor basis |
|---|---|---|---|
| Spectre v1 | clamps (§2); `lfence` after the conditional `swapgs` on interrupt entry; a program's registers zeroed on `SYSCALL` entry and on every trap from ring 3 | always | Intel SA-00088, CVE-2019-1125 (SWAPGS) |
| Spectre v2, program → kernel | enhanced IBRS (Intel, `IBRS_ALL`), else AutoIBRS (AMD, `EFER.AIBRSE`), else IBRS on AMD parts that say it may be left on (`CPUID 0x8000_0008.EBX[16]`) | whichever the processor offers | Intel *Speculative Execution Side Channel Mitigations*; AMD *Software Techniques for Managing Speculation* |
| Spectre v2, program → program | `IBPB` and a 32-entry return stack refill when a processor switches to another program's address space; `STIBP` unless enhanced IBRS already covers the sibling thread | `IBPB`/`STIBP` where offered; the refill always | as above; the refill also covers SpectreRSB |
| Speculative store bypass | `SSBD`, through `IA32_SPEC_CTRL` or AMD's `VIRT_SPEC_CTRL` | unless `SSB_NO` | Intel SA-00115, AMD SSBD whitepaper |
| MDS | `VERW` on every return to ring 3 | Intel, `MD_CLEAR`, no `MDS_NO` | Intel SA-00233 |
| Meltdown, L1TF | **none** — reported | — | §6 |

**Why these, and why always on.** Linux enables most of these conditionally —
`SSBD` and `IBPB` only for programs that ask, by `prctl` or `seccomp`. The item
cannot know which of its programs run a JIT or a sandbox, and its one claim is
that none of them can read another. So `on` takes the strict form of each and
the cost is measured (§8) rather than assumed.

**The switch barrier is per processor and per root.** `install_user_root`
calls `speculation::entered_space`, which issues the barrier only when the
processor last ran a *different* address space: a thread returning after the
idle loop or a kernel thread costs nothing. A root table freed and reused for a
new process would look like the old one, so `prepare_user_root` wipes a new
root from every processor's record first.

**No retpolines.** rustc 1.97.1 accepts
`-C target-feature=+retpoline-indirect-calls` only with *"this was previously
accepted by the compiler but is being phased out; it will become a hard error
in a future release"* (rust-lang #116344); the supported spelling,
`-Zretpoline`, is nightly-only. And the kernel links the precompiled `core` and
`alloc`, which would stay unmitigated without `-Zbuild-std`, also unstable. So
the item relies on the IBRS forms above, and a processor that offers none of
them is outside the reference configuration (AoU-11). Such a processor — an
Intel part before enhanced IBRS, an AMD part with neither AutoIBRS nor the
always-on IBRS bit — would need IBRS written on every kernel entry and exit,
which is not built.

---

## 4. AArch64

Decided from `MIDR_EL1` and the ID registers, and from what firmware says
through SMCCC, asked only after `PSCI_FEATURES` has said SMCCC is there — an
unimplemented `hvc` on a machine with no EL2 is an undefined instruction
(`arch/aarch64/speculation.rs`).

| Hazard | `on` does | Condition |
|---|---|---|
| Spectre v1 | clamps with `csdb` (§2) | always |
| Spectre v2 | `SMCCC_ARCH_WORKAROUND_1` when a core switches address space | no `CSV2`, and firmware offers it |
| Spectre-BHB | a loop of taken branches on every vector entry from EL0 — 8 on Cortex-A57/A72, 24 on A76/A77/N1, 32 on A78/X1/A710/X2/N2/V1, 38 on A715/A720, 132 on X3/V2 (Linux's figures) | the core is on Arm's list and lacks `ECBHB` |
| Speculative store bypass | `SCTLR_EL1.DSSBS` cleared, so EL1 runs with `PSTATE.SSBS` clear from every exception on, and `MSR SSBS` where the core has it; a program starts with it clear. Without `SSBS`, `SMCCC_ARCH_WORKAROUND_2` on each core | `SSBS`, or firmware offers workaround 2 |
| Meltdown | **none** — reported | §6 |

**The reference CPU**, QEMU's `cortex-a72`, has neither `CSV2` nor `SSBS`, and
QEMU's firmware offers neither workaround, so its boot log reads:

```
  cpu      speculation defences: clamped indices, BHB loop on entry
  cpu      speculation exposure: Spectre v2 NOT covered: no CSV2, and firmware offers no ARCH_WORKAROUND_1 (AoU-11); Spectre-BHB covered; store bypass NOT covered: no SSBS, and firmware offers no ARCH_WORKAROUND_2 (AoU-11); Meltdown not affected
```

That is honest for real Cortex-A72 silicon without TF-A's workarounds, and it
is what AoU-11 turns into an obligation: real hardware must run firmware that
implements them. Under TCG nothing speculates, so the test platform is not a
counter-example. `FERRIX_ARM_CPU=max` boots the `SSBS` path (`DSSBS` written and
read back on four cores) and reports Spectre v2 not affected by `CSV2`.

Arm lists the Cortex-A57 and A72 as affected by variant 3a (a system register
read speculatively); the kernel keeps no secret in a system register EL0 can
name, and does nothing for it.

---

## 5. ARMv7-A

`arch/armv7a/speculation.rs`. The reference core — QEMU's `cortex-a7` and the
STM32MP157's — is an in-order design Arm lists as affected by **none** of the
variants, so `on` there is the clamp alone, whose `csdb` the A7 executes as a
hint. For the cores Arm lists as affected by Spectre v2, which the kernel boots
on but the reference configuration does not include, a switch of address space
issues `BPIALL` (Cortex-A8, A9, A12, A17) or `ICIALLU` (Cortex-A15,
Brahma-B15); on the A8 and A15 those work only if secure firmware set
`ACTLR.IBE`, which the boot log reports. Spectre-BHB on the affected ARMv7
cores is not handled.

---

## 6. KPTI and KASLR: evaluated, not built

**KPTI** — a user page table with the kernel unmapped but for an entry
trampoline — is the defence against Meltdown and L1TF. It is needed only on
Intel parts without `RDCL_NO` (before roughly 2019) and on the Cortex-A75. None
is in the reference configuration: QEMU's `qemu64` reports AMD, the KVM gate
runs on an AMD host, and Arm lists the Cortex-A72 and A7 as unaffected. So
KPTI is not the most fitting defence for the reference CPUs, and it is not
built. A processor that needs it says so at boot — *"Meltdown EXPOSED: no
RDCL_NO, and KPTI is not built (AoU-11)"* — and AoU-11 excludes it. Building
it would need, in order:

1. a second root per address space on x86-64 holding the user half, the entry
   and exit trampolines, the IDT, GDT, TSS and per-CPU entry stacks, and nothing
   else of the kernel's (`mm::share_kernel_slots` shares all 256 today);
2. a `CR3` switch as the first act of `ferrix_syscall_stub`, the trap stubs and
   the paranoid entry, and back before `sysretq`/`iretq`, with PCID so the switch
   does not flush the TLB twice per call;
3. on AArch64, `TTBR1_EL1` swapped to a trampoline table on exit to EL0 and a
   trampoline vector page mapped in both;
4. the `user/check.rs` walks and the W^X sweep taught that the user root no
   longer maps the kernel.

**KASLR** — the kernel at a random virtual address each boot — would turn a
kernel pointer leak from an address into a guess. It is not built, and nothing
of it is half built. What it needs:

1. the kernel linked position-independent (`-C relocation-model=pie`) rather
   than static, which `.cargo/config.toml` pins today, and
   `kernel/linker/kernel.ld`'s fixed `KERNEL_VIRT_BASE` made a default;
2. each loader in `boot/` choosing a slot from the firmware RNG
   (`EFI_RNG_PROTOCOL` where offered), mapping the kernel there, and applying
   its `R_*_RELATIVE` relocations before the jump. The loaders are themselves
   relocated by firmware — the ARMv7-A one is a static PIE whose relocations
   `xtask/src/pe.rs` turns into PE base relocations — and the kernel is not;
3. `BootInfo` carrying the chosen slide, and every consumer of the fixed
   address — `KERNEL_VIRT_BASE` in `libs/bootinfo`, the loader's check of the
   image against it, the backtrace's image bounds, the symboliser in
   `xtask/src/symbolize` — reading it;
4. the direct map and the vmap arena randomised too, or a leak of either gives
   the kernel's slide away.

Both are recorded in F-31 as what remains.

---

## 7. What the boot says and checks

Once, on the boot processor, before the second processor starts:

```
  cpu      speculation defences: clamped indices, SWAPGS fence, entry registers cleared, AutoIBRS, STIBP, SSBD, predictor barrier on switch, RSB fill on switch
  cpu      speculation exposure: Spectre v2 into ring 0 covered; between programs covered; store bypass covered; Meltdown not affected (AMD)
```

(`x86_64 --accel kvm` on an AMD Ryzen 9 9900X host.) Every secondary applies
the same plan as it starts and records what it applied. After stage 7's
programs, `arch::check_speculation` (`arch/speculation_check.rs`) requires:

* the clamp to return an index inside its bound unchanged, and — run on its
  own, as a misprediction runs it — one outside as zero (unchanged when built
  `off`);
* every running processor to have recorded, and nothing it wrote to
  `IA32_SPEC_CTRL`, `EFER`, `VIRT_SPEC_CTRL` or `SCTLR_EL1` to have failed to
  read back;
* a switch barrier to have been issued where the plan has one;
* on x86-64, `VERW` to take its operand, on machines whose exit path never
  runs it;
* on AArch64, the entry loop's count to agree with the plan.

```
  cpu      speculation defences read back on 4 processors, 207 switch barriers
```

A failure is FX-0307. Processors that applied less than the boot processor —
a big.LITTLE machine's little cores — are counted and named, not failed.

---

## 8. What it costs

Measured on the KVM gate, where the controls are real: x86-64 under
`--accel kvm` on an AMD Ryzen 9 9900X (Zen 5) host, which gives the guest
AutoIBRS, STIBP, SSBD and IBPB. Each figure is the median of eight
`test-vfs` boots per setting, alternating, on a shared and loaded host (load
average 6 to 9), with two commands added for the measurement and not committed:

| Workload | `off` | `on` | Cost |
|---|---:|---:|---:|
| `dd if=/dev/zero bs=1 count=1000000 >/dev/null` — two million `read`/`write` calls | 1.015 s | 1.025 s | **+1.0%** |
| 1,000 × fork, exec `/bin/true`, wait — every one a switch barrier | 2.360 s | 2.425 s | **+2.8%** |

The boot's own cost lines, three boots each, did not move beyond their noise:
stage 7's `handlers` 103 → 104 ms, `procs` 13 → 13, `fork` 177 → 178, `execve`
37 → 38 (medians), and time to `FERRIX-BOOT-OK` varied more between boots of
one setting than between settings.

Under TCG — the default gates, and every Arm figure — the controls do not
exist and nothing speculates, so a timing there measures QEMU. What `on` adds
there is instructions: on x86-64 two per clamp, fourteen `xor`s and an `lfence`
per entry, a thirty-two-call refill per switch; on AArch64 a twelve-instruction
entry sequence with eight loop iterations on the A72; on ARMv7-A three
instructions per clamp.

---

## 9. What this does not cover

* **KPTI and KASLR** (§6).
* **CET** — neither indirect branch tracking nor shadow stacks. Both need
  compiler support (`-Z cf-protection`, nightly) and loader cooperation.
* **Cache timing between processes.** Two programs that share a cache can
  still time each other's accesses (Prime+Probe), and two that share a page —
  a file's page cache, a shared VMO — can Flush+Reload it. Nothing here
  partitions a cache; cache colouring or core partitioning is the defence, and
  it is an integrator's (AoU-11).
* **Sibling hyperthreads on an MDS-affected part.** `VERW` clears buffers on
  the way out of the kernel, not while a sibling runs. Such a part is to run
  with SMT off (AoU-11).
* **An exhaustive Spectre v1 audit.** §2's sites are the system call boundary.
  No tool at the pinned toolchain finds gadgets in Rust, and the deeper tables
  were not read for them.
* **`csdb` in the libraries' clamps** (§2), the handle and descriptor tables.
* **Spectre-BHB on affected ARMv7-A cores**, and variant 3a on the A57/A72.
* **The paranoid entry's registers.** An NMI or `#MC` taken in ring 3 on
  x86-64 does not clear the program's registers the way an ordinary trap does
  (a ring-3 `#DB` moves to the ordinary path and does); its return to ring 3
  does run `VERW`, through the shared restore.
