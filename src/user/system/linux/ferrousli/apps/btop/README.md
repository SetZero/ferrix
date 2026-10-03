# btop

[btop](https://github.com/aristocratos/btop) 1.4.7, the resource monitor,
as a static x86-64 program against ferrousli, Ferrix's C library, and the
C++ runtime ferrousli's libcxx port builds on it.

An app (`docs/APPS.md`) built by a script rather than cargo: `build.sh
<arch> <out>` downloads the pinned source, builds it with btop's own
Makefile using ferrousli's port toolkit, and writes `<out>/btop`. It needs a
Linux host with gcc, which on Windows is WSL's, and builds ferrousli's
libcxx port first when that is not installed.

A C++ build of minutes, so no image starts one: `cargo xtask build-apps
--arch x86_64` (or `test-apps`) builds the package, and `run` and
`run-compositor` carry the last one built, saying so when there is none.
Under `--everything` they build it when there is none.
