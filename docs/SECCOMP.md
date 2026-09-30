# seccomp: a filter that judges every system call

Stage 13. A process may promise to make only a few system calls
(`SECCOMP_MODE_STRICT`), or install a classic-BPF filter that judges each one
(`SECCOMP_MODE_FILTER`). It is the last of the three things the stage's exit
criterion asks of a container: a user namespace, a memory limit, and a filter
that blocks a syscall.

## 1. What it does

| | |
|---|---|
| `seccomp(SECCOMP_SET_MODE_FILTER, flags, &sock_fprog)` and `prctl(PR_SET_SECCOMP, 2, &sock_fprog)` | installs a filter; it judges every Linux call the process makes from the next one on |
| `seccomp(SECCOMP_SET_MODE_STRICT)` and `prctl(PR_SET_SECCOMP, 1)` | only `read`, `write`, `exit`, `exit_group`, `sigreturn`, `rt_sigreturn`; anything else ends the process with `SIGKILL` |
| `seccomp(SECCOMP_GET_ACTION_AVAIL, 0, &action)` | `0` for `KILL_PROCESS`, `KILL_THREAD`, `TRAP`, `ERRNO`, `LOG`, `ALLOW`; `EOPNOTSUPP` for `TRACE` and `USER_NOTIF`, which Ferrix cannot take |
| `prctl(PR_GET_SECCOMP)` | the mode: 0, 1 or 2 |
| inheritance | a fork child takes its parent's filters, sharing them; `execve` keeps them; `PR_SET_NO_NEW_PRIVS` is inherited and kept too (it was not before) |

**Who may install one.** A process that set `no_new_privs`, or holds
`CAP_SYS_ADMIN` in its own user namespace; otherwise `EACCES`. Without it a
filter could make a set-id program misbehave as a privileged process.

**What it is judged by.** The number the program called, the architecture's
`AUDIT_ARCH_*` (`X86_64`, `I386` for a 32-bit program on x86-64, `AARCH64`,
`ARM`), the instruction after the call and the six raw argument registers, in
Linux's `struct seccomp_data`. Numbers and constants are checked against the
local UAPI headers (`/usr/include/linux/seccomp.h`, QEMU's `linux-headers`):
`seccomp` is 317 (x86-64), 354 (i386), 277 (AArch64), 383 (ARM).

**The answer.** Every filter runs, newest first, and the most restrictive
result wins (`KILL_PROCESS`, `KILL_THREAD`, `TRAP`, `ERRNO`, `USER_NOTIF`,
`TRACE`, `LOG`, `ALLOW`; Linux compares the action halves as signed numbers).
On a tie the newer filter's data stands.

| Action | What Ferrix does |
|---|---|
| `ALLOW`, `LOG` | the call goes through (`LOG` is not logged) |
| `ERRNO \| n` | the call fails with `min(n, 4095)`; `n == 0` succeeds without running it |
| `TRAP` | `SIGSYS`, forced (unblocked, handler reset if ignored), `si_code` `SYS_SECCOMP`, `si_errno` the data, `si_call_addr`, `si_syscall`, `si_arch` |
| `KILL_THREAD`, `KILL_PROCESS`, any action nobody defined | the process ends with `SIGSYS` |
| `TRACE`, `USER_NOTIF` | `ENOSYS`, as Linux answers with no tracer and no supervisor |

**What a filter may be** (`ferrix_seccomp::validate`, Linux's
`bpf_check_classic` and `seccomp_check_filter`): 1 to 4096 instructions, at
most 2^18 across a process's filters; loads of aligned 32-bit words of the
64-byte data, of its length, constants and sixteen scratch words; arithmetic
(division and modulus by a constant zero and shifts of 32 or more are refused;
by a zero index they end the filter killing); forward jumps inside the program;
a final return. No byte or half-word loads, no indexed loads, no packet loads.

## 2. Where it lives

* `src/lib/kernel/seccomp` (`ferrix-seccomp`): the instruction, `validate`,
  `run`, `run_all`, `data`, the action constants and `more_restrictive`. Pure,
  `no_std`, eight host tests; the kernel calls it and nothing else.
* `src/kernel/src/syscall/seccomp.rs`: `State` on the `Process` (mode and
  filters, a lock of its own, the mode also in an atomic so a process with no
  filter pays one load per call), `enforce` (called first in
  `linux::dispatch`), `sys_seccomp`, `prctl_set`, `charged`.
* A filter is charged to the job that installed it (F-37), by its
  instructions and its record; `kmem_check` fills a job with filters until
  `ENOMEM`.
* `fs/seccomp_check.rs`: the `seccomp` boot line (FX-0889).

## 3. How it differs from Linux

* **One filter stack per process, not per thread.** That is
  `SECCOMP_FILTER_FLAG_TSYNC` always: a filter installed by one thread judges
  every thread. A program cannot ask for one thread alone. The failure `TSYNC`
  exists to prevent, a thread that escapes the filter, cannot happen.
* `SECCOMP_FILTER_FLAG_NEW_LISTENER` (user notification), `TRACE` and
  `GET_NOTIF_SIZES` are not built (`EINVAL` / `ENOSYS` as above).
  `SECCOMP_FILTER_FLAG_LOG` and `SPEC_ALLOW` are accepted and do nothing.
* A call is judged before its arguments are rewritten, so a 32-bit program
  sees its own registers; `socketcall` is judged as `socketcall`, as on Linux.
* `KILL_THREAD` ends the whole process: a process here has the one thread it
  is judged on.

## 4. Checks

The `seccomp` line (`fs/seccomp_check.rs`, FX-0889): a process with neither
`no_new_privs` nor a capability is refused a filter (`EACCES`); an empty
filter, a byte load, a jump off the end, a filter with no final return, the
`NEW_LISTENER` flag and an unknown flag are `EINVAL`; a filter fails the call
it names with its errno and lets the others through; of two filters the more
restrictive wins and on a tie the newer's data; a fork child is judged as its
parent and a filter it adds does not reach the parent; `KILL_PROCESS` ends the
process; strict mode allows `read` and `write` and kills at anything else; and
the actions `GET_ACTION_AVAIL` knows. Negative controls, each stopping the boot
with its own message: `enforce` skipped ("a filter did not fail the call it
names with its errno"), the fork not copying the state ("a fork child was not
judged as its parent was"), and the charge dropped (`kmem`: "a job made more
than its limit could hold").
