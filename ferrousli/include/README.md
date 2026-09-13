# Headers

These are musl 1.2.5's headers for x86-64, as its `make install-headers`
generates them, unmodified. musl is MIT licensed; its copyright notice is in
[COPYRIGHT.musl](COPYRIGHT.musl).

Ferrousli implements the interface these headers declare. They describe the
API a C program is compiled against. Binary compatibility with glibc, meaning
the symbols and layouts a program built against glibc expects, is a separate
matter: it is held in the library, not here.

A header edited to match ferrousli rather than musl says so at the edit.
