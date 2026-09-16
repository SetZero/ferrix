# Linux UAPI headers for `gen-abi.py`

The three headers `tools/gen-abi.py` reads ferrousli's system call numbers and
errno values from, copied here so that every host generates and checks the same
numbers. Before they were copied the script read `/usr/include`, which made the
ferrousli gate Linux-only and made the numbers whatever the host's
`linux-libc-dev` happened to be.

| File | Copied from |
|---|---|
| `x86_64-linux-gnu/asm/unistd_64.h` | `/usr/include/x86_64-linux-gnu/asm/unistd_64.h` |
| `asm-generic/errno-base.h` | `/usr/include/asm-generic/errno-base.h` |
| `asm-generic/errno.h` | `/usr/include/asm-generic/errno.h` |

Source: Ubuntu 24.04's `linux-libc-dev` 6.8.0-138.138, on 2026-09-16. These
are the numbers the generated files already held: `gen-abi.py --check` passed
against that package's `/usr/include` before the copy and passes against the
copy after it.

sha256:

```text
e34e39ab6237ba98b63d03e677f80c780c7416b9374b747f46c474d1920809fe  x86_64-linux-gnu/asm/unistd_64.h
2c148e92b8318deeb767fabd60822113e575ee664ff09a1873aed8f7a495793c  asm-generic/errno-base.h
fb7b5a504015f3a9074c641e7371b250d867d751d90e4a22a8ac17fced3d50af  asm-generic/errno.h
```

## Licence

Linux's UAPI headers are `GPL-2.0 WITH Linux-syscall-note`: the note exempts
programs that use the kernel's interface through them from the GPL, which is
how every C library includes them. They are not linked into anything here; a
script reads numbers out of them.

## Refreshing

A system call newer than 6.8 needs newer headers. From a Linux host, or WSL,
with the `linux-libc-dev` wanted installed:

```sh
cp /usr/include/x86_64-linux-gnu/asm/unistd_64.h tools/linux-uapi/x86_64-linux-gnu/asm/
cp /usr/include/asm-generic/errno-base.h /usr/include/asm-generic/errno.h tools/linux-uapi/asm-generic/
python3 tools/gen-abi.py
```

then update the version and the hashes above in the same commit. A number that
changed for an existing call is a kernel ABI break, which does not happen; if
`git diff src/generated` shows one, the copy is wrong.
