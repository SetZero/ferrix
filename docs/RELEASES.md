# Ferrix — releases

Each testable milestone on `main` is an annotated tag, verified by the product
owner before tagging as `docs/BACKLOG.md`'s *Milestones* rule says. The notes
here are the tag's, kept short: what a person can try, and what is known not
to work yet. Newest first.

## stage-9.1-console-and-iommu — 2026-09-13, commit 5fb365f

Arm console input, translated IOMMU domains, and the day's intermittent
failures fixed.

- Typed input works on the Arm machines: the PL011 and STM32 USART receive,
  proven on the STM32MP157D-DK1 (the first keystroke read on hardware), and
  the console drains before power-off so the last line arrives whole.
- glibc programs run as init: `/proc/self/exe` is absolute, so Ubuntu's
  static busybox reaches its prompt
  (`FERRIX_INIT=/usr/bin/busybox cargo xtask run --arch x86_64`).
- Stage 10: VT-d on x86-64 and SMMUv3 on AArch64 translate every device's
  DMA, an out-of-domain write is caught by the unit's fault record, a driver
  can pin VMO pages for its device, and IOMMU waits run with interrupts on.
- Stage 11, host side: btrfs reads verify every data sector against the
  checksum tree, honour the default subvolume and parse `INODE_EXTREF`.
- Fixed: unmount left a dentry per mount cycle (stage 8 frame-count
  failures); frame counts raced the reaper (stages 6 and 8); the preemption
  count could be raised on one processor and lowered on another (FX-0503); a
  new task was not charged before an insert (FX-0701); frame 0 was handed out
  under OVMF (a root-table panic under KVM).
- ferrousli is on the build path: `cargo xtask check --ferrousli` runs its
  gates, CI runs them, and busybox builds against it from pinned sources (the
  link still wants 154 functions).
- Verified on the STM32MP157D-DK1 on 2026-09-13, 21:25–21:31, flashed by hand
  from Windows (U-Boot `ums`, the three files checked by hash): stages 1–9,
  the console receiving by interrupt 84, `test-shell`'s script typed line by
  line, a 1000-character paste arriving whole, and the final line draining
  whole before power-off.
- Known limits: no threads, no file-backed `mmap`, no ring-3 disk driver yet;
  the interrupt-driven console and reboot-to-U-Boot are on `develop`.

What to test:

```
cargo xtask run --arch armv7a --init '~/.local/share/ferrix/busybox/{arch}/bin/busybox.static'
FERRIX_INIT=/usr/bin/busybox cargo xtask run --arch x86_64
```

Type at the `ferrix#` prompt on the Arm machine; the second starts Ubuntu's
glibc busybox as init on x86-64.

## stage-9 — 2026-09-13, commit 80d0ea7

Stages 1–9 done, every known stage 5 and 6 intermittent failure fixed.

- Boots from UEFI firmware (EDK2, U-Boot) into a Rust kernel on x86-64,
  AArch64 and ARMv7-A; the boot marker reads `FERRIX-BOOT-OK stages 1-9`.
- Memory: buddy allocator, slab heap, guard-paged vmap arena, demand paging,
  copy-on-write fork, W^X sweep, boot memory reclaimed.
- Traps, interrupts and time on every architecture; SMP with every vCPU
  online, IPIs, TLB shootdown, grace periods.
- EEVDF fair scheduler with load tracking, placement, affinity and balancing.
- User mode and the Linux syscall ABI: `fork`, `vfork`, `execve`, `wait4`,
  pipes, `futex`, signal delivery with `sigreturn`, a terminal with job control
  and Ctrl-C, about 150 calls answered.
- Static musl busybox runs a script and an interactive shell on all three
  architectures.
- On hardware, verified after the tag at `fd4442e`: an STM32MP157D-DK1 at two
  cores reaches `FERRIX-BOOT-OK stages 1-9` and runs `test-shell`'s script.
  Known defect: the last console line before power-off is cut, because the
  STM32 USART driver does not wait for its transmitter to drain.
- VFS with tmpfs, devfs, procfs and initramfs; `ls -R /proc`,
  `cat /proc/self/maps` and a forking shell script pass; `top`, `ps`, `mpstat`
  run.
- Native ABI: handles, channels with handle passing, ports, jobs, VMOs,
  `Interrupt` and `IoMapping` objects for ring-3 drivers.
- Stage 10 under way: PCI enumeration, device nodes, MSI-X vectors, virtio-rng
  driven by DMA with an MSI-X completion, VT-d translating on x86-64.
- Known limits: no threads, no file-backed `mmap`, no disk yet, console input
  only on x86-64.

What to test:

```
cargo xtask test-boot  --arch all
cargo xtask test-shell --arch all --init '~/.local/share/ferrix/busybox/{arch}/bin/busybox.static'
cargo xtask test-vfs   --arch all --init '~/.local/share/ferrix/busybox/{arch}/bin/busybox.static'
FERRIX_INIT=~/.local/share/ferrix/busybox/x86_64/bin/busybox.static cargo xtask run --arch x86_64
```

The last gives an interactive busybox shell on the serial console; type after
the prompt appears.
