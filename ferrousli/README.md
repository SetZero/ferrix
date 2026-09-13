# Ferrousli

A C library for Linux, written in Rust. The name is *ferrous* and *musl*.

The model is musl: small, correct, and built to be linked statically. The
destination is further. A program built against glibc should one day be able to
load this library in glibc's place. That means matching glibc's exported
symbols, structure layouts and startup contract, not only its API. This is
why `crt1.o` already calls `__libc_start_main` with glibc's arguments.

It lives in the Ferrix tree but stands alone. It is its own cargo workspace,
depends on no Ferrix crate, and reaches the kernel only through Linux system
calls. It therefore builds and tests on any x86-64 Linux host, and runs on
Ferrix the way any other static program does.

## Building and testing

From this directory:

```
cargo test
```

The unit tests call the functions from Rust. `tests/c_programs.rs` compiles the
programs in `tests/c/` with the host's `cc`, links them against `crt1.o` and
`libferrousli.a` and nothing else, runs them, and checks their output and exit
status. Each program is built at `-O0` and at `-O2`.

`cargo build` leaves `target/debug/libferrousli.a`. `crt1.o` is built by
`build.rs` into cargo's `OUT_DIR`. A program links as:

```
cc -static -no-pie -nostdlib -nostdinc -fno-stack-protector \
   -o prog path/to/crt1.o prog.c target/debug/libferrousli.a
```

## Where it stands

x86-64, static programs only.

| Area | There | Not yet |
|---|---|---|
| Startup | `_start`, `__libc_start_main`, `environ`, `.preinit_array` and `.init_array` | auxiliary vector, thread pointer, `.fini_array` |
| `stdlib.h` | `exit`, `_Exit`, `getenv` | `atexit`, `malloc`, everything else |
| `string.h` | `strlen`, `strcmp`, `strncmp`, `memcpy`, `memmove`, `memset`, `memcmp`, `bcmp` | the rest |
| `unistd.h` | `read`, `write`, `_exit` | the rest |
| `stdio.h` | `puts`, unbuffered | `FILE`, `printf` |
| `errno.h` | `__errno_location`, one per process | one per thread |

## Next

1. **A thread pointer.** Set `%fs` to a thread control block, with the stack
   canary at `%fs:0x28` taken from `AT_RANDOM`. Programs could then drop
   `-fno-stack-protector`, which every distribution's compiler turns on by
   default.
2. **`atexit` and `.fini_array`**, run by `exit`.
3. **`malloc`.**
4. **Headers and a sysroot**: `include/`, and `lib/` holding `crt1.o` and
   `libc.a`, so that `cc --sysroot` builds an unmodified program.
5. **Buffered stdio and `printf`.**
6. **AArch64 and ARMv7**, the other two architectures Ferrix runs.
7. **Dynamic linking**: a loader, then glibc's symbol versions.
