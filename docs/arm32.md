# ARMv7-A — the third architecture

`docs/ARCHITECTURE.md` says what is being built and `docs/ROADMAP.md` in what
order. This says how a third architecture — 32-bit ARMv7-A, the Cortex-A7 of
the STM32MP157 — joins the two that boot today, without any of stages 1–3
being rewritten to admit it.

The target is QEMU's `virt` machine with a Cortex-A7, booted by U-Boot, to
`FERRIX-BOOT-OK stages 1-3`, in CI, beside the other two. The board itself is
not in this plan; what is in it is everything the board will need that QEMU
can prove, and a stated list of what QEMU cannot.

Everything under *Decisions* was checked by experiment on this machine with
the pinned toolchain (1.97.1) unless it says otherwise. The experiments are
described so they can be repeated; none of them touched the tree.

**Status: done.** Phases A to D have landed, and ARMv7-A is in the boot test
beside the other two. Stage 4 arrived on `main` while the port was under way,
so the target moved from `stages 1-3` to `stages 1-4`, and the port reached
it with PSCI `CPU_ON` for its secondaries as AArch64 does. Decision 6 is half
done: AArch64 runs on the shared GICv2 driver, moved after the port landed and
boot-tested on its own first; the PL011 half is still open. The plan below is
kept as it was written; *To verify at first boot* records what the first boot
answered.

---

## What survives, and what has to change

Four claims the documents make today survive the port intact, and it is worth
saying so first, because each one was the thing most likely not to:

* **No assembly at boot.** Firmware still calls a Rust `efi_main`; the only
  loader assembly is still the page-table switch. What changes is the parenthesis
  "with the CPU already in 64-bit mode".
* **One loader.** `boot/` gains a third `arch` file and no second program.
  U-Boot implements enough of UEFI to run it, and so does the board's U-Boot.
* **One facade.** `kernel/src/arch/mod.rs` gains a third `cfg` branch and the
  same twenty-four symbols. Generic code does not learn the word `arm`.
* **The `libs/` rule.** Every new piece of byte-level logic — ELF32, LPAE
  descriptors, the ELF-to-PE conversion, the device-tree lookups — is written
  as a pure function of bytes and tested on the host before the kernel calls it.

Five claims break and are replaced rather than patched:

* "for x86-64 and AArch64", in six places, becomes three architectures.
* "identical layout constants" (`ARCHITECTURE.md` §4) becomes one layout for
  the 64-bit pair and a 32-bit layout that is argued, not merely different.
* "48-bit addresses, four-level paging" becomes a *geometry* the paging crate
  is parameterised over, with the encoding unchanged.
* "ACPI (x86-64) and device tree (AArch64)" becomes "and device tree only, on
  ARMv7-A": `libs/fdt` gets its first consumer, at stage 1 rather than 10.
* "115 lines of assembly" grows by roughly 130, inside the 800-line cap, each
  line argued in `docs/ASSEMBLY.md`.

---

## Decisions

### 1. Boot path: UEFI, with U-Boot as the firmware

The alternative was a direct `-kernel` boot: QEMU jumps to the kernel with the
MMU off and a device tree in `r2`. It is faster to bring up and it was rejected,
because it costs exactly the things the project is organised around — bootstrap
assembly that builds page tables before Rust can run, a second boot path that
bypasses `boot/`, and a kernel that has to be told about its memory by a
different mechanism on one architecture. The STM32MP157's own firmware is
U-Boot, which has had `bootefi` for years; the UEFI path is not a QEMU
convenience, it is what the board does.

U-Boot rather than EDK2 for the QEMU firmware, for two reasons. Upstream EDK2
has dropped `ArmVirtPkg`'s 32-bit build, and the distribution package that
carried it (`qemu-efi-arm`) is already absent on Ubuntu 26.04 — the CI runners
will follow. And U-Boot is what the hardware runs, so a boot log full of its
messages is the log the board will produce, not a QEMU-only one.

`u-boot-qemu` (Debian, Ubuntu) ships `/usr/lib/u-boot/qemu_arm/u-boot.bin`,
which QEMU takes as `-bios`. Its default boot scans the virtio disk for
`EFI/BOOT/BOOTARM.EFI` and runs it, installs the device tree QEMU generated as
the `DEVICE_TREE_GUID` configuration table, and provides the four protocols the
loader uses. The two things it does not provide — ACPI, and a graphics output —
the loader already tolerates.

### 2. Building a UEFI application without a UEFI target

rustc has no `armv7*-uefi` target, and this is the one place the port has to
invent something. The invention is small: the loader is built as an ordinary
ELF32 static PIE and `xtask` converts it to PE32, translating `R_ARM_RELATIVE`
entries into PE base relocations. This is what systemd's `elf2efi.py` does for
its own ARM32 EFI stub, for the same reason.

Which target to build the ELF for was decided by experiment, and the answer is
not the obvious one:

| Attempt | Result |
|---|---|
| `armv7a-none-eabi`, `-C relocation-model=pic -pie -znotext` | **Fails to link.** The prebuilt `core` for this target is compiled with `relocation-model=static`, so it materialises addresses with `movw`/`movt` pairs. lld: `relocation R_ARM_MOVW_ABS_NC cannot be used against symbol` — from `core::panicking::panic_const_add_overflow`. A split immediate cannot become a dynamic relocation, and neither U-Boot nor EDK2 applies the PE relocation type that would express one (`IMAGE_REL_BASED_ARM_MOV32`; EDK2 asserts on it, U-Boot has no case for it). |
| `-Zbuild-std` with a PIC `core` | Works, and is nightly-only. The toolchain is pinned stable and stays that way. |
| A self-relocating loader (`--emit-relocs`, patch `movw`/`movt` immediates at runtime) | Feasible, and fragile: the code that applies the fixups must reach no static before it has run, which Rust cannot promise, and it adds an instruction-cache flush and a load-bias computation to the assembly. Rejected while a simpler route exists. |
| `armv7-unknown-linux-musleabi`, `-C linker=rust-lld -C linker-flavor=ld.lld -C link-self-contained=no`, `-pie -znotext --no-dynamic-linker`, a small linker script | **Links.** ELF32 `ET_DYN`, `EM_ARM`, EABI5 soft-float, three `PT_LOAD`s (`R E`, `R`, `RW`) and a `PT_DYNAMIC` carrying `DT_REL`/`DT_RELSZ`/`DT_RELENT`; every relocation is `R_ARM_RELATIVE`. |

So the loader is built for `armv7-unknown-linux-musleabi`. The word `linux` in
the name buys nothing and costs nothing: `core` contains no operating system,
`#![no_main]` means no C runtime is entered, `-C link-self-contained=no` keeps
musl's `crt1.o` and `libc.a` out of the link, and `rust-lld` is the linker as
it is for every other target here. What the target *does* supply is the three
things the loader needs and `armv7a-none-eabi` lacks: a `core` compiled
position-independent, a soft-float AAPCS (a UEFI application may not assume
the firmware enabled the VFP), and ARM rather than Thumb code generation.
`extern "efiapi"` lowers to `arm_aapcscc` on this target — confirmed in the
emitted LLVM IR — so `efi_main`'s signature does not change.

The kernel is unaffected: it stays on `armv7a-none-eabi`, static, with its own
linker script, exactly as the other two kernels. Using two different targets
for the two programs also dissolves a problem the port would otherwise have
had, which is that `.cargo/config.toml` sets rustflags *per target* and the
loader and the kernel need different link recipes.

The converter (`xtask/src/pe.rs`) is a few hundred lines of the same kind as
the FAT32 writer beside it: hand-written, dependency-free, byte-reproducible,
and tested by parsing its own output back with an independent reader. Its
input contract is the ELF the linker script above produces — `PT_LOAD`s that
start at `0x1000` so the first page can hold the PE headers, and a
`PT_DYNAMIC` — and its output is what both firmwares check for: machine
`IMAGE_FILE_MACHINE_ARMTHUMB_MIXED` (`0x01C2`, the value the UEFI specification
names for AArch32), subsystem `EFI_APPLICATION`, 4 KiB section alignment,
sections that are writable or executable and never both, and a `.reloc`
section of `IMAGE_REL_BASED_HIGHLOW` entries. The image base is zero, so an
RVA is a link address and the firmware's relocation delta is simply where it
loaded us. `DT_TEXTREL` is set — read-only data holds absolute pointers — and
that is fine for a PE, whose loader applies relocations before it applies
section protections; it is only an ELF dynamic linker that would object.

### 3. The 32-bit address space

A 4 GiB space cannot hold any of the constants in `libs/bootinfo`, and the
sentence "one layout is one set of bugs instead of two" has to become "one
layout per address width, and the differences are these". The split is 2/2 —
`TTBCR.T1SZ = 1` puts the top 2 GiB under `TTBR1`, the bottom 2 GiB under
`TTBR0` — because a 32-bit user process wants the larger user half and the
kernel half has to hold a direct map of most of the RAM a board of this class
ships with.

```
0x0000_0000 .. 0x7FFF_FFFF   user                               TTBR0
0x8000_0000 .. 0x9FFF_FFFF   kernel vmap (MMIO, guard-paged stacks)   TTBR1
0xA000_0000 .. 0xEFFF_FFFF   direct map of RAM, 1.25 GiB        TTBR1
0xF000_0000 .. 0xFFFF_FFFF   the kernel image                   TTBR1
```

Three consequences, each of which is a change to code the 64-bit pair runs:

* **The direct map has a physical origin.** RAM on QEMU's `virt` starts at
  1 GiB on both ARM machines; a direct map from physical zero would spend a
  quarter of the kernel half on nothing. `BootInfo` gains `physmap_phys`, the
  physical address at `PHYSMAP_BASE`, and `physmap(p)` becomes
  `PHYSMAP_BASE + p - physmap_phys`. The rule is the same on all three
  architectures — the lowest RAM address rounded down to 2 MiB — and is zero on
  x86-64 because RAM starts at zero there. On AArch64 this stops the direct map
  covering the MMIO hole below 1 GiB as cacheable memory, which nothing read
  through and which was wrong anyway; the W^X sweep's leaf count in the roadmap
  moves, and the roadmap is updated with the measured number.
* **The fixed early windows move with the arena.** `vmap.rs` reserves 4 GiB
  below the arena for the console window, the framebuffer and stage 3's
  on-demand window; a 32-bit vmap is 512 MiB in total. The reservation becomes
  a layout constant, `KERNEL_VMAP_RESERVED` (4 GiB on the 64-bit pair, 64 MiB
  here), and the three windows are placed at fixed fractions of it rather than
  at literal offsets in three files.
* **The kernel-half test changes shape.** `is_kernel_address` compares against
  `PHYSMAP_BASE` today, which is the lowest kernel address on the 64-bit pair
  and not here. A `KERNEL_HALF_BASE` constant names the boundary, and the
  compile-time assertions become pairwise disjointness checks between the three
  kernel regions rather than a fixed ordering.

Everything in `BootInfo` is already `u64`, except two raw pointers — `regions`
and `cmdline` — which are 4 bytes on a 32-bit build and 8 on a 64-bit one.
They become `u64` addresses, so the hand-off structure has one layout on every
width and the host tests exercise the layout the 32-bit kernel sees. With
`physmap_phys` and the device tree fields below, `BOOTINFO_VERSION` goes to 2.

The kernel image at `0xF000_0000` is 256 MiB of room, and the linker script
learns its base from the target rather than hard-coding one: each block in
`.cargo/config.toml` passes `--defsym=KERNEL_VIRT_BASE=…`, the script uses the
symbol, and the loader's existing refusal to start a kernel linked anywhere
but `KERNEL_VIRT_BASE` is what keeps the two in step. One script still serves
all three kernels; what it discards gains `.ARM.exidx*` and `.ARM.extab*`.

### 4. Page tables: LPAE is AArch64's descriptors with a shorter walk

ARMv7-A with the Large Physical Address Extension uses the long-descriptor
format — 64-bit descriptors, 512 per 4 KiB table, and the same bit positions
VMSAv8-64 uses for valid, table-or-page, `AttrIndx`, `AP`, `SH`, `AF`, `nG` and
`PXN`. The differences are that the output address is 40 bits rather than 48,
and bit 54 is `XN` for every privilege level rather than `UXN` for the
unprivileged one. `MAIR0`/`MAIR1` are the two halves of the `MAIR_EL1` value
the loader and kernel already agree on.

What differs is the walk: three levels, not four, with the first lookup at what
`libs/paging` calls `Level::GIGABYTE`, over a 32-bit input address. So the
crate grows a *geometry*, not a second mapper: `Encoding` gains `ROOT_LEVEL`
and `VIRT_BITS` with the current values as defaults, the canonical-address
check moves from `VirtAddr` onto the encoding, and `Mapper` starts every walk
at `E::ROOT_LEVEL`. `libs/paging/src/armv7a.rs` is then a thin sibling of
`aarch64.rs`, and every existing test that is generic over `E: Encoding` runs
a third time.

The level-1 table has two live entries for the kernel half and two for the
user half. The mapper indexes a root frame by address bits 31:30, so the
kernel half occupies entries 2 and 3 of its frame — and `TTBR1` is pointed at
`root + 16` so that the hardware, which indexes the `TTBR1` table from the
region base, reads the same two entries. Linux does exactly this
(`TTBR1_OFFSET` in `proc-v7-3level.S`), and it is the one line of the port
that a reader should not have to rediscover.

`TTBCR.T0SZ = 0`, so the identity map the loader needs for the switch lives in
a separate `TTBR0` tree, is dropped by the kernel with `EPD0` exactly as
AArch64 drops its own, and the W^X sweep asks the same `identity_root` question
it asks there. `SEPARATE_IDENTITY_TABLE` is true.

### 5. The machine is described by a device tree, and the tree is copied

There is no ACPI on a Cortex-A7. The GIC, the timer's interrupt and the console
come from the device tree, and `libs/fdt` — 49 tests, no consumer — becomes
reachable from the kernel two stages earlier than the roadmap's table says.

Where the tree lives matters more than where it came from. U-Boot copies it
into memory of type `EfiACPIReclaimMemory` before installing the configuration
table, which the loader maps to `MemKind::AcpiReclaim` and the kernel hands to
the buddy allocator after interrupt bring-up. That ordering happens to be
right for stage 3 and wrong from stage 10, which needs the tree for the life of
the system. So the loader copies the blob into an allocation of its own — a new
`MemKind::DeviceTree`, never reclaimed — and `BootInfo.dtb`/`dtb_len` name the
copy. AArch64 gets the same treatment for the tree EDK2 offers it, so the field
means one thing. `kernel/src/fdt.rs` is the generic counterpart of `acpi.rs`:
it borrows the copy through the direct map with the same bounds discipline and
hands `libs/fdt` a slice.

`libs/fdt` needs four small additions, each with tests: `arm,armv7-timer`
beside `arm,armv8-timer`; `arm,cortex-a7-gic` and `arm,gic-400` beside
`arm,cortex-a15-gic` (QEMU says `a15` for every 32-bit `virt`; the STM32MP1 says
`a7`); a decoder for the timer node's `interrupts` triples that returns the
virtual timer's PPI as a GIC identifier; and the `/psci` node's `method`, which
is how the kernel learns whether `SYSTEM_OFF` is an `smc` or an `hvc`.

### 6. What is shared with AArch64

Two drivers are byte-for-byte the same hardware on both ARM machines and are
currently written under `aarch64/`: the GICv2 register driver (`gic.rs`, minus
its MADT lookup) and the PL011 (`console.rs`, minus its hard-coded address).
They move to `kernel/src/arch/gicv2.rs` and `kernel/src/arch/pl011.rs`, gated
by `#[cfg(any(target_arch = "aarch64", target_arch = "arm"))]` in
`arch/mod.rs` — a conditional inside the arch directory, which is where the
layering script permits them. `aarch64/gic.rs` keeps the MADT walk and calls
the shared driver; `armv7a/gic.rs` does the same with the device tree. The
move is the first commit of the kernel phase and is boot-tested on AArch64
before any ARMv7-A code is added, so a regression there is attributable.

*What happened instead, for the GIC:* the port wrote `gicv2.rs` for ARMv7-A
alone, and AArch64 moved onto it after the port had landed — by then carrying
stage 4's per-core setup and SGI path, which the shared driver gained first.
`aarch64/gic.rs` is now the MADT walk alone: the layout, the check that every
core's CPU interface is the same banked address, the version check, and a
call to `gicv2::init`. The move was boot-tested on AArch64 on its own before
the other two, as this paragraph asks. The one change in behaviour is
harmless: AArch64's old `configure` wrote each priority word four times, and
the shared one steps by four. *The PL011 half has not moved*: AArch64's
`console.rs` still has its own driver and its hard-coded address.

The generic timer is the same counter reached through `cp15` rather than `mrs`,
so `armv7a/timer.rs` mirrors `aarch64/timer.rs` rather than sharing it — the
register access is the whole file. The trap path shares nothing: the ARMv7-A
exception model has banked registers and processor modes, and the argument in
`docs/ASSEMBLY.md` for its vector table is a different argument.

### 7. Names

`armv7a`, everywhere a name is needed: `--arch armv7a`, `build/armv7a/`,
`Arch::Armv7a`, `kernel/src/arch/armv7a/`, `boot/src/arch/armv7a.rs`,
`libs/paging/src/armv7a.rs`, the CI matrix, and `armv7a` in the boot log's
first line. `arm` on its own means too many things, and `arm32` names a width
rather than an architecture. Accepted spellings for `--arch` are `armv7a`,
`armv7`, `arm32` and `armhf`; `--arch all` means all three, and `--arch both`
stays as a synonym for `all` so that a habit does not silently drop coverage.
The boot file is `BOOTARM.EFI`, which is the specification's name and is 8.3.

---

## The work, in order

Four phases, each ending in something that runs or, for the first, in
something the host tests prove. Sizes are in the roadmap's units.

### Phase A — host-testable groundwork  ·  *weekend*

Everything here is a pure function of bytes, lands under `libs/`, and passes
`cargo xtask check --fast` before any freestanding code depends on it.

**`libs/elf` — ELF32.** `Elf::parse` reads `EI_CLASS` and decodes either
header; `EHDR`/`PHDR` sizes and field offsets become per-class; `Segment` and
`Header` keep their `u64` fields. `EM_ARM = 40` and `R_ARM_RELATIVE = 23` are
added. Relocations grow the `REL` form (`DT_REL`/`DT_RELSZ`/`DT_RELENT`, 8-byte
entries, addend in place): `Relocation` carries an `Addend` that is either
explicit or in-place, and the one caller that applies relocations handles both.
`ElfError::NotElf64` becomes `UnsupportedClass(u8)`. Tests build ELF32 images
by hand as the ELF64 ones are built today; `scripts/seed-fuzz-corpus.py` gains
an ELF32 shape for each ELF64 shape it already writes, and the `elf_parse`
target replays them.

**`libs/paging` — geometry.** `Encoding::ROOT_LEVEL` and `Encoding::VIRT_BITS`
with defaults; `is_canonical` and `canonical()` become functions of
`VIRT_BITS`; `Mapper::map_one`, `translate`, `find_leaf`, `mapping_level` and
`for_each_leaf` start at `E::ROOT_LEVEL`; `Path` keeps three steps (a
three-level walk uses two). `armv7a.rs` implements the LPAE encoding with a
40-bit address mask and `XN` semantics, and exports `MAIR0`/`MAIR1` derived
from `aarch64::MAIR_EL1`. Every `fn foo<E: Encoding>()` test gains an
`_on_armv7a` instantiation; new tests pin the root level, the 32-bit
canonical rule, the address mask and the `XN`/`PXN` round trip.

**`libs/bootinfo` — version 2.** `Arch::Armv7a = 2` with `elf_machine() = 40`.
The 32-bit layout constants under `#[cfg(target_pointer_width = "32")]`
(the layering script explicitly allows width conditionals anywhere), the
64-bit ones under `"64"`, with the same names on both. `KERNEL_HALF_BASE`,
`KERNEL_VMAP_RESERVED`, the pairwise disjointness assertions. `physmap_phys`,
`dtb_len`, `MemKind::DeviceTree = 13`; `regions` and `cmdline` as `u64`;
`validate` additionally requires `physmap_phys` to be 2 MiB aligned and
`physmap_len` to fit the direct-map region. Tests updated and extended.

**`libs/fdt` — the four additions** from decision 5, each with a synthesised
tree in the tests.

**Exit:** `cargo xtask check --fast` passes; the host test count and the fuzz
corpus have grown; nothing under `kernel/` or `boot/` has changed yet.

### Phase B — the build path and the loader  ·  *week*

**Toolchain and configuration.** `rust-toolchain.toml` adds
`armv7a-none-eabi` and `armv7-unknown-linux-musleabi`. `.cargo/config.toml`
gains `[target.armv7a-none-eabi]` with the kernel recipe (linker script,
`--defsym`, `-zmax-page-size=4096`, `relocation-model=static`) and
`[target.armv7-unknown-linux-musleabi]` with the loader recipe
(`linker = "rust-lld"`, `linker-flavor=ld.lld`, `link-self-contained=no`,
`-pie`, `-znotext`, `--no-dynamic-linker`, `-Tboot/linker/armv7a.ld`). The two
64-bit kernel blocks gain their `--defsym` and `kernel/linker/kernel.ld` drops
its literal base. `boot/linker/armv7a.ld` is new: `ENTRY(efi_main)`, sections
from `0x1000`, `.dynamic` and `.rel.dyn` kept, `.ARM.exidx*` discarded.

**`xtask`.** `Arch::Armv7a` and its seven methods; `Arch::ALL` replaces the
two-element literals in `args.rs` and `check.rs`; the usage string and the
parse error name three architectures. `cargo.rs` learns that a loader is
either a PE the target emits or an ELF to convert, and for `Armv7a` runs
`pe::convert` on `target/armv7-unknown-linux-musleabi/<profile>/ferrix-boot`
and returns the `.efi` beside it. `pe.rs` and `pe/tests.rs` are the converter
from decision 2; `xtask/Cargo.toml` gains `ferrix-elf` as its first dependency,
and the comment that says there are none is rewritten to say why this one is
different (it is ours, and the alternative is a second ELF reader). `paths.rs`
grows a firmware kind — pflash pair, or a `-bios` image — with `u-boot.bin`
searched under `/usr/lib/u-boot/qemu_arm`, `/usr/share/u-boot/qemu_arm` and
next to QEMU, overridable by `FERRIX_UBOOT`. `qemu.rs` adds the
`-machine virt -cpu cortex-a7 -bios …` arm and skips the variable store for a
`-bios` firmware. `fat.rs` needs nothing: `BOOTARM.EFI` is 8.3.

**`boot/`.** `boot/src/arch/armv7a.rs`: `ARCH`, `ELF_MACHINE`, a new
`ELF_CLASS` (added to all three and checked by `load::parse_kernel`),
`prepare_cpu` (refuse anything but SVC mode — U-Boot under `virtualization=on`
hands off in HYP — and refuse a CPU whose `ID_MMFR0.VMSA` says no LPAE),
`clean_dcache` (`CTR` line size, `DCCIMVAC` per line, `dsb`/`isb`), and
`enter_kernel`: interrupts masked, MMU and caches off, `MAIR0`/`MAIR1`,
`TTBCR` with `EAE`, `T0SZ = 0`, `T1SZ = 1`, `TTBR0` and `TTBR1` via `mcrr`,
`TLBIALL`, MMU on, stack, `bx`. About forty lines, all bound to named registers
for the reason the AArch64 file gives. `arch/mod.rs` gains the third branch and
`PageEncoding`. `main.rs`/`load.rs` change for every architecture: the direct
map starts at `physmap_phys`, the identity map refuses RAM above the `TTBR0`
region on a 32-bit build, and the device tree is copied into a `DeviceTree`
allocation. The UEFI bindings need no change — `UINTN` is `usize` and every
address field is already `u64`, which is the specification's layout.

**Exit:** `cargo xtask build --arch all` writes three images;
`llvm-readobj --coff-basereloc` on `build/armv7a`'s loader shows one `HIGHLOW`
entry per `R_ARM_RELATIVE` in the ELF; the two 64-bit architectures still boot
with the direct-map and device-tree changes; and — with QEMU and U-Boot
available — the ARMv7-A boot log shows U-Boot running `BOOTARM.EFI` and the
loader printing its lines up to the jump. That last one is Phase B's real exit
and the first point at which the *to verify at first boot* list below gets
answered.

### Phase C — the kernel  ·  *week*

In this order, each step boot-tested on AArch64 as well as ARMv7-A once the
latter reaches the console:

1. **The shared drivers move** (decision 6). AArch64 boots unchanged.
2. **`kernel/src/fdt.rs`**, generic, alongside `acpi.rs`.
3. **`kernel/src/arch/armv7a/mod.rs`** — the facade's twenty-four symbols.
   `init_console` parses the tree, finds the console by `stdout-path` (falling
   back to the first `arm,pl011`), and maps its first `reg` range at
   `KERNEL_VMAP_BASE`. `identity_root` returns `ttbr0_phys`;
   `drop_identity_map` sets `TTBCR.EPD0` and zeroes `TTBR0`. `shutdown` makes
   the PSCI `SYSTEM_OFF` call through the conduit the tree names, after the
   marker as the other two do. `Irq` masks with `cpsid i` and restores from a
   saved `CPSR.I`, never by unconditionally unmasking.
4. **`cpu.rs`** — the `cp15` primitives, one instruction each: `wfi`,
   `cpsid`/`cpsie`, `CPSR`, `VBAR`, `TTBCR`/`TTBR0`, the four `CNTV*`
   registers, the four fault registers, `TLBIALLIS` with its barriers, and
   `smc`/`hvc` under `.arch_extension sec`/`virt`. Every one assembled in the
   experiment.
5. **`trap.rs`** — the vector table and the save path. Eight entries at 4-byte
   offsets, 32-byte aligned as `VBAR` requires; each stub adjusts the banked
   `lr` by its mode's offset (8 for a data abort, 4 for a prefetch abort, IRQ
   and undefined instruction, 0 for `svc`), then `srsdb sp!, #19` stores the
   return state on the *SVC* stack from whichever mode the CPU entered,
   `cps #19` switches to it with interrupts still masked, and a single common
   path pushes `r0`–`r12` and `lr`, reads `DFSR`/`DFAR`/`IFSR`/`IFAR`, calls
   `ferrix_trap_entry`, and returns with `rfeia sp!`. No per-mode stacks: that
   is what `srs` is for. The frame is 80 bytes and asserted 8-aligned, as
   AAPCS requires at a call boundary. `classify` reads the long-descriptor
   fault status — the same six-bit codes as AArch64, so the same
   translation/permission distinction — and `bkpt #0` arrives as a prefetch
   abort with the debug-event status; `advance_past_breakpoint` adds 4.
6. **`gic.rs`** (device tree → shared driver), **`timer.rs`** (`cp15` generic
   timer, PPI from the tree, `CNTFRQ` required non-zero as on AArch64).
7. **`init_interrupts`** wires them and returns the `Report`; the boot log
   reads `irqs GICv2, virtual timer at 62.500 MHz` on QEMU.

Generic code changes in exactly three places, none of them architecture-aware:
`physmap()` and `EarlyMemory` gain the physical origin; `vmap.rs`, `mm.rs` and
`main.rs` take their window offsets from `KERNEL_VMAP_RESERVED`; and the stack
alignment check's comment says 16 on the 64-bit pair and 8 here (the check
itself stays at 16, which satisfies both). A u64 address cast to a pointer is
already how every MMIO access is written, and on a 32-bit target it truncates a
value that is known to fit; `u128` arithmetic, `AtomicU64` and 64-bit volatile
accesses all have implementations on ARMv7 (`ldrexd`/`strexd`, the compiler
builtins), and the kernel's `u64`-everywhere address plumbing carries over
without a width conditional.

**Exit:** `cargo xtask test-boot --arch armv7a` reaches
`FERRIX-BOOT-OK stages 1-3` with the same self-checks the other two run —
two breakpoints, four page faults with the exact frame bound, a thousand ticks
measured against the counter, the identity map dropped and its absence checked,
a W^X sweep that finds mappings and no writable-executable one, and a non-zero
reclaim. The measured tick rate and sweep count go into the roadmap beside the
other two.

### Phase D — gates, CI and the documents  ·  *weekend*

| Gate | Change |
|---|---|
| `scripts/asm-allowlist.json` | Three entries: `boot/src/arch/armv7a.rs` (60), `kernel/src/arch/armv7a/cpu.rs` (100), `kernel/src/arch/armv7a/trap.rs` (90); the `max_total_lines` comment says "all three" |
| `scripts/check-crate-layering.sh` | Rule 3's pattern becomes `arch::(x86_64\|aarch64\|armv7a)::`; the comments say three |
| `xtask/src/check.rs` | Already loops `Arch::ALL` after Phase B: six freestanding clippy passes |
| `.github/workflows/ci.yml` | The three `targets:` lists gain both triples; `build` and `boot` matrices gain `armv7a`; two clippy steps; `u-boot-qemu` in the apt line |
| `docs/ASSEMBLY.md` | A `### ARMv7-A` table: the vector table and `srs`/`rfe` path, the `cp15` primitives, the loader switch. "Boot: none" reworded — 64-bit mode is no longer the reason there is no bootstrap assembly; UEFI is |
| `docs/ARCHITECTURE.md` | §4 gains the 32-bit layout block and the geometry sentence; §7 names the device tree on ARMv7-A; §9 names three architectures; §10 loses "in 64-bit mode" |
| `docs/ROADMAP.md` | *Where it stands* says three; a block before stage 4 records this work with its exit criterion met; stage 1's "either architecture" and the `libs/fdt` row change; the measured figures |
| `docs/RELIABILITY.md`, `README.md` | "both" becomes "every"; the sample boot output gains its third `boot ok` line; the target count; `qemu-system-arm` and `u-boot-qemu` in the prerequisites; the assembly figure regenerated from `check-asm-budget.py --json` (it is already stale: 115 lines, 99.55%) |
| `docs/BOOT-LOG.md` | A section for U-Boot's lines, written from the first real log in the file's evidence-first idiom — not before |

---

## Verified on this machine

* `armv7a-none-eabi` target spec (nightly `--print target-spec-json`, same
  LLVM): `relocation-model = static`, features
  `+soft-float,-neon,+strict-align`, `max-atomic-width = 64`, `rust-lld`,
  `panic = abort`, pointer width 32.
* A PIE against that target's `core` does not link; the lld message is quoted
  in decision 2.
* The same crate against `armv7-unknown-linux-musleabi` links into the ELF
  decision 2 describes, with six `R_ARM_RELATIVE` relocations and nothing else,
  and `efi_main` is `arm_aapcscc` in the IR.
* The assembler accepts every instruction Phase C uses, `hvc`/`smc` only under
  their `.arch_extension` directives.
* This Windows host has no QEMU. The WSL Ubuntu 24.04 install has none either,
  and can install `qemu-system-arm` 8.2.2 and `u-boot-qemu` 2025.10 — the same
  pair `ubuntu-latest` will give CI. Phases B–D are boot-tested from there.
  (`qemu-efi-arm` 2024.02 is also still packaged on 24.04, which is worth one
  local run under EDK2 as a second opinion; it is not the CI configuration,
  because 26.04 has already dropped it.)

## To verify at first boot, with the fallback for each

**Answered at first boot: all seven as expected, and no fallback was needed.**
U-Boot's `efi_mgr` found `BOOTARM.EFI` on the table-less volume and ran it
with its relocations applied; the kernel's check that the map describes the
loader's own allocations passed; the tree carried `stdout-path` and the GIC
and timer nodes; `CNTFRQ` read 62.5 MHz. The one that was wrong in the plan,
(5), was caught by dumping QEMU's tree before the first boot rather than by
it. `docs/BOOT-LOG.md` has U-Boot's lines.

1. **U-Boot's autoboot finds `EFI/BOOT/BOOTARM.EFI` on a FAT volume with no
   partition table.** Expected: bootstd treats a table-less disk as one
   partition. Fallback: `fat.rs` writes a protective MBR with one partition —
   a change the 64-bit images would take too, so that there is one image
   format.
2. **U-Boot honours the loader's own memory types** (`0x8000_0000` and up) in
   `AllocatePages` and reports them back in the map. Expected: yes, it accepts
   the OS-loader range. Fallback: allocate as `LoaderData` and have the loader
   relabel its own allocations in the copied map — it knows their addresses —
   which keeps the kernel's "the map describes the loader's own allocations"
   check meaningful.
3. **The configuration table carries the QEMU device tree**, and its
   `/chosen/stdout-path`, GIC and timer nodes are what `libs/fdt` expects.
   Fallback for a missing `stdout-path`: the first `arm,pl011` node.
4. **`ExitBootServices` may turn the MMU off** — U-Boot's ARM32 GRUB
   workaround does, on some boards. The switch sequence handles both states,
   because it clears `SCTLR.M` before installing the regime either way.
5. **The PSCI conduit** on `virt` without `virtualization=on` is `hvc` —
   QEMU emulates PSCI as a hypervisor when the machine has neither EL2 nor
   EL3, and its generated tree says `method = "hvc"` (checked by dumping it).
   The kernel reads the tree rather than assuming, because a board with
   TF-A underneath says `smc`.
6. **`CNTFRQ`** is 62.5 MHz on QEMU and non-zero; the timer PPI is 27.
7. **U-Boot loads a PE whose `.text` starts at RVA `0x1000` and applies
   `HIGHLOW` relocations to read-only sections before protecting them.** This
   is the converter's whole contract and the first thing the loader's first
   line proves.

## Deferred, stated rather than hidden

* **RAM above 2 GiB physical.** The STM32MP157's DDR is at `0xC000_0000`. The
  loader's identity map has to be where physical equals virtual, and for that
  board that address is inside the kernel half. The resolution is board-level:
  place the direct map so that it *is* the identity map for that RAM
  (`PHYSMAP_BASE == physmap_phys`) and map the loader's own text executable
  through it for the length of the switch. QEMU's `virt` puts RAM at 1 GiB and
  cannot exercise this; the loader refuses such a machine with a message rather
  than guessing.
* **RAM beyond the direct map.** A 32-bit kernel with more than 1.25 GiB of RAM
  needs a high-memory scheme; the kernel ignores the excess and says how much,
  loudly, in the boot log. No board this port targets has it. Such a machine
  does *boot*, though, which it did not before 2026-09-13: the loader measures
  where the direct map ends before allocating and keeps everything the kernel
  is handed below it, and when firmware has loaded the loader itself inside
  the kernel's half — QEMU's `virt` with 2 GiB puts it in the direct map's
  virtual range — it copies the switch to a page below the split and runs it
  from there (`IdentityTree::Trampoline`, and `docs/ASSEMBLY.md` under
  *Boot*). `test-boot --arch armv7a --memory 2048` reaches the marker at two
  processors and at four, reporting 768 MiB unused; 3 GiB does too.
* **The board's console.** The STM32MP1 UART is `st,stm32h7-uart`, not a PL011.
  A second early console driver, chosen by `compatible`, is board bring-up
  work.
* **Thumb-2.** Everything is ARM-mode code. Thumb would shrink the kernel by a
  third and is a target-feature change with an assembler audit behind it; not
  now.
* **VFP/NEON.** Soft-float throughout, as on the other two; the kernel does no
  floating point and the trap frame does not save it.
* **SMP.** The Cortex-A7 is dual-core on the board and `-smp 4` under QEMU; PSCI
  `CPU_ON` is stage 4's, and the plan leaves the `virt` GIC's second CPU
  interface exactly as stage 3 leaves it on AArch64. *No longer deferred*:
  stage 4 landed on `main` during the port, and ARMv7-A's secondaries came
  up through it — see the status note at the top. One thing QEMU cannot
  check stays owed to the board: `ACTLR.SMP`, which a Cortex-A7 needs set
  before its caches are coherent with the other core's. The kernel leaves it
  to the PSCI firmware, which sets it on real boards, because a non-secure
  write can be undefined; the board's first two-core boot is what confirms it.
* **AArch64 from the device tree.** With `kernel/src/fdt.rs` in the tree, the
  hard-coded PL011 address in `aarch64/console.rs` could go; it stays, so that
  this port changes AArch64's boot only where the shared design forces it.
* **The PL011 half of decision 6.** AArch64 is on the shared GICv2 driver;
  its console is not yet on the shared `pl011.rs`. The same shape as the GIC
  move — the architecture finds the UART, the shared driver drives it — and
  the same rule: moved on its own, and boot-tested on AArch64 before anything
  else rides on it.

## Conventions this work follows

* One topic branch, `arch-armv7a`, off `main`; commits in the phase order
  above, each with a body that argues the why in the style of the last two
  stage commits, and none with a `Co-authored-by:` trailer — `.githooks`
  refuses one, and `pre-push` refuses it again.
* Every `unsafe` block with a `SAFETY:` claim and one operation; every
  exemption `#[expect(..., reason = "AUDIT: …")]`; every new allow-list entry in
  the same commit as its assembly, with the argument in the entry.
* Nothing under `kernel/` or `boot/` names `armv7a` outside `arch/`; the
  layering script is updated in the same commit that would otherwise trip it.
