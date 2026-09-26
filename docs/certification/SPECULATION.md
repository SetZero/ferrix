# Side-channel defences

The argument for F-31: which speculative-execution defences the item carries,
per architecture, why that set and not another, how one build switch takes all
of them out, and what they cost; and the layout randomisation (KASLR) that the
same switch turns on, in §6.

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
| `xtask/src/cargo.rs` | `--mitigations off` adds `--config target.<triple>.rustflags=["--cfg","ferrix_mitigations_off","-C","relocation-model=static"]` to every kernel build, and builds into `target/mitigations-off` so neither setting's cache is thrown away for the other's. A `--config` array is appended to `.cargo/config.toml`'s, where `RUSTFLAGS` would have replaced the linker flags the image needs. |
| `kernel/build.rs` | links the kernel so the loader can move it (§6) unless the `cfg` is set: a static PIE on x86-64 and AArch64, `--emit-relocs` on ARMv7-A. Built `off`, it is the static fixed-address image it always was. |
| `xtask/src/check.rs` | `cargo xtask check` runs clippy over the kernel for all three architectures in both settings, so code only one setting compiles cannot rot in the other. |
| `Cargo.toml` | declares the `cfg` to `unexpected_cfgs`, so a misspelt one is a warning. |
| `kernel/src/arch/speculation.rs` | `HARDENED`, the one constant every defence tests. |
| `libs/sync/src/nospec.rs` | the libraries' clamp, the identity when the `cfg` is set. |

**What `off` removes:** every clamp (they become the identity after the
ordinary bounds check), every write to a speculation control, every switch
barrier, the extra instructions on the entry and exit paths, which the
assembler leaves out (`.if` on a constant), KASLR — the kernel is linked at
its fixed address with the static relocation model, and the loader leaves the
image, the direct map and the vmap arena where they are — and x86-64's UMIP,
which only KASLR needs. **What it keeps:** SMEP, SMAP and
PAN, which are memory protection rather than side-channel defences (F-32), and
every architectural bounds check. The boot log says which build it is:

```
  kaslr    NOT randomised: the kernel is a fixed-address image (--mitigations off); kernel at its link address 0xffffffff80000000
  cpu      descriptor table addresses kept from ring 3: UMIP off, as built
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

Decided by **each core for itself**, as it starts, from its own `MIDR_EL1`
and ID registers and from what firmware says about it through SMCCC
(`arch/aarch64/speculation.rs`). The cores of one machine need not be alike: a
Pixel 7 boots on a Cortex-A55 and starts two A78s and two X1s, and the two
kinds need different things. Only how firmware is reached, and whether it
answers `SMCCC_ARCH_FEATURES` at all, is found once, on the boot processor —
asked only after `PSCI_FEATURES` has said SMCCC is there, because an
unimplemented `hvc` on a machine with no EL2 is an undefined instruction.

| Hazard | `on` does | Condition, per core |
|---|---|---|
| Spectre v1 | clamps with `csdb` (§2) | always |
| Spectre v2 | `SMCCC_ARCH_WORKAROUND_1` when that core switches address space | the core lacks `CSV2`, is not a Cortex-A35, A53 or A55, and firmware answers that *this core* needs the workaround |
| Spectre-BHB | a loop of taken branches on every vector entry from EL0 — 8 on Cortex-A57/A72, 24 on A76/A77/N1, 32 on A78/X1/A710/X2/N2/V1, 38 on A715/A720, 132 on X3/V2 (Linux's figures); one count for the machine, the largest any core needs | the core is on Arm's list and lacks `ECBHB` |
| Speculative store bypass | `SCTLR_EL1.DSSBS` cleared, so EL1 runs with `PSTATE.SSBS` clear from every exception on, and `MSR SSBS` where the core has it; a program starts with it clear. Without `SSBS`, `SMCCC_ARCH_WORKAROUND_2` | the core has `SSBS`, or firmware answers that this core needs workaround 2 |
| Meltdown | **none** — reported | §6 |

**Spectre v2's list of unaffected cores** is Linux's `spectre_v2_safe_list`
(`arch/arm64/kernel/proton-pack.c`, read in the Android common kernel 6.1.157
that the Pixel 7 work builds modules against), for Arm's own parts: the
Cortex-A35, A53 and A55, in-order designs. Their part numbers — `0xD04`,
`0xD03`, `0xD05` — were checked against `ARM_CPU_PART_*` in the same tree's
`arch/arm64/include/asm/cputype.h`, and again in this host's 7.0 kernel
headers, not written from memory. The list's other entries are other
implementers' cores (Broadcom's Brahma-B53, HiSilicon's TSV110, Qualcomm's Kryo
silver parts) and are not built: such a core without `CSV2` is reported NOT
covered unless firmware covers it.

**Firmware is asked per core.** The SMC Calling Convention (ARM DEN 0028D,
issue 1.3, §7.5.2 and §7.6.2) defines `SMCCC_ARCH_FEATURES`' answer about each workaround
per processor: *not supported* is the same on every core, but where firmware
implements the workaround, `0` says the core that asked needs it and `1` that
it does not, and the workaround is then safe, if wasted, on every core. Its
Appendix B suggests exactly this on big.LITTLE: ask on each core and leave out the calls
where the answer is `1`. So each core asks about itself — about workaround 1
only if its own hardware does not settle Spectre v2, about workaround 2 only
if it lacks `SSBS` — and keeps its own switch barrier bit, which
`switch_barrier` reads on the core that is switching. A core with `CSV2`, or on
the list, never pays for a firmware call it does not need; one that needs it
always makes it, whichever core the machine booted on.

**The exposure is said once every processor has started**, a line per kind of
core — the same part, told the same by firmware, having applied the same —
with how many there are. The boot processor alone could not say it: a Pixel 7's
A55 is on neither Arm's Spectre v2 nor its Spectre-BHB list, and its X1s need
the loop. The reference CPU, QEMU's `cortex-a72`, has neither `CSV2` nor
`SSBS`, is not on the Spectre v2 list, and QEMU's firmware offers neither
workaround, so its boot log reads:

```
  cpu      speculation defences: clamped indices, BHB loop on entry
  cpu      speculation exposure: 4 x Cortex-A72: Spectre v2 NOT covered: no CSV2, and firmware offers no ARCH_WORKAROUND_1 (AoU-11); Spectre-BHB covered (8 branches); store bypass NOT covered: no SSBS, and firmware offers no ARCH_WORKAROUND_2 (AoU-11); Meltdown not affected
```

A machine of mixed cores gets a line per kind. Simulated under QEMU's `max`
with the ID registers overridden (a scratch edit, not committed) as a Pixel 7
— a Cortex-A55 boot core, Cortex-X1 secondaries:

```
  cpu      speculation exposure: 1 x Cortex-A55: Spectre v2 not affected (Arm lists this core as unaffected); Spectre-BHB not affected (not on Arm's list); store bypass not affected; Meltdown not affected
  cpu      speculation exposure: 3 x Cortex-X1: Spectre v2 not affected (CSV2); Spectre-BHB covered (32 branches); store bypass covered; Meltdown not affected
```

That is honest for real Cortex-A72 silicon without TF-A's workarounds, and it
is what AoU-11 turns into an obligation: real hardware must run firmware that
implements them. Under TCG nothing speculates, so the test platform is not a
counter-example. `FERRIX_ARM_CPU=max` boots the `SSBS` path (`DSSBS` written and
read back on four cores) and reports Spectre v2 not affected by `CSV2`.

A second simulation gave one secondary a Cortex-A72's registers and firmware's
answer that this core needs workaround 1 (the call itself left out, as QEMU has
none behind `hvc`). That core alone planned the barrier and issued all of
stage 7's 18; the A55 and X1s issued none. The same boot with
`switch_barrier` reading one core's bit for every core — the boot-processor
flag this replaced — failed FX-0307, *"a processor issued a switch barrier its
plan does not have"*.

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

## 6. KASLR, built; KPTI, evaluated and not built

### 6.1 KASLR

**What it is for.** An exploit that ends in a kernel write or a return into
kernel code needs an address: of a function, of a table, of the data it means
to change. With the kernel at the same address every boot, the address is in
the ELF anyone can read. KASLR puts the kernel somewhere new each boot, so the
exploit needs a second bug first, one that discloses where. It does not stop
the first bug; it makes one bug not enough.

**What moves, and how far.** The loader (`boot/src/kaslr.rs`) moves three
regions, each from its own random word, so that learning one gives away one:

| Region | Step | Candidates (512 MiB of RAM) | Bits, x86-64 / AArch64 | Bits, ARMv7-A |
|---|---|---|---:|---:|
| the kernel image, above its link address in its region, never at it | a page (64 KiB on ARMv7-A, see below) | top 2 GiB less the image and a 2 MiB guard; ARMv7-A's top 256 MiB, the same way | **18** | **11** |
| the direct map, anywhere in its region | 1 GiB (2 MiB on ARMv7-A) | 127 TiB region; ARMv7-A's 1.25 GiB | **16** | **8** |
| the top of the vmap arena, where its top-down search starts | 64 KiB | the arena's top 8 GiB; ARMv7-A's top 32 MiB | **17** | **9** |

The bits are the base-two logarithm of the number of places, rounded down
(`ferrix_bootinfo::Slots::bits`), as the boot log prints them; a machine with
more RAM has a larger direct map and a few fewer places for it. For
comparison, Linux on x86-64 moves its image in 2 MiB steps within 1 GiB: 9
bits.

*Why the direct map moves.* It maps every byte of RAM, the kernel's own image
included, and firmware allocates the image at the same physical address every
boot. A direct map that stayed where it was would give the image's data a
fixed address — writable, since the direct map is `KERNEL_DATA` — whatever the
image's own slide. A gibibyte is the step on the 64-bit pair so that the
loader's block mappings keep the shape they had.

*Why the arena moves.* Kernel stacks and device windows are allocated from its
top down; a fixed top gave the first stacks of every boot the same addresses.
The fixed windows below the arena, reserved for early boot's console,
framebuffer and on-demand mapping, do not move. They hold device registers
and pixels, not kernel pointers.

*ARMv7-A's step.* Its precompiled `core` builds addresses with `movw`/`movt`
pairs. A bias that is a multiple of 64 KiB leaves every `movw` as it is and adds
its top half to every `movt`, which is the only way to move those
instructions without decoding their pairing.

**How the image is made movable.** On `--mitigations on`, `kernel/build.rs`
links:

* **x86-64: a static PIE.** Code built for the static relocation model
  addresses the kernel with sign-extended 32-bit absolutes, which no dynamic
  relocation can move, and lld refuses the PIE link. So `.cargo/config.toml`
  builds every x86-64 crate for the PIC model. That is a target-wide flag, so
  it also says `-no-pie`, and only the kernel's build script says `-pie` after
  it: the native programs stay the fixed-address executables the kernel's ELF
  loader takes. 8,947 `R_X86_64_RELATIVE` fixups, 210 KiB of `.rela.dyn`.
* **AArch64: a static PIE, from static-model code.** On AArch64 the static
  model already addresses everything relative to the program counter
  (`adrp`/`add`). Only the words that hold addresses need moving, and lld
  writes them as `R_AARCH64_RELATIVE`. Some are in read-only data, hence
  `-z notext`: the loader patches the image before it maps it. 7,809 fixups,
  183 KiB.
* **ARMv7-A: a fixed-address image with `--emit-relocs`.** It cannot be a PIE.
  The target's prebuilt `core` is compiled for the static model and uses
  `movw`/`movt`, which no dynamic relocation expresses, and the PIE link fails
  (`docs/arm32.md` tried every combination). So the image keeps every
  relocation the linker resolved, in sections beside the ones they patch. The
  loader applies the absolute ones, against a symbol that is an address:
  13,239 `R_ARM_ABS32` and 30,924 `movw`/`movt` pairs. Place-relative ones
  (`CALL`, `JUMP24`, ...) are right wherever the image goes. A relocation
  against an absolute, undefined or null symbol is a number and stays.

`ferrix_elf::Elf::fixups` reads both forms into one stream and refuses
anything it does not know, rather than skipping it: a missed fixup is a
pointer that was never moved. The linker script loads `.rela.dyn` in the
read-only segment, where lld puts every allocated section, and declares no
`PT_DYNAMIC`; the loader finds the table by section header. `flash` strips the
card's ARMv7-A kernel with `--strip-debug`, which keeps the relocations, not
`--strip-all`, which would take them with the symbol table they index.

**Where the randomness comes from.** In order:

1. **`EFI_RNG_PROTOCOL`**, 24 bytes asked for separately from the 32 the
   kernel's generator is seeded with, so a leaked layout says nothing about the
   seed. Every QEMU machine here offers it (xtask attaches virtio-rng), and so
   does U-Boot on a board with a driver for its generator. The STM32MP157 has
   one; whether the DK1's U-Boot build offers it has not been seen in a boot
   log in this work.
2. **`RDRAND`**, on x86-64, where CPUID says so. AArch64's `RNDR` is ARMv8.5,
   which the reference Cortex-A72 lacks; ARMv7-A has no such instruction.
3. **A cycle counter**: the TSC, or the generic timer's virtual count, where
   an ARMv7-A core has one. This is **not KASLR**. Anyone who can estimate
   how long firmware took can narrow it to a few bits. The loader says so
   (`from the cycle counter, which is guessable and so not KASLR`), and the
   kernel reports the layout as `NOT randomised against a local attacker`.

With none of them, or with `nokaslr` on the command line (`CMDLINE.TXT`, or
`cargo xtask run --gdb`, which adds it so that a debugger's symbols are where
the kernel runs), everything stays at its fixed address and the log says
why. A kernel with no fixups — built `off`, or a copy stripped of them — stays
too. The Pixel 7 loader (`bootloaders/pixel7`) applies a PIE's fixups at the
link address and says it does not randomise. It has no tested source of
randomness on the phone, and moving the kernel there has not been tried
without a device session. `nokaslr` is safe to honour: whoever writes the boot
volume can replace the kernel.

**What is printed, and why that is not a leak.** The loader prints, on
firmware's console:

```
  kaslr    kernel at 0xffffffffa3912000, slide 0x23912000, 18 bits from EFI_RNG
  kaslr    direct map at 0xffff9b9900000000, 16 bits; vmap arena top at 0xffffffed57140000, 17 bits
```

and a panic prints `kaslr     slide 0x…` before its backtrace, which is how
`xtask`'s symboliser and `scripts/coverage-report.py` take run-time addresses
back to link-time ones. The attacker in [SECURITY-TARGET.md](SECURITY-TARGET.md)
is a program. The console is not something a program reads back. The
kernel keeps no log (`sys_syslog` returns nothing, `/proc` exposes no kernel
address: `wchan`, `kstkeip` and `kstkesp` are zero), and output to the
console goes to the serial line and the screen, not into anything a program
can read. Who sees the slide is who sits at the serial port or the screen,
and that person could as well write `nokaslr` into `CMDLINE.TXT`. The one path
that crosses is a program that can read the framebuffer the boot console
drew on. That is the display server, which the integrator places, and AoU-11
says so. Linux takes the same position: it prints the offset on a panic.

**What the boot checks.** Stage 1 (`check_layout`, FX-0101):

* the image runs where the loader says. `__kernel_start` as the code computes
  it is `BootInfo::kernel_virt`, which after the fixups is where the image
  really is;
* a loader that says it moved the image did move it. The link address is
  never a slot, so a moved image is never where it was linked;
* a kernel built `on` that arrived as a fixed-address image lost its fixups
  on the way, and is refused;
* every other reason not to move is reported, not failed.

`BootInfo::validate` holds each region to the places the layout allows. Every
`test-boot` requires `moved … from EFI_RNG`, a slide that is not zero and the
kernel's confirmation on `on`. On `off` it requires the fixed image, and with
`nokaslr` the loader's refusal. **`cargo xtask test-kaslr`** boots one image
twice and requires two layouts. The image's slide alone repeats once in 2^18
pairs of boots (2^11 on ARMv7-A), so that is reported, not failed; all three
regions coming back the same fails.

**x86-64: UMIP, or `SIDT` gives it away.** The IDT is a static in the image,
and a program can execute `SIDT` to read its address unless `CR4.UMIP` is set.
`SGDT` likewise. So on `on` the boot processor sets UMIP where CPUID offers it
(secondaries inherit it). `SIDT`, `SGDT`, `SLDT`, `SMSW` and `STR` then fault
in ring 3, as on Linux. xtask asks QEMU for `+umip`, which TCG emulates. A
processor without UMIP is logged as one where SIDT gives the slide away, and
AoU-11 excludes it. The Arm architectures have no equivalent: `VBAR` cannot be
read below EL1 or PL1.

**What KASLR does not do.**

* **It does not withstand a local timing attacker.** Without KPTI (§6.2) the
  kernel is mapped, supervisor-only, in every program's tables. A program can
  find it by timing: prefetch (Gruss et al., CCS 2016), TLB and page-walk
  timing (Hund et al., S&P 2013; Jang et al., DrK, CCS 2016), and on Intel
  even through KPTI's trampoline (EntryBleed, CVE-2022-4543). So KASLR here
  turns a single memory-corruption bug into a need for a disclosure as well.
  It does not hide the layout from a program with a timer. That is why it is
  not part of ASR-1's separation argument and appears in no AoU as a
  condition of it.
* **The physical placement is fixed.** The direct map's slide is independent
  of the image's. A program that learns a direct-map address and knows the
  machine's firmware can still compute the image's alias in the direct map.
  That alias is writable, since the direct map is `KERNEL_DATA`, the image's
  text included. Moving the image in physical memory too (Linux's
  `efi_random_alloc`), or mapping the text's alias read-only, would close
  this. Neither is built.
* **ARMv7-A's 11, 8 and 9 bits are few.** A guess at the image costs the
  attacker a kernel fault, which is a reboot, 2,048 times on average for the
  image alone.
* **No exhaustive audit** of every value the kernel copies out for kernel
  addresses. Handles and descriptors are indices; the `/proc` fields named
  above are zero. Nothing else was searched for.

**What it costs.** At boot, the fixups: 8,947 on x86-64, 7,809 on AArch64 and
75,087 on ARMv7-A, in the loader, before the kernel runs. In memory, the
x86-64 and AArch64 relocation tables stay loaded: 210 and 183 KiB of read-only
data nothing reads after boot. On AArch64 and ARMv7-A the code is the same code:
both settings build for the static model. On x86-64 the PIC model makes `.text`
0.9% larger than `off`'s static model (3,739,332 bytes against 3,707,252,
debug profile, which includes the side-channel defences' own instructions).
Its addresses are `rip`-relative where the static model's were 32-bit
absolutes, which costs no instructions. §8 has the timings.

### 6.2 KPTI

**KPTI** — a user page table with the kernel unmapped but for an entry
trampoline — is the defence against Meltdown and L1TF. It is needed only on
Intel parts without `RDCL_NO` (before roughly 2019) and on the Cortex-A75. None
is in the reference configuration: QEMU's `qemu64` reports AMD, the KVM gate
runs on an AMD host, and Arm lists the Cortex-A72 and A7 as unaffected. So
KPTI is not the most fitting defence for the reference CPUs, and it is not
built. A processor that needs it says so at boot — *"Meltdown EXPOSED: no
RDCL_NO, and KPTI is not built (AoU-11)"* — and AoU-11 excludes it. It would
also blunt the timing attacks on KASLR above, though EntryBleed shows not
entirely. Building it would need, in order:

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

---

## 7. What the boot says and checks

KASLR is checked first, at stage 1, and §6.1 says how. Then, once, on the boot
processor, before the second processor starts:

```
  cpu      speculation defences: clamped indices, SWAPGS fence, entry registers cleared, AutoIBRS, STIBP, SSBD, predictor barrier on switch, RSB fill on switch
  cpu      speculation exposure: Spectre v2 into ring 0 covered; between programs covered; store bypass covered; Meltdown not affected (AMD)
```

(`x86_64 --accel kvm` on an AMD Ryzen 9 9900X host.) Every secondary applies
the boot processor's plan as it starts and records what it applied. On
AArch64 each core decides everything for itself instead (§4): a Pixel 7 boots
on a Cortex-A55, which needs neither the loop nor a switch barrier, and starts
A78s and X1s, which need 32 branches of the loop. The loop's count is one word
every entry reads, so it is the largest any core needs. There the exposure line
is printed once every processor has started, a line per kind of core, rather
than here. After stage 7's
programs, `arch::check_speculation` (`arch/speculation_check.rs`) requires:

* the clamp to return an index inside its bound unchanged, and — run on its
  own, as a misprediction runs it — one outside as zero (unchanged when built
  `off`);
* every running processor to have recorded, and nothing it wrote to
  `IA32_SPEC_CTRL`, `EFER`, `VIRT_SPEC_CTRL` or `SCTLR_EL1` to have failed to
  read back;
* a switch barrier to have been issued where any processor's plan has one —
  any processor's, not the boot processor's, which on a mixed machine may need
  none while others do;
* no processor to have issued a switch barrier its own plan does not have
  (the count is kept per processor);
* on x86-64, `VERW` to take its operand, on machines whose exit path never
  runs it;
* on AArch64, the entry loop's count to be non-zero exactly when some
  processor recorded that it needs the loop.

```
  cpu      speculation defences read back on 4 processors, 207 switch barriers
```

A failure is FX-0307. Processors that applied something other than the boot
processor — a big.LITTLE machine's little cores, or its big ones when it boots
on a little one — are counted and named, not failed.

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

**Timing, and FX-1004.** Stage 10's block-ring check failed three times with
FX-1004 *"quiescing the instant a driver died was refused: its channel was
still open"* in the first `test-shell` runs of this work, which is more than
that race's background rate, so an alternating control was run on x86-64 TCG
(2026-09-26, logs in `~/ferrix-logs/f31/fx1004-ctl/`):

| | `test-boot` | `test-shell` (four boots a run) |
|---|---:|---:|
| main at 74e1ea46 | 0 of 20 | 0 of 20 runs |
| this branch, `on` | 0 of 20 | 1 of 20 runs |
| this branch, `off` | 0 of 20 | — |
| this branch with the kernel half of 6afa79fc (row 238's fix) | — | 0 of 6 runs |

Each branch also hit FX-0884, the signalfd check, once in its twenty
`test-shell` runs. One in twenty against none is not a difference the control
can distinguish from chance, and every FX-1004 seen is the refusal
`docs/BACKLOG.md` row 238 describes — the driver's end still referenced by
`Endpoint::write` while it wakes the reader — on a path this work does not
touch: nothing here changes `object/`, `claim.rs` or `block_ring/`. What `on`
does add is a little time at every switch of address space and every entry,
which can widen that window without being a cause of its own. Row 238's fix
is being landed separately.

Under TCG — the default gates, and every Arm figure — the controls do not
exist and nothing speculates, so a timing there measures QEMU. What `on` adds
there is instructions: on x86-64 two per clamp, fourteen `xor`s and an `lfence`
per entry, a thirty-two-call refill per switch; on AArch64 a twelve-instruction
entry sequence with eight loop iterations on the A72; on ARMv7-A three
instructions per clamp.

---

## 9. What this does not cover

* **KPTI** (§6.2), and so **KASLR against a local timing attacker**, the
  **fixed physical placement** of the image and the writable alias of its text
  in the direct map, and ARMv7-A's few bits (§6.1).
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
* **Mixed ARMv7-A machines.** ARMv7-A still decides on the boot processor for
  every core, as AArch64 did before it decided per core (§4): a big.LITTLE
  Cortex-A15/A7 machine booted on an A7 would plan no barrier for its A15s.
  The reference STM32MP157 has two A7s, so this is outside the reference
  configuration.
* **Other implementers' cores on Linux's Spectre v2 list** (Brahma-B53,
  TSV110, Kryo silver), which the item reports as NOT covered unless they have
  `CSV2` or firmware covers them (§4).
* **The paranoid entry's registers.** An NMI or `#MC` taken in ring 3 on
  x86-64 does not clear the program's registers the way an ordinary trap does
  (a ring-3 `#DB` moves to the ordinary path and does); its return to ring 3
  does run `VERW`, through the shared restore.
