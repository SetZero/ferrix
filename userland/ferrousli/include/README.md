# Headers

These are musl 1.2.5's headers, as its `make install-headers` generates them,
unmodified. musl is MIT licensed; its copyright notice is in
[COPYRIGHT.musl](COPYRIGHT.musl).

Most of musl's headers are the same on every architecture. The 22 in `bits/`
that are not have one copy per architecture, generated for each: in
`bits/x86_64/`, `bits/aarch64/` and `bits/arm/` (ARMv7-A, hard float). In
their place in `bits/` is a file of a few lines that includes the compiler's
architecture's copy, and stops with an error on any other. So one tree serves
all three targets, and a program sees exactly the headers musl would have
installed for its own.

Ferrousli implements the interface these headers declare. They describe the
API a C program is compiled against. Binary compatibility with glibc, meaning
the symbols and layouts a program built against glibc expects, is a separate
matter: it is held in the library, not here.

A header edited to match ferrousli rather than musl says so at the edit.
