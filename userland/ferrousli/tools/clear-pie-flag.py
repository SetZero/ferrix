#!/usr/bin/env python3
"""Clear DF_1_PIE in an ELF object's DT_FLAGS_1, in place.

build-shared.sh runs it on ferrousli's loader. The loader is linked as the
position-independent executable its target makes, and the link marks it so;
glibc's ld-linux is a shared object and is not marked. Nothing reads the flag
at run time -- the kernel starts either kind -- but GNU ld refuses an object
marked PIE as input, and glibc's libc.so linker script names the loader, so
gcc could not link a C program on ferrousli. Linking the loader with -shared
instead would make every symbol it exports preemptible, which it cannot
resolve before it has relocated itself.

Little-endian ELF32 and ELF64. Exits 1 if the object has no DT_FLAGS_1.
"""

import struct
import sys

PT_DYNAMIC = 2
DT_NULL = 0
DT_FLAGS_1 = 0x6FFFFFFB
DF_1_PIE = 0x08000000


def main(path):
    with open(path, "r+b") as f:
        data = bytearray(f.read())
        if data[:4] != b"\x7fELF" or data[5] != 1:
            sys.exit(f"{path}: not a little-endian ELF object")
        wide = data[4] == 2
        if wide:
            phoff, = struct.unpack_from("<Q", data, 0x20)
            phentsize, phnum = struct.unpack_from("<HH", data, 0x36)
        else:
            phoff, = struct.unpack_from("<I", data, 0x1C)
            phentsize, phnum = struct.unpack_from("<HH", data, 0x2A)
        dyn_at = dyn_size = None
        for index in range(phnum):
            at = phoff + index * phentsize
            p_type, = struct.unpack_from("<I", data, at)
            if p_type != PT_DYNAMIC:
                continue
            if wide:
                dyn_at, = struct.unpack_from("<Q", data, at + 8)
                dyn_size, = struct.unpack_from("<Q", data, at + 32)
            else:
                dyn_at, = struct.unpack_from("<I", data, at + 4)
                dyn_size, = struct.unpack_from("<I", data, at + 16)
        if dyn_at is None:
            sys.exit(f"{path}: no PT_DYNAMIC")
        entry = struct.Struct("<qQ" if wide else "<iI")
        for at in range(dyn_at, dyn_at + dyn_size, entry.size):
            tag, value = entry.unpack_from(data, at)
            if tag == DT_NULL:
                break
            if tag == DT_FLAGS_1:
                entry.pack_into(data, at, tag, value & ~DF_1_PIE)
                f.seek(0)
                f.write(data)
                return
        sys.exit(f"{path}: no DT_FLAGS_1")


if __name__ == "__main__":
    main(sys.argv[1])
