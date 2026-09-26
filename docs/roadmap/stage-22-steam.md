# Stage 22 — Steam  ·  *unsized, over 300 points*

Steam runs on Ferrix, logs in, and a game bought there plays. Put on the
roadmap by the customer on 2026-09-18 as the step after the GPU decision. It
does not wait for stage 21: it is a guest's stage first, on the GPU of Path
A, and moves to bare metal when stage 21 does.

Steam is the hardest Linux program to be somebody else's binary for, which
is why it is the right exit for `docs/ARCHITECTURE.md` §2's promise: a
closed client that is a 32-bit program starting 64-bit helpers, one of them
a whole browser; a runtime that containerises every game with bubblewrap;
and games that draw with Vulkan through Wine. Everything below is something
Steam needs that Ferrix does not have, in the order it can be tested. The
dynamic linking section above already says the first part; the rest was not
staged until now.

* **The 32-bit x86 ABI.** The Steam client itself is an `i386` program
  today, and every 32-bit Windows game Proton runs is 32-bit Wine underneath,
  so this is needed whether or not Valve's 64-bit client arrives first. The
  `i386` system call table and its `compat` layouts -- `struct stat64`,
  `off_t` pairs, `epoll_event` packed, `iovec` and `msghdr` with 32-bit
  pointers -- entered through `int $0x80` and the vDSO-less path glibc falls
  back to, 32-bit address spaces below 4 GiB on the 64-bit kernel, 32-bit
  `mmap` and `brk` limits, `TLS` through `set_thread_area` and the `GDT`
  entries it needs, 32-bit signal frames, and `AT_SYSINFO` absent as
  `AT_SYSINFO_EHDR` is. x86-64 only; the Arm architectures have nothing
  to run. Unsized: the table is as long as stage 7's was. Under way
  since 2026-09-26, at the customer's request: `docs/I386.md` sizes I1 to
  I4 at 42 points, taking it from the way in to Alpine's and Debian's i386
  busyboxes and an i686 thread program, and I5, unsized, is what the
  Steam runtime finds missing. **I1 is on `main`** (2026-09-26): the GDT
  in Linux's order and numbers with real 32-bit segments, `int $0x80` as a
  DPL-3 gate decoding through an i386 table in `libs/proto/linux-abi`, an
  `EM_386` image `execve`d into a 4 GiB space and entered in compatibility
  mode, and compatibility mode's `SYSCALL` and `SYSENTER` made harmless;
  the boot runs a hand-assembled i386 program. **I2a is on `main`** the
  same day (5 points): the thread pointer, through `set_thread_area`,
  `get_thread_area` and `%gs`, with each thread's GDT descriptors and
  segment selectors kept across a switch, and every saved selector checked
  against the descriptor it names before it is loaded; reviewed by the
  certification consultant. I2b, `fork`, `clone` and the signal frames, is
  next.
* **glibc's place, taken.** The dynamic linking stage's third part, glibc's
  names, with Steam as its stress test: `ld-linux` and `libc.so.6` requested
  by name, `dlopen` from the client and from every Steam runtime library,
  `GLIBC_2.x` versions back to the ones a 2012 runtime binary asks for, and
  the `/etc/ld.so.cache`, `LD_PRELOAD` and `LD_LIBRARY_PATH` behaviour the
  runtime's launcher scripts lean on. 13 points are priced there, and 10 of
  them are done on x86-64 (2026-09-21 and 22): `ld-linux` and `libc.so.6` by
  name with glibc's versions, `dlopen`, and `LD_LIBRARY_PATH`. `LD_PRELOAD`
  is not there, and `/etc/ld.so.cache` is deliberately not read. Steam adds
  whatever those scripts find missing, unsized.
* **The container the games run in.** Steam's `pressure-vessel` runs each
  game inside bubblewrap: user, mount and pid namespaces, `pivot_root`,
  `seccomp` filters, and the Chromium helper's own sandbox on top. That is
  stage 13 entire, plus what stage 13 does not name and Steam will:
  `/proc/<pid>/` fields the runtime reads (`inotify`, `pidfd_open` and
  `pidfd_send_signal` have been answered since 2026-09-27, `SO_PEERCRED`
  since 2026-09-13), and
  `prctl` beyond what stage 7
  answers. Stage 13's month, plus 13 points of the rest.
* **Somewhere to put it.** A Steam library is tens of gigabytes on a
  filesystem that survives a reboot: stage 12, btrfs write, and a root on
  it rather than an initramfs. Both are done (2026-09-21 and 22); what is
  left is a root volume larger than the 1 GiB `build/root.img` that `cargo
  xtask run` makes.
* **XWayland.** The client is an X11 program; stage 19 lists XWayland as
  its largest single gap and this is what finally needs it. An X server on
  the compositor's protocol: Xwayland built on ferrousli with its dynamic
  loading (the C path), or a Rust X server that answers the requests Steam
  and Wine make, which is the smaller subset than it sounds and the larger
  program than it looks. `xwayland_shell_v1` on the compositor's side is a
  table. 40 points as a first guess, most of it the server, counted in
  stage 19's remainder and not again here.
* **Sound.** Playback is done, 2026-09-26 (`docs/AUDIO.md` §8): a
  `virtio-snd` driver in ring 3, the audio core and `/dev/snd`, and Chrome
  playing through them, which `test-audio` and `test-chrome-audio` gate.
  A driver that dies is started again by devmgr and the card comes back as
  `C0`, a dead driver's DMA pins kept by the kernel until the next driver has
  reset the device (F-38, 2026-09-26). alsa-lib and `aplay` are built on
  ferrousli and play through it (U1, 2026-09-27). What stage 22 still needs
  of it is the server. As it was written: Ferrix had no audio at all: a `virtio-snd` driver in ring 3,
  an audio core with a `/dev/snd` shaped enough for a client library, and a
  server speaking the PulseAudio or PipeWire protocol over a Unix socket,
  which is what Steam and every game link against. 30 points as a first
  guess. `docs/AUDIO.md` is the design of the first two parts (drafted
  2026-09-26; the customer moved it forward to current work the same
  day): 24 points for the driver, the core and a gate on QEMU's `wav`
  backend, with the server unsized.
* **The GPU, with Vulkan.** Path A gives OpenGL, which the client and a
  native game can use. Proton draws with Vulkan through DXVK, and Vulkan
  under virtio-gpu is Venus, which needs a Linux host with KVM: on the
  Windows machine, Proton games wait for stage 21, and native GL games do
  not. Venus is Path A's steps 1 and 2 again for a second capability set,
  8 points on top of Path A, and Mesa's Venus driver on ferrousli or a Rust
  Vulkan loader over it, which is Path A's 3a question asked a second
  time.
* **Wine and Proton.** Not Ferrix's to write and everything Ferrix's to
  run: Proton is Wine plus DXVK plus the runtime, and every one of its
  kernel needs is one of the bullets above. What it will find missing is
  found by running it.

**Exit,** in three steps, each a boot of its own the way stage 18's and
19's are: the Steam client starts on Ferrix, logs in and shows its store,
with the browser helper drawing; a native Linux game from the library
installs to btrfs, launches and draws through the GPU path with sound; and
a Windows game runs through Proton. The first two are a guest's exit on
the Linux host; the third is the GPU's Vulkan, wherever that comes first.

None of it is sized past a first guess, and the sum of the first guesses is
already over 300 points, so the stage is written as a list of what has to be
true rather than a plan.

---

