"""Generate the committed fuzz corpus for the ELF reader.

Seeds are synthesised rather than copied from a build output: they are a few
hundred bytes each instead of a megabyte, they are reproducible, and each one
is a *shape* the parser has to get right rather than an arbitrary binary that
happens to exercise some of them.
"""

import pathlib
import struct

OUT = pathlib.Path(__file__).resolve().parent.parent / "fuzz" / "corpus" / "elf_parse"
OUT.mkdir(parents=True, exist_ok=True)

EHDR_SIZE = 64
PHDR_SIZE = 56

ET_EXEC, ET_DYN = 2, 3
EM_X86_64, EM_AARCH64 = 62, 183
PT_LOAD, PT_DYNAMIC, PT_NOTE = 1, 2, 4
PF_X, PF_W, PF_R = 1, 2, 4
DT_NULL, DT_RELA, DT_RELASZ, DT_RELAENT = 0, 7, 8, 9
R_X86_64_RELATIVE = 8


def build(elf_type, machine, entry, segments):
    """segments: list of (kind, flags, vaddr, data, zero_fill)."""
    phnum = len(segments)
    image = bytearray(EHDR_SIZE + phnum * PHDR_SIZE)

    image[0:4] = b"\x7fELF"
    image[4] = 2  # ELFCLASS64
    image[5] = 1  # ELFDATA2LSB
    image[6] = 1  # EI_VERSION
    struct.pack_into("<HHI", image, 16, elf_type, machine, 1)
    struct.pack_into("<Q", image, 24, entry)
    struct.pack_into("<Q", image, 32, EHDR_SIZE)  # e_phoff
    struct.pack_into("<HHH", image, 52, EHDR_SIZE, PHDR_SIZE, phnum)

    for index, (kind, flags, vaddr, data, zero_fill) in enumerate(segments):
        offset = len(image)
        image += data
        base = EHDR_SIZE + index * PHDR_SIZE
        struct.pack_into("<II", image, base, kind, flags)
        struct.pack_into("<QQQ", image, base + 8, offset, vaddr, vaddr)
        struct.pack_into("<QQQ", image, base + 32, len(data), len(data) + zero_fill, 0x1000)

    return bytes(image)


def write(name, data):
    (OUT / name).write_bytes(data)
    print(f"  {name}: {len(data)} bytes")


# 1. The shape the loader meets every boot: read-execute text, read-write data
#    with a .bss tail, linked at the kernel's own base.
write(
    "kernel-shaped",
    build(
        ET_EXEC,
        EM_X86_64,
        0xFFFFFFFF80001000,
        [
            (PT_LOAD, PF_R | PF_X, 0xFFFFFFFF80000000, b"\x90" * 256, 0),
            (PT_LOAD, PF_R | PF_W, 0xFFFFFFFF80001000, b"\xAA" * 128, 0x880),
        ],
    ),
)

# 2. The other architecture, so the machine check is exercised both ways.
write(
    "aarch64",
    build(
        ET_EXEC,
        EM_AARCH64,
        0xFFFFFFFF80000000,
        [(PT_LOAD, PF_R | PF_X, 0xFFFFFFFF80000000, b"\x1f\x20\x03\xd5" * 64, 0)],
    ),
)

# 3. A static PIE with relative relocations -- the path through DT_RELA,
#    vaddr_to_bytes and the relocation iterator.
payload = bytearray()
for offset, addend in [(0x2000, 0x2100), (0x2008, 0x2200), (0x2010, 0x2300)]:
    payload += struct.pack("<QQq", offset, R_X86_64_RELATIVE, addend)
rela_size = len(payload)
dyn_offset = len(payload)
for tag, value in [
    (DT_RELA, 0x1000),
    (DT_RELASZ, rela_size),
    (DT_RELAENT, 24),
    (DT_NULL, 0),
]:
    payload += struct.pack("<qQ", tag, value)

pie = bytearray(
    build(
        ET_DYN,
        EM_X86_64,
        0x1000,
        [
            (PT_LOAD, PF_R | PF_W, 0x1000, bytes(payload), 0),
            (PT_DYNAMIC, PF_R, 0x1000 + dyn_offset, b"", 0),
        ],
    )
)
# Point the PT_DYNAMIC header at the dynamic table inside the PT_LOAD.
load_offset = struct.unpack_from("<Q", pie, EHDR_SIZE + 8)[0]
dynamic = EHDR_SIZE + PHDR_SIZE
struct.pack_into("<Q", pie, dynamic + 8, load_offset + dyn_offset)
struct.pack_into("<Q", pie, dynamic + 32, 4 * 16)
struct.pack_into("<Q", pie, dynamic + 40, 4 * 16)
write("static-pie", bytes(pie))

# 4. An image that loads nothing, which must not be mistaken for one that does.
write("no-load-segments", build(ET_EXEC, EM_X86_64, 0, [(PT_NOTE, PF_R, 0x1000, b"note", 0)]))

# 5. Degenerate inputs the parser must reject rather than trust.
write("empty", b"")
write("magic-only", b"\x7fELF")
write("truncated-header", build(ET_EXEC, EM_X86_64, 0x1000, [])[:40])

# 6. A header claiming far more program headers than the file can hold. This is
#    the bounds check in `Elf::parse` that everything else relies on.
lying = bytearray(build(ET_EXEC, EM_X86_64, 0x1000, [(PT_LOAD, PF_R, 0x1000, b"x" * 16, 0)]))
struct.pack_into("<H", lying, 56, 0xFFFF)
write("phnum-overflow", bytes(lying))

# 7. A segment whose file contents run past the end of the image.
oversize = bytearray(build(ET_EXEC, EM_X86_64, 0x1000, [(PT_LOAD, PF_R, 0x1000, b"x" * 16, 0)]))
struct.pack_into("<Q", oversize, EHDR_SIZE + 32, 0xFFFFFFFF)
struct.pack_into("<Q", oversize, EHDR_SIZE + 40, 0xFFFFFFFF)
write("segment-past-end", bytes(oversize))

# 8. A segment whose address range wraps the address space.
wrapping = bytearray(build(ET_EXEC, EM_X86_64, 0x1000, [(PT_LOAD, PF_R, 0x1000, b"x" * 16, 0)]))
struct.pack_into("<Q", wrapping, EHDR_SIZE + 16, 0xFFFFFFFFFFFFFFF0)
struct.pack_into("<Q", wrapping, EHDR_SIZE + 40, 0x1000)
write("segment-wraps", bytes(wrapping))

print(f"corpus written to {OUT}")  # committed; see docs/RELIABILITY.md
