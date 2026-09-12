# What a shell actually needs — stage 7's exit criterion, measured

Stage 7's exit is "a static musl `busybox sh` starts, runs a script, and
exits". `docs/ROADMAP.md` describes the work as the syscall entry path plus
"the core of the surface: memory, files, process (`clone`, `execve`, `wait4`,
`exit_group`), threads and `futex`, signals with `sigaltstack` and
`rt_sigreturn`, time, and identity".

That list is what stage 7 will eventually owe. It is **not** what the exit
criterion requires, and the difference is large enough to change the order the
work should be done in. This file records what a real shell actually asks for,
measured rather than recalled, so that nobody builds `futex` before `write`.

**Delete this file when stage 7 meets its exit criterion.** `docs/ROADMAP.md`
is the durable record; this is scaffolding.

---

## 1. How this was measured

Three traces, all reproducible on the development machine:

```
# A static glibc busybox, natively.
strace -f -o bb.trace /usr/bin/busybox sh -c 'echo hello'
strace -f -o bb-script.trace /usr/bin/busybox sh ./demo.sh

# A static ARM EABI musl binary, which is closest to what Ferrix targets.
cargo build --release --target armv7-unknown-linux-musleabi   # rust-lld links it
qemu-arm -strace ./musltest one two
```

And, for the question that matters most — *which* of those calls a program
cannot survive losing — `strace`'s fault injection, which answers it by
experiment instead of by reading libc's source and hoping:

```
strace -e inject=readlinkat:error=ENOENT /usr/bin/busybox sh -c 'echo hello'
```

---

## 2. The headline

`busybox sh -c 'echo hello'` makes **33 system calls, 22 of them distinct**.
The script-from-a-file case makes 39, 26 distinct. The static musl binary makes
**26 calls, 12 distinct**.

Two things about that set matter more than its size.

**It never forks.** A script built from shell builtins — `echo`, arithmetic,
`while`, `[` — runs entirely inside the shell process. There is no `clone`, no
`execve` of a child, no `wait4`, no `futex`, and no signal is ever *delivered*.
`busybox sh` calls `rt_sigaction` six times, but only to record dispositions;
nothing raises a signal in a clean run, so no frame is ever pushed onto a user
stack and `rt_sigreturn` is never called.

**Most of the rest is optional.** Every syscall in this table can return an
error and the shell still prints `hello` and exits 0. This was tested, one
injection per row:

| Syscall | Injected | Result |
|---|---|---|
| `readlinkat` (`/proc/self/exe`) | `ENOENT` | prints `hello`, rc 0 |
| `newfstatat` | `ENOENT` | prints `hello`, rc 0 |
| `prctl` | `EINVAL` | prints `hello`, rc 0 |
| `getrandom` | `ENOSYS` | prints `hello`, rc 0 |
| `uname` | `EFAULT` | prints `hello`, rc 0 |
| `prlimit64` | `EPERM` | prints `hello`, rc 0 |
| `rt_sigaction` | `EINVAL` | prints `hello`, rc 0 |
| `set_robust_list` | `ENOSYS` | prints `hello`, rc 0 |
| `rseq` | `ENOSYS` | prints `hello`, rc 0 |
| `set_tid_address` | `ENOSYS` | prints `hello`, rc 0 |
| `fcntl` | `EINVAL` | prints `hello`, rc 0 |
| `brk` | `ENOMEM` | prints `hello`, rc 0 — libc falls back to `mmap` |
| `getuid`/`getgid`/`getpid`/`getppid` | `EPERM` | prints `hello`, rc 0 |
| `setuid`/`setgid` | `EPERM` | prints `hello`, rc 0 |

`readlinkat` on `/proc/self/exe` is the important one: **busybox does not need
procfs to start**, so stage 8 is not secretly a prerequisite for stage 7.

### What is load-bearing

Only these. Each was injected and each killed the program:

| Syscall | Injected | Result |
|---|---|---|
| TLS setup — `arch_prctl(ARCH_SET_FS)` on x86-64 | `EINVAL` | `Fatal glibc error: Cannot allocate TLS block`, rc 127 |
| `mprotect` | `EACCES` | `cannot apply additional memory protection after relocation`, rc 127 |
| `write` | `EBADF` | no output, rc 1 |

Plus, trivially, the ones with no alternative: a way to *start* the program at
all, and `exit_group` to end it.

On ARMv7-A the TLS call is `__ARM_NR_set_tls` (`0x0f0005`), not `arch_prctl`.
It is easy to miss: musl builds the number as `0x0f0000 + 5` at run time rather
than as one literal, so grepping a disassembly for `0xf0005` finds nothing —
the constant is there, and a byte search for `0x000f0000` finds it. AArch64
needs no call at all; it writes `TPIDR_EL0` itself.

### musl is not glibc, and Ferrix targets musl

The two libcs disagree about the startup path in ways that change what to build
first:

* **musl allocates with `mmap2`; glibc allocates with `brk`.** The glibc trace
  shows five `brk` calls and no `mmap` at all. The musl trace shows five
  `mmap2` and two `brk`. Building `brk` first because the glibc trace leans on
  it would be building the wrong one.
* glibc calls `rseq`, `set_robust_list` and `prlimit64` at startup; musl calls
  none of them.
* musl uses `set_tid_address`'s **return value** as its process id. It survives
  `ENOSYS`, but a stub returning a plausible constant is worse than an error —
  give it the real thread id, which Ferrix already has.
* The `poll`, `sigaltstack` and `SIGSEGV` handler in the musl trace are Rust's
  standard library setting up stack-overflow detection, not musl itself. A C
  program linked against musl does none of it.

---

## 3. Against what Ferrix answers today

`kernel/src/syscall/mod.rs` answers eight calls: `getpid`, `gettid`,
`getppid`, the four credential calls, and `sched_yield`.

**None of them appears in the musl trace.** Measured honestly, Ferrix currently
answers **zero** of the twelve distinct calls a static musl binary makes. The
eight that are implemented are the ones needing no process state, which is why
they were first — they were reachable before user mode existed, not because
they were the most useful.

The twelve musl asks for, in the order it asks:

| # | Call | Status | Depends on |
|---|---|---|---|
| 1 | `mmap2` | missing | `AddressSpace` + `find_free` |
| 2 | `set_tid_address` | missing | current task |
| 3 | `set_tls` (ARMv7-A only) | missing | the transition path |
| 4 | `rt_sigaction` | missing | somewhere to record dispositions |
| 5 | `sigaltstack` | missing | same |
| 6 | `rt_sigprocmask` | missing | same |
| 7 | `mprotect` | missing | `vma::protect` (exists) |
| 8 | `brk` | missing | a heap region in the `AddressSpace` |
| 9 | `mmap2` (`MAP_FIXED`) | missing | `vma::map_fixed` (exists) |
| 10 | `write` | missing | `copy_from_user` + console |
| 11 | `munmap` | missing | `vma::remove` (exists) |
| 12 | `exit_group` | missing | task teardown |

The good news is in the right-hand column: `libs/vma` already implements
`insert`, `map_fixed`, `remove`, `protect`, `find_free` and `clone_for_fork`,
all host-tested. The interval-tree work that `mmap`, `munmap` and `mprotect`
are *about* is done. What is missing is the plumbing from a syscall argument to
a call on it, plus `AddressSpace` exposing `find_free` so `mmap(NULL, ...)` can
pick an address.

---

## 4. The order to build it in

Four milestones. Each one runs, which is the roadmap's first rule.

### A — a hand-written binary reaches ring 3 and prints

This is stage 6's own exit criterion, and it needs **two** syscalls: `write`
and `exit_group`. No `mmap`, no TLS, no signals, no files, no libc.

Needed:

* The ring-3 / EL0 / USR transition and the syscall entry vectors — *stage 6
  owner's*.
* `Task` carrying an `Option<Arc<AddressSpace>>`, the root swap in
  `choose_next`, and `arch::set_kernel_stack` — *stage 6 owner's*.
* `copy_from_user` / `copy_to_user` — *stage 7 owner's*. Must resolve through
  the `AddressSpace` and call `fault`, not dereference, and must reject
  anything `is_user_address` refuses **before** any length arithmetic: no
  SMAP/SMEP/PAN is enabled anywhere in this tree.
* The ELF loader into an `AddressSpace` — *stage 7 owner's*. `libs/elf` parses
  already; map `PT_LOAD` then copy, and leave the `p_memsz` tail alone because
  a committed anonymous page is already zeroed.
* `write(1|2, ...)` straight to the console, and `exit_group`. No file
  descriptor table yet — two hardcoded numbers is honest at this stage and is
  not something a later stage has to unpick, because the fd table replaces the
  lookup rather than the call.
* The initial process stack — **done**, `libs/ustack`.

### B — a real static musl binary runs

Adds the rest of the twelve: `mmap2`/`munmap`/`mprotect` over `libs/vma`,
`brk`, `set_tid_address`, `set_tls` on ARMv7-A, and `rt_sigaction` /
`sigaltstack` / `rt_sigprocmask` **recording dispositions only**.

That last point is the single biggest saving available. Signal *delivery* — the
frame pushed on the user stack, `rt_sigreturn` to unwind it — is a large piece
of work that the exit criterion does not need, because nothing in a clean run
raises a signal. Build the table; leave delivery until something must be
killed.

### C — `busybox sh -c 'echo hello'`

Adds only cheap calls, every one of which may legitimately fail: `uname`, the
credential calls (already answered), `prctl(PR_GET_NAME)`, `getrandom`,
`readlinkat`, `newfstatat`, `prlimit64`. Returning `ENOSYS` or `ENOENT` for
most of them is a correct implementation at this point, not a stub.

Note that `-c` takes the script **in `argv`**, so this milestone needs no
filesystem at all.

### D — `busybox sh ./script.sh`

Adds `openat`, `read`, `close`, `fcntl(F_DUPFD_CLOEXEC)`, and therefore a real
file descriptor table and something to open. That is stage 8's cpio initramfs
or tmpfs, and it is the only part of the exit criterion that genuinely crosses
into stage 8.

If the exit criterion is read as "runs a script", milestone C satisfies it with
`-c`. If it is read as "runs a script *file*", D is required. Worth settling
deliberately rather than discovering late.

---

## 5. What the roadmap's list overstates

Stated plainly, because the list is what someone will plan from:

* **`clone`, `fork`, `vfork`, `execve`, `wait4`, `waitid`** — not needed. A
  builtin-only script never creates a process. (The *first* process needs an
  address space built and entered, but that is the kernel doing it, not an
  `execve` syscall.)
* **`futex`** — not needed. Single-threaded, uncontended.
* **Signal delivery, `sigaltstack` semantics, `rt_sigreturn`** — not needed.
  Only the disposition table is.
* **`time`** — not called at all in any of the three traces.
* **The VFS** — not needed for `-c`. Needed only for milestone D.
* **procfs** — not needed. `readlinkat("/proc/self/exe")` returning `ENOENT` is
  survivable.

None of these is wasted work; all of it is owed before `rustc` runs, which is
the actual goal. The point is only that none of it is on the path to the exit
criterion, and building it first delays the first moment somebody else's binary
runs on Ferrix — which is the thing worth reaching early, because it is the
first time the ABI is tested by something that did not come from this tree.

---

## 6. Open questions

1. **Which reading of "runs a script"** — `-c`, or a file? Decides whether
   stage 7 ends at milestone C or needs a slice of stage 8.
2. **Where the first binary comes from.** Building a static musl `busybox` for
   three architectures is a toolchain problem, not a kernel one, and it should
   be solved before milestone A rather than during it. The development machine
   has `armv7-unknown-linux-musleabi` and `rust-lld`, which is enough to
   produce a static ARM EABI binary today with no cross-compiler installed —
   that is how the musl trace here was made, and it is the cheapest first
   target.
3. **Whether `write` to fd 1 goes through the console lock.** The console is
   shared with the boot log and with panics; a user program writing to it
   concurrently with another processor's panic is worth thinking about once
   rather than discovering.
