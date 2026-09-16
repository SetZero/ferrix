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
host's kernel UAPI headers and nothing from its C library. On Windows,
`tools/busybox/build-windows.sh` builds the same program without WSL: this
library for `x86_64-unknown-linux-gnu`, busybox with clang and lld against
Alpine's pinned `linux-headers`, and busybox's host programs with mingw gcc and
the few POSIX headers in `tools/busybox/hostcompat/`. `tools/busybox/sources.sh`
pins the busybox.net tarball and Alpine's config for its `busybox-static`
1.37.0-r20 by checksum, and `tools/busybox/config.sh` lists the changes made to
that config.

It downloads and builds under `~/.local/share/ferrix/busybox/ferrousli`, and
installs `x86_64/busybox` there once the program links. Until then it exits 1
and writes `undefined-symbols.txt` beside it: the functions busybox calls that
this library does not have yet. Ferrix's `cargo xtask test-shell --init` and
`test-vfs` are to run the installed binary, beside Alpine's musl build and the
glibc one.

busybox links against this library with nothing stubbed. Its applets also
call name resolution, pattern matching and a few math functions, and for a
while each of those was a stub that wrote that the function was not
implemented yet and aborted, listed in `src/stubs.rs`. The last of them were
replaced by the real functions on 2026-09-16, and the file is gone.

## Headers

`include/` holds musl 1.2.5's headers, unmodified. See
[include/README.md](include/README.md).

## Where it stands

x86-64, static programs linked at a fixed address.

| Area | There | Not yet |
|---|---|---|
| Startup | `_start`, `__libc_start_main`, `environ`, the auxiliary vector, `.preinit_array` and `.init_array` | position-independent static programs |
| Threads | a control block in glibc's layout for every thread, static TLS, the stack protector's canary; `pthread_create`, `pthread_join`, `pthread_detach`, `pthread_exit`, attributes, names, `gettid`, `pthread_sigqueue`; mutexes, including recursive, error-checking, robust and priority-inheriting ones; condition variables, read-write locks, keys and `pthread_once`; barriers, private and process-shared, and spin locks; scheduling policy, priority and affinity per thread, `sched.h`'s policy calls, `pthread_getcpuclockid`; semaphores, unnamed, process-shared and named in `/dev/shm`, with `sem_clockwait`; a futex lock for the library's own state | cancellation, C11 `threads.h`, `pthread_atfork`, `set*id` across threads |
| Memory | the `malloc` family, on `mmap`, with size classes and integrity checks | returning empty regions to the kernel |
| `stdlib.h` | `exit`, `_Exit`, `atexit` without a limit, `abort`, the environment functions; `strtol` and `strtod` families, correctly rounded for `float`, `double` and x87 `long double`, with glibc's `__isoc23_` names; `qsort`, `qsort_r`, `bsearch`; `abs` and `div` families; `rand`, `random` and `rand48` families; `quick_exit` and `at_quick_exit`; `secure_getenv`, `a64l`, `l64a` and `getsubopt` | `ecvt`, `fcvt`, `gcvt`; NaN payloads and rounding modes in parsing |
| `assert.h` | `assert`, whose `__assert_fail` writes musl's message straight to standard error and aborts | |
| `endian.h` | all 12 host, big-endian and little-endian conversions, both as macros and callable functions | |
| `stdatomic.h` | C atomic types, memory orders, fences, compare-exchange, exchange, load, store, fetch operations and flags over the compiler's atomic builtins | |
| `math.h`, `fenv.h` | the floating-point environment; rounding (`rint`, `nearbyint`, `lrint`, `lround` and the rest), manipulation (`frexp`, `ldexp`, `scalbn`, `logb`, `modf`, `nextafter`, `nan`), `fmod`, `remainder`, `remquo` and `fma` for `double` and `float`; `sin`, `cos`, `exp`, `log`, `pow` and `atan2` for `double`, ported from musl and giving its bits and exceptions in every rounding mode; `fpclassify`, `isinf`, `isnan`, `isnormal`, `isfinite`, `signbit`, `isunordered` and the comparison macros for `float`, `double` and x87 `long double`; `math_errhandling` is `MATH_ERREXCEPT`, as in musl | the rest of `math.h`, the `float` transcendental and `long double` functions, `errno` set by math functions as glibc does |
| `string.h`, `strings.h` | everything, with word-at-a-time scans and two-way `strstr` and `memmem`; `strcoll_l`, `strxfrm_l`, `strcasecmp_l` and `strncasecmp_l` | |
| `ctype.h` | the C locale, glibc's `__ctype_b_loc` tables, and the `_l` forms | |
| `locale.h`, `langinfo.h` | `setlocale`, `localeconv`, `newlocale`, `duplocale`, `freelocale`, `uselocale`, `nl_langinfo`; musl's C and C.UTF-8 locales, any other name behaving as UTF-8 | message catalogues, glibc's `locale_t` layout |
| Multibyte and wide characters | UTF-8 conversion in `stdlib.h`, `wchar.h` and `uchar.h`, strict as musl's; `wctype.h`'s classes, case mappings and `wcwidth` from musl's Unicode 12.1 tables, with every difference from glibc recorded; `wchar.h`'s string and memory functions, with `wcslcpy` and `wcslcat`; the `wcstol` and `wcstod` families, `wcstoimax` and `wcstoumax`, through the narrow parsers; `wcsftime`; wide-character stream I/O, `fgetwc` to `ungetwc` and `fwide`, with glibc's `_unlocked` names; the `wprintf` family, through the narrow formatter; the `wscanf` family, through the narrow scanner; `open_wmemstream` | `iconv` |
| Error text | `strerror`, `strerror_l`, `strerror_r` (XSI), `__xpg_strerror_r`, `strsignal` | glibc's GNU `strerror_r` |
| `unistd.h` | files, directories and links, `pipe` and `dup`, identities, `fork` on `clone`, `execve`, `execv`, `execvp`, `sleep`, `alarm`, `sysconf`, `isatty`, `syscall`; `chown` and its `l`, `f` and `at` forms, `utimes`, `gethostid` | `getcwd(NULL, 0)`, `set*id` across threads |
| `fcntl.h`, `sys/stat.h`, `sys/mman.h` | every function, with glibc's `*64` and `__xstat` names | |
| Mounts and file systems | `mount`, `umount`, `umount2`, `pivot_root`, `chroot`, `swapon`, `swapoff`, `sync`, `syncfs`, `readahead`; `statfs`, `statvfs` and their `f` forms, with glibc's `64` names; `mntent.h`'s `setmntent`, `getmntent`, `getmntent_r`, `endmntent` and `hasmntopt`, with octal escapes | `addmntent` |
| Sockets and addresses | `socket`, `socketpair`, `bind`, `connect`, `listen`, `accept`, `accept4`, `getsockname`, `getpeername`, `getsockopt`, `setsockopt`, `shutdown`, `send`, `sendto`, `sendmsg`, `recv`, `recvfrom`, `recvmsg`, `sockatmark`; `inet_aton`, `inet_addr`, `inet_ntoa`, `inet_ntop`, `inet_pton`, the byte order functions, `in6addr_any`, `in6addr_loopback` | `inet_network`, `inet_makeaddr`, `inet_netof`, `inet_lnaof` |
| Name resolution | `getaddrinfo`, `freeaddrinfo`, `getnameinfo`, `gai_strerror`; `gethostbyname`, `gethostbyname2`, `gethostbyaddr` and their `_r` forms, `getservbyname`, `getservbyport` and theirs, `h_errno`, `hstrerror`, `herror`; the hosts, networks, protocols and services databases; the stub resolver over `/etc/resolv.conf`, `res_query`, `res_send`, `dn_expand` and the `ns_` parser | a hosts-file cache, `/etc/nsswitch.conf`, DNSSEC |
| Interfaces and hardware addresses | `if_nametoindex`, `if_indextoname`, `if_nameindex`, `if_freenameindex`; `getifaddrs` and `freeifaddrs` over route netlink, falling back to `SIOCGIFCONF`; the `ether_` conversions and `/etc/ethers` | interface statistics through `ifa_data` |
| System V IPC | shared memory, semaphores and message queues, every function | `ftok` |
| Linux's own calls | `prctl`, `capget`, `capset`, `personality`, `setns`, `unshare`, `reboot`, `klogctl`, `inotify_init`, `inotify_init1`, `inotify_add_watch`, `inotify_rm_watch`, `sendfile`, `sysinfo`, `flock`; `sched_yield`, `sched_getaffinity`, `sched_setaffinity`, `CPU_COUNT` | `epoll`, `eventfd`, `signalfd`, `timerfd` |
| `termios.h` | every function: attributes, the `cf*speed` calls, `cfmakeraw`, `tcdrain`, `tcflow`, `tcflush`, `tcsendbreak`, `tcgetsid`, and POSIX.1-2024's `tcgetwinsize` and `tcsetwinsize`; `unistd.h`'s `tcgetpgrp`, `tcsetpgrp`, `ttyname` and `ttyname_r` | `posix_openpt` and the rest of the pseudo-terminal calls |
| Running programs and temporary files | `system`, `popen`, `pclose`, `execl`, `execle`, `execlp`, `daemon`; `mkstemp`, `mkostemp`, `mkstemps`, `mkostemps` and their `64` names, `mkdtemp`, `mktemp`; `realpath` | `posix_spawn`, so `system` and `popen` fork |
| Users and groups | `getpwnam`, `getpwuid`, `getpwent`, `setpwent`, `endpwent`, `getpwnam_r`, `getpwuid_r`; `getgrnam`, `getgrgid`, `getgrent`, `setgrent`, `endgrent`, `getgrnam_r`, `getgrgid_r`, `getgrouplist`, `initgroups`; `getspnam_r`; `getlogin`, `getlogin_r`; `getusershell`, `setusershell`, `endusershell` | nscd, `fgetpwent` and `putpwent`, the rest of `shadow.h` |
| Password hashing | `crypt` and `crypt_r`: the traditional DES hash and BSDi's extended `_` form, with musl's reading of salts outside the alphabet and its self test; `$1$` MD5, `$5$` SHA-256 and `$6$` SHA-512, with musl's key, salt and `rounds=` limits; checked against musl's, Drepper's and libxcrypt's vectors | `$2*$` blowfish, which gives `"*"` until it is here; `encrypt`, `setkey` |
| Logging and login records | `openlog`, `syslog`, `vsyslog`, `setlogmask`, `closelog`, as datagrams to `/dev/log`; `utmpx.h` and `utmp.h`, which keep no records, as musl's do | a logger over the network |
| Clocks | `time`, `clock_gettime` and the rest, `gettimeofday`, `settimeofday`, `nanosleep`, `clock`, `times`, `setitimer`, `getitimer`, `adjtimex`, `clock_adjtime` | the vDSO |
| Calendar time | `gmtime`, `localtime`, `mktime`, `timegm`, `difftime`, `asctime` and `ctime`, with their `_r` forms, over the whole 64-bit `time_t`; `tzset`, `tzname`, `timezone` and `daylight`, from POSIX `TZ` strings or validated TZif files; `strftime`, `strftime_l`, `strptime` | `getdate`, `wcsftime`, leap seconds |
| Processes and I/O | the `wait` family, `getrlimit` family, `uname`, `sethostname`, `setdomainname`, `getresuid`, `getresgid`, `setpgrp`, `poll`, `select`, `getrandom`, `ioctl`, vector I/O, `rename` | |
| `stdio.h` | `FILE` streams, fully, line or not buffered, each with a recursive lock; `fopen`, `fdopen`, `freopen`, `fmemopen`, `open_memstream`, `fopencookie`, `open_wmemstream`; reading, writing, seeking and the `_unlocked` forms; the `printf` family with glibc's `__*printf_chk` names, exact for `double` and x87 `long double`; streams flushed at `exit`; `tmpnam`; the `wprintf` and `wscanf` families | `gets` |
| Formatted input | `scanf`, `fscanf`, `sscanf` and their `v` forms, with glibc's `__isoc99_` and `__isoc23_` names: every conversion, widths, `*`, `%N$`, `m` allocation and wide strings | C23's `%b` and `0b` |
| `signal.h` | `sigaction`, `signal`, sets and masks, `sigpending`, `sigsuspend`, `sigtimedwait`, `sigqueue`, `kill`, `sigaltstack`, `raise`, `abort`, `pthread_kill` | `psignal` |
| `setjmp.h` | `setjmp`, `longjmp`, `sigsetjmp`, `siglongjmp`, glibc's `__sigsetjmp` and `__longjmp_chk`, with saved pointers mangled | |
| `dirent.h` | `opendir`, `fdopendir`, `readdir`, `readdir_r`, `rewinddir`, `seekdir`, `telldir`, `dirfd`, `closedir`, `scandir`, `alphasort`, `versionsort`, with glibc's `64` names; records the kernel sends are checked against the bytes it filled | |
| `getopt.h` | `getopt`, `getopt_long`, `getopt_long_only`, permuting as glibc does unless `POSIXLY_CORRECT` or a leading `+`, and `optreset` | |
| `fnmatch.h` | `fnmatch`, with musl's linear-time matching: `*`, `?`, brackets with ranges, negation and classes, `FNM_PATHNAME`, `FNM_PERIOD`, `FNM_NOESCAPE`, `FNM_LEADING_DIR` and `FNM_CASEFOLD`, cross-checked against glibc | multibyte characters, which wait for a locale other than C |
| `libgen.h` | `basename`, glibc's `__xpg_basename`, `dirname` | |
| `regex.h` | `regcomp`, `regexec`, `regerror`, `regfree`: basic and extended expressions with musl's grammar, `REG_ICASE`, `REG_NEWLINE`, `REG_NOSUB`, `REG_NOTBOL`, `REG_NOTEOL`, back-references, and POSIX's leftmost-longest match with its submatches, found by simulating the whole automaton at once rather than backtracking | multibyte characters and collating elements, which wait for a locale other than C |
| `errno.h` | `__errno_location`, per thread | |
| `sys/auxv.h` | `getauxval` | |
| C++ runtime | `__cxa_atexit`, and `__cxa_finalize` for a static program | |

## Next

1. In progress: **`glob` and `search.h`**, **thread
   cancellation, semaphores and C11 threads**, and **the rest of the math
   library**.
2. **libc-test**, musl's conformance suite, as the measure of progress, and a
   compiler wrapper that builds an unmodified program against the library.
3. **`long double` math and `complex.h`.**
4. **AArch64 and ARMv7**, the other two architectures Ferrix runs.
5. **Dynamic linking**: a loader, then glibc's symbol versions.
