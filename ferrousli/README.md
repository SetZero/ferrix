# Ferrousli

A C library for Linux, written in Rust. The name is *ferrous* and *musl*.

The model is musl: small, correct, and built to be linked statically. The
destination is further. A program built against glibc should one day be able to
load this library in glibc's place. That means matching glibc's exported
symbols, structure layouts and startup contract, not only its API. This is
why `crt1.o` already calls `__libc_start_main` with glibc's arguments, and why
the thread control block keeps glibc's layout.

It lives in the Ferrix tree but stands alone. It is its own cargo workspace,
depends on no Ferrix crate, and reaches the kernel only through Linux system
calls. It therefore builds and tests on any x86-64 Linux host, and runs on
Ferrix the way any other static program does.

## Building and testing

From this directory:

```
cargo test
```

The unit tests call the functions from Rust. The C program tests, in
`tests/c_*.rs` with their shared harness in `tests/common`, compile the
programs in `tests/c/` with the host's `cc`, against the headers in `include/`
rather than the host's. The harness links them against `crt1.o` and
`libferrousli.a` and nothing else, runs each in a fresh directory, and checks
its output and how it ends. Each program is built at `-O0` and at `-O2`, with
the stack protector on, as a distribution's compiler leaves it.

Seven unit tests compare with the glibc the test binary links: `ctype.h`'s
tables and functions, `wctype.h` over every code point against
`src/wctype/glibc-differences.txt`, and `printf`'s output. Their data was
recorded on the glibc named in `src/host_glibc.rs`. On any other they return
early after one note on standard error, and every other test still runs.

From the repository root, `cargo xtask check --ferrousli` runs Ferrix's local
gate with ferrousli's added: the generated-ABI check, formatting, clippy, and
the tests in the debug and release profiles. CI runs the same command.

[CONVENTIONS.md](CONVENTIONS.md) has the rules every change here follows.

`cargo build` leaves `target/debug/libferrousli.a`. `crt1.o` is built by
`build.rs` into cargo's `OUT_DIR`. A program builds as:

```
cc -static -no-pie -nostdlib -nostdinc -isystem include \
   -o prog path/to/crt1.o prog.c target/debug/libferrousli.a
```

## busybox

`tools/busybox/build.sh` builds busybox 1.37.0 against this library, as a
static x86-64 program: musl's headers, `crt1.o` and `libferrousli.a`, with the
host's kernel UAPI headers and nothing from its C library. The busybox.net
tarball and Alpine's config for its `busybox-static` 1.37.0-r20 are pinned by
checksum, and `tools/busybox/config.sh` lists the changes made to that config.

It downloads and builds under `~/.local/share/ferrix/busybox/ferrousli`, and
installs `x86_64/busybox` there once the program links. Until then it exits 1
and writes `undefined-symbols.txt` beside it: the functions busybox calls that
this library does not have yet. Ferrix's `cargo xtask test-shell --init` and
`test-vfs` are to run the installed binary, beside Alpine's musl build and the
glibc one.

busybox also calls functions from the three areas allowed to wait: pattern
matching, a few math functions, and name resolution. Until each is written,
the library defines a stub that writes that the function is not implemented
yet and aborts: 31 functions in `src/stubs.rs`. That list only shrinks, and
the busybox Ferrix is tested with must reach none of them.

## Headers

`include/` holds musl 1.2.5's headers, unmodified. See
[include/README.md](include/README.md).

## Where it stands

x86-64, static programs linked at a fixed address.

| Area | There | Not yet |
|---|---|---|
| Startup | `_start`, `__libc_start_main`, `environ`, the auxiliary vector, `.preinit_array` and `.init_array` | position-independent static programs |
| Threads | a control block in glibc's layout for every thread, static TLS, the stack protector's canary; `pthread_create`, `pthread_join`, `pthread_detach`, `pthread_exit`, attributes, names, `gettid`, `pthread_sigqueue`; mutexes, including recursive, error-checking, robust and priority-inheriting ones; condition variables, read-write locks, keys and `pthread_once`; a futex lock for the library's own state | cancellation, barriers, semaphores, C11 `threads.h`, `pthread_atfork`, `set*id` across threads |
| Memory | the `malloc` family, on `mmap`, with size classes and integrity checks | returning empty regions to the kernel |
| `stdlib.h` | `exit`, `_Exit`, `atexit` without a limit, `abort`, the environment functions; `strtol` and `strtod` families, correctly rounded for `float`, `double` and x87 `long double`, with glibc's `__isoc23_` names; `qsort`, `qsort_r`, `bsearch`; `abs` and `div` families; `rand`, `random` and `rand48` families | `ecvt`, `fcvt`, `gcvt`; NaN payloads and rounding modes in parsing |
| `string.h`, `strings.h` | everything, with word-at-a-time scans and two-way `strstr` and `memmem`; `strcoll_l` and `strxfrm_l` | `strcasecmp_l`, `strncasecmp_l` |
| `ctype.h` | the C locale, glibc's `__ctype_b_loc` tables, and the `_l` forms | |
| `locale.h`, `langinfo.h` | `setlocale`, `localeconv`, `newlocale`, `duplocale`, `freelocale`, `uselocale`, `nl_langinfo`; musl's C and C.UTF-8 locales, any other name behaving as UTF-8 | message catalogues, glibc's `locale_t` layout |
| Multibyte and wide characters | UTF-8 conversion in `stdlib.h`, `wchar.h` and `uchar.h`, strict as musl's; `wctype.h`'s classes, case mappings and `wcwidth` from musl's Unicode 12.1 tables, with every difference from glibc recorded; `wchar.h`'s string and memory functions | wide stdio, `wcstol` and `wcstod`, `iconv` |
| Error text | `strerror`, `strerror_l`, `strerror_r` (XSI), `__xpg_strerror_r`, `strsignal` | glibc's GNU `strerror_r` |
| `unistd.h` | files, directories and links, `pipe` and `dup`, identities, `fork` on `clone`, `execve`, `execv`, `execvp`, `sleep`, `alarm`, `sysconf`, `isatty`, `syscall`; `chown` and its `l`, `f` and `at` forms, `utimes`, `gethostid` | `getcwd(NULL, 0)`, `set*id` across threads |
| `fcntl.h`, `sys/stat.h`, `sys/mman.h` | every function, with glibc's `*64` and `__xstat` names | |
| Mounts and file systems | `mount`, `umount`, `umount2`, `pivot_root`, `chroot`, `swapon`, `swapoff`, `sync`, `syncfs`, `readahead`; `statfs`, `statvfs` and their `f` forms, with glibc's `64` names; `mntent.h`'s `setmntent`, `getmntent`, `getmntent_r`, `endmntent` and `hasmntopt`, with octal escapes | `addmntent` |
| Sockets and addresses | `socket`, `socketpair`, `bind`, `connect`, `listen`, `accept`, `accept4`, `getsockname`, `getpeername`, `getsockopt`, `setsockopt`, `shutdown`, `send`, `sendto`, `sendmsg`, `recv`, `recvfrom`, `recvmsg`; `inet_aton`, `inet_addr`, `inet_ntoa`, `inet_ntop`, `inet_pton`, the byte order functions, `if_nametoindex` | name resolution, `getifaddrs`, `if_nameindex` |
| System V IPC | shared memory, semaphores and message queues, every function | `ftok` |
| Linux's own calls | `prctl`, `capget`, `capset`, `personality`, `setns`, `unshare`, `reboot`, `klogctl`, `inotify_init`, `inotify_init1`, `inotify_add_watch`, `inotify_rm_watch`, `sendfile`, `sysinfo`, `flock`; `sched_yield`, `sched_getaffinity`, `sched_setaffinity`, `CPU_COUNT` | `epoll`, `eventfd`, `signalfd`, `timerfd` |
| `termios.h` | every function: attributes, the `cf*speed` calls, `cfmakeraw`, `tcdrain`, `tcflow`, `tcflush`, `tcsendbreak`, `tcgetsid`, and POSIX.1-2024's `tcgetwinsize` and `tcsetwinsize`; `unistd.h`'s `tcgetpgrp`, `tcsetpgrp`, `ttyname` and `ttyname_r` | `posix_openpt` and the rest of the pseudo-terminal calls |
| Running programs and temporary files | `system`, `popen`, `pclose`, `execl`, `execle`, `execlp`, `daemon`; `mkstemp`, `mkostemp`, `mkstemps`, `mkostemps` and their `64` names, `mkdtemp`, `mktemp`; `realpath` | `posix_spawn`, so `system` and `popen` fork |
| Users and groups | `getpwnam`, `getpwuid`, `getpwent`, `setpwent`, `endpwent`, `getpwnam_r`, `getpwuid_r`; `getgrnam`, `getgrgid`, `getgrent`, `setgrent`, `endgrent`, `getgrnam_r`, `getgrgid_r`, `getgrouplist`, `initgroups`; `getspnam_r`; `getlogin`, `getlogin_r`; `getusershell`, `setusershell`, `endusershell` | nscd, `fgetpwent` and `putpwent`, the rest of `shadow.h`, `crypt` |
| Logging and login records | `openlog`, `syslog`, `vsyslog`, `setlogmask`, `closelog`, as datagrams to `/dev/log`; `utmpx.h` and `utmp.h`, which keep no records, as musl's do | a logger over the network |
| Clocks | `time`, `clock_gettime` and the rest, `gettimeofday`, `settimeofday`, `nanosleep`, `clock`, `times`, `setitimer`, `getitimer`, `adjtimex`, `clock_adjtime` | the vDSO |
| Calendar time | `gmtime`, `localtime`, `mktime`, `timegm`, `difftime`, `asctime` and `ctime`, with their `_r` forms, over the whole 64-bit `time_t`; `tzset`, `tzname`, `timezone` and `daylight`, from POSIX `TZ` strings or validated TZif files; `strftime`, `strftime_l`, `strptime` | `getdate`, `wcsftime`, leap seconds |
| Processes and I/O | the `wait` family, `getrlimit` family, `uname`, `sethostname`, `setdomainname`, `getresuid`, `getresgid`, `setpgrp`, `poll`, `select`, `getrandom`, `ioctl`, vector I/O, `rename` | |
| `stdio.h` | `FILE` streams, fully, line or not buffered, each with a recursive lock; `fopen`, `fdopen`, `freopen`, `fmemopen`, `open_memstream`, `fopencookie`; reading, writing, seeking and the `_unlocked` forms; the `printf` family with glibc's `__*printf_chk` names, exact for `double` and x87 `long double`; streams flushed at `exit` | `scanf`, `popen`, wide-character streams, `tmpnam`, `gets` |
| `signal.h` | `sigaction`, `signal`, sets and masks, `sigpending`, `sigsuspend`, `sigtimedwait`, `sigqueue`, `kill`, `sigaltstack`, `raise`, `abort`, `pthread_kill` | `psignal` |
| `setjmp.h` | `setjmp`, `longjmp`, `sigsetjmp`, `siglongjmp`, glibc's `__sigsetjmp` and `__longjmp_chk`, with saved pointers mangled | |
| `dirent.h` | `opendir`, `fdopendir`, `readdir`, `readdir_r`, `rewinddir`, `seekdir`, `telldir`, `dirfd`, `closedir`, `scandir`, `alphasort`, `versionsort`, with glibc's `64` names; records the kernel sends are checked against the bytes it filled | |
| `getopt.h` | `getopt`, `getopt_long`, `getopt_long_only`, permuting as glibc does unless `POSIXLY_CORRECT` or a leading `+`, and `optreset` | |
| `errno.h` | `__errno_location`, per thread | |
| `sys/auxv.h` | `getauxval` | |
| C++ runtime | `__cxa_atexit`, and `__cxa_finalize` for a static program | |

## Next

1. In progress: **`scanf`**, **`glob`, `fnmatch`, `regex`, `search.h` and
   `libgen.h`**, **thread cancellation, semaphores and C11 threads**, and **the
   math library**.
2. **libc-test**, musl's conformance suite, as the measure of progress, and a
   compiler wrapper that builds an unmodified program against the library.
3. **`long double` math and `complex.h`.**
4. **AArch64 and ARMv7**, the other two architectures Ferrix runs.
5. **Dynamic linking**: a loader, then glibc's symbol versions.
