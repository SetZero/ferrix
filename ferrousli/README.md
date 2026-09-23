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
calls. It therefore builds and tests on any Linux host, and runs on Ferrix
the way any other static program does. It supports x86-64, AArch64 and ARMv7-A
(hard float), the three architectures Ferrix runs.

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

### On AArch64 and ARMv7-A

The same tests run for the Arm targets on an x86-64 host, under QEMU's user
mode, when three variables name the target, a C compiler for it and the
emulator:

```
cargo test --lib --target aarch64-unknown-linux-gnu
FERROUSLI_TEST_TARGET=aarch64-unknown-linux-gnu CC=aarch64-linux-gnu-gcc \
  FERROUSLI_TEST_RUNNER='qemu-aarch64 -L /usr/aarch64-linux-gnu' cargo test --tests

cargo test --lib --target armv7-unknown-linux-gnueabihf
FERROUSLI_TEST_TARGET=armv7-unknown-linux-gnueabihf CC=arm-linux-gnueabihf-gcc \
  FERROUSLI_TEST_RUNNER=qemu-arm cargo test --tests
```

The unit tests need cargo's linker and runner for the target set as usual
(`CARGO_TARGET_<TRIPLE>_LINKER` and `_RUNNER`); the C tests build their
programs with `CC` against this library alone, and run them with the runner.
Under a runner the harness defines `FERROUSLI_TEST_EMULATED` for the C
programs, and gives each ten times as long. A program checks under that name
only what user-mode QEMU cannot give it: `execve` of itself, which QEMU without
`binfmt_misc` hands to the host; flags and socket types QEMU drops or passes
on instead of refusing; a 32-bit program's directory positions on a 64-bit
host's ext4; and `getsockopt`'s new socket timeouts on a 32-bit target.

All three pass in full, every unit test and every C program, on 2026-09-23
under QEMU 9.2.4 for the Arm targets. The checks that differ between targets
are the ones whose answer does: a 32-bit `long`, a
`long double` that is IEEE binary128 on AArch64 and a `double` on ARMv7-A, and
the host glibc comparisons, which a cross build has no host glibc for.

`tools/abi-compare/` prints the size and field offsets of every structure the
library shares with C, compiled against these headers, for comparing a
target's layouts with musl's or glibc's.

### The gate

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

## The uutils family

`tools/uutils/build.sh` builds uutils/coreutils 0.9.0, findutils 0.9.1 and
diffutils v0.5.0 against this library, as six static x86-64 programs: Rust's own `std` and unwinder for the
`x86_64-unknown-linux-musl` target, and `libferrousli.a` in the C library's
place. On Windows, `tools/uutils/build-windows.sh` builds the same program
without WSL, with clang for the one C dependency in uutils' tree and lld for
the link. `tools/uutils/sources.sh` pins the release by checksum and says why
the target is the musl one when this library is closer to glibc.

`cargo xtask uutils` runs whichever script the host needs. The binaries
install under `x86_64/bin/` in `~/.local/share/ferrix/uutils/ferrousli` (or
`$FERRIX_UUTILS`), and a link that fails writes `undefined-symbols.txt` beside
them, as busybox's does. All six linked with nothing missing from this library
on 2026-09-18. coreutils and diffutils are multicall binaries, which pick
their utility from `argv[0]`; findutils builds one program per utility.

One thing this build does that the C ports do not: it links against a copy of
`libferrousli.a` whose `rust_begin_unwind` has been weakened. This library is
Rust, and a `no_std` one must define a `#[panic_handler]`; a program that
brings `std` defines it too, and two strong definitions do not link. It is the
same clash `rust_eh_personality` has, which `src/lib.rs` settles by defining
it weak in assembly, and which stable Rust cannot express for a
`#[panic_handler]`. The C ports link the library itself, untouched.

These are the utilities that replace busybox's, and since 2026-09-18 they are
what `/bin` holds: every name uutils provides is uutils', `/bin/sh` is zinc,
and busybox keeps the rest. `../docs/UUTILS.md` is the plan, including §6a on
the two projects of the family that do not build for this target.

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

## Ports

`tools/ports/` builds other programs against this library the way
`tools/busybox/` builds busybox: static x86-64 programs with the host's gcc,
from sources pinned by checksum, downloaded and built under
`~/.local/share/ferrix/ports/ferrousli` (or `$FERRIX_PORTS`). `common.sh` holds
what they share: the pinned download, the release library, and compiler
wrappers, `ferrousli-cc` and `ferrousli-c++`, that compile against `include/`
and link `crt1.o` and `libferrousli.a` and nothing from the host's C library.
Each port installs into `x86_64/` there with the layout it has on the guest,
and a link that fails writes `undefined-symbols.txt` beside its build, as
busybox's does.

| Port | What | Installs |
|---|---|---|
| `curl` | curl 8.22.0 over Mbed TLS 3.6.7, curl.se's extract of Mozilla's CA certificates, and Mbed TLS's test server for `test-net` | `bin/curl`, `etc/ssl/certs/ca-certificates.crt`, `usr/libexec/ferrix/ssl_server2`, `usr/share/ferrix/tls-test` |
| `libcxx` | LLVM 23.1.1's libc++, libc++abi and libunwind, the C++ runtime, built with gcc | `include/c++/v1`, `lib/libc++.a`, `lib/libc++abi.a`, `lib/libunwind.a` |
| `btop` | btop 1.4.7, C++23, over `libcxx` | `bin/btop` |
| `zlib` | zlib 1.3.2, for git | `include/zlib.h`, `lib/libz.a` |
| `git` | git 2.55.0 over `zlib` and `curl`'s libcurl, without Perl, Python, Tcl, gettext, iconv or its Rust half | `usr/bin/git` with a `bin/git` link, `usr/libexec/git-core`, `usr/share/git-core/templates` |

`cargo xtask ports` runs them in that order, on a Linux host. Every image that carries a
busybox carries the ports that are installed, on x86_64, and `cargo xtask
test-net` fetches with curl as well as with `wget` when curl is there.

curl linked with nothing missing from this library on 2026-09-16. On Linux it
fetches over HTTPS and refuses a self-signed certificate. On Ferrix it fetches
over HTTP and HTTPS. The kernel takes the time of day and a random seed from
firmware, and `test-net` checks a certificate against Mbed TLS's test server
inside the guest. The curl port installs that server, `ssl_server2`, in
`usr/libexec/ferrix`, and its test certificates in `usr/share/ferrix/tls-test`.

btop linked once this library had what libc++ needs: `dl_iterate_phdr`,
`dladdr`, the message catalogues, the `strtod_l` family, `pathconf`,
`copy_file_range`, `getloadavg`, and thread cancellation, which btop uses to
stop a stalled collector. On Ferrix it draws its CPU, memory, network and
process panels in the serial console, refreshing them.

## In glibc's place

`ld/` is the dynamic loader, `ld-ferrousli`, and `tools/build-shared.sh`
builds it together with this library linked as `libc.so.6`: every symbol at
the version glibc gives it, from `tools/glibc-versions/x86_64.txt`, which
`tools/gen-glibc-versions.py` reads out of a glibc installation. A program
linked against glibc then runs on the two in glibc's place; Debian's own
busybox does, on Ferrix, since 2026-09-21:

```
tools/build-shared.sh        # leaves libc.so.6 and ld.so in target/shared/x86_64
```

and from the repository root, `cargo xtask test-shell --interpreter ferrousli
--library ferrousli` with a dynamic `--init`. The loader and the library
share one interface, `__ferrousli_loader` (`ld/src/interface.rs` and
`src/loader.rs`): the static TLS layout, and the program's initialisers,
which the library asks the loader to run once it has started. A statically
linked program sees none of this: the reference is weak, and null there.

Two things changed for every program with it. `regex_t` and `regmatch_t`
have glibc's layout, which a program built against glibc allocates; and
`environ` is glibc's `__environ` with `environ` and `_environ` as weak
aliases. The loader binds everything at load, so a program that imports a
function this library lacks does not start at all, even if it never calls
it: `ld-ferrousli: undefined symbol` names it.

## Headers

`include/` holds musl 1.2.5's headers, unmodified. See
[include/README.md](include/README.md).

## Where it stands

Static programs linked at a fixed address, and as glibc's stand-in under
its own loader, on x86-64, AArch64 and ARMv7-A.

What differs between the architectures is kept to a module each, beside the
code that needs it: system calls and their numbers (`syscall/`, generated
tables in `src/generated/`), the thread pointer, `clone` and the TLS layout
(`arch/`; AArch64 and ARMv7-A put the thread control block below the thread
pointer, x86-64 above it), `setjmp`, `va_list`, the floating-point environment,
the few math instructions (`math/`), `complex.h`'s exception-exact arithmetic
and cancellation's system call. On AArch64 `long double` is IEEE binary128,
done in software (`math/ld128.rs`); on ARMv7-A it is `double`.

ARMv7-A is a 32-bit target with a 64-bit `time_t`, as musl 1.2 has it. Every
call that carries a time uses the kernel's 64-bit form (`clock_gettime64`,
`futex_time64` and the rest), and where the kernel has no such form, the
library converts: `itimerval`, `rusage`, the SysV IPC `*_ds` structures,
`stat64` and `statfs64`, `SO_RCVTIMEO` and `SO_SNDTIMEO` with the old
option's fallback, and `timex`. musl's headers rename those functions on a
32-bit target (`stat` to `__stat_time64`, say); `src/arm_names.rs` answers to
the new names, and to the `l` math functions, which are the `double` ones
there. A signal handler without `SA_SIGINFO` returns through `sigreturn`,
because ARMv7-A's kernel builds it an old-style frame.

| Area | There | Not yet |
|---|---|---|
| Startup | `_start`, `__libc_start_main`, `environ`, the auxiliary vector, `.preinit_array` and `.init_array` | position-independent static programs |
| Threads | a control block in glibc's layout for every thread, static TLS, the stack protector's canary; `pthread_create`, `pthread_join`, `pthread_detach`, `pthread_exit`, attributes, names, `gettid`, `pthread_sigqueue`; mutexes, including recursive, error-checking, robust and priority-inheriting ones; condition variables, read-write locks, keys and `pthread_once`; barriers, private and process-shared, and spin locks; scheduling policy, priority and affinity per thread, `sched.h`'s policy calls, `pthread_getcpuclockid`; semaphores, unnamed, process-shared and named in `/dev/shm`, with `sem_clockwait`; C11's `threads.h` over the `pthread.h` objects; a futex lock for the library's own state; cancellation, deferred and asynchronous, with `pthread_cancel`, `pthread_testcancel` and musl's cancellation points on the blocking calls, the library's own internal uses of them guarded | `pthread_atfork`, `set*id` across threads |
| Memory | the `malloc` family, on `mmap`, with size classes and integrity checks | returning empty regions to the kernel |
| `stdlib.h` | `exit`, `_Exit`, `atexit` without a limit, `abort`, the environment functions; `strtol` and `strtod` families, correctly rounded for `float`, `double` and x87 `long double`, with glibc's `__isoc23_` names; `qsort`, `qsort_r`, `bsearch`; `abs` and `div` families; `rand`, `random` and `rand48` families; `quick_exit` and `at_quick_exit`; `secure_getenv`, `a64l`, `l64a` and `getsubopt` | `ecvt`, `fcvt`, `gcvt`; NaN payloads and rounding modes in parsing |
| `assert.h` | `assert`, whose `__assert_fail` writes musl's message straight to standard error and aborts | |
| `endian.h` | all 12 host, big-endian and little-endian conversions, both as macros and callable functions | |
| `stdatomic.h` | C atomic types, memory orders, fences, compare-exchange, exchange, load, store, fetch operations and flags over the compiler's atomic builtins | |
| `math.h`, `fenv.h` | the floating-point environment; rounding (`rint`, `nearbyint`, `lrint`, `lround` and the rest), manipulation (`frexp`, `ldexp`, `scalbn`, `logb`, `modf`, `nextafter`, `nan`), `fmod`, `remainder`, `remquo` and `fma` for `double` and `float`; the error and gamma functions with `signgam`, and the Bessel functions; the hyperbolic functions and `hypot` for `double` and `float`; `tan`, `asin`, `acos` and `atan` for `double`, and the trigonometric functions for `float`; `exp2`, `expm1`, `log2`, `log10` and `log1p` for `double` and `float`, with `expf`, `logf` and `powf`; `sin`, `cos`, `exp`, `log`, `pow` and `atan2` for `double`, ported from musl and giving its bits and exceptions in every rounding mode; `fpclassify`, `isinf`, `isnan`, `isnormal`, `isfinite`, `signbit`, `isunordered` and the comparison macros for `float`, `double` and x87 `long double`; `math_errhandling` is `MATH_ERREXCEPT`, as in musl | the `long double` functions, `nexttoward`, `errno` set by math functions as glibc does |
| `complex.h` | every `double complex` and `float complex` function, `cabs` to `ctanh` and `cabsf` to `ctanhf`, with `creal`, `cimag` and their `float` forms callable | the `long double complex` functions |
| `string.h`, `strings.h` | everything, with word-at-a-time scans and two-way `strstr` and `memmem`; on AArch64, `memcpy`, `memmove`, `memset`, `memcmp`, `memchr`, `strlen` and `strchrnul` sixteen bytes at a time in Advanced SIMD (`src/string/aarch64.rs`); `strcoll_l`, `strxfrm_l`, `strcasecmp_l` and `strncasecmp_l` | |
| `ctype.h` | the C locale, glibc's `__ctype_b_loc` tables, and the `_l` forms | |
| `locale.h`, `langinfo.h` | `setlocale`, `localeconv`, `newlocale`, `duplocale`, `freelocale`, `uselocale`, `nl_langinfo`; musl's C and C.UTF-8 locales, any other name behaving as UTF-8; `nl_types.h`'s message catalogues, `catopen`, `catgets` and `catclose`, reading `gencat`'s files as musl does; `strtod_l`, `strtof_l` and `strtold_l` | glibc's `locale_t` layout |
| Multibyte and wide characters | UTF-8 conversion in `stdlib.h`, `wchar.h` and `uchar.h`, strict as musl's; `wctype.h`'s classes, case mappings and `wcwidth` from musl's Unicode 12.1 tables, with every difference from glibc recorded; `wchar.h`'s string and memory functions, with `wcslcpy` and `wcslcat`; the `wcstol` and `wcstod` families, `wcstoimax` and `wcstoumax`, through the narrow parsers; `wcsftime`; wide-character stream I/O, `fgetwc` to `ungetwc` and `fwide`, with glibc's `_unlocked` names; the `wprintf` family, through the narrow formatter; the `wscanf` family, through the narrow scanner; `open_wmemstream` | `iconv` |
| Error text | `strerror`, `strerror_l`, `strerror_r` (XSI), `__xpg_strerror_r`, `strsignal` | glibc's GNU `strerror_r` |
| `unistd.h` | files, directories and links, `pipe` and `dup`, identities, `fork` on `clone`, `execve`, `execv`, `execvp`, `sleep`, `alarm`, `sysconf`, `isatty`, `syscall`; `chown` and its `l`, `f` and `at` forms, `utimes`, `gethostid`; `pathconf` and `fpathconf`, from musl's table; `copy_file_range` | `getcwd(NULL, 0)`, `set*id` across threads |
| `fcntl.h`, `sys/stat.h`, `sys/mman.h` | every function, with glibc's `*64` and `__xstat` names; `utime.h`'s `utime`; Linux's `sync_file_range` | |
| Mounts and file systems | `mount`, `umount`, `umount2`, `pivot_root`, `chroot`, `swapon`, `swapoff`, `sync`, `syncfs`, `readahead`; `statfs`, `statvfs` and their `f` forms, with glibc's `64` names; `mntent.h`'s `setmntent`, `getmntent`, `getmntent_r`, `endmntent` and `hasmntopt`, with octal escapes | `addmntent` |
| Sockets and addresses | `socket`, `socketpair`, `bind`, `connect`, `listen`, `accept`, `accept4`, `getsockname`, `getpeername`, `getsockopt`, `setsockopt`, `shutdown`, `send`, `sendto`, `sendmsg`, `recv`, `recvfrom`, `recvmsg`, `sockatmark`; `inet_aton`, `inet_addr`, `inet_ntoa`, `inet_ntop`, `inet_pton`, the byte order functions, `in6addr_any`, `in6addr_loopback` | `inet_network`, `inet_makeaddr`, `inet_netof`, `inet_lnaof` |
| Name resolution | `getaddrinfo`, `freeaddrinfo`, `getnameinfo`, `gai_strerror`; `gethostbyname`, `gethostbyname2`, `gethostbyaddr` and their `_r` forms, `getservbyname`, `getservbyport` and theirs, `h_errno`, `hstrerror`, `herror`; the hosts, networks, protocols and services databases; the stub resolver over `/etc/resolv.conf`, `res_query`, `res_send`, `dn_expand` and the `ns_` parser | a hosts-file cache, `/etc/nsswitch.conf`, DNSSEC |
| Interfaces and hardware addresses | `if_nametoindex`, `if_indextoname`, `if_nameindex`, `if_freenameindex`; `getifaddrs` and `freeifaddrs` over route netlink, falling back to `SIOCGIFCONF`; the `ether_` conversions and `/etc/ethers` | interface statistics through `ifa_data` |
| System V IPC | shared memory, semaphores and message queues, every function | `ftok` |
| Linux's own calls | `prctl`, `capget`, `capset`, `personality`, `setns`, `unshare`, `reboot`, `klogctl`, `inotify_init`, `inotify_init1`, `inotify_add_watch`, `inotify_rm_watch`, `sendfile`, `sysinfo`, `getloadavg`, `flock`; `sched_yield`, `sched_getaffinity`, `sched_setaffinity`, `CPU_COUNT` | `signalfd`, `timerfd` |
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
| `glob.h` | `glob` and `globfree`, with musl's flags and glibc's `GLOB_TILDE`, `GLOB_TILDE_CHECK` and `64` names | |
| `libgen.h` | `basename`, glibc's `__xpg_basename`, `dirname` | |
| `search.h` | `hsearch` and glibc's `_r` forms over a growing table, `tsearch`, `tfind`, `tdelete`, `twalk` and glibc's `tdestroy` over a balanced tree, `lfind`, `lsearch`, `insque`, `remque` | |
| `regex.h` | `regcomp`, `regexec`, `regerror`, `regfree`: basic and extended expressions with musl's grammar, `REG_ICASE`, `REG_NEWLINE`, `REG_NOSUB`, `REG_NOTBOL`, `REG_NOTEOL`, back-references, and POSIX's leftmost-longest match with its submatches, found by simulating the whole automaton at once rather than backtracking; glibc's `regex_t` and `regmatch_t` layouts and `REG_STARTEND`, and GNU's `re_compile_pattern`, `re_search` and `re_syntax_options` | multibyte characters and collating elements, which wait for a locale other than C |
| `errno.h` | `__errno_location`, per thread | |
| `sys/auxv.h` | `getauxval` | |
| C++ runtime | `__cxa_atexit`, and `__cxa_finalize` for a static program; `dl_iterate_phdr` and `dladdr` over the program's own headers, which libunwind finds unwind tables by, and over every loaded object when `ld-ferrousli` loaded the library, with `dlopen`, `dlsym`, `dlclose` and `dlerror` too. LLVM's libc++, libc++abi and libunwind build against it (`tools/ports/libcxx`) | `__cxa_thread_atexit_impl`, which libc++abi does without |

## Next

1. In progress: **`long double` math**.
2. **libc-test**, musl's conformance suite, as the measure of progress, and a
   compiler wrapper that builds an unmodified program against the library.
3. **`long double` math, and with it `complex.h`'s `long double` forms.**
4. **Dynamic linking**: a loader, then glibc's symbol versions. Both run on
   all three architectures (below), with general-dynamic TLS and
   `dlfcn.h`; on ARMv7-A `libc.so.6` answers glibc's time64 names, and
   leaves out the ones glibc keeps for a 32-bit `time_t`.
