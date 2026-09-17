# Replacing busybox with the uutils family and zinc

Version 1. Written 2026-09-17 on the customer's order: *replace busybox with
uutils/coreutils*, and *btop, git and curl should link against ferrousli*.
The three scope decisions in §2 are the product owner's, taken the same day.
§3 is a spike that was run before any of this was written, and it is the
reason the plan below is a plan rather than a proposal.

## 1. What busybox is doing here, and why it goes

busybox is the userland Ferrix is measured with. One static binary lands in
the initramfs at `/bin/busybox`, every applet name is a symlink beside it, and
three gates drive it: `test-shell` runs `sh -c` with a script, `test-vfs` runs
stage 8's exit programs and a sweep of applets, and `test-net` configures
`eth0` and fetches a file. It is also ferrousli's main consumer: the busybox
built against ferrousli is the primary one, and `undefined-symbols.txt` from
its failed link has been ferrousli's to-do list since it was written.

Two reasons it goes. It is C, in a tree whose case is that an operating system
is better written in Rust, and the exception has never been argued for — only
tolerated because nothing else could boot. And it drags the kernel's UAPI
headers in with it: `ferrousli/tools/busybox/` needs the host's `<linux/*>` on
Linux and Alpine's pinned copy on Windows, and ferrousli carries three
pass-through headers for it. Both go when busybox goes.

**This document does not claim uutils is a busybox replacement.** It is not.
uutils/coreutils is a *GNU coreutils* replacement: 105 utilities, no shell, no
networking, no `grep`, `sed`, `awk` or `tar`. What replaces busybox is a set:
the uutils family for the utilities, zinc for the shell, and — for a while —
nothing at all for a list this document names in §5 and does not hide.

## 2. The product owner's decisions, 2026-09-17

| # | Question | Decision |
|---|---|---|
| D1 | uutils has no shell, and the gates all run `sh` | **zinc becomes the shell.** `/bin/sh` is zinc; `test-shell`'s script and every `sh -c` in the gates run it |
| D2 | What happens to the applets uutils/coreutils lacks | **Bring in the uutils family**: findutils, diffutils, procps and util-linux beside coreutils |
| D3 | uutils is Rust, so it links Rust's `std`, not ferrousli | **ferrousli only.** One build, `x86_64-unknown-linux-gnu` with `+crt-static`, `libferrousli.a` in glibc's place. No musl build |

D3 is the consequential one. It costs the AArch64 and ARMv7-A userland that a
musl build would have given for nothing, because ferrousli is built for x86-64
alone; and it puts every one of these programs on ferrousli's correctness,
with no second implementation to compare against when one misbehaves. It buys
the thing that made busybox worth having: a large, real, hostile consumer of
the C library, which is what has driven ferrousli's POSIX work all along.
**On AArch64 and ARMv7-A the image carries zinc and nothing else** until
ferrousli is built for those architectures. That is a new backlog row, not a
thing this plan solves.

## 3. The spike: Rust `std` links against ferrousli, and so does uutils

Run on nazuna, 2026-09-17, before the plan was written, because everything
here rests on an unproven claim: that Rust's `std` for a glibc target can be
linked against ferrousli the way busybox's C is.

**Part one — a `std` hello-world.** Built for `x86_64-unknown-linux-gnu` with
`-C target-feature=+crt-static -C link-self-contained=no` and a linker wrapper
giving ferrousli's `crt1.o` and `libferrousli.a` and nothing from the host's C
library — the shape `ferrousli/tools/busybox/build.sh` already uses. Three
things stopped it:

1. `rust_eh_personality` is defined by ferrousli *and* by `std`. ferrousli is
   itself a Rust staticlib, so it exports its own. The spike made ferrousli's
   copy local with `objcopy --localize-symbol`; the real fix is ferrousli's.
2. `dlsym` is missing. `std`'s weak-symbol machinery calls it. A static
   program has nothing to look up and musl's static `dlsym` returns null.
3. `_dl_find_object` is missing. The host's `libgcc_eh` calls it first and
   falls back to `dl_iterate_phdr`, which ferrousli has, when told "not found".

With those three worked around, the program **linked and ran**: arguments,
environment, current directory, a file written and read back, a spawned
`std::thread` that returned a value, and an exit status of 7 the shell saw.

**Part two — uutils/coreutils 0.9.0 itself**, `--no-default-features
--features feat_os_unix_musl` (the feature set uutils already maintains for
targets that cannot produce the `cdylib` that `stdbuf` needs). The whole tree
compiled — ~340 crates, `nix`, `crossterm`, `jiff`, `onig_sys`'s C — and the
link named **17 undefined symbols and nothing else**:

```
cfsetispeed@GLIBC_2.2.5   posix_spawnattr_init          posix_spawn_file_actions_init
cfsetospeed@GLIBC_2.2.5   posix_spawnattr_setflags      posix_spawnp
gnu_get_libc_version      posix_spawnattr_setpgroup     pthread_atfork
lutimes                   posix_spawnattr_setsigdefault __res_init
__memcpy_chk              posix_spawn_file_actions_adddup2  splice
posix_spawnattr_destroy   posix_spawn_file_actions_destroy
```

Four notes on that list. `posix_spawn` is nine of the seventeen, and
`include/spawn.h` already declares all of it — musl's header is there and the
implementation is not; `src/spawn.rs`'s own comment says so. The two `termios`
functions exist in `src/termios.rs` but the `libc` crate asks for them by
their versioned glibc names, so ferrousli owes a `.symver` alias, not a
function. `__memcpy_chk` comes from `onig_sys`'s fortified C and brings the
rest of the `_chk` family with it. `__res_init` is a no-op in musl.

**This is the result that makes the rest of the document a plan.** The risk
that mattered — that Rust's `std` and ferrousli were simply incompatible —
is retired.

**Part three — S1, and the link with nothing stubbed.** The seventeen went
into ferrousli, and the spike ran again against that library with no shim
archive and no localized symbol. uutils/coreutils 0.9.0 **links with zero
undefined symbols** and the 14 MiB static binary runs on the host. That is
S1's exit, and it is met.

## 3a. What the spike found that this plan did not expect

`coreutils echo hi` prints uutils' usage and says `<unknown binary name>`. It
is not a ferrousli fault, and the control says so: the same probe built as a
**static glibc** binary, no ferrousli anywhere, behaves identically, while the
ordinary dynamic glibc build works.

The cause is `src/common/validation.rs` in uutils. On Linux, when the target
environment is not musl, it does not trust `argv[0]`; it asks
`rustix::param::linux_execfn()` for the kernel's `AT_EXECFN` and prefers that,
to stop `env -a` bypassing AppArmor and SELinux on hard-linked binaries. In
rustix 1.1.5's `linux_raw` backend that value is read through `prctl`'s
`PR_GET_AUXV` or `/proc/self/auxv`, never through the C library, and in a
static binary it comes back empty. uutils then has no name, and the multicall
dispatch — which is the whole of how `/bin/ls` becomes `ls` — never happens.

ferrousli's `getauxval(AT_EXECFN)` answers correctly; a C program against this
library prints the path. rustix simply does not ask it.

**So D3 needs a decision, and it is the product owner's.** Three ways out,
and the first is the one to try:

1. **Build for the `x86_64-unknown-linux-musl` triple, still linking
   ferrousli.** uutils' `#[cfg(not(target_env = "musl"))]` then falls away and
   it uses `argv[0]`, which works. It also suits ferrousli, whose headers *are*
   musl's. Two things stop it today, both the same shape as the three in §3 and
   both ferrousli's: `rust_begin_unwind` is defined by ferrousli's
   `#[panic_handler]` and by `std`, and the musl target asks for `-lunwind`
   where the gnu target asked for `libgcc_eh`. Perhaps 3 points.
2. **Carry a patch to uutils' `validation.rs`.** Smallest change, and a patch
   to maintain for ever against a moving upstream.
3. **Give one binary per utility instead of a multicall one.** No dispatch to
   get right, and roughly a hundred copies of a 14 MiB binary, which the
   initramfs cannot hold.

Until one is chosen, S3 — the initramfs carrying `/bin/coreutils` with a
symlink per utility — cannot work, because the symlinks are exactly what the
dispatch reads. **S1 does not depend on this**, and is done.

## 4. Where each part of the userland comes from

| Source | Pinned at | Gives |
|---|---|---|
| zinc | in tree | `sh`, and the init the kernel starts |
| uutils/coreutils | 0.9.0 | the 105: `cat ls cp mv rm mkdir chmod chown stat head tail wc od dd ln readlink truncate touch env id printf echo test sleep seq sort uniq cut tr df du mknod mkfifo tty stty uname hostname nproc kill nohup timeout base64 md5sum sha256sum shuf split comm paste expr factor yes tee sync chroot nice pwd true false …` |
| uutils/findutils | 0.9.1 | `find`, `xargs`, `locate`, `updatedb` |
| uutils/diffutils | v0.5.0 | `diff`, `cmp` |
| uutils/procps | 0.0.1 | `ps`, `top`, `free`, `pwdx`, `w`, `watch`, `sysctl`, `pgrep`, `pkill`, `pidof`, `pmap`, `vmstat`, `slabtop`, `tload` |
| uutils/util-linux | 0.0.1 | `dmesg`, `last`, `mountpoint`, `rev`, `setsid`, `hexdump`, `lscpu`, `blockdev`, `renice`, `cal`, `mesg`, `nologin`, `uuidgen` |
| ports, against ferrousli | as pinned | `curl`, `btop`, and **`git`**, the new one the customer named |

`procps` covers more of `test-vfs` than was expected: `sysctl`, `top`, `pwdx`
and `w` are all there, and they are four of the applets the `/proc` checks
were written around. Both `procps` and `util-linux` are at `0.0.1` and will
disappoint somewhere; where they do, the row says so rather than the gate
being deleted.

## 5. What nobody provides, and what that costs

Named here so that no gate is quietly dropped:

| Missing | Used by | What happens |
|---|---|---|
| `ifconfig`, `route`, `netstat`, `arp`, `ip`, `udhcpc`, `ping`, `nslookup`, `nc`, `wget` | **all of `test-net`** | `test-net` keeps busybox until Ferrix has its own. `curl` already replaces `wget` for the fetch |
| `grep`, `sed`, `awk` | scripts in `test-vfs`, and every real script | zinc's own pattern matching covers some; the rest is a row |
| `tar`, `gzip`, `cpio` | nothing gated today | a row, low |
| `mount`, `umount`, `fdisk`, `swapon`, `losetup` | `test-vfs` mount checks | util-linux has none of them yet; a row |
| `su`, `adduser`, `passwd`, `login`, `getty` | the multi-user slices (`dac`…`dac5`) | a row, and those slices keep busybox until it is filled |
| `vi`, `less` | nothing gated | not replaced |

**So busybox does not leave in one step.** It leaves `test-shell` and the
`test-vfs` rows the uutils family covers, and stays in the image — as one
binary with a shrinking symlink set — for `test-net` and the rows above. The
last slice in §6 is the one that deletes it, and it is not scheduled here.

## 6. The slices

Points, not time, per the customer's rule. Each lands on `main` with the gate
its table row in `docs/BACKLOG.md` names.

| # | Slice | Points | Gate |
|---|---|---|---|
| S1 | **Done.** ferrousli gained the 17: `posix_spawn` and its eight, `pthread_atfork`, `splice`, `lutimes`, `__res_init`, `gnu_get_libc_version`, three `_chk` functions, the two versioned `termios` names, `dlsym` and the rest of `dlfcn.h`, `_dl_find_object`, and a weak `rust_eh_personality`. `execvpe` and `errno::get` came with `posix_spawn`, and the `.init_array` constructors now get `argc`, `argv` and `envp` as glibc and musl pass them | 8 | `check --ferrousli` passes; uutils links with 0 undefined symbols |
| S2 | `ferrousli/tools/uutils/` (`sources.sh`, `build.sh`, `build-windows.sh`) and `cargo xtask uutils`, mirroring `tools/busybox/` and `xtask/src/busybox.rs`, with the staleness rule | 5 | the binary builds on Linux and on Windows |
| S3 | The initramfs carries `/bin/coreutils` with a symlink per utility, and `/bin/sh` is zinc | 3 | `test-boot` on x86-64 |
| S4 | `test-shell` runs zinc, not `busybox sh`; the script's expected transcript re-recorded against zinc | 5 | `test-shell`, and zinc's own gate |
| S5 | `test-vfs`'s expectations rewritten from busybox's messages to GNU's, row by row, each row saying which program now answers it | 8 | `test-vfs` on x86-64 |
| S6 | findutils, diffutils, procps and util-linux built and installed the same way | 5 | `test-vfs`'s rows that need them |
| S7 | `git` as a ferrousli port, beside `curl` and `btop` | 8 | it clones and commits on Ferrix |
| S8 | busybox deleted: `tools/busybox/`, `xtask/src/busybox.rs`, the UAPI headers, ferrousli's three `<linux/*>` pass-throughs, the applet list | 3 | the whole matrix, once §5 is empty |

S1 is the only one nothing else could start without, and it has landed.
S2 and S4 can go in parallel now; **S3 is blocked on §3a**, the
multicall dispatch; S8 is blocked on §5 and is not scheduled.

**Total, S1–S7: 42 points.** S8 is 3 more, whenever §5 empties.

## 7. Risks

* **`procps` and `util-linux` are `0.0.1`.** `test-vfs` checks what a program
  printed, not only that it exited. Early versions will differ from busybox in
  ways that are not bugs. Each such row is re-recorded, not deleted, and the
  commit says which program's output it now holds.
* **One C library, no control.** D3 leaves every utility resting on ferrousli
  with nothing to compare against. When a utility misbehaves on Ferrix the
  question "ferrousli or the kernel?" loses the answer busybox's musl and
  glibc builds used to give. The mitigation is that uutils runs on the Linux
  host too, against the host's glibc, which is the comparison that remains.
* **`std` is a much larger consumer than busybox was.** 17 symbols is what
  *linking* needs. What ferrousli must get *right* for `std` — threads, TLS,
  `posix_spawn`, signals — is a larger surface than busybox ever touched, and
  the failures will be at run time, not at link time.
* **zinc is on the critical path early.** D1 puts a shell that has no job
  control, no line editor and no completion in front of every gate. The `sh
  -i` and controlling-terminal checks stage 7 owns have no home until zinc has
  job control; they keep busybox's `sh` until it does.
