# Ferrousli against POSIX.1-2024

POSIX.1-2024 is a goal by the customer's decision of 2026-09-13, on the
condition that it never breaks Linux compatibility: where the two differ, Linux
wins (`docs/BACKLOG.md`, Decisions). Ferrix takes POSIX through its C library
over the Linux ABI, so the library half of that goal is `ferrousli/`'s. This
file measures it, one interface at a time, against the standard's own list of
system interfaces, and prices what is missing in story points.

It was measured on 2026-09-14 at `develop` 55753ea, whose `ferrousli/` is
unchanged since 5e9b0b6. Each landing that closes part of the gap updates the
tables below in the same commit, the way a stage's roadmap section is updated.

## The count

The standard's index lists 1243 interfaces: functions, and the macros and
variables it specifies beside them.

| Status | Interfaces | Meaning |
|---|---|---|
| present | 683 | defined by `libferrousli.a`, or a macro the standard specifies as one and `include/` defines |
| macro only | 19 | a function the standard requires, which the header defines only as a macro: a call works, taking its address or `#undef` does not |
| broken | 13 | present, but it fails to link or gives a wrong answer |
| stubbed | 16 | defined in `src/stubs.rs`, which ends the program |
| absent | 512 | not there |

560 interfaces are missing in one of the last four ways. 116 of them are
already written on the three unlanded branches of 2026-09-13
(`ferrousli-threads`, `ferrousli-math`, `ferrousli-misc`), which were
committed without a build and are not reviewed.

## What is left, by area, in points

Areas group headers by what implements them. Points follow
`docs/BACKLOG.md`: 1 is a change whose pattern and tests already exist, 13 a
new subsystem. Every area's missing names are in the index at the end.

| Area | Interfaces | Missing | On a branch | Points | What the points buy |
|---|---|---|---|---|---|
| Language support and the standard library | 118 | 48 | 0 | 5 | `<stdatomic.h>` over the compiler's builtins 2 (the build passes `-nostdinc`, so the compiler's own header is out of reach); the 12 `endian.h` functions 1; `quick_exit` and `at_quick_exit` 1; `a64l`, `l64a`, `getsubopt`, `secure_getenv` 1. `setkey` is counted with `encrypt` |
| Strings and characters | 72 | 2 | 0 | 1 | `strcasecmp_l`, `strncasecmp_l` |
| Wide and multibyte characters | 118 | 35 | 0 | 14 | wide-character streams (`fgetwc` to `vwscanf`, `fwide`, `ungetwc`) 8; the `wcstol` and `wcstod` families with `wcstoimax` and `wcstoumax` 3; `open_wmemstream` 2; `wcsftime`, `wcslcpy`, `wcslcat` 1 |
| Standard I/O | 70 | 1 | 0 | 1 | `tmpnam` |
| Math and the floating-point environment | 201 | 173 | 43 | 26 | landing `ferrousli-math`: `fenv.h`, rounding, manipulation, remainders, `fma` 5; the transcendental functions for `double` and `float`, the Bessel functions and `signgam`, replacing six stubs 13; every `long double` form 8. The classifiers are in the link-breakers |
| Complex arithmetic | 69 | 66 | 0 | 8 | `complex.h` over the math library, `creal` and `cimag` as functions |
| Locales, messages and conversion | 32 | 24 | 0 | 15 | the `gettext` family with `.mo` catalogues 5; `iconv` 5; `catopen`, `catgets`, `catclose` 2; `strfmon`, `strfmon_l` 2; `getlocalename_l` 1 |
| Files, directories and I/O multiplexing | 46 | 1 | 0 | 1 | `posix_getdents` |
| Processes, identity and the system | 103 | 11 | 0 | 8 | `confstr`, `pathconf`, `fpathconf` 2; `setresuid`, `setresgid` 1; `nice`, `lockf` 1; `posix_close` 1; `fmtmsg` 1; `encrypt` and `setkey` 2. `crypt`'s DES hash is in the link-breakers |
| Spawning | 25 | 25 | 0 | 7 | the `posix_spawn` family on `clone(CLONE_VM\|CLONE_VFORK)`, which lets `system` and `popen` stop forking, 5; `_Fork` 1; `fexecve` 1 |
| Signals and non-local jumps | 28 | 4 | 0 | 2 | `psignal`, `psiginfo` 1; `sig2str`, `str2sig` 1 |
| Time and clocks | 29 | 4 | 1 | 3 | `getdate` and `getdate_err` 2; `timespec_get` 1. `pthread_getcpuclockid` comes with `ferrousli-threads` |
| Threads and scheduling | 145 | 65 | 57 | 19 | landing `ferrousli-threads`: barriers, spin locks, semaphores, `sched.h`, C11 `threads.h` 8; cancellation, `pthread_cancel` and `pthread_testcancel` with their cancellation points 8; the `clock` variants of the condition, mutex, read-write lock and semaphore waits 2; `pthread_atfork` 1 |
| Memory mapping and System V IPC | 21 | 1 | 0 | 1 | `ftok` |
| Realtime: asynchronous I/O, message queues, timers, shared memory | 29 | 29 | 0 | 11 | `aio.h` over threads 3; `mqueue.h` 3; `timer_*` with `SIGEV_THREAD` 3; `shm_open`, `shm_unlink` 1; `clock_getcpuclockid` 1. Typed memory (`posix_typed_mem_*`, `posix_mem_offset`) is the TYM option, which ferrousli does not claim: 0 |
| Terminals and devices | 25 | 7 | 0 | 3 | `posix_openpt`, `grantpt`, `unlockpt`, `ptsname`, `ptsname_r`, `ctermid` 2; `posix_devctl` and `<devctl.h>` 1 |
| Networking and name resolution | 55 | 28 | 0 | 13 | `getaddrinfo`, `getnameinfo`, `freeaddrinfo`, `gai_strerror` over `/etc/hosts` and a DNS stub resolver, replacing five stubs, 8; the hosts, networks, protocols and services databases 3; `if_nameindex`, `if_freenameindex`, `if_indextoname` 1; `in6addr_any`, `in6addr_loopback`, `sockatmark` 1 |
| Patterns, paths and search | 23 | 22 | 15 | 13 | landing `ferrousli-misc`: `search.h`, `libgen.h`, `glob` 3; `regex.h`, replacing four stubs, 5; `wordexp` 3; `nftw` 2 |
| Users, groups and databases | 29 | 9 | 0 | 3 | `<ndbm.h>` and the `dbm_*` functions |
| Dynamic loading | 5 | 5 | 0 | 2 | `dlfcn.h` for a static program, failing cleanly; a loader is the README's fifth item and is not priced here |
| Across areas | | | | 7 | the link-breakers below 4; POSIX.1-2024's declarations in `include/` 3 |
| **All** | **1243** | **560** | **116** | **163** | |

## Present but broken

These look present and are not, which makes them worse than a missing name: a
program compiles against the header and then fails to link, or runs and gets
the wrong answer. Four points remain.

`assert` was in this list. `__assert_fail` is now defined, so a false assertion
writes its diagnostic and aborts instead of failing to link.

* **`fpclassify`** calls `__fpclassify`, `__fpclassifyf` or `__fpclassifyl` for
  every type, and **`isinf`, `isnan`, `isnormal`, `isfinite` and `signbit`**
  call `__fpclassifyl` or `__signbitl` for `long double`. So do **`isgreater`,
  `isgreaterequal`, `isless`, `islessequal`, `islessgreater` and
  `isunordered`**, whose `long double` forms test `isnan`. None of those four
  helpers exists, so those uses fail to link. 1 point.
* **`crypt`** gives `"*"`, its failure value, for a salt of two characters from
  `[a-zA-Z0-9./]`. POSIX leaves the algorithm implementation-defined, but
  requires `crypt` to take exactly such a salt, and the traditional DES hash is
  what every other C library computes for it. 3 points.

## Where Ferrix differs from Linux

**Credentials are per process.** Linux keeps user and group ids per thread in
the kernel (setuid(2), "C library/kernel differences"). POSIX requires every
thread of a process to share them, and glibc's and musl's `set*id` wrappers
emulate that by making every thread repeat the call. Ferrix's kernel keeps
`Credentials` on the `Process` (`kernel/src/syscall/process.rs`), which is what
POSIX requires, so on Ferrix one thread's `setuid` changes them all without the
broadcast. The product owner's ruling of 2026-09-14 is to keep it: no kernel
change. What follows from it:

* A program that relies on per-thread credentials through the raw system call,
  such as a file server that gives each thread its own ids, sees a difference on
  Ferrix.
* `ferrousli` on a Linux host does not conform until its `set*id` functions
  broadcast to every thread as NPTL's do. That broadcast is harmless on Ferrix,
  where each repeat sets ids the process already has.

## Headers

`include/` holds musl 1.2.5's headers unmodified, and musl 1.2.5 predates
POSIX.1-2024.

* **Missing:** `<devctl.h>`, `<ndbm.h>` and `<stdatomic.h>`.
* **Not declared anywhere in `include/`**, beyond those three headers' contents:
  `posix_getdents`; the six `gettext` `_l` forms and `getlocalename_l`; the
  `clock` waits `pthread_cond_clockwait`, `pthread_mutex_clocklock`,
  `pthread_rwlock_clockrdlock`, `pthread_rwlock_clockwrlock` and
  `sem_clockwait`; `sig2str` and `str2sig`;
  `posix_spawn_file_actions_addchdir` and `posix_spawn_file_actions_addfchdir`;
  `wcslcpy` and `wcslcat`; the typed-memory functions.
* **Not checked:** the constants, types and structure members POSIX.1-2024
  adds, such as `O_CLOFORK`, `FD_CLOFORK` and `SOCK_CLOFORK`. They are part of
  the 3 points for declarations.

## What this does not measure

* **Behaviour.** An interface counts as present when the archive defines it.
  Whether it does what the standard says is libc-test's to measure, the P2
  row in `docs/BACKLOG.md`.
* **Anything but x86-64** and static linking, the only configuration ferrousli
  builds today.
* **The utilities and the shell**, the other half of POSIX, which busybox and
  the kernel answer, not the library.
* **The kernel's side.** Each interface needs Linux system calls answered as
  Linux answers them. The POSIX.1-2024 interface sweep, a P1 row, measures
  that.

## Beyond POSIX: what relibc has

For comparison, os-08 checked relibc (master 1490c4d) the same way: every
`extern "C"` function outside its Redox-only files against the same archive.
relibc has 398 functions ferrousli lacks. Most are in the tables above. These
are the ones POSIX does not require, and several are what real programs call:

| Area | Functions |
|---|---|
| netdb | `__h_errno_location`, `gethostbyaddr`, `gethostbyname`, `herror`, `hstrerror` |
| pthread | `pthread_getconcurrency`, `pthread_setconcurrency` |
| unistd | `brk`, `getdtablesize`, `getpass`, `getwd`, `sbrk`, `ualarm` |
| stdlib | `ecvt`, `fcvt`, `gcvt`, `ttyslot` |
| err | `err`, `err_set_exit`, `err_set_file`, `errc`, `errx`, `verr`, `verrc`, `verrx`, `vwarn`, `vwarnc`, `vwarnx`, `warn`, `warnc`, `warnx` |
| stdio | `__fpending`, `__fpurge`, `__freadable`, `__freading`, `__fwritable`, `__fwriting`, `cuserid`, `gets`, `renameat2`, `tempnam` |
| time | `timelocal`, `timespec_getres` |
| sys/epoll | `epoll_create`, `epoll_create1`, `epoll_ctl`, `epoll_pwait`, `epoll_wait` |
| arpa/inet | `inet_lnaof`, `inet_makeaddr`, `inet_netof`, `inet_network` |
| shadow | `endspent`, `getspent`, `getspnam`, `setspent` |
| dirent | `fdclosedir` |
| ifaddrs | `freeifaddrs`, `getifaddrs` |
| pty | `forkpty`, `openpty` |
| cxa.rs | `__cxa_thread_atexit_impl` |
| dl-tls | `__tls_get_addr` |
| float | `flt_rounds` |
| sgtty | `gtty` |
| string | `strnlen_s` |
| sys/ptrace | `ptrace` |
| sys/timeb | `ftime` |
| utime | `utime` |
| utmp | `login_tty` |

Ferrousli in turn exports about 170 functions relibc lacks, mostly glibc's
binary names (`__xstat`, the `*64` names, `__isoc23_`, `__*_chk`,
`__libc_start_main`) and Linux's own calls.

## How it was measured

1. **The standard's list.** The system interfaces index of POSIX.1-2024,
   `https://pubs.opengroup.org/onlinepubs/9799919799/idx/functions.html`, gives
   1243 names. Each name's own page gives its header: the last `#include` before
   its declaration in the SYNOPSIS, skipping includes marked `[OH]`. It also
   gives its option codes: the margin codes around the declaration, shown in the
   index without `CX` and `OH`. `_Exit.html` and `_exit.html` are one file on a
   case-insensitive disk, so those two headers were set by hand.
2. **The library's list.** `nm -g --defined-only` over
   `ferrousli/target/x86_64-unknown-linux-gnu/release/libferrousli.a`, built
   from 5e9b0b6, minus the 30 names in `src/stubs.rs`. Sort both lists with
   `LC_ALL=C` before comparing them.
3. **Macros.** A name counts as present through `include/` only when the
   standard specifies it as a macro (`FD_SET`, `va_arg`, `errno`, `isgreater`
   and its five siblings, `CMPLX`, `pthread_cleanup_push`). A required function
   that its own header defines only as a macro is "macro only". A macro in
   `<tgmath.h>` counts for nothing. The macros that expand to helpers were read
   in musl's headers, and a macro whose helper is missing is "broken" even when
   the standard specifies it as a macro: that is how the link-breakers above
   were found.
4. **Branches.** A missing name is "written on" a branch when that branch's
   diff from `develop` adds an `extern "C" fn` of that name.

## Index

Every interface, by area and header. Option codes follow a name in
parentheses: `XSI` is the X/Open System Interfaces option; `SPN`, `MSG`, `PS`,
`TPS`, `TSH`, `TYM`, `SHM`, `ADV`, `ML`, `MLR`, `MC1`, `IP6`, `TCT`, `CPT`,
`FSC`, `SIO` and `DC` are the other options; `OB` marks an obsolescent
interface.

### Language support and the standard library

| Header | Status | Interfaces |
|---|---|---|
| `<assert.h>` | present (1) | `assert` |
| `<endian.h>` | macro only (12) | `be16toh`, `be32toh`, `be64toh`, `htobe16`, `htobe32`, `htobe64`, `htole16`, `htole32`, `htole64`, `le16toh`, `le32toh`, `le64toh` |
| `<errno.h>` | present (1) | `errno` |
| `<inttypes.h>` | present (4) | `imaxabs`, `imaxdiv`, `strtoimax`, `strtoumax` |
| `<stdarg.h>` | present (4) | `va_arg`, `va_copy`, `va_end`, `va_start` |
| `<stdatomic.h>` | absent (29) | `atomic_compare_exchange_strong`, `atomic_compare_exchange_strong_explicit`, `atomic_compare_exchange_weak`, `atomic_compare_exchange_weak_explicit`, `atomic_exchange`, `atomic_exchange_explicit`, `atomic_fetch_add`, `atomic_fetch_add_explicit`, `atomic_fetch_and`, `atomic_fetch_and_explicit`, `atomic_fetch_or`, `atomic_fetch_or_explicit`, `atomic_fetch_sub`, `atomic_fetch_sub_explicit`, `atomic_fetch_xor`, `atomic_fetch_xor_explicit`, `atomic_flag_clear`, `atomic_flag_clear_explicit`, `atomic_flag_test_and_set`, `atomic_flag_test_and_set_explicit`, `atomic_init`, `atomic_is_lock_free`, `atomic_load`, `atomic_load_explicit`, `atomic_signal_fence`, `atomic_store`, `atomic_store_explicit`, `atomic_thread_fence`, `kill_dependency` |
| `<stdlib.h>` | present (60) | `_Exit`, `abort`, `abs`, `aligned_alloc`, `atexit`, `atof`, `atoi`, `atol`, `atoll`, `bsearch`, `calloc`, `div`, `drand48` (XSI), `erand48` (XSI), `exit`, `free`, `getenv`, `initstate` (XSI), `jrand48` (XSI), `labs`, `lcong48` (XSI), `ldiv`, `llabs`, `lldiv`, `lrand48` (XSI), `malloc`, `mblen`, `mbstowcs`, `mbtowc`, `mkdtemp`, `mkostemp`, `mkstemp`, `mrand48` (XSI), `nrand48` (XSI), `posix_memalign` (ADV), `putenv` (XSI), `qsort`, `qsort_r`, `rand`, `random` (XSI), `realloc`, `reallocarray`, `realpath`, `seed48` (XSI), `setenv`, `setstate` (XSI), `srand`, `srand48` (XSI), `srandom` (XSI), `strtod`, `strtof`, `strtol`, `strtold`, `strtoll`, `strtoul`, `strtoull`, `system`, `unsetenv`, `wcstombs`, `wctomb` |
| `<stdlib.h>` | absent (7) | `a64l` (XSI), `at_quick_exit`, `getsubopt`, `l64a` (XSI), `quick_exit`, `secure_getenv`, `setkey` |

### Strings and characters

| Header | Status | Interfaces |
|---|---|---|
| `<ctype.h>` | present (28) | `isalnum`, `isalnum_l`, `isalpha`, `isalpha_l`, `isblank`, `isblank_l`, `iscntrl`, `iscntrl_l`, `isdigit`, `isdigit_l`, `isgraph`, `isgraph_l`, `islower`, `islower_l`, `isprint`, `isprint_l`, `ispunct`, `ispunct_l`, `isspace`, `isspace_l`, `isupper`, `isupper_l`, `isxdigit`, `isxdigit_l`, `tolower`, `tolower_l`, `toupper`, `toupper_l` |
| `<string.h>` | present (37) | `memccpy` (XSI), `memchr`, `memcmp`, `memcpy`, `memmem`, `memmove`, `memset`, `stpcpy`, `stpncpy`, `strcat`, `strchr`, `strcmp`, `strcoll`, `strcoll_l`, `strcpy`, `strcspn`, `strdup`, `strerror`, `strerror_l`, `strerror_r`, `strlcat`, `strlcpy`, `strlen`, `strncat`, `strncmp`, `strncpy`, `strndup`, `strnlen`, `strpbrk`, `strrchr`, `strsignal`, `strspn`, `strstr`, `strtok`, `strtok_r`, `strxfrm`, `strxfrm_l` |
| `<strings.h>` | present (5) | `ffs` (XSI), `ffsl` (XSI), `ffsll` (XSI), `strcasecmp`, `strncasecmp` |
| `<strings.h>` | absent (2) | `strcasecmp_l`, `strncasecmp_l` |

### Wide and multibyte characters

| Header | Status | Interfaces |
|---|---|---|
| `<inttypes.h>` | absent (2) | `wcstoimax`, `wcstoumax` |
| `<uchar.h>` | present (4) | `c16rtomb`, `c32rtomb`, `mbrtoc16`, `mbrtoc32` |
| `<wchar.h>` | present (43) | `btowc`, `mbrlen`, `mbrtowc`, `mbsinit`, `mbsnrtowcs`, `mbsrtowcs`, `wcpcpy`, `wcpncpy`, `wcrtomb`, `wcscasecmp`, `wcscasecmp_l`, `wcscat`, `wcschr`, `wcscmp`, `wcscoll`, `wcscoll_l`, `wcscpy`, `wcscspn`, `wcsdup`, `wcslen`, `wcsncasecmp`, `wcsncasecmp_l`, `wcsncat`, `wcsncmp`, `wcsncpy`, `wcsnlen`, `wcsnrtombs`, `wcspbrk`, `wcsrchr`, `wcsrtombs`, `wcsspn`, `wcsstr`, `wcstok`, `wcswidth` (XSI), `wcsxfrm`, `wcsxfrm_l`, `wctob`, `wcwidth` (XSI), `wmemchr`, `wmemcmp`, `wmemcpy`, `wmemmove`, `wmemset` |
| `<wchar.h>` | absent (33) | `fgetwc`, `fgetws`, `fputwc`, `fputws`, `fwide`, `fwprintf`, `fwscanf`, `getwc`, `getwchar`, `open_wmemstream`, `putwc`, `putwchar`, `swprintf`, `swscanf`, `ungetwc`, `vfwprintf`, `vfwscanf`, `vswprintf`, `vswscanf`, `vwprintf`, `vwscanf`, `wcsftime`, `wcslcat`, `wcslcpy`, `wcstod`, `wcstof`, `wcstol`, `wcstold`, `wcstoll`, `wcstoul`, `wcstoull`, `wprintf`, `wscanf` |
| `<wctype.h>` | present (36) | `iswalnum`, `iswalnum_l`, `iswalpha`, `iswalpha_l`, `iswblank`, `iswblank_l`, `iswcntrl`, `iswcntrl_l`, `iswctype`, `iswctype_l`, `iswdigit`, `iswdigit_l`, `iswgraph`, `iswgraph_l`, `iswlower`, `iswlower_l`, `iswprint`, `iswprint_l`, `iswpunct`, `iswpunct_l`, `iswspace`, `iswspace_l`, `iswupper`, `iswupper_l`, `iswxdigit`, `iswxdigit_l`, `towctrans`, `towctrans_l`, `towlower`, `towlower_l`, `towupper`, `towupper_l`, `wctrans`, `wctrans_l`, `wctype`, `wctype_l` |

### Standard I/O

| Header | Status | Interfaces |
|---|---|---|
| `<stdio.h>` | present (69) | `asprintf`, `clearerr`, `dprintf`, `fclose`, `fdopen`, `feof`, `ferror`, `fflush`, `fgetc`, `fgetpos`, `fgets`, `fileno`, `flockfile`, `fmemopen`, `fopen`, `fprintf`, `fputc`, `fputs`, `fread`, `freopen`, `fscanf`, `fseek`, `fseeko`, `fsetpos`, `ftell`, `ftello`, `ftrylockfile`, `funlockfile`, `fwrite`, `getc`, `getc_unlocked`, `getchar`, `getchar_unlocked`, `getdelim`, `getline`, `open_memstream`, `pclose`, `perror`, `popen`, `printf`, `putc`, `putc_unlocked`, `putchar`, `putchar_unlocked`, `puts`, `remove`, `rename`, `renameat`, `rewind`, `scanf`, `setbuf`, `setvbuf`, `snprintf`, `sprintf`, `sscanf`, `stderr`, `stdin`, `stdout`, `tmpfile`, `ungetc`, `vasprintf`, `vdprintf`, `vfprintf`, `vfscanf`, `vprintf`, `vscanf`, `vsnprintf`, `vsprintf`, `vsscanf` |
| `<stdio.h>` | absent (1) | `tmpnam` (OB) |

### Math and the floating-point environment

| Header | Status | Interfaces |
|---|---|---|
| `<fenv.h>` | absent, written on ferrousli-math (11) | `feclearexcept`, `fegetenv`, `fegetexceptflag`, `fegetround`, `feholdexcept`, `feraiseexcept`, `fesetenv`, `fesetexceptflag`, `fesetround`, `fetestexcept`, `feupdateenv` |
| `<math.h>` | present (28) | `cbrt`, `cbrtf`, `ceil`, `ceilf`, `copysign`, `copysignf`, `fabs`, `fabsf`, `fdim`, `fdimf`, `floor`, `floorf`, `fma`, `fmaf`, `fmax`, `fmaxf`, `fmin`, `fminf`, `fmod`, `fmodf`, `rint`, `rintf`, `round`, `roundf`, `sqrt`, `sqrtf`, `trunc`, `truncf` |
| `<math.h>` | broken (12) | `fpclassify`: expands to __fpclassify, __fpclassifyf or __fpclassifyl, all absent: fails to link; `isfinite`: long double form calls __fpclassifyl, absent: fails to link; `isgreater`: long double form goes through isunordered and isnan to __fpclassifyl, absent: fails to link; `isgreaterequal`: long double form goes through isunordered and isnan to __fpclassifyl, absent: fails to link; `isinf`: long double form calls __fpclassifyl, absent: fails to link; `isless`: long double form goes through isunordered and isnan to __fpclassifyl, absent: fails to link; `islessequal`: long double form goes through isunordered and isnan to __fpclassifyl, absent: fails to link; `islessgreater`: long double form goes through isunordered and isnan to __fpclassifyl, absent: fails to link; `isnan`: long double form calls __fpclassifyl, absent: fails to link; `isnormal`: long double form calls __fpclassifyl, absent: fails to link; `isunordered`: long double form goes through isunordered and isnan to __fpclassifyl, absent: fails to link; `signbit`: long double form calls __signbitl, absent: fails to link |
| `<math.h>` | stubbed (6) | `atan2`, `cos`, `exp`, `log`, `pow`, `sin` |
| `<math.h>` | absent (112) | `acos`, `acosf`, `acosh`, `acoshf`, `acoshl`, `acosl`, `asin`, `asinf`, `asinh`, `asinhf`, `asinhl`, `asinl`, `atan`, `atan2f`, `atan2l`, `atanf`, `atanh`, `atanhf`, `atanhl`, `atanl`, `cbrtl`, `ceill`, `copysignl`, `cosf`, `cosh`, `coshf`, `coshl`, `cosl`, `erf`, `erfc`, `erfcf`, `erfcl`, `erff`, `erfl`, `exp2`, `exp2f`, `exp2l`, `expf`, `expl`, `expm1`, `expm1f`, `expm1l`, `fabsl`, `fdiml`, `floorl`, `fmal`, `fmaxl`, `fminl`, `fmodl`, `frexpl`, `hypot`, `hypotf`, `hypotl`, `ilogbl`, `j0` (XSI), `j1` (XSI), `jn` (XSI), `ldexpl`, `lgamma`, `lgammaf`, `lgammal`, `llrintl`, `llroundl`, `log10`, `log10f`, `log10l`, `log1p`, `log1pf`, `log1pl`, `log2`, `log2f`, `log2l`, `logbl`, `logf`, `logl`, `lrintl`, `lroundl`, `modfl`, `nanl`, `nearbyintl`, `nextafterl`, `nexttoward`, `nexttowardf`, `nexttowardl`, `powf`, `powl`, `remainderl`, `remquol`, `rintl`, `roundl`, `scalblnl`, `scalbnl`, `signgam` (XSI), `sinf`, `sinh`, `sinhf`, `sinhl`, `sinl`, `sqrtl`, `tan`, `tanf`, `tanh`, `tanhf`, `tanhl`, `tanl`, `tgamma`, `tgammaf`, `tgammal`, `truncl`, `y0` (XSI), `y1` (XSI), `yn` (XSI) |
| `<math.h>` | absent, written on ferrousli-math (32) | `frexp`, `frexpf`, `ilogb`, `ilogbf`, `ldexp`, `ldexpf`, `llrint`, `llrintf`, `llround`, `llroundf`, `logb`, `logbf`, `lrint`, `lrintf`, `lround`, `lroundf`, `modf`, `modff`, `nan`, `nanf`, `nearbyint`, `nearbyintf`, `nextafter`, `nextafterf`, `remainder`, `remainderf`, `remquo`, `remquof`, `scalbln`, `scalblnf`, `scalbn`, `scalbnf` |

### Complex arithmetic

| Header | Status | Interfaces |
|---|---|---|
| `<complex.h>` | present (3) | `CMPLX`, `CMPLXF`, `CMPLXL` |
| `<complex.h>` | macro only (6) | `cimag`, `cimagf`, `cimagl`, `creal`, `crealf`, `creall` |
| `<complex.h>` | absent (60) | `cabs`, `cabsf`, `cabsl`, `cacos`, `cacosf`, `cacosh`, `cacoshf`, `cacoshl`, `cacosl`, `carg`, `cargf`, `cargl`, `casin`, `casinf`, `casinh`, `casinhf`, `casinhl`, `casinl`, `catan`, `catanf`, `catanh`, `catanhf`, `catanhl`, `catanl`, `ccos`, `ccosf`, `ccosh`, `ccoshf`, `ccoshl`, `ccosl`, `cexp`, `cexpf`, `cexpl`, `clog`, `clogf`, `clogl`, `conj`, `conjf`, `conjl`, `cpow`, `cpowf`, `cpowl`, `cproj`, `cprojf`, `cprojl`, `csin`, `csinf`, `csinh`, `csinhf`, `csinhl`, `csinl`, `csqrt`, `csqrtf`, `csqrtl`, `ctan`, `ctanf`, `ctanh`, `ctanhf`, `ctanhl`, `ctanl` |

### Locales, messages and conversion

| Header | Status | Interfaces |
|---|---|---|
| `<iconv.h>` | absent (3) | `iconv`, `iconv_close`, `iconv_open` |
| `<langinfo.h>` | present (2) | `nl_langinfo`, `nl_langinfo_l` |
| `<libintl.h>` | absent (15) | `bind_textdomain_codeset`, `bindtextdomain`, `dcgettext`, `dcgettext_l`, `dcngettext`, `dcngettext_l`, `dgettext`, `dgettext_l`, `dngettext`, `dngettext_l`, `gettext`, `gettext_l`, `ngettext`, `ngettext_l`, `textdomain` |
| `<locale.h>` | present (6) | `duplocale`, `freelocale`, `localeconv`, `newlocale`, `setlocale`, `uselocale` |
| `<locale.h>` | absent (1) | `getlocalename_l` |
| `<monetary.h>` | absent (2) | `strfmon`, `strfmon_l` |
| `<nl_types.h>` | absent (3) | `catclose`, `catgets`, `catopen` |

### Files, directories and I/O multiplexing

| Header | Status | Interfaces |
|---|---|---|
| `<dirent.h>` | present (11) | `alphasort`, `closedir`, `dirfd`, `fdopendir`, `opendir`, `readdir`, `readdir_r` (OB), `rewinddir`, `scandir`, `seekdir` (XSI), `telldir` (XSI) |
| `<dirent.h>` | absent (1) | `posix_getdents` |
| `<fcntl.h>` | present (6) | `creat`, `fcntl`, `open`, `openat`, `posix_fadvise` (ADV), `posix_fallocate` (ADV) |
| `<poll.h>` | present (2) | `poll`, `ppoll` |
| `<sys/select.h>` | present (6) | `FD_CLR`, `FD_ISSET`, `FD_SET`, `FD_ZERO`, `pselect`, `select` |
| `<sys/stat.h>` | present (16) | `chmod`, `fchmod`, `fchmodat`, `fstat`, `fstatat`, `futimens`, `lstat`, `mkdir`, `mkdirat`, `mkfifo`, `mkfifoat`, `mknod` (XSI), `mknodat` (XSI), `stat`, `umask`, `utimensat` |
| `<sys/statvfs.h>` | present (2) | `fstatvfs`, `statvfs` |
| `<sys/uio.h>` | present (2) | `readv` (XSI), `writev` (XSI) |

### Processes, identity and the system

| Header | Status | Interfaces |
|---|---|---|
| `<fmtmsg.h>` | absent (1) | `fmtmsg` (XSI) |
| `<sys/resource.h>` | present (5) | `getpriority` (XSI), `getrlimit`, `getrusage` (XSI), `setpriority` (XSI), `setrlimit` |
| `<sys/times.h>` | present (1) | `times` |
| `<sys/utsname.h>` | present (1) | `uname` |
| `<sys/wait.h>` | present (3) | `wait`, `waitid`, `waitpid` |
| `<syslog.h>` | present (4) | `closelog` (XSI), `openlog` (XSI), `setlogmask` (XSI), `syslog` (XSI) |
| `<unistd.h>` | present (78) | `_exit`, `access`, `alarm`, `chdir`, `chown`, `close`, `dup`, `dup2`, `dup3`, `environ`, `execl`, `execle`, `execlp`, `execv`, `execve`, `execvp`, `faccessat`, `fchdir`, `fchown`, `fchownat`, `fdatasync` (SIO), `fork`, `fsync` (FSC), `ftruncate`, `getcwd`, `getegid`, `getentropy`, `geteuid`, `getgid`, `getgroups`, `gethostid` (XSI), `gethostname`, `getlogin`, `getlogin_r`, `getopt`, `getpgid`, `getpgrp`, `getpid`, `getppid`, `getresgid` (XSI), `getresuid` (XSI), `getsid`, `getuid`, `lchown`, `link`, `linkat`, `lseek`, `optarg`, `opterr`, `optind`, `optopt`, `pause`, `pipe`, `pipe2`, `pread`, `pwrite`, `read`, `readlink`, `readlinkat`, `rmdir`, `setegid`, `seteuid`, `setgid`, `setpgid`, `setregid` (XSI), `setreuid` (XSI), `setsid`, `setuid`, `sleep`, `swab` (XSI), `symlink`, `symlinkat`, `sync` (XSI), `sysconf`, `truncate`, `unlink`, `unlinkat`, `write` |
| `<unistd.h>` | broken (1) | `crypt` (XSI): the traditional DES hash (two-character salt) returns "*" |
| `<unistd.h>` | absent (9) | `confstr`, `encrypt`, `fpathconf`, `lockf` (XSI), `nice` (XSI), `pathconf`, `posix_close`, `setresgid` (XSI), `setresuid` (XSI) |

### Spawning

| Header | Status | Interfaces |
|---|---|---|
| `<sched.h>` | absent (4) | `posix_spawnattr_getschedparam`, `posix_spawnattr_getschedpolicy`, `posix_spawnattr_setschedparam`, `posix_spawnattr_setschedpolicy` |
| `<spawn.h>` | absent (19) | `posix_spawn` (SPN), `posix_spawn_file_actions_addchdir` (SPN), `posix_spawn_file_actions_addclose` (SPN), `posix_spawn_file_actions_adddup2` (SPN), `posix_spawn_file_actions_addfchdir` (SPN), `posix_spawn_file_actions_addopen` (SPN), `posix_spawn_file_actions_destroy` (SPN), `posix_spawn_file_actions_init` (SPN), `posix_spawnattr_destroy` (SPN), `posix_spawnattr_getflags` (SPN), `posix_spawnattr_getpgroup` (SPN), `posix_spawnattr_getsigdefault` (SPN), `posix_spawnattr_getsigmask` (SPN), `posix_spawnattr_init` (SPN), `posix_spawnattr_setflags` (SPN), `posix_spawnattr_setpgroup` (SPN), `posix_spawnattr_setsigdefault` (SPN), `posix_spawnattr_setsigmask` (SPN), `posix_spawnp` (SPN) |
| `<unistd.h>` | absent (2) | `_Fork`, `fexecve` |

### Signals and non-local jumps

| Header | Status | Interfaces |
|---|---|---|
| `<setjmp.h>` | present (4) | `longjmp`, `setjmp`, `siglongjmp`, `sigsetjmp` |
| `<signal.h>` | present (20) | `kill`, `killpg` (XSI), `pthread_kill`, `pthread_sigmask`, `raise`, `sigaction`, `sigaddset`, `sigaltstack` (XSI), `sigdelset`, `sigemptyset`, `sigfillset`, `sigismember`, `signal`, `sigpending`, `sigprocmask`, `sigqueue`, `sigsuspend`, `sigtimedwait`, `sigwait`, `sigwaitinfo` |
| `<signal.h>` | absent (4) | `psiginfo`, `psignal`, `sig2str`, `str2sig` |

### Time and clocks

| Header | Status | Interfaces |
|---|---|---|
| `<sys/time.h>` | present (1) | `utimes` (XSI) |
| `<time.h>` | present (24) | `asctime` (OB), `asctime_r`, `clock`, `clock_getres`, `clock_gettime`, `clock_nanosleep`, `clock_settime`, `ctime` (OB), `ctime_r`, `daylight` (XSI), `difftime`, `gmtime`, `gmtime_r`, `localtime`, `localtime_r`, `mktime`, `nanosleep`, `strftime`, `strftime_l`, `strptime` (XSI), `time`, `timezone` (XSI), `tzname`, `tzset` |
| `<time.h>` | absent (3) | `getdate` (XSI), `getdate_err`, `timespec_get` |
| `<time.h>` | absent, written on ferrousli-threads (1) | `pthread_getcpuclockid` (TCT) |

### Threads and scheduling

| Header | Status | Interfaces |
|---|---|---|
| `<pthread.h>` | present (79) | `pthread_attr_destroy`, `pthread_attr_getdetachstate`, `pthread_attr_getguardsize`, `pthread_attr_getinheritsched` (TPS), `pthread_attr_getschedparam`, `pthread_attr_getschedpolicy` (TPS), `pthread_attr_getscope` (TPS), `pthread_attr_getstack`, `pthread_attr_getstacksize` (TSS), `pthread_attr_init`, `pthread_attr_setdetachstate`, `pthread_attr_setguardsize`, `pthread_attr_setinheritsched` (TPS), `pthread_attr_setschedparam`, `pthread_attr_setschedpolicy` (TPS), `pthread_attr_setscope` (TPS), `pthread_attr_setstack`, `pthread_attr_setstacksize` (TSS), `pthread_cleanup_pop`, `pthread_cleanup_push`, `pthread_cond_broadcast`, `pthread_cond_destroy`, `pthread_cond_init`, `pthread_cond_signal`, `pthread_cond_timedwait`, `pthread_cond_wait`, `pthread_condattr_destroy`, `pthread_condattr_getclock`, `pthread_condattr_getpshared` (TSH), `pthread_condattr_init`, `pthread_condattr_setclock`, `pthread_condattr_setpshared` (TSH), `pthread_create`, `pthread_detach`, `pthread_equal`, `pthread_exit`, `pthread_getspecific`, `pthread_join`, `pthread_key_create`, `pthread_key_delete`, `pthread_mutex_consistent`, `pthread_mutex_destroy`, `pthread_mutex_getprioceiling` (RPP or TPP), `pthread_mutex_init`, `pthread_mutex_lock`, `pthread_mutex_setprioceiling` (RPP or TPP), `pthread_mutex_timedlock`, `pthread_mutex_trylock`, `pthread_mutex_unlock`, `pthread_mutexattr_destroy`, `pthread_mutexattr_getprioceiling` (RPP or TPP), `pthread_mutexattr_getprotocol` (MC1), `pthread_mutexattr_getpshared` (TSH), `pthread_mutexattr_getrobust`, `pthread_mutexattr_gettype`, `pthread_mutexattr_init`, `pthread_mutexattr_setprioceiling` (RPP or TPP), `pthread_mutexattr_setprotocol` (MC1), `pthread_mutexattr_setpshared` (TSH), `pthread_mutexattr_setrobust`, `pthread_mutexattr_settype`, `pthread_once`, `pthread_rwlock_destroy`, `pthread_rwlock_init`, `pthread_rwlock_rdlock`, `pthread_rwlock_timedrdlock`, `pthread_rwlock_timedwrlock`, `pthread_rwlock_tryrdlock`, `pthread_rwlock_trywrlock`, `pthread_rwlock_unlock`, `pthread_rwlock_wrlock`, `pthread_rwlockattr_destroy`, `pthread_rwlockattr_getpshared` (TSH), `pthread_rwlockattr_init`, `pthread_rwlockattr_setpshared` (TSH), `pthread_self`, `pthread_setcancelstate`, `pthread_setcanceltype`, `pthread_setspecific` |
| `<pthread.h>` | absent (7) | `pthread_atfork` (OB), `pthread_cancel`, `pthread_cond_clockwait`, `pthread_mutex_clocklock`, `pthread_rwlock_clockrdlock`, `pthread_rwlock_clockwrlock`, `pthread_testcancel` |
| `<pthread.h>` | absent, written on ferrousli-threads (15) | `pthread_barrier_destroy`, `pthread_barrier_init`, `pthread_barrier_wait`, `pthread_barrierattr_destroy`, `pthread_barrierattr_getpshared` (TSH), `pthread_barrierattr_init`, `pthread_barrierattr_setpshared` (TSH), `pthread_getschedparam` (TPS), `pthread_setschedparam` (TPS), `pthread_setschedprio` (TPS), `pthread_spin_destroy`, `pthread_spin_init`, `pthread_spin_lock`, `pthread_spin_trylock`, `pthread_spin_unlock` |
| `<sched.h>` | present (1) | `sched_yield` |
| `<sched.h>` | absent, written on ferrousli-threads (7) | `sched_get_priority_max` (PS or TPS), `sched_get_priority_min` (PS or TPS), `sched_getparam` (PS), `sched_getscheduler` (PS), `sched_rr_get_interval` (PS or TPS), `sched_setparam` (PS), `sched_setscheduler` (PS) |
| `<semaphore.h>` | absent, written on ferrousli-threads (11) | `sem_clockwait`, `sem_close`, `sem_destroy`, `sem_getvalue`, `sem_init`, `sem_open`, `sem_post`, `sem_timedwait`, `sem_trywait`, `sem_unlink`, `sem_wait` |
| `<threads.h>` | macro only (1) | `thrd_equal` |
| `<threads.h>` | absent, written on ferrousli-threads (24) | `call_once`, `cnd_broadcast`, `cnd_destroy`, `cnd_init`, `cnd_signal`, `cnd_timedwait`, `cnd_wait`, `mtx_destroy`, `mtx_init`, `mtx_lock`, `mtx_timedlock`, `mtx_trylock`, `mtx_unlock`, `thrd_create`, `thrd_current`, `thrd_detach`, `thrd_exit`, `thrd_join`, `thrd_sleep`, `thrd_yield`, `tss_create`, `tss_delete`, `tss_get`, `tss_set` |

### Memory mapping and System V IPC

| Header | Status | Interfaces |
|---|---|---|
| `<sys/ipc.h>` | absent (1) | `ftok` (XSI) |
| `<sys/mman.h>` | present (9) | `mlock` (MLR), `mlockall` (ML), `mmap`, `mprotect`, `msync` (XSI or SIO), `munlock` (MLR), `munlockall` (ML), `munmap`, `posix_madvise` (ADV) |
| `<sys/msg.h>` | present (4) | `msgctl` (XSI), `msgget` (XSI), `msgrcv` (XSI), `msgsnd` (XSI) |
| `<sys/sem.h>` | present (3) | `semctl` (XSI), `semget` (XSI), `semop` (XSI) |
| `<sys/shm.h>` | present (4) | `shmat` (XSI), `shmctl` (XSI), `shmdt` (XSI), `shmget` (XSI) |

### Realtime: asynchronous I/O, message queues, timers, shared memory

| Header | Status | Interfaces |
|---|---|---|
| `<aio.h>` | absent (8) | `aio_cancel`, `aio_error`, `aio_fsync` (FSC or SIO), `aio_read`, `aio_return`, `aio_suspend`, `aio_write`, `lio_listio` |
| `<mqueue.h>` | absent (10) | `mq_close` (MSG), `mq_getattr` (MSG), `mq_notify` (MSG), `mq_open` (MSG), `mq_receive` (MSG), `mq_send` (MSG), `mq_setattr` (MSG), `mq_timedreceive` (MSG), `mq_timedsend` (MSG), `mq_unlink` (MSG) |
| `<sys/mman.h>` | absent (5) | `posix_mem_offset` (TYM), `posix_typed_mem_get_info` (TYM), `posix_typed_mem_open` (TYM), `shm_open` (SHM), `shm_unlink` (SHM) |
| `<time.h>` | absent (6) | `clock_getcpuclockid` (CPT), `timer_create`, `timer_delete`, `timer_getoverrun`, `timer_gettime`, `timer_settime` |

### Terminals and devices

| Header | Status | Interfaces |
|---|---|---|
| `<devctl.h>` | absent (1) | `posix_devctl` (DC) |
| `<stdio.h>` | absent (1) | `ctermid` |
| `<stdlib.h>` | absent (5) | `grantpt` (XSI), `posix_openpt` (XSI), `ptsname` (XSI), `ptsname_r` (XSI), `unlockpt` (XSI) |
| `<termios.h>` | present (13) | `cfgetispeed`, `cfgetospeed`, `cfsetispeed`, `cfsetospeed`, `tcdrain`, `tcflow`, `tcflush`, `tcgetattr`, `tcgetsid`, `tcgetwinsize`, `tcsendbreak`, `tcsetattr`, `tcsetwinsize` |
| `<unistd.h>` | present (5) | `isatty`, `tcgetpgrp`, `tcsetpgrp`, `ttyname`, `ttyname_r` |

### Networking and name resolution

| Header | Status | Interfaces |
|---|---|---|
| `<arpa/inet.h>` | present (8) | `htonl`, `htons`, `inet_addr` (OB), `inet_ntoa` (OB), `inet_ntop`, `inet_pton`, `ntohl`, `ntohs` |
| `<net/if.h>` | present (1) | `if_nametoindex` |
| `<net/if.h>` | absent (3) | `if_freenameindex`, `if_indextoname`, `if_nameindex` |
| `<netdb.h>` | stubbed (5) | `freeaddrinfo`, `getaddrinfo`, `getnameinfo`, `getservbyname`, `getservbyport` |
| `<netdb.h>` | absent (17) | `endhostent`, `endnetent`, `endprotoent`, `endservent`, `gai_strerror`, `gethostent`, `getnetbyaddr`, `getnetbyname`, `getnetent`, `getprotobyname`, `getprotobynumber`, `getprotoent`, `getservent`, `sethostent`, `setnetent`, `setprotoent`, `setservent` |
| `<netinet/in.h>` | absent (2) | `in6addr_any` (IP6), `in6addr_loopback` (IP6) |
| `<sys/socket.h>` | present (18) | `accept`, `accept4`, `bind`, `connect`, `getpeername`, `getsockname`, `getsockopt`, `listen`, `recv`, `recvfrom`, `recvmsg`, `send`, `sendmsg`, `sendto`, `setsockopt`, `shutdown`, `socket`, `socketpair` |
| `<sys/socket.h>` | absent (1) | `sockatmark` |

### Patterns, paths and search

| Header | Status | Interfaces |
|---|---|---|
| `<fnmatch.h>` | present (1) | `fnmatch` |
| `<ftw.h>` | absent (1) | `nftw` (XSI) |
| `<glob.h>` | absent, written on ferrousli-misc (2) | `glob`, `globfree` |
| `<libgen.h>` | stubbed (1) | `dirname` (XSI) (written on ferrousli-misc) |
| `<libgen.h>` | absent, written on ferrousli-misc (1) | `basename` (XSI) |
| `<regex.h>` | stubbed (4) | `regcomp`, `regerror`, `regexec`, `regfree` |
| `<search.h>` | absent, written on ferrousli-misc (11) | `hcreate` (XSI), `hdestroy` (XSI), `hsearch` (XSI), `insque` (XSI), `lfind` (XSI), `lsearch` (XSI), `remque` (XSI), `tdelete` (XSI), `tfind` (XSI), `tsearch` (XSI), `twalk` (XSI) |
| `<wordexp.h>` | absent (2) | `wordexp`, `wordfree` |

### Users, groups and databases

| Header | Status | Interfaces |
|---|---|---|
| `<grp.h>` | present (7) | `endgrent` (XSI), `getgrent` (XSI), `getgrgid`, `getgrgid_r`, `getgrnam`, `getgrnam_r`, `setgrent` (XSI) |
| `<ndbm.h>` | absent (9) | `dbm_clearerr` (XSI), `dbm_close` (XSI), `dbm_delete` (XSI), `dbm_error` (XSI), `dbm_fetch` (XSI), `dbm_firstkey` (XSI), `dbm_nextkey` (XSI), `dbm_open` (XSI), `dbm_store` (XSI) |
| `<pwd.h>` | present (7) | `endpwent` (XSI), `getpwent` (XSI), `getpwnam`, `getpwnam_r`, `getpwuid`, `getpwuid_r`, `setpwent` (XSI) |
| `<utmpx.h>` | present (6) | `endutxent` (XSI), `getutxent` (XSI), `getutxid` (XSI), `getutxline` (XSI), `pututxline` (XSI), `setutxent` (XSI) |

### Dynamic loading

| Header | Status | Interfaces |
|---|---|---|
| `<dlfcn.h>` | absent (5) | `dladdr`, `dlclose`, `dlerror`, `dlopen`, `dlsym` |
