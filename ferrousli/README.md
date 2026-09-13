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

[CONVENTIONS.md](CONVENTIONS.md) has the rules every change here follows.

`cargo build` leaves `target/debug/libferrousli.a`. `crt1.o` is built by
`build.rs` into cargo's `OUT_DIR`. A program builds as:

```
cc -static -no-pie -nostdlib -nostdinc -isystem include \
   -o prog path/to/crt1.o prog.c target/debug/libferrousli.a
```

## Headers

`include/` holds musl 1.2.5's headers, unmodified. See
[include/README.md](include/README.md).

## Where it stands

x86-64, static programs linked at a fixed address.

| Area | There | Not yet |
|---|---|---|
| Startup | `_start`, `__libc_start_main`, `environ`, the auxiliary vector, `.preinit_array` and `.init_array` | position-independent static programs |
| Threads | the main thread's control block in glibc's layout, static TLS, the stack protector's canary | `pthread_*` |
| Memory | the `malloc` family, on `mmap`, with size classes and integrity checks | returning empty regions to the kernel |
| `stdlib.h` | `exit`, `_Exit`, `atexit` without a limit, `abort`, the environment functions; `strtol` and `strtod` families, correctly rounded for `float`, `double` and x87 `long double`, with glibc's `__isoc23_` names; `qsort`, `qsort_r`, `bsearch`; `abs` and `div` families; `rand`, `random` and `rand48` families | `ecvt`, `fcvt`, `gcvt`; NaN payloads and rounding modes in parsing |
| `string.h`, `strings.h` | everything but the allocating functions; word-at-a-time scans; two-way `strstr` and `memmem` | `strdup`, `strndup`, the `_l` forms |
| `ctype.h` | the C locale, and glibc's `__ctype_b_loc` tables | the `_l` forms |
| Error text | `strerror`, `strerror_r` (XSI), `__xpg_strerror_r`, `strsignal` | glibc's GNU `strerror_r` |
| `unistd.h` | files, directories and links, `pipe` and `dup`, identities, `fork` on `clone`, `execve`, `execv`, `execvp`, `sleep`, `alarm`, `sysconf`, `isatty`, `syscall` | the `execl` family, `getcwd(NULL, 0)`, `set*id` across threads |
| `fcntl.h`, `sys/stat.h`, `sys/mman.h` | every function, with glibc's `*64` and `__xstat` names | |
| Clocks | `time`, `clock_gettime` and the rest, `gettimeofday`, `nanosleep` | the vDSO |
| Processes and I/O | the `wait` family, `getrlimit` family, `uname`, `poll`, `select`, `getrandom`, `ioctl`, vector I/O, `rename` | |
| `stdio.h` | `puts`, unbuffered | `FILE`, `printf`, `scanf` |
| `signal.h` | `sigaction`, `signal`, sets and masks, `sigpending`, `sigsuspend`, `sigtimedwait`, `sigqueue`, `kill`, `sigaltstack`, `raise`, `abort` | `psignal`, `pthread_kill` |
| `setjmp.h` | `setjmp`, `longjmp`, `sigsetjmp`, `siglongjmp`, glibc's `__sigsetjmp` and `__longjmp_chk`, with saved pointers mangled | |
| `errno.h` | `__errno_location`, per thread | |
| `sys/auxv.h` | `getauxval` | |
| C++ runtime | `__cxa_atexit`, and `__cxa_finalize` for a static program | |

## Next

1. In progress: **buffered stdio, `printf` and `scanf`**, **time zones and
   `strftime`**, **locales and wide characters**, **`dirent`, `getopt`, `glob`
   and `regex`**, **threads**, and **the math library**.
2. **libc-test**, musl's conformance suite, as the measure of progress, and a
   compiler wrapper that builds an unmodified program against the library.
3. **`long double` math and `complex.h`.**
4. **AArch64 and ARMv7**, the other two architectures Ferrix runs.
5. **Dynamic linking**: a loader, then glibc's symbol versions.
