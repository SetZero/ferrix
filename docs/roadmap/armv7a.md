# ARMv7-A — a third architecture ✅

32-bit ARMv7-A — the Cortex-A7 of the STM32MP157 — on QEMU's `virt` machine,
booted by U-Boot. `docs/arm32.md` is the plan and argues each decision; this
records what the port changed and what it proved. None of stages 1–3 was
rewritten to admit it.

* **The same loader, converted.** rustc has no 32-bit UEFI target, so the
  loader is built as an ELF static PIE and `tools/common/xtask/src/pe.rs` rewrites it as a
  PE32 with base relocations, tested against an independent reader of its own
  output. U-Boot runs it as `BOOTARM.EFI`. No bootstrap assembly was added.
* **A 32-bit address space, argued rather than shrunk.** A 2/2 split with a
  1.25 GiB direct map; LPAE tables, which are AArch64's descriptors on a
  three-level walk, so `src/lib/kernel/paging` gained a geometry rather than a second
  mapper. The direct map now begins at the lowest RAM address on every
  architecture — which on AArch64 stopped it mapping the device hole below RAM
  as cacheable memory, and took that sweep from 822 leaves to 311.
* **One hand-off layout on every width.** `BootInfo` version 2 carries `u64`
  addresses where it had pointers, the direct map's physical origin, and a
  device tree copied into memory of its own kind, so that it outlives the
  reclaim that returns the firmware's copy.
* **Device tree only.** `src/lib/platform/fdt` got its first consumer nine stages early:
  the console by `stdout-path`, the GICv2, the virtual timer's interrupt, and
  the PSCI conduit — which on QEMU is `hvc`, not the `smc` the plan first
  guessed; dumping the generated tree settled it before a line depended on it.
* **Traps without mode stacks.** Eight vector entries, and one path through
  `srsdb` and `rfeia` on the SVC stack, so no other processor mode is ever
  given a stack to get wrong.

**Exit criterion met, and in the boot test.** `cargo xtask test-boot --arch
armv7a` runs the same self-checks as the other two and reaches the same
marker: two breakpoints, four page faults with the exact frame bound, 1001
ticks measured against the counter at 920 Hz for a requested 1000, the
identity map dropped and its absence checked, 317 mappings swept with none
writable-and-executable, and 3 MiB reclaimed.

**And stage 4, which landed on `main` while the port was under way.** The
secondaries are found in the device tree's `/cpus` rather than the MADT and
started through PSCI `CPU_ON`, entering as AArch64's do: through an identity
map of their entry sequence, with every parameter loaded in one `ldm` before
the MMU goes on, and refused if the entry would sit above the 2 GiB that
`TTBR0` covers. TLB invalidation is broadcast by the hardware, as on AArch64,
so there is no shootdown IPI. The run recorded when it landed: four of four
online, 100 rounds of work woken by 300 IPIs, 100 grace periods against 34,900
reads with none stale, and the counter at exactly 100,000 with its shares
overlapping and 39,725 updates lost by the unlocked count beside it. The
sweep then found 341 mappings, and the reclaim 4 MiB.

**Deferred, with the reasons in `docs/arm32.md`:** RAM above 2 GiB physical,
which the board has and QEMU cannot place; using RAM beyond the direct map
(a machine with it boots, since 2026-09-13, and reports the excess unused); the
board's own UART; Thumb-2; VFP. The UART has since landed and carried the run
below, and so has RAM above the 2 GiB split (`9ae0180f`), which the board's
DDR at 3 GiB needs. Thumb-2 and VFP are deferred for the kernel's own code
only: a program whose entry is Thumb is entered in Thumb state, and every
task's VFP registers are saved and restored with it, both since stage 7.

**On the board, stages 1–9.** On 2026-09-13, at `fd4442e`, an STM32MP157D-DK1
— two Cortex-A7s and 512 MiB, under mainline TF-A, OP-TEE and U-Boot — booted
the kernel `cargo xtask test-shell` builds, copied to the card through U-Boot's
`ums` and started with `bootefi`. One boot on each, the same kernel:

| | QEMU `virt`, `--smp 2` | STM32MP157D-DK1 |
|---|---|---|
| Boot marker | `FERRIX-BOOT-OK stages 1-9` | `FERRIX-BOOT-OK stages 1-9` |
| `ACTLR.SMP` | clear on 2 of 2 | set on 2 of 2 |
| Stage 3 | 251 ticks at 998 Hz | 251 ticks at 999 Hz |
| Stage 4 | 50000 of 50000 | 50000 of 50000 |
| Stage 5 | 2462 switches, 44 steals | 2743 switches, 5 steals |
| Stages 6–9 | every check line | the same lines, except that the card carried no initramfs and the board has no PCI or IOMMU to find |
| W^X sweep | 961 mappings, 392 executable | 944 mappings, 392 executable |
| Stage 7's script | seven lines, then `the shell exited with 7` | the same seven lines; the exit line cut off |

The exit line is lost because the STM32 USART driver returns as soon as the
transmit register has room, and the PSCI `SYSTEM_OFF` after the last line
powers the board off while that line is still being sent. The console now
drains before power-off (`571316a9`), and a rerun on the board the same day
received the exit line whole (`docs/vendor/st/stm32mp157-dk.md`). The first run's serial
log is kept outside the repository, at
`~/.local/share/ferrix/board-boot-fd4442e-2026-09-13.log`.

---

