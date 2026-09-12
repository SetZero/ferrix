# What stage 8's exit asks of the kernel

**Scaffolding. Delete this file when stage 8 ends.** It exists so that the
people building the descriptor table, the path calls and procfs build what the
exit criterion's programs actually call, measured, rather than what a reading
of the criterion suggests. When `cargo xtask test-vfs` passes on all three
architectures this has done its job, and what is worth keeping moves into
stage 8's section of `docs/ROADMAP.md`.

> **Exit:** `busybox ls -R /proc`, `cat /proc/self/maps` and a shell script
> that manipulates files under tmpfs, all under the boot test.

Everything below was measured on 2026-09-13 against Alpine's `busybox-static`
1.37.0, the same binaries stage 7's exit was met with, on the host with
`strace` (x86-64) and under `qemu-aarch64 -strace` and `qemu-arm -strace`
(AArch64, ARMv7-A). How to repeat it is at the end.

## The answer first

* **Nothing forks, but only because the tmpfs part is split.** This busybox's
  `ash` runs *every* applet as `fork` + `execve` + `wait4`: it is built without
  `FEATURE_SH_NOFORK` and without `FEATURE_SH_STANDALONE`. `unset PATH; mkdir x`
  gives `sh: mkdir: not found`, and with `PATH` set the trace shows one `fork`
  and one `execve` per `mkdir`, `cat`, `mv`, `ln`, `rm` and `rmdir`. Its
  builtins are `. : [ [[ alias bg break cd chdir command continue echo eval exec
  exit export false fg getopts hash help history jobs kill let local printf pwd
  read readonly return set shift source test times trap true type ulimit umask
  unalias unset wait` — nothing that makes a directory, renames or removes.

  So **a single shell script cannot manipulate files under tmpfs on Ferrix
  until `clone`, `execve` and `wait4` exist**, if "manipulate" includes making
  directories, renaming, linking or removing. A script *can* create, append,
  truncate, read back and test files with redirections and builtins, and it
  can `cd`. `test-vfs` therefore reads the criterion as a sequence of eleven
  programs the kernel starts one after another — three of them `sh -c` scripts
  of builtins, the rest the applets a script would have forked — sharing one
  tmpfs. Each was traced as exactly one process.

* **No `/dev` file is opened by any of the commands.** Descriptors 0, 1 and 2
  are used as they arrive. Devfs is not on this path.

* **The `/proc` files opened:** `/proc` itself as a directory, every
  directory under it that is reachable without following a symbolic link, and
  `/proc/self/maps` through the `self` link. `ls -R` does `lstat` on *every*
  entry it lists, so every name procfs reports must `lstat` successfully.

## The commands `test-vfs` runs, in order

| # | argv | status | what is checked |
|---|------|--------|-----------------|
| 0 | `ls -R /proc` | 0 | a `/proc:` section listing `self`, and a section for the process's own directory (`/proc/self:` or `/proc/<pid>:`) listing `maps` |
| 1 | `cat /proc/self/maps` | 0 | at least one line, and every line parses as a maps line |
| 2 | `mkdir -p /tmp/vfs/deep` | 0 | |
| 3 | `sh -c` *write script* | 5 | its six lines, in order |
| 4 | `mv /tmp/vfs/file /tmp/vfs/deep/moved` | 0 | |
| 5 | `ln -s deep/moved /tmp/vfs/link` | 0 | |
| 6 | `cat /tmp/vfs/link` | 0 | `tmpfs: one`, `tmpfs: two` |
| 7 | `sh -c` *check script* | 6 | its five lines, in order |
| 8 | `rm /tmp/vfs/link /tmp/vfs/deep/moved /tmp/vfs/empty` | 0 | |
| 9 | `rmdir /tmp/vfs/deep /tmp/vfs` | 0 | |
| 10 | `sh -c` *gone script* | 7 | `tmpfs: removed` |

The scripts are in `xtask/src/vfs.rs`. The statuses are not zero for the same
reason stage 7's is 7: a shell that died and reported success cannot pass.

**Output is checked, not just statuses, because the statuses lie.** Injecting
`ENOSYS` into `getdents64` makes `ls -R` print its headings and no entries and
still exit 0. Injecting it into `poll` makes `while read` read nothing and the
script carry on to its chosen status. A test that only compared statuses would
pass on both.

**A negative test is only as good as a positive one beside it.** A refused
`stat` makes `[ -e name ]` false exactly as a missing file does. The first
version of command 10, `[ -e /tmp/vfs ] || echo "tmpfs: removed"`, passed on
a kernel that answered no file call at all. It now checks `[ -d /tmp ]` first.

## System calls, per command and architecture

Every command also makes the startup calls stage 7 already answers:
`arch_prctl`/`set_tls`, `set_tid_address`, `brk`, `mmap`/`mmap2`, `munmap`,
`rt_sigprocmask`, `getuid`, `getgid`, `setgid`, `setuid` (the `32` forms on
ARMv7-A), and `exit_group`. AArch64 and ARMv7-A `ls` also make one
`clock_gettime`/`clock_gettime64`. Only the calls below are new.

**Load-bearing** means that injecting `ENOSYS` into that call alone, on x86-64,
changed the output or the status. The other calls in each row were injected
too, and the command's output and status did not change.

### `ls -R /proc`

| x86-64 | AArch64 | ARMv7-A | load-bearing |
|--------|---------|---------|--------------|
| `stat` on the argument | `newfstatat(AT_FDCWD, p, _, 0)` | `statx(AT_FDCWD, p, AT_NO_AUTOMOUNT\|AT_STATX_SYNC_AS_STAT, STATX_BASIC_STATS)` | yes |
| `open(dir, O_RDONLY\|O_DIRECTORY\|O_CLOEXEC\|O_LARGEFILE)` | `openat` (same flags) | `open` (same flags) | yes |
| `getdents64(fd, _, 2048)` | same | same | yes; exit stays 0 without it |
| `lstat` on every entry | `newfstatat(…, AT_SYMLINK_NOFOLLOW)` | `statx(…, AT_SYMLINK_NOFOLLOW\|…)` | yes |
| `writev(1, …)` | same | same | yes |
| `close` | same | same | no |
| `fcntl(fd, F_SETFD, FD_CLOEXEC)` | `fcntl` | `fcntl64` | no (musl's fallback for `O_CLOEXEC`) |
| `ioctl(0 and 1, TIOCGWINSZ)` | same | same | no; `ENOTTY` gives one name per line, which the test expects either way |

`ls -R` does not descend into `/proc/self`, because `lstat` says it is a
symbolic link. The listing reaches the process's own files only if procfs also
presents `/proc/<pid>` as a directory, as Linux does, or makes `self` a
directory.

### `cat /proc/self/maps`

| x86-64 | AArch64 | ARMv7-A | load-bearing |
|--------|---------|---------|--------------|
| `open("/proc/self/maps", O_RDONLY\|O_LARGEFILE)` | `openat` | `open` | yes; the walk follows `self` |
| `sendfile(1, 3, NULL, 16 MiB)` | `sendfile` | `sendfile64` | no: on `EINVAL` or `ENOSYS` it falls back to `read` and `write` |
| `read(3, _, 65536)` after the fallback | same | same | yes, if `sendfile` fails |
| `write(1, …)` after the fallback | same | same | yes, if `sendfile` fails |
| `close` | same | same | no |

If procfs answers `sendfile` with `EINVAL`, as Linux's procfs does, `cat` reads
the file in 64 KiB chunks through a buffer it `mmap`s.

### The applets

| command | x86-64 | AArch64 | ARMv7-A | load-bearing |
|---------|--------|---------|---------|--------------|
| `mkdir -p` | `stat` each prefix, `mkdir` each (`EEXIST` expected), `umask` twice | `newfstatat`, `mkdirat` | `statx`, `mkdir` | `mkdir`, `stat` |
| `mv` | `stat(dest)` wanting `ENOENT`, `rename` | `newfstatat`, `renameat` (not `renameat2`) | `statx`, `rename` | both; mv refuses to go on if the stat fails with anything but `ENOENT` |
| `ln -s` | `stat(dest)`, `symlink` | `newfstatat`, `symlinkat` | `statx`, `symlink` | `symlink` |
| `cat` a link | `open` following the link, `sendfile` | `openat`, `sendfile` | `open`, `sendfile64` | `open` |
| `rm` | `lstat`, `access(W_OK)`, `unlink` per name | `newfstatat(NOFOLLOW)`, `faccessat`, `unlinkat(0)` | `statx`, `access`, `unlink` | `lstat`, `unlink` |
| `rmdir` | `rmdir` | `unlinkat(AT_REMOVEDIR)` | `rmdir` | `rmdir` |

### The shell scripts

One process each. The calls, beyond startup:

| x86-64 | AArch64 | ARMv7-A | load-bearing |
|--------|---------|---------|--------------|
| `getpid`, `getppid`, `uname`, `getcwd`, `rt_sigaction` at startup | same | same | no |
| `chdir` | same | same | yes |
| `open(name, O_WRONLY\|O_CREAT\|O_TRUNC, 0666)` for `>` and `: >` | `openat` | `open` | yes |
| `open(name, O_WRONLY\|O_CREAT\|O_APPEND, 0666)` for `>>` | `openat` | `open` | yes |
| `open(name, O_RDONLY)` for `<` | `openat` | `open` | yes |
| `fcntl(1 or 0, F_DUPFD_CLOEXEC, 10)` to save the descriptor a redirection replaces | `fcntl` | `fcntl64` | yes: `sh: fcntl(1,F_DUPFD,10): Function not implemented` |
| `fcntl(10, F_SETFD, FD_CLOEXEC)` | `fcntl` | `fcntl64` | no |
| `dup2(3, 1)` to redirect, `dup2(10, 1)` to restore | `dup3(…, 0)` | `dup2` | yes, and worse than failing: with `dup2` refused the shell never exits (killed by `timeout` after 10 s) |
| `close` | same | same | no |
| `poll([{0, POLLIN}], 1, -1)` before each byte `read` reads | `ppoll` | `poll` | yes: `while read` ends at once and the script goes on |
| `read(0, _, 1)`, one byte at a time | same | same | yes |
| `stat` for `test -f`, `-d`, `-s`, `-e` | `newfstatat` | `statx` | yes |
| `lstat` for `test -L` | `newfstatat(NOFOLLOW)` | `statx(NOFOLLOW)` | yes |
| `write(1, …)` | same | same | yes |

`poll` on a regular tmpfs file must say `POLLIN`: Linux reports regular files
as always ready.

## Not measured

* Whether each call does what Linux does. What is known from Ferrix itself is
  below, under "What Ferrix answers today".
* What the calls do against Ferrix's own procfs layout: the host's `/proc` has
  thousands of entries, so `ls -R` was traced over a small fixture with the
  same shape (`self` a link to `1`, `1/maps` a file). The calls per entry do not
  depend on how many entries there are.

## What Ferrix answers today

From `cargo xtask test-vfs --arch all` on this branch, before the descriptor
table, path calls and procfs land. The kernel prints each call it answered
`ENOSYS` while a command ran, which is how these were read:

* **ARMv7-A's musl falls back from `statx` (397) to `stat64` (195) and
  `lstat64` (196)** when `statx` is refused, not to `fstatat64`. Answering
  either pair is enough for `ls`, `mv`, `ln`, `rm` and `test`.
* **`mkdir -p` calls `umask` (x86-64 95, AArch64 166, ARMv7-A 60) before and
  after**, on every architecture. It does not care about the answer.
* The shell's `getcwd` at startup is not load-bearing; its `chdir` is.
* `ioctl(TIOCGWINSZ)` on descriptors 0 and 1 is the first call `ls` makes that
  Ferrix refuses, and `ls` carries on.

## Repeating it

```sh
BB=~/.local/share/ferrix/busybox/x86_64/bin/busybox.static

# Which applets ash runs in-process: none. Builtins only.
env -i "$BB" sh -c help
env -i "$BB" sh -c 'unset PATH; mkdir x'           # sh: mkdir: not found

# Every call, and whether anything forked (one pid in the trace = no fork).
strace -f -o ls.trace "$BB" ls -R /proc
strace -f -o maps.trace "$BB" cat /proc/self/maps
awk '{print $1}' ls.trace | sort -u | wc -l

# A call is load-bearing if refusing it changes the output or the status.
strace -o /dev/null -e inject=getdents64:error=ENOSYS "$BB" ls -R /proc; echo $?
strace -o /dev/null -e inject=sendfile:error=ENOSYS "$BB" cat /proc/self/maps

# The other two architectures' names for the same calls.
qemu-aarch64 -strace ~/.local/share/ferrix/busybox/aarch64/bin/busybox.static cat /proc/self/maps
qemu-arm -strace ~/.local/share/ferrix/busybox/armv7a/bin/busybox.static cat /proc/self/maps
```

For the scripts, run `"$BB" sh -c "…"` under `strace -f` with each script from
`xtask/src/vfs.rs`, with `/tmp` replaced by a scratch directory, and count the
pids in the trace. Wrap every fault-injection run in `timeout 10`, because
refusing `dup2` hangs the shell.
