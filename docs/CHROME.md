# Chrome on Ferrix: what it would take

Asked by the customer on 2026-09-18, and answered by reading the tree rather
than by guessing. This is an assessment, not a decision and not a stage: what
a browser needs, what Ferrix has, what is missing, and what the missing part
would cost. Whether any of it is worth doing was the product owner's to say,
and on 2026-09-23 the customer asked for the work to start.

## Where it stands, 2026-09-24

**Chrome is not on the image, and does not run on Ferrix.** No part of
Chromium has been built, ported or tried yet. What exists is the ground it
would stand on, and since 2026-09-24 the first foreign Wayland client
running on it:

| | state |
|---|---|
| Dynamic linking: a glibc program on ferrousli's loader and `libc.so.6`, `dlopen` and every TLS model | **done** 2026-09-23, on all three architectures (§2.1, §4) |
| Somewhere to put it: btrfs written from Ferrix | **done** 2026-09-21, stage 12 (§2.3) |
| `/dev/shm`, and `clone` refusing the namespaces it cannot give | **done** 2026-09-19, on `main` 2026-09-23 (§2.4, §3) |
| The compositor a window would appear in, drawing on the GPU | **done**, stage 19's GPU path (§1, §5) |
| `execve` of a binary past 64 MiB, mapped from the file on demand | **done** 2026-09-24, on all three architectures, with `execve("/proc/self/exe")` from a fork (§2.2) |
| `timerfd` | **done** 2026-09-24, on all three architectures, the first kernel row of §6's foot (§3) |
| `madvise` and `signalfd` | **done** 2026-09-24, on all three architectures (§2.3, §3) |
| A vDSO | not started (§3); what is left of the ≈ 10 points the three were sized at |
| libwayland-client, libxkbcommon, fontconfig with freetype and expat, a font | **done** 2026-09-24, built against ferrousli with foot (§3, §6) |
| foot, a Wayland terminal nobody here wrote, drawing on the compositor on Ferrix | **done** 2026-09-24, x86-64, `cargo xtask test-foot` (§6) |
| Chromium built against ferrousli, with Alpine's musl patches rebased | not started (§5), 40+ points |
| What running it finds missing | unsized, ≥ 40 (§5) |
| A guest with the ~2 GiB a page wants | the default is 512 MiB (§2.3) |

**Done, 2026-09-24: a foreign toolkit client inside the guest** (§6).
`foot` 1.24.0, a real Wayland terminal nobody here wrote, runs on Ferrix's
compositor and draws a program's output in the image's font. It is built
by `ferrousli/tools/ports/foot` against ferrousli with libffi 3.5.2,
wayland 1.24.0, wayland-protocols 1.45, libxkbcommon 1.11.0, pixman 0.46.4,
freetype 2.14.1, expat 2.7.3, fontconfig 2.17.1, tllist 1.1.0 and fcft
3.3.2, and DejaVu Sans Mono 2.37 is the font. **Next:** `chrome --headless
--screenshot`, which needs no compositor, GPU, input or fonts but
exercises everything hard -- processes over Mojo, hundreds of threads,
PartitionAlloc and V8 -- and needed two kernel rows first: `execve` past
64 MiB, done on 2026-09-24 (§2.2), and `madvise`, done the same day (§2.3).
A vDSO, the other half of that second row, is still to come; Chrome runs
without one, a trap per clock read. **Then** a window, for which foot has
now proved the client libraries.

**Re-checked on 2026-09-19**, against a tree 37 commits further on. Everything
in §2, §3 and §4 still holds but the two loose fixes in §6, which are now
done, and §5's last paragraph, which was overtaken the day after it was
written. Each is marked where it stands. One of the two turned out not to cost
what this document said it did, which is recorded rather than quietly
corrected: see §3.

**Re-checked on 2026-09-23**, when the customer asked for this work to start.
Two of §2's four walls have fallen since: dynamic linking is done on all
three architectures, `dlopen` included (§2.1, §4), and btrfs is writable
(§2.3). §2.2, the rest of §2.3 and §3 stand as written; §5's first row is
now zero.

`docs/ROADMAP.md` stage 22 already names a browser once -- Steam's client
starts one as a helper -- but a browser of Ferrix's own is on no stage, and
nothing on the roadmap arrives at one on the way to something else.

**The short answer:** on the order of 120 to 170 points of new work, on top of
roughly 95 points that were already planned for other reasons when this was
written -- of which dynamic linking and btrfs write have since landed,
leaving stage 13's ≈ 60, and those only if the sandbox is wanted. That is Steam's shape and
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
* **The C library.** `docs/POSIX-2024.md` counts 1045 of POSIX.1-2024's 1243
  interfaces present in ferrousli, none stubbed -- 1040 when this was
  written, and the five pseudo-terminal calls foot opens its terminal with
  since. Threads are complete down to
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
`dlopen`s more at run time. **Done, 2026-09-23:** a program linked against
glibc runs on ferrousli's loader and `libc.so.6` in glibc's place on all
three architectures, with `dlopen`, `dlsym` and the rest of `dlfcn.h` and
every TLS model (§4). One limit is Chrome's to meet: a library `dlopen`ed
after start-up may not have a `PT_TLS` of its own yet, since that needs a
dynamic thread vector, and ANGLE's and Mesa's libraries are the kind a
browser opens late. A fully static Chromium against a musl-shaped library
is not a configuration anybody ships: Alpine, which is the only distribution
that builds Chromium against musl at all, builds it dynamically and carries a
patch set to do it.

This is the 39-point section `docs/ROADMAP.md` already stages, and §4 below
says how much of it is done. It is unavoidable on either route.

### 2.2 The exec path cannot load a binary this size

**Done, 2026-09-24.** `execve` does not read the program any more. It opens
the file and reads its first page -- a script's `#!` line, or an ELF file's
header and program header table, read on to the table's end if that is
further -- and maps the segments from the file's page cache, the same object
an `mmap` of the file maps (`kernel/src/syscall/program.rs`,
`kernel/src/syscall/load.rs`). Every page wholly inside one segment's file
contents is a private mapping of the file: read from the disk the first time
the program touches it, shared by every process running the same program,
and copied into the process's own object the first time it is written, so a
writable segment never writes its file. The partial pages at a segment's
two ends, a page two segments share and `.bss` are anonymous and are copied
or zeroed as before. A file with no object to map, and a program built into
the kernel, are still copied, a piece at a time rather than whole. The linker
`PT_INTERP` names is loaded the same way; the libraries it maps never had the
limit, since `mmap` of a file always mapped its object. What is left of a
limit is memory for page tables.

The boot check (`kernel/src/fs/exec_check.rs`, FX-0871) loads a 72 MiB
program whose file is served by a page source that counts what it is asked
for, as btrfs serves its page cache from disk. On every boot it reports
`a 72 MiB program was loaded reading 3 of its 18437 pages, 67 by the time it
had been touched and had run`: the headers' page, the data segment's and the
partial page the large segment ends on; then one run of 32 for a read 40 MiB
in, and one for a write 50 MiB in, which must read back while the file's page
keeps its byte. The bytes past the segment's file contents must be zeros
though the file's are not, and the program must run to its status. Made to
read the whole file again, the check fails with `loading read 18437 of the
program's 18437 pages`.

**`/proc/self/exe`, the same day.** Chrome starts every child process -- the
GPU process, the network service, each renderer -- by forking and running
`execvp("/proc/self/exe")`, and that failed with `ENOENT`, which Chrome
reports as `LaunchProcess: failed to execvp: /proc/self/exe` and then `GPU
process isn't usable. Goodbye.` Two things were missing. A fork child did not
inherit what its parent was started as, so its `/proc/self/exe` had no
target at all; and the link was followed as its text, so a program whose file
had been renamed or deleted could not be run again through it. A fork now
takes its parent's identity (`Process::forked`), and `/proc/<pid>/exe` is a
magic link, as on Linux: followed, it leads to the file the program was
loaded from, by `Inode::link_location` (`libs/vfs/src/walk.rs`), whatever its
name is now; read, it is that file's path with ` (deleted)` after a file since
removed. The boot check forks the sparse 72 MiB program, removes its name,
and has the fork `execve("/proc/self/exe")`, which must run it to its status.
With the fork's identity left out, as before, it fails with `errno 2`, and
with the link followed as text, with `errno 2` again.

**On Chrome, 2026-09-24**, x86-64 under KVM with 2 GiB, the 198 MB
`chrome-headless-shell` 154.0.8037.57 on the btrfs volume
`scripts/fetch-chrome.sh` makes (branch `chrome/headless`), run by `cargo
xtask test-chrome`:

* on Debian's `ld-linux` and glibc, `execve` of the 198 MB file succeeds, the
  linker maps its libraries and Chrome runs, until PartitionAlloc's first
  `madvise` is refused with `ENOSYS`. Its `CHECK` executes `int3`, which a
  program on Ferrix is ended for with `SIGSEGV`: `pid 215 ended by signal 11
  at 0x0, pc 0x55555726974d`, the `int3` after `madvise@plt` at
  `0x1d1474d`. With `madvise` answered 0, a trial kernel that is not landed,
  it prints `Google Chrome for Testing 154.0.8037.57`; `--dump-dom` then
  starts its child processes through `/proc/self/exe` with no `execvp`
  failure, and two of them end at a `CHECK` of their own
  (`+0x3f11f11`) while the browser ends reading a null pointer in libc.
  Those are the next track's, not this one's;
* on ferrousli's `ld.so` and `libc.so.6` in glibc's place, with the volume's
  other libraries on `LD_LIBRARY_PATH`, the program is loaded and the loader
  stops at the first glibc name ferrousli lacks: `ld-ferrousli: undefined
  symbol: program_invocation_short_name`.

Neither stops at `execve`. What is deliberately not done: `ETXTBSY`, so a
running program's file can be written, and the program sees the new bytes
in pages it has not yet copied, where Linux refuses the write; the partial
pages at a segment's ends are copied, not mapped, so `/proc/<pid>/maps` shows
a segment as its file's pages with an anonymous page either side, where
Linux shows one run; and `/proc/<pid>/cwd`, `root` and `fd/<n>` are still
followed as their text.

What follows is how it stood before.

`execve` reads the whole file through `fs::read_file`, whose `READ_FILE_LIMIT`
is 64 MiB (`kernel/src/fs/mod.rs`), and `load.rs` maps the segments and copies
the image in. There is no demand-paged, file-backed executable mapping and no
page cache behind `execve`. Chrome's binary with its resources is 180 to 250
MiB.

This is the same wall btop hit at 4.6 MiB one order of magnitude up, and the
fix is a different one: btop was cured by reading into a `vmap::Buffer` instead
of one heap block, and this needs the pages to arrive on fault from the file.

### 2.3 Nowhere to put it, and not enough memory

The root filesystem is the initramfs, which is RAM; btrfs was read-only
until stage 12, which landed on 2026-09-21, so there is now somewhere to put
a browser that is not memory; the guest is 512 MiB by default. Chrome wants something like 2 GiB to
open one page. And `madvise` decodes but has no handler, so PartitionAlloc and
V8 could never give memory back -- on a guest this size that is the difference
between slow and dead.

**`madvise` done, 2026-09-24** (§3): `MADV_DONTNEED` and `MADV_FREE` now give
a range's frames back to the allocator at once and leave it mapped, so the
next touch reads zeros. The memory is still 512 MiB against Chrome's 2 GiB;
what changed is that an allocator which gives pages back actually gets them
back.

### 2.4 The sandbox, and a bug worth fixing whatever is decided

Chrome's zygote wants user, pid and mount namespaces and a seccomp-bpf filter.
Ferrix has neither: `unshare` refuses everything it cannot honour
(`kernel/src/syscall/namespace.rs`), and there is no `seccomp` or `bpf` number
at all. Running `--no-sandbox` is the honest first target; stage 13 is the
other answer.

**Done, 2026-09-19:** `clone` and `clone3` refuse the `CLONE_NEW*` flags with
`EINVAL`, as `unshare` always did and as a Linux built without `CONFIG_*_NS`
does. A ring-3 program on all three architectures proves it. What follows is
what the state was, and why it mattered.

But `clone` and `clone3` did not check the `CLONE_NEW*` flags at all.
`libs/linux-abi` defines them -- `CLONE_NEWNS`, `CLONE_NEWUSER`, `CLONE_NEWPID`
and `CLONE_NEWNET` are all in `types.rs` -- and no line of
`kernel/src/syscall/family.rs` ever tested one: `clone_with` checked the
`CLONE_THREAD`, `CLONE_SIGHAND` and `CLONE_VM` combinations and `CLONE_PIDFD`,
and nothing else. So `clone(CLONE_NEWUSER|CLONE_NEWPID|SIGCHLD)` succeeded and
handed back an ordinary child in the one namespace there is. A program that
asked for a sandbox was told it got one. `unshare` was honest about exactly the
same request, which made it an inconsistency inside the kernel as well as a lie
to the caller. It was a small fix and it was worth making on its own account.

It changes nothing about §2.4's real answer: `--no-sandbox` is still the
honest first target, and stage 13 is still the other one. What it changes is
that a program which asks for isolation now finds out it cannot have it.

---

## 3. The smaller things, each of which would bite

* **No vDSO.** `AT_SYSINFO_EHDR` is deliberately absent, so every
  `clock_gettime` is a trap. Chrome calls it per task, per timer and per trace
  point. **Still open, 2026-09-24:** of the three kernel rows §5 sized
  together -- `madvise`, a vDSO and `signalfd` -- this is the one that
  remains.
* ~~**No `madvise`.**~~ **Done, 2026-09-24.** The number decoded and nothing
  answered it, so PartitionAlloc and V8 (§2.3) could hand memory back and
  never get it. `MADV_DONTNEED` and `MADV_FREE` now drop a range's pages and
  leave it mapped: private anonymous memory reads as zeros on its next touch,
  a private file mapping as its file, and a shared mapping keeps its contents,
  as Linux's do. The frames go back to the allocator in `madvise` itself, in
  the order every unmap here keeps -- translations down under the address
  space's lock, one shootdown with the lock let go, and only then the frames.
  `MADV_FREE` is allowed to keep its pages until memory is short, and here
  drops them at once, as Linux does with no swap to age them against.
  `MADV_REMOVE` punches a hole in shared anonymous memory; on a file it is
  `EOPNOTSUPP`, as `fallocate`'s hole is here, which Chrome's discardable
  memory on a memfd only logs. The hints are accepted where Linux accepts
  them, and `MADV_WIPEONFORK` is refused rather than accepted and ignored,
  since BoringSSL keys its reseeding on it. The boot check counts the frames:
  eight written pages dropped must come back as exactly eight frames, on all
  three architectures.
* ~~**No `/dev/shm`.**~~ **Done, 2026-09-19**, and it was not one line, which
  is what this said. Chromium's shared memory prefers `memfd_create`, which
  exists with seals, but falls back to `/dev/shm`, and ferrousli's named
  semaphores live there too. Adding the directory to the initramfs would have
  done nothing: the boot mounts devfs on `/dev` and that shadows whatever the
  archive unpacked there. devfs cannot create a name and has no storage, so
  `/dev/shm` has to be a tmpfs mount — and nothing could be mounted anywhere
  inside `/dev`, because devfs does not cache lookups and a mount point is a
  cached dentry. That took a VFS change, `Inode::caches_lookup_of`, which lets
  a directory of coming-and-going names keep the one name that never changes.
  Roughly 200 lines with its two checks rather than one. The lesson is the
  usual one: a cost this document calls trivial is the kind most worth
  checking before it is quoted.
* ~~**No `timerfd` and no `signalfd`.**~~ **Both done, 2026-09-24:
  `timerfd` first, `signalfd` the same day.** Chrome's and glib's event
  loops take `SIGCHLD` and `SIGTERM` through a `signalfd` in their epoll
  set: the signals are blocked, and a read takes each one pending as a
  `signalfd_siginfo`. `signalfd4` answers on all three architectures and
  `signalfd` on the two that have it, with `SFD_NONBLOCK` and
  `SFD_CLOEXEC`, and a read takes the reading thread's signals and then
  its process's, as `rt_sigtimedwait` does. The waking wanted the same
  care as `timerfd`'s, for a different reason: a signal a thread blocks
  wakes nobody here, so a blocked read, `poll` or `epoll_wait` would have
  learned of it only at its own recheck a second later. Every process now
  has a queue its signals wake as they become pending, Linux's
  `signalfd_wqh`, and the boot check requires that wake, not the recheck,
  to end each of the three waits; they came back within 90 us of the
  signal on x86-64, 102 us on AArch64 and 73 us on ARMv7-A (71 us at two
  processors). ferrousli's
  `signalfd` wrapper landed with it. As for `timerfd`: foot, §6's first
  client, calls `timerfd_create`,
  `timerfd_settime` and `timerfd_gettime` about 45 times: its cursor blink,
  its flash, a delayed render and key repeat. All three are answered on all
  three architectures, with the `time64` forms on ARMv7-A, on
  `CLOCK_MONOTONIC`, `CLOCK_REALTIME` and `CLOCK_BOOTTIME`, with
  `TFD_TIMER_ABSTIME` and `TFD_TIMER_CANCEL_ON_SET`. The thing worth
  checking was not the counting but the waking: a `poll` or `epoll_wait` on
  a timerfd sleeps up to a second between looks of its own, so a timer that
  became readable only when somebody looked would blink a cursor a second
  late. A kernel thread, `timerfds`, sleeps until the earliest deadline and
  wakes the timer's waiters there. The boot check arms a timer only once a
  blocked read, a `poll` and an `epoll_wait` are each waiting on it, and
  requires the thread's wake to end each wait within a quarter of that
  second. In the seven boots made for it on nazuna's QEMU -- x86-64,
  AArch64, and ARMv7-A at four processors and at two -- no waiter came back
  more than 3.5 ms after its deadline, and most within half a millisecond.
  What is not as Linux has it is in `docs/ROADMAP.md`, stage 17.
* **No AVX.** `kernel/src/arch/x86_64/switch.rs` saves a 512-byte `FXSAVE`
  area -- x87 and SSE -- and `CR4.OSXSAVE` is never set, so `CPUID` reports no
  OS support and V8 and Skia fall back to SSE2. That is correct rather than
  corrupting, and it is slow.
* ~~**Four C ports that do not exist.**~~ **Done, 2026-09-24,** for foot
  (§6): libwayland-client and libxkbcommon, which Ozone links; and
  fontconfig with freetype and expat, plus an actual font on the image. They
  are static libraries in foot's build today; Chromium, built dynamically,
  will want them as shared ones. The compositor still draws its own text
  with the coverage cells `compositor/term` carries; a client brings its own
  fonts, which is what these are for. Note that `compositor/README.md`'s "no
  C device stack, ever" is a rule about the compositor, not about its
  clients.
* **No audio at all**, which a browser survives and a person notices.

---

## 4. Where dynamic linking and btrfs write actually stand

**Both are done.** Dynamic linking met its exit on all three architectures
on 2026-09-23: Debian's glibc busybox runs on ferrousli's loader and
`libc.so.6` with nothing of glibc on the image, after ferrousli itself was
ported to AArch64 and ARMv7-A. btrfs write, stage 12, landed on 2026-09-21.
`docs/ROADMAP.md` has both. What follows is the history of this section.

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

**Overtaken again on 2026-09-21: 27 of the 39 are done, 12 left.**
`ferrousli/ld` is a working loader -- symbol versions, `COPY`, initial-exec
TLS, `DT_FINI` -- and `ferrousli/tools/build-shared.sh` links ferrousli as a
versioned `libc.so.6`. Debian's own dynamic busybox runs on Ferrix with
glibc's loader on all three architectures, and with ferrousli's in glibc's
place on x86-64. What Chrome still needs of it is most of the 12:
`dlopen` and the rest of `dlfcn.h`, and general-dynamic TLS, which a
`dlopen`ed library uses. `docs/ROADMAP.md`'s dynamic-linking section is the
current account.

What was not done is `PT_INTERP`, and the hook for it is one place --
`load.rs`'s refusal. `libs/elf` already parses the relative relocations a
static PIE carries and reads symbol tables; ferrousli has the load-bias
arithmetic, a real `dl_iterate_phdr` and a real `dladdr`, which is what makes
C++ unwinding work. What does not exist anywhere, on any ref, is a loader: a
search of every commit in the repository for `PT_INTERP`, `DT_NEEDED`,
`JUMP_SLOT` and GNU-hash code finds only musl's vendored
`ferrousli/include/elf.h`. **34 points remain**, not 39.

*(btrfs write is overtaken too: stage 12 landed on 2026-09-21, and the
roadmap has how it stands. What follows is 2026-09-18.)*

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
| ~~**Already planned:** dynamic linking, the rest of it (34 when this was written; 12 on 2026-09-21)~~ *done 2026-09-23* | ~~12~~ |
| ~~**Already planned:** stage 12, btrfs write~~ *landed 2026-09-21* | ~~≈ 60~~ |
| **Already planned, only if the sandbox is wanted:** stage 13 | ≈ 60 |
| Demand-paged file-backed `execve`, and binaries past 64 MiB | 13 |
| A vDSO; `madvise` and `signalfd` were sized with it, and are done (`/dev/shm` 2026-09-19, `timerfd`, `madvise` and `signalfd` 2026-09-24) | what is left of ≈ 10 |
| ~~libwayland-client, libxkbcommon, fontconfig with freetype and expat, a font~~ *built with foot, 2026-09-24* | ~~13~~ |
| The Chromium cross-build against ferrousli, with Alpine's musl patches rebased | 40+, mostly unknown |
| What running it finds missing | unsized, ≥ 40 |

The GPU path of stage 19 (52 points, `docs/GPU.md`) is not required and is the
difference between a browser and a slideshow. It began on 2026-09-18, while
this was being written, and **it landed on 2026-09-19**: steps 1, 2, 3b and 5
are done, the desktop composites on the GPU, and a 1080p frame went from 39 ms
to 12 ms (`docs/GPU.md` §3.7 and §3.8). The sentence that stood here -- "the
compositor still draws every pixel on the CPU" -- was true for one more day.
So the row is not a cost a browser would have to carry; it is already paid.

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

**Done, 2026-09-24.** `cargo xtask ports` builds foot and the ten libraries
under it statically against ferrousli (`ferrousli/tools/ports/foot`), and
`cargo xtask test-foot` boots the compositor with foot running `hyprctl
version`. foot finds DejaVu Sans Mono through fontconfig, lays out a 7x13
grid, starts four render threads and draws the three lines, which came
through a ferrousli pseudoterminal. The test requires foot's own account
of the font and the grid, no error from it, and antialiased text on the
virtio-gpu's screen with the compositor's background around it. What it
took beyond the ports was small, and each piece was found by linking or
running rather than by reading:

* the kernel's `timerfd` (§3);
* nine names in ferrousli: the three `timerfd` wrappers, `posix_openpt`,
  `grantpt`, `unlockpt` and `ptsname`, `fallocate`, and `eaccess`, which
  libxkbcommon checks its include paths with;
* a user in `/etc/passwd`, which foot looks up for its shell even when it
  is given a program, and `/var/cache/fontconfig`, which fontconfig will not
  make itself.

What it still says, and none of it stops it: `fallocate` cannot punch a
hole in a memfd, so foot's buffer pool keeps pages it could give back;
libxkbcommon finds no `/usr/share/X11/xkb`, which it does not need while the
compositor hands it the keymap; and foot is ported to x86-64 only. It was
first run against the compositor on nazuna's own kernel, which proved the
static build a working Wayland client before the kernel had `timerfd`.
One thing the test's first version got wrong is worth keeping: it counted
colours on QEMU's default console, which was the firmware's text screen,
and passed. It reads the virtio-gpu now, and requires the compositor's
background, which the firmware's screen does not have.

**Then headless Chrome, not a window.** `chrome --headless --screenshot` drops
the compositor, the GPU, input and the whole font-theme surface, and still
exercises everything genuinely hard: the multi-process model, Mojo over
`AF_UNIX`, hundreds of threads, PartitionAlloc and V8. If a headless Chrome
writes a PNG on Ferrix, the Wayland half afterwards is comparatively small,
because §1 says the compositor is already ready for it.

And two fixes that stood on their own merits, whatever is decided about any of
the above: mount `/dev/shm`, and make `clone` refuse `CLONE_NEW*` rather than
ignore it. **Both are done, 2026-09-19.** They were the only part of this
document that was worth doing before anyone decides whether a browser is
wanted, because neither needs a browser to be worth having: the first is where
ferrousli's named semaphores already live, and the second was the kernel
telling a caller something untrue.

---

## 7. What was checked, and what was not

Read for this: `kernel/src/syscall/` against the `Syscall` enum, `libs/elf`,
`libs/btrfs` and `libs/btrfs-vfs`, `libs/virtio-blk` and `libs/blkring`,
`ferrousli/src` and `ferrousli/tools`, `compositor/server` and
`compositor/hyprix`, `xtask/src/initramfs.rs`, and the history of every ref for
the two prerequisites in §4. Chromium's own requirements were taken from
Alpine's `community/chromium` APKBUILD and its musl patch set, which is the
only evidence that a Chromium against a musl-shaped C library builds at all.

Read again on 2026-09-19, for the re-check at the top: `kernel/src/fs/devfs.rs`
and `libs/vfs`'s dentry cache, for what `/dev/shm` actually costs;
`kernel/src/syscall/family.rs` again; and, for §4, that no loader has appeared
(`DT_NEEDED` and `JUMP_SLOT` are still in no Rust in the tree), that
`libs/btrfs-vfs` still answers `EROFS`, and that `madvise`, `AT_SYSINFO_EHDR`,
`timerfd` and `signalfd` are all still where §3 left them.

Not checked, and each could move the numbers: whether Chromium's build system
can be pointed at a sysroot shaped like ferrousli's without a patch of its own;
how much of Alpine's patch set applies to a library that is closer to glibc
than to musl; and what a 512 MiB guest does to a program that expects to be
told how much memory it has. None of those is answerable without trying it,
which is the honest reason the last row of §5 is unsized.
