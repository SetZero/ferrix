# Stage 20 — Self-hosting

Build Ferrix — and its compositor — on Ferrix. At that point the acceptance
test writes itself: the image produced by the Ferrix-hosted compiler boots
and passes every test above. Moved from 17 on 2026-09-13, when the compositor
became the goal after `rustc`; it is not on the compositor's path, and the
compositor is cross-compiled until it is.

**Exit:** the image the Ferrix-hosted compiler produces boots and passes every
test above.

**The first step is met (2026-09-23): Ferrix builds its own x86-64 image.**
`cargo xtask test-selfhost` gives the guest one btrfs volume, mounted at
`/data`: the stage 16 toolchain -- now with Cargo and the standard libraries
for `x86_64-unknown-none` and `x86_64-unknown-uefi` --, every file git tracks
in the checkout, and the workspace's crates.io dependencies from `cargo vendor`
with a Cargo home pointing at them. zinc runs `cargo xtask build --arch
x86_64` there, the command a person runs on a Linux host: Cargo compiles xtask
for the guest, a glibc program like `rustc`, and xtask has Cargo compile the
loader, the kernel and the native programs and writes the FAT image, with no
network. The kernel commits the volume on its way to the power-off. The host
then has `btrfs check --check-data-csum` judge what Ferrix wrote, takes the
image and the kernel ELF out with `btrfs restore`, and boots that image with
`test-boot`'s judgement. On nazuna under KVM, with four processors and 8 GiB,
the build takes about 90 seconds of the guest's time:

```
   93.49 |   image /data/src/build/x86_64/ferrix.img (63 KiB loader, 75510 KiB kernel, 10666 KiB initramfs)
   93.54 |   init     the shell exited with 20
  x86_64: btrfs check found nothing wrong
  x86_64: booting the image Ferrix built
  x86_64: boot ok
  x86_64: Ferrix built its own image, and it booted
```

The toolchain was never the problem: the same tree builds the same image in a
`bwrap` sandbox on the host with nothing else of the host's. Four things in
the kernel were, and the gate found three of them:

* **Every file written on a writable btrfs was at most a page long.** The
  writable mount answered a write with the offset it started at rather than
  the one past it, so the kernel, which copies a `write` in 4 KiB at a time,
  put every piece over the first. Stage 12 had written only through offsets
  it named. rustc read back object files of exactly 4096 bytes.
* **A btrfs file's writes could go with its inode.** The dirty pages and the
  new length lived in the inode object, and only the VFS's bounded cache of
  names kept that alive; a file closed and pushed out came back as the last
  commit had it. Found by reading, on the way to the first; the mount now
  holds every inode with dirty pages until its writeback.
* **`MAP_FIXED` was two steps.** An unmap, which lets the space's lock go for
  its shootdown, and then a map; another thread's `mmap` could take the range
  between them. jemalloc re-maps its memory that way, and rustc died with
  `SIGSEGV` within minutes, once reading memory it had just been given from a
  three-page hole between two of its regions. A per-space layout lock now
  makes each call that changes the map one step, as `mmap_lock` does on Linux.
* **jemalloc did not know memory overcommits.** With no
  `/proc/sys/vm/overcommit_memory` it assumed it must give memory back, and
  did so with a `MAP_FIXED` pair and a shootdown each time: xtask alone took
  minutes to compile and the address space broke into thousands of regions.
  The file now says 1, Linux's `OVERCOMMIT_ALWAYS`, which is what the kernel
  does.

Building the C programs on Ferrix found three more, all fixed (2026-09-23
and 24):

* **What `lld` wrote through a mapping was lost.** It writes its output
  through `MAP_SHARED`, which a writable btrfs never wrote back, so a
  proc-macro uutils needs read back as invalid metadata. See stage 12.
* **GNU grep could not drain a pipe.** Writing to `/dev/null` from a pipe,
  grep empties its input with `splice` and falls back to `read` only on
  `EINVAL`; Ferrix answered `ENOSYS`, so `echo x | grep x >/dev/null` failed
  and curl's `configure` concluded there was no grep. `splice` and
  `copy_file_range` now go through a kernel buffer as `sendfile` does, with
  the boot's pipe check calling both by number.
* **Every new file was dated 1970.** The filesystems' clock read the counter
  since boot after `CLOCK_REALTIME` had learned firmware's time, so a file
  written now was older than any a tar archive unpacked, and automake's
  "newly created file is older than distributed files" stopped curl's
  `configure`. Files are dated by `CLOCK_REALTIME` now, and the pipe check
  requires it.

**Every build, not only the image (2026-09-24).** The rest of the exit is
the test matrix booting only what Ferrix compiled, and three pieces for it
are in place:

* `FERRIX_BUILDS=record:<DIR>` makes xtask write down every build it makes
  -- Cargo's and the C programs' build scripts, each keyed by SHA-256 over
  its command, its environment and the files it reads -- as a plan, while
  the matrix runs as usual (`xtask/src/builds.rs`).
  `FERRIX_BUILDS=replay:<DIR>/store` makes no build at all: each is answered
  from the store, and one the store lacks fails with "was not built on
  Ferrix".
* `cargo xtask test-selfhost --plan <DIR>` gives Ferrix the plan, the
  vendored crates of every workspace and the sources the C builds read, and
  zinc runs `cargo xtask builds-execute` there; the store it writes comes
  back to the host.
* `scripts/test/selfhost-matrix.sh record|replay <DIR>` runs the matrix both
  ways, in a home of its own. A plan belongs to the tree it was recorded on.

The toolchain grew to match: every target's standard library, gcc and g++ 15,
binutils, make, cmake, ninja, meson, bison, pkg-config, Perl, Python and
wayland-scanner 1.24.0 for foot, 151 Debian packages pinned by
`scripts/fetch/fetch-rustc-sysroot.sh`. foot builds from that tree alone in a
sandbox shaped like the guest. On 2026-09-24 the whole matrix recorded 147
builds; 26 of its 31 rows passed, and the five that failed were kernel
self-check flakes and one slow frame on a host at a load of 20 to 50. Ferrix
then made builds of the plan until two runs in a row stopped on FX-0001, a
processor that did not answer a TLB shootdown within its one second; that is
the next step, below.

What the exit needs now:

* **Room for a preempted processor.** Both FX-0001 stops came with nazuna at
  a load of 42 to 52 on 24 cores, eight virtual processors deep in a parallel
  build. `smp.rs` already gives a shootdown holder four seconds because "a
  holder preempted on a host with more virtual processors than real ones can
  lose whole seconds without being stuck"; the processors it waits on get
  one. Giving them the same room, as Linux's unbounded wait does, is the fix
  to try first; then run the plan again.
* **The plan made on Ferrix, then replayed.** Record on the final tree,
  `test-selfhost --plan`, then `selfhost-matrix.sh replay` until every row
  passes on what Ferrix built.
* **The Arm C programs.** The Arm ports and busybox are built with cross gcc
  and ferrousli's Arm `libc.so.6` with Rust targets the sysroot lacks
  (`aarch64-unknown-linux-gnu`, `armv7-unknown-linux-gnueabihf`,
  `armv7-unknown-linux-musleabihf`), so the matrix's Arm rows boot Alpine's
  musl busybox and `test-shell` on ferrousli's loader runs on x86-64 only.
* **Chrome.** `test-chrome` needs the volume `scripts/fetch/fetch-chrome.sh`
  makes, and is not in the matrix.

---

