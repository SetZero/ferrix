# Ferrousli conventions

Ferrix's [conventions](../docs/CONVENTIONS.md) apply here too: a commit names
one author and carries no `Co-authored-by` or tool trailer; commit from a
worktree of your own; read `git diff --cached --stat` before every commit. The
rules below are particular to this library.

## Exporting a C function

* Write it as `#[cfg_attr(not(test), unsafe(no_mangle))] pub extern "C" fn`.
  The library exports the C name. Unit tests keep the mangled name, so it does
  not collide with the host libc's function of the same name in the test
  binary.
* A function that dereferences a pointer it was given is `unsafe extern "C"`
  and has a `# Safety` section. Every `unsafe` block holds one operation and a
  `// SAFETY:` comment that says why it is sound.
* Use `core::ffi` types. A structure C sees is `#[repr(C)]`, with its size and
  offsets asserted in `const _: () = assert!(...)`. The layout comes from the
  header in `include/`, which is the API programs compile against. Where glibc's
  layout differs, say so and say which one is kept.
* Put one module per header area in `src/`, listed alphabetically in
  `lib.rs`.

## Code that must not call itself

rustc and LLVM turn Rust code into calls to `memcpy`, `memmove`, `memset`,
`memcmp`, `bcmp` and `strlen`. Inside those functions, and anything they call,
that call is infinite recursion. So that code must not use:

* `copy_from_slice`, `fill`, `ptr::copy` or `ptr::write_bytes`
* iterator adapters such as `zip`
* any move of a value larger than two machine words

Write plain `while` loops over raw pointers instead. More generally, the
library must work when its unoptimised debug build is linked, and the tests
link both builds.

## The kernel's numbers and layouts

* System call numbers come from `syscall::nr`, and errno values from `errno`.
  Both are generated from the kernel's headers by `tools/gen-abi.py`. To use a
  new call, add its name to the script and run it. Never write a number by
  hand.
* A structure the kernel reads or writes, such as `struct stat`, the kernel's
  `sigaction` or `struct timespec`, is taken from its UAPI header under
  `/usr/include/asm*` or `/usr/include/linux`, not from memory. Assert its
  size.
* Return a system call's result through `errno::from_syscall`, or through
  `errno::decode` and `errno::set`.

## No panics

`unwrap`, `expect`, `panic!`, indexing and slicing are denied, and a panic
traps. Use `get`, `checked_*` and early returns, and report the failure the
way C expects: a return value and `errno`.

## Tests

* Pure logic is unit tested beside the code, in `#[cfg(test)] mod tests`.
* C programs live in `tests/c/<area>/<name>.c` and are run by
  `tests/c_<area>.rs` through `tests/common`. Each is built at `-O0` and `-O2`
  with `-std=c11 -Wall -Werror -fstack-protector-strong` against `include/`.
  It runs in a fresh directory with nothing inherited, and must finish within
  30 seconds.
* `#include "check.h"` provides `CHECK(expr)` and `t_status` for programs that
  should not depend on stdio.
* libc-test and musl (both MIT, in `~/.local/share/ferrix/ferrousli-ref`) are
  references. Their test cases may be adapted, with credit in the test. glibc
  is LGPL: consult it for its ABI, and never copy it.

## Before committing

```
python3 tools/gen-abi.py --check
cargo fmt --check
cargo clippy --all-targets    # no warnings
cargo test
cargo test --release
```
