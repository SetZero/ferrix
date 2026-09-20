# Chrome on Ferrix: what it would take

Asked by the customer on 2026-09-18, and answered by reading the tree rather
than by guessing. This is an assessment, not a decision and not a stage: what
a browser needs, what Ferrix has, what is missing, and what the missing part
would cost. Whether any of it is worth doing is the product owner's, and
`docs/BACKLOG.md` carries no row for it until they write one.

`docs/ROADMAP.md` stage 22 already names a browser once -- Steam's client
starts one as a helper -- but a browser of Ferrix's own is on no stage, and
nothing on the roadmap arrives at one on the way to something else.

**The short answer:** on the order of 120 to 170 points of new work, on top of
roughly 95 points already planned for other reasons. That is Steam's shape and
Steam's size, and like Steam most of the total is discovered by running the
thing rather than by planning it.

---

## 1. What is already in a browser's favour

This is further along than the question usually starts from, and the reasons
are worth naming because each was built for something else and pays here.

* **The system-call surface.** About 237 of the 263 names `libs/linux-abi`
  carries are answered with real work. The ones Chrome's process model stands
  on are among them: `clone`/`clone3` with real threads, `futex`, all six
  `epoll` calls, `eventfd2`, `memfd_create` with seals, and `AF_UNIX` with
  `SCM_RIGHTS` descriptor passing and full `cmsg` handling. That last one is
  Mojo, Chrome's own IPC, and it works today.
* **The C library.** `docs/POSIX-2024.md` counts 1040 of POSIX.1-2024's 1243
  interfaces present in ferrousli, none stubbed. Threads are complete down to
  robust and priority-inheriting mutexes and cancellation, and the thread
  control block keeps glibc's layout.
* **A C++ runtime that is exercised.** `ferrousli/tools/ports/libcxx` builds
  LLVM 23.1.1's libc++, libc++abi and libunwind against ferrousli, exceptions
  and all, and btop 1.4.7 -- a C++23 program with threads -- draws its panels
  on Ferrix. curl fetches over HTTPS and git clones, both built the same way.
* **A compositor a toolkit will start against.** `compositor/hyprix` advertises
  every global Chromium's Ozone backend binds: `wl_compositor`,
  `wl_subcompositor`, `wl_shm`, `wl_seat`, `wl_data_device_manager`,
  `xdg_wm_base` at 6, `zxdg_decoration_manager_v1`, `wp_viewporter`,
  `wp_fractional_scale_v1`, `zwp_text_input_v3`, `wp_presentation`, and the
  relative-pointer and pointer-constraints pair. The `wl_shm` buffer path needs
  nothing this compositor has not got.

---

## 2. The four things that actually stop it

### 2.1 Dynamic linking

Chrome is a position-independent executable linked against glibc, and it
`dlopen`s more at run time. `kernel/src/syscall/load.rs` reads `PT_INTERP` and
refuses it by name (`LoadError::NeedsInterpreter`), and ferrousli's `dlopen`
returns null by design. A fully static Chromium against a musl-shaped library
is not a configuration anybody ships: Alpine, which is the only distribution
that builds Chromium against musl at all, builds it dynamically and carries a
patch set to do it.

This is the 39-point section `docs/ROADMAP.md` already stages, and §4 below
says how much of it is done. It is unavoidable on either route.

### 2.2 The exec path cannot load a binary this size

`execve` reads the whole file through `fs::read_file`, whose `READ_FILE_LIMIT`
is 64 MiB (`kernel/src/fs/mod.rs`), and `load.rs` maps the segments and copies
the image in. There is no demand-paged, file-backed executable mapping and no
page cache behind `execve`. Chrome's binary with its resources is 180 to 250
MiB.

This is the same wall btop hit at 4.6 MiB one order of magnitude up, and the
fix is a different one: btop was cured by reading into a `vmap::Buffer` instead
of one heap block, and this needs the pages to arrive on fault from the file.

### 2.3 Nowhere to put it, and not enough memory

The root filesystem is the initramfs, which is RAM; btrfs is read-only until
stage 12; the guest is 512 MiB by default. Chrome wants something like 2 GiB to
open one page. And `madvise` decodes but has no handler, so PartitionAlloc and
V8 could never give memory back -- on a guest this size that is the difference
between slow and dead.

### 2.4 The sandbox, and a bug worth fixing whatever is decided

Chrome's zygote wants user, pid and mount namespaces and a seccomp-bpf filter.
Ferrix has neither: `unshare` refuses everything it cannot honour
(`kernel/src/syscall/namespace.rs`), and there is no `seccomp` or `bpf` number
at all. Running `--no-sandbox` is the honest first target; stage 13 is the
other answer.

But `clone` and `clone3` do not check the `CLONE_NEW*` flags at all.
`libs/linux-abi` defines them -- `CLONE_NEWNS`, `CLONE_NEWUSER`, `CLONE_NEWPID`
and `CLONE_NEWNET` are all in `types.rs` -- and no line of
`kernel/src/syscall/family.rs` ever tests one: `clone_with` checks the
`CLONE_THREAD`, `CLONE_SIGHAND` and `CLONE_VM` combinations and `CLONE_PIDFD`,
and nothing else. So `clone(CLONE_NEWUSER|CLONE_NEWPID|SIGCHLD)` succeeds and
hands back an ordinary child in the one namespace there is. A program that asks
for a sandbox is told it got one. `unshare` is honest about exactly the same
request, which makes this an inconsistency inside the kernel as well as a lie
to the caller. It is a small fix and it is worth making on its own account.

---

## 3. The smaller things, each of which would bite

* **No vDSO.** `AT_SYSINFO_EHDR` is deliberately absent, so every
  `clock_gettime` is a trap. Chrome calls it per task, per timer and per trace
  point.
* **No `/dev/shm`.** The initramfs makes `bin`, `dev`, `etc`, `proc` and `tmp`
  and nothing else, though `mount -t tmpfs` works at run time. Chromium's
  shared memory prefers `memfd_create`, which exists with seals, but falls back
  to `/dev/shm`, and ferrousli's named semaphores live there too. One line.
* **No `timerfd` and no `signalfd`.**
* **No AVX.** `kernel/src/arch/x86_64/switch.rs` saves a 512-byte `FXSAVE`
  area -- x87 and SSE -- and `CR4.OSXSAVE` is never set, so `CPUID` reports no
  OS support and V8 and Skia fall back to SSE2. That is correct rather than
  corrupting, and it is slow.
* **Four C ports that do not exist:** libwayland-client and libxkbcommon, which
  Ozone links; and fontconfig with freetype and expat, plus an actual font on
  the image. The compositor draws its own text with the coverage cells
  `compositor/term` carries, and no client could ask it for a font. Note that `compositor/README.md`'s "no C
  device stack, ever" is a rule about the compositor, not about its clients.
* **No audio at all**, which a browser survives and a person notices.

---

## 4. Where dynamic linking and btrfs write actually stand

Checked on 2026-09-18, because both are prerequisites above and both were
believed to be further along than they are.

**Dynamic linking: about 5 of the 39 points are on `main`.** `ffe7b264` places
an `ET_DYN` image with no interpreter at `PIE_BASE` and moves the entry,
`AT_PHDR` and the heap with it, with a boot check that loads a synthetic static
PIE on all three architectures. It landed for the threads work, because rustc's
default musl x86-64 target is a static PIE. `f7779ab5` is documentation: it
staged the 39-point plan across the roadmap, the backlog and the model, and
changed no code.

**Overtaken on 2026-09-20:** `PT_INTERP` is loaded now. `execve` places the
linker the program names at `INTERP_BASE`, enters it, and fills `AT_BASE`;
`libs/elf` reads the path. That is 3 of the kernel half's 5 points, so **31
remain**, and what remains is the part this paragraph already said was the
expensive one -- there is still no loader anywhere. What follows is how it
stood on 2026-09-18.

What was not done is `PT_INTERP`, and the hook for it is one place --
`load.rs`'s refusal. `libs/elf` already parses the relative relocations a
static PIE carries and reads symbol tables; ferrousli has the load-bias
arithmetic, a real `dl_iterate_phdr` and a real `dladdr`, which is what makes
C++ unwinding work. What does not exist anywhere, on any ref, is a loader: a
search of every commit in the repository for `PT_INTERP`, `DT_NEEDED`,
`JUMP_SLOT` and GNU-hash code finds only musl's vendored
`ferrousli/include/elf.h`. **34 points remain**, not 39.

**btrfs write: no code, on any ref.** Searches across every commit for
`delayed_ref`, a transaction commit and the free-space tree return nothing.
`libs/btrfs` says in its own header that it "knows nothing about transactions
or allocation", and `libs/btrfs-vfs` answers `EROFS` from `write_at`,
`set_len` and `create`.

What is there, and it is the expensive half, landed with stage 11: the page
cache (`ffc95eac` and `f36551cf`, a file's VMO filled from a `PageSource`),
file-backed `MAP_SHARED` writing through to the file (`c63167ee`), and a block
stack that can already write -- `libs/virtio-blk` has `Write` and `Flush`
request types and `libs/blkring` copies write payloads in. Writing a sector is
plumbed end to end and nothing above it uses that path. Three recorded
decisions park log-tree replay, eviction and writeback here.

Neither item has an owner: `docs/BACKLOG.md`'s owners table has both in the
`open` row.

One cleanup this turned up: `stage8-filemmap`, `vmo-map-wip` and `757065aa` on
`origin` look like pending work and are not -- their subjects duplicate commits
already on `main`. They are rebase leftovers.

---

## 5. What it would cost

Sizes are in the roadmap's currency and are first guesses, not an owner's
estimate. The first two rows are wanted for other reasons and are counted
separately for that reason.

| what | points |
|---|---|
| **Already planned:** dynamic linking, the rest of it | 34 |
| **Already planned:** stage 12, btrfs write | ≈ 60 |
| **Already planned, only if the sandbox is wanted:** stage 13 | ≈ 60 |
| Demand-paged file-backed `execve`, and binaries past 64 MiB | 13 |
| `madvise`, a vDSO, `/dev/shm`, `timerfd` and `signalfd` | ≈ 15 |
| libwayland-client, libxkbcommon, fontconfig with freetype and expat, a font | 13 |
| The Chromium cross-build against ferrousli, with Alpine's musl patches rebased | 40+, mostly unknown |
| What running it finds missing | unsized, ≥ 40 |

The GPU path of stage 19 (52 points, `docs/GPU.md`) is not required and is the
difference between a browser and a slideshow. It began on 2026-09-18, while
this was being written: step 1's wire format is complete and a program can
open step 2's render node, and what stands between that and a frame the GPU
drew is virgl's encoder, which is step 3b. The compositor still draws every
pixel on the CPU.

---

## 6. The order worth doing it in

Two milestones before the browser, each of which is worth having on its own.

**First, a foreign toolkit client inside the guest.** `foot` is already the
compositor's real-client probe, but `compositor/hyprix/probe/real-client.sh`
says in its own header that it is a development-host check: no client that was
not written against this tree's crates has ever run *on* Ferrix. Building
libwayland-client and libxkbcommon against ferrousli and running foot in the
guest proves the client story end to end for a fraction of a browser's cost,
and it is on the browser's path rather than beside it.

**Then headless Chrome, not a window.** `chrome --headless --screenshot` drops
the compositor, the GPU, input and the whole font-theme surface, and still
exercises everything genuinely hard: the multi-process model, Mojo over
`AF_UNIX`, hundreds of threads, PartitionAlloc and V8. If a headless Chrome
writes a PNG on Ferrix, the Wayland half afterwards is comparatively small,
because §1 says the compositor is already ready for it.

And two fixes that stand on their own merits, whatever is decided about any of
the above: mount `/dev/shm`, and make `clone` refuse `CLONE_NEW*` rather than
ignore it.

---

## 7. What was checked, and what was not

Read for this: `kernel/src/syscall/` against the `Syscall` enum, `libs/elf`,
`libs/btrfs` and `libs/btrfs-vfs`, `libs/virtio-blk` and `libs/blkring`,
`ferrousli/src` and `ferrousli/tools`, `compositor/server` and
`compositor/hyprix`, `xtask/src/initramfs.rs`, and the history of every ref for
the two prerequisites in §4. Chromium's own requirements were taken from
Alpine's `community/chromium` APKBUILD and its musl patch set, which is the
only evidence that a Chromium against a musl-shaped C library builds at all.

Not checked, and each could move the numbers: whether Chromium's build system
can be pointed at a sysroot shaped like ferrousli's without a patch of its own;
how much of Alpine's patch set applies to a library that is closer to glibc
than to musl; and what a 512 MiB guest does to a program that expects to be
told how much memory it has. None of those is answerable without trying it,
which is the honest reason the last row of §5 is unsized.
