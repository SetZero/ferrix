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
| `stdlib.h` | `exit`, `_Exit`, `atexit`, `abort`, `getenv` | `malloc`, conversions, sorting, the rest |
| `string.h` | `strlen`, `strcmp`, `strncmp`, `memcpy`, `memmove`, `memset`, `memcmp`, `bcmp` | the rest |
| `unistd.h` | `read`, `write`, `_exit` | the rest |
| `stdio.h` | `puts`, unbuffered | `FILE`, `printf` |
| `signal.h` | `raise` | `sigaction`, signal masks |
| `errno.h` | `__errno_location`, per thread | |
| `sys/auxv.h` | `getauxval` | |
| C++ runtime | `__cxa_atexit`, and `__cxa_finalize` for a static program | |

`atexit` holds 32 handlers, the POSIX minimum, until there is `malloc`.

## Next

1. **`malloc`.**
2. **The rest of `string.h` and `ctype.h`, and `stdlib.h`'s conversions.**
3. **Buffered stdio and `printf`.**
4. **libc-test**, musl's conformance suite, as the measure of progress, and a
   compiler wrapper that builds an unmodified program against the library.
5. **The file, process, time and memory system calls.**
6. **Threads.**
7. **AArch64 and ARMv7**, the other two architectures Ferrix runs.
8. **Dynamic linking**: a loader, then glibc's symbol versions.
