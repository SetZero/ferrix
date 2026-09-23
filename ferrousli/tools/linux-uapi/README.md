# Linux UAPI headers for `gen-abi.py`

The headers `tools/gen-abi.py` reads ferrousli's system call numbers and errno
values from, copied here so that every host generates and checks the same
numbers. Before they were copied the script read `/usr/include`, which made the
ferrousli gate Linux-only and made the numbers whatever the host's
`linux-libc-dev` happened to be.

| File | Copied from |
|---|---|
| `x86_64-linux-gnu/asm/unistd_64.h` | `/usr/include/x86_64-linux-gnu/asm/unistd_64.h` |
| `asm-generic/errno-base.h` | `/usr/include/asm-generic/errno-base.h` |
| `asm-generic/errno.h` | `/usr/include/asm-generic/errno.h` |
| `aarch64-linux-gnu/asm/unistd_64.h` | `linux-libc-dev-arm64-cross`: `/usr/aarch64-linux-gnu/include/asm/unistd_64.h` |
| `arm-linux-gnueabihf/asm/unistd.h` | `linux-libc-dev-armhf-cross`: `/usr/arm-linux-gnueabihf/include/asm/unistd.h` |
| `arm-linux-gnueabihf/asm/unistd-eabi.h` | `linux-libc-dev-armhf-cross`: `/usr/arm-linux-gnueabihf/include/asm/unistd-eabi.h` |

Source of the first three: Ubuntu 24.04's `linux-libc-dev` 6.8.0-138.138, on
2026-09-16. These are the numbers the generated files already held:
`gen-abi.py --check` passed against that package's `/usr/include` before the
copy and passes against the copy after it.

Source of the AArch64 and ARMv7-A tables: Ubuntu 26.04's cross packages
`linux-libc-dev-arm64-cross` and `linux-libc-dev-armhf-cross`
7.0.0-13.13cross1, on 2026-09-23. Error numbers are the same on all three
architectures, so the generic headers serve them all. ARMv7-A's `unistd.h`
includes `unistd-oabi.h` only for the old ABI, which is not copied.

sha256:

```text
e34e39ab6237ba98b63d03e677f80c780c7416b9374b747f46c474d1920809fe  x86_64-linux-gnu/asm/unistd_64.h
2c148e92b8318deeb767fabd60822113e575ee664ff09a1873aed8f7a495793c  asm-generic/errno-base.h
fb7b5a504015f3a9074c641e7371b250d867d751d90e4a22a8ac17fced3d50af  asm-generic/errno.h
a6277c8757890bd4fe8afaf76a8ea79274e7b0484ec147e717634c16fe54e1d9  aarch64-linux-gnu/asm/unistd_64.h
ba6e49c6c6e16c8f308cd57faa45b5fe7af91292bd32d05161ae4b6854a05b8d  arm-linux-gnueabihf/asm/unistd.h
249c96f0b831f7fe3fd01a288de8c4ff65739e82d355db2e79b30ad2d6052301  arm-linux-gnueabihf/asm/unistd-eabi.h
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

The Arm tables come from the cross packages the same way, from
`/usr/aarch64-linux-gnu/include/asm/` and `/usr/arm-linux-gnueabihf/include/asm/`.
Then update the version and the hashes above in the same commit. A number that
changed for an existing call is a kernel ABI break, which does not happen; if
`git diff src/generated` shows one, the copy is wrong.
