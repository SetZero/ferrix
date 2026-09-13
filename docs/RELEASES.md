# Ferrix — releases

Each testable milestone on `main` is an annotated tag, verified by the product
owner before tagging as `docs/BACKLOG.md`'s *Milestones* rule says. The notes
here are the tag's, kept short: what a person can try, and what is known not
to work yet. Newest first.

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
