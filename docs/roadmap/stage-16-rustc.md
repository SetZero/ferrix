# Stage 16 — `rustc`  ·  *the goal*  ·  *≈ 40 guessed, 8 spent*

The remaining syscall surface, the memory scale, the process spawn path for
`rust-lld`, and a sysroot on btrfs. Then run it.

**Exit:** `rustc hello.rs && ./hello` on Ferrix, in CI.

**Met on 2026-09-22,** on nazuna under KVM, with `cargo xtask test-rustc`;
the CI job `rustc` runs the same command on every push, and has passed on
GitHub since its first run there on 2026-09-22. The compiler is not one
built here. It is the rust-lang.org release of 1.97.1: a glibc
position-independent executable whose LLVM is a 190 MiB shared library of
its own, run by Debian 13's `ld-linux`. It links as it does on any Linux
machine, through `cc` (Debian's gcc 14 driver), which runs `collect2`, which
runs the `ld.lld` rustc points it at, which runs `rust-lld`. So one compile
is five programs nobody here wrote, four `execve`s deep, over some 350 MiB of
shared libraries mapped from btrfs, and the program they make is a sixth.
From the shell's first line to `hello` is under two seconds:

```
rustc 1.97.1 (8bab26f4f 2026-07-14)
...
rustc-gate: hello from rustc on Ferrix
  init     the shell exited with 16
```

Most of what the stage names had already arrived under other names. The
system calls and the threads came with stages 7, 17 and 18. The spawn path
came with stage 8 and dynamic linking, and the glibc loader was proven on
Debian's busybox. The sysroot on btrfs is stage 12's write path and the
`/data` mount. Three things were new:

* **A sysroot.** `scripts/fetch/fetch-rustc-sysroot.sh` downloads the compiler and
  `rust-std` from rust-lang.org, and `libc6`, `libc6-dev`, `libgcc-s1`,
  `libgcc-14-dev`, gcc 14's driver and `zlib1g` from Debian. Every download
  is pinned by the SHA-256 its own index gave. The script lays them out as
  a Debian tree with the toolchain under `rust/` and writes a 1.7 GiB btrfs
  image of it with `mkfs.btrfs --rootdir`. It takes no `cc1` and no
  binutils, since nothing is compiled from C and the linker is rust-lld.
  The same tree, in an otherwise empty bwrap sandbox on the host, compiles
  and runs the same program, which is what separates a broken sysroot from
  a broken kernel.
* **The gate.** `cargo xtask test-rustc` attaches the image under QEMU's
  `snapshot=on`, so a run never changes it. The image has no `ferrix-root`
  label, so the kernel mounts it at `/data`. The initramfs carries five
  links for the absolute paths glibc and gcc name:
  `/lib64`, `/lib/x86_64-linux-gnu`, `/usr/lib/x86_64-linux-gnu`,
  `/usr/lib/gcc` and `/usr/libexec`. zinc runs `rustc -vV`, then
  `rustc hello.rs`, then `./hello`, each step with an exit status of its
  own, so a failure says which one it was. The guest gets 4 GiB unless
  `--memory` says otherwise, and the gate is x86-64 only, because the volume
  holds x86-64 binaries.
* **Faults that fill from the disk.** The first boot died in glibc's linker:
  `cc`, `rust-lld` and `rustc` all stopped at the same instruction, a read of
  `l_info[DT_STRTAB]` that was null. The page holding `libc.so.6`'s dynamic
  section had been mapped as zeros. A btrfs file's pages are a page cache
  over a source, filled when a `read` reaches them. A fault through a
  mapping reaches the VMO directly and committed any absent page as zeros,
  which is right for tmpfs and wrong for every file on a disk. It was
  latent since stage 12 began offering btrfs files for mapping, and
  `libs/fs/vfs` had written the rule down: no store over a source may be
  mapped until a fault can fill it. The VMO of such a file now carries its
  source as a `Filler`. The address space asks it for the page, with a
  32-page read-ahead run, before taking its own lock, because the fill waits
  for the disk. A page the disk cannot give is `SIGBUS`, as on Linux. The
  boot check `check_a_mapping_faults_in_its_source` maps a store over a
  source shared and privately, reads a page nothing had read, and writes
  another privately. Its negative control, the fill taken out, stops the
  boot with *a shared mapping of a store over a page source read something
  other than the source's byte*.

`AArch64` is the same script with arm64 packages and is not needed for the
exit. Neither is compiling anything larger than `hello.rs`: Cargo, a crate
graph and a build of Ferrix itself are stage 20's.

---

