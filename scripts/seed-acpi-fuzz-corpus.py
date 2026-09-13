#!/usr/bin/env python3
"""Write the seed corpus for the `acpi_tables` fuzz target.

The target reads its input as physical memory, an address being an offset
into it. So there are two kinds of seed:

* Whole machines: an RSDP at address zero, a root table, and every table it
  lists at the addresses it lists. One is shaped like QEMU's q35 with
  intel-iommu (XSDT, FADT, MADT, HPET, MCFG, DMAR), one like QEMU's AArch64
  `virt` with an SMMUv3 (XSDT, FADT, MADT with GIC structures, GTDT, MCFG,
  IORT), and one an ACPI 1.0 machine with a revision-0 RSDP and an RSDT.
* Single tables at address zero, and tables with the faults firmware ships:
  a MADT entry of length zero, a table whose length runs past memory, a bad
  checksum, a MADT entry cut short, a DMAR structure of length zero and an
  IORT whose node count lies.

The values -- the local APIC at 0xfee00000, the HPET at 0xfed00000, the ECAM
window at 0xb0000000, the VT-d unit at 0xfed90000, the GIC and SMMU addresses
of `virt` -- are the ones QEMU uses, so the tables read like the ones the
kernel meets under `cargo xtask test-boot`. Checksums are set as firmware
sets them.

Usage:
    python3 scripts/seed-acpi-fuzz-corpus.py
"""

import pathlib
import struct

OUT = pathlib.Path(__file__).resolve().parent.parent / "fuzz" / "corpus" / "acpi_tables"


def checksum_fix(table, at):
    table[at] = (-sum(table)) & 0xFF


def sdt(signature, body, revision=1):
    """A system description table: the 36-byte header, then `body`."""
    table = bytearray(36) + body
    table[0:4] = signature
    struct.pack_into("<IBB6s8sIII", table, 4, len(table), revision, 0,
                     b"FERRIX", b"SEEDTBL ", 1, 0x4C544E49, 1)
    checksum_fix(table, 9)
    return table


def rsdp(revision, rsdt=0, xsdt=0):
    table = bytearray(36 if revision >= 2 else 20)
    table[0:8] = b"RSD PTR "
    struct.pack_into("<6sBI", table, 9, b"FERRIX", revision, rsdt)
    checksum_fix(table, 8)
    if revision >= 2:
        struct.pack_into("<IQ", table, 20, 36, xsdt)
        checksum_fix(table, 32)
    return table


def generic_address(space, width, address):
    return struct.pack("<BBBBQ", space, width, 0, 3, address)


# ---------------------------------------------------------------------------
# Tables, shaped like QEMU's
# ---------------------------------------------------------------------------

def madt_x86():
    entries = b""
    for cpu in range(2):
        entries += struct.pack("<BBBBI", 0, 8, cpu, cpu, 1)
    entries += struct.pack("<BBBBII", 1, 12, 0, 0, 0xFEC00000, 0)
    entries += struct.pack("<BBBBIH", 2, 10, 0, 0, 2, 0)
    for irq in (5, 9, 10, 11):
        entries += struct.pack("<BBBBIH", 2, 10, 0, irq, irq, 0x000D)
    entries += struct.pack("<BBBHB", 4, 6, 0xFF, 0, 1)
    entries += struct.pack("<BBHQ", 5, 12, 0, 0xFEE00000)
    entries += struct.pack("<BBHIII", 9, 16, 0, 0x100, 1, 0x100)
    return sdt(b"APIC", struct.pack("<II", 0xFEE00000, 1) + entries, revision=3)


def madt_arm():
    entries = b""
    for cpu in range(2):
        gicc = struct.pack(
            "<BBHIIIIIQQQQIQQ", 11, 76 if cpu == 0 else 80, 0, cpu, cpu, 1, 0, 23,
            0, 0x08010000, 0x08040000, 0x08030000, 25, 0, cpu,
        )
        if cpu == 1:
            gicc += bytes([0, 0, 0, 0])
        entries += gicc
    entries += struct.pack("<BBHIQIB3s", 12, 24, 0, 0, 0x08000000, 0, 2, b"\0\0\0")
    entries += struct.pack("<BBHIQIHH", 13, 24, 0, 0, 0x08020000, 1, 64, 80)
    entries += struct.pack("<BBHQI", 14, 16, 0, 0x080A0000, 0xF60000)
    return sdt(b"APIC", struct.pack("<II", 0, 0) + entries, revision=4)


def fadt(arm=False):
    body = bytearray(276 - 36)

    def put(offset, fmt, *values):
        struct.pack_into(fmt, body, offset - 36, *values)

    put(36, "<I", 0)           # FIRMWARE_CTRL
    put(40, "<I", 0x7FFE0000)  # DSDT
    put(76, "<I", 0x608)       # PM_TMR_BLK
    put(91, "<B", 4)           # PM_TMR_LEN
    put(108, "<B", 0x32)       # CENTURY
    put(109, "<H", 0 if arm else 0x0002)
    put(112, "<I", 0x000084A5 | (1 << 20 if arm else 0))
    put(129, "<H", 0x0003 if arm else 0)
    put(140, "<Q", 0x7FFE0000)  # X_DSDT
    body[208 - 36:220 - 36] = generic_address(1, 32, 0x608)
    return sdt(b"FACP", bytes(body), revision=6)


def hpet():
    body = struct.pack("<I", 0x8086A201) + generic_address(0, 0, 0xFED00000)
    body += struct.pack("<BHB", 0, 0x80, 0)
    return sdt(b"HPET", body)


def mcfg(base):
    return sdt(b"MCFG", bytes(8) + struct.pack("<QHBB4s", base, 0, 0, 0xFF, b"\0\0\0\0"))


def dmar():
    scope = lambda bus, device, function: struct.pack("<BBHBBBB", 1, 8, 0, 0, bus, device, function)
    bridge = struct.pack("<BBHBBBBBB", 2, 10, 0, 0, 0, 0x1C, 0, 0x00, 0)
    scopes = scope(0, 3, 0) + scope(0, 4, 0) + bridge
    drhd = struct.pack("<HHBBHQ", 0, 16 + len(scopes), 0, 0, 0, 0xFED90000) + scopes
    usb = scope(0, 0x1D, 0)
    rmrr = struct.pack("<HHHHQQ", 1, 24 + len(usb), 0, 0, 0x7F000000, 0x7F0FFFFF) + usb
    atsr = struct.pack("<HHBBH", 2, 8, 0, 0, 0)
    return sdt(b"DMAR", struct.pack("<BB10s", 38, 1, bytes(10)) + drhd + rmrr + atsr)


def iort():
    header_len = 48
    its = struct.pack("<BHBIIII", 0, 24, 1, 0, 0, 0, 1) + struct.pack("<I", 0)
    smmu_offset = header_len + len(its)
    smmu_mapping = struct.pack("<IIIII", 0, 0xFFFF, 0, header_len, 0)
    smmu = struct.pack(
        "<BHBIIIQIIQIIIII", 4, 68 + len(smmu_mapping), 4, 1, 1, 68,
        0x09050000, 1, 0, 0, 0, 74, 75, 77, 76,
    ) + struct.pack("<II", 0, 0) + smmu_mapping
    rc_mappings = struct.pack("<IIIII", 0, 0xFFFF, 0, smmu_offset, 0)
    rc_mappings += struct.pack("<IIIII", 0x10000, 0, 0x5, smmu_offset, 1)
    rc = struct.pack("<BHBIIIQIIB3s", 2, 36 + len(rc_mappings), 4, 2, 2, 36,
                     0, 1, 0, 48, bytes(3)) + rc_mappings
    body = struct.pack("<III", 3, header_len, 0) + its + smmu + rc
    return sdt(b"IORT", body, revision=5)


def gtdt():
    body = struct.pack("<QI", 0xFFFFFFFFFFFFFFFF, 0)
    for gsiv in (29, 30, 27, 26):
        body += struct.pack("<II", gsiv, 4)
    body += struct.pack("<QIIII", 0xFFFFFFFFFFFFFFFF, 0, 0, 28, 4)
    return sdt(b"GTDT", body, revision=2)


# ---------------------------------------------------------------------------
# Machines
# ---------------------------------------------------------------------------

def machine(tables, legacy=False):
    """An RSDP at zero, a root table after it, then the tables, each at a
    four-byte boundary as firmware would place them."""
    root_at = 36 if not legacy else 20
    width = 4 if legacy else 8
    root_len = 36 + width * len(tables)
    at = root_at + root_len
    addresses, image_tail = [], bytearray()
    for table in tables:
        while (at + len(image_tail)) % 4:
            image_tail.append(0)
        addresses.append(at + len(image_tail))
        image_tail += table
    fmt = "<" + ("I" if legacy else "Q") * len(tables)
    root = sdt(b"RSDT" if legacy else b"XSDT", struct.pack(fmt, *addresses))
    pointer = rsdp(0, rsdt=root_at) if legacy else rsdp(2, xsdt=root_at)
    return bytes(pointer + root + image_tail)


def write(name, data):
    (OUT / name).write_bytes(data)
    print(f"  {name}: {len(data)} bytes")


def main():
    OUT.mkdir(parents=True, exist_ok=True)

    write("q35-intel-iommu", machine([fadt(), madt_x86(), hpet(), mcfg(0xB0000000), dmar()]))
    write("virt-aarch64-smmuv3",
          machine([fadt(arm=True), madt_arm(), gtdt(), mcfg(0x4010000000), iort()]))
    write("acpi-1.0-rsdt", machine([fadt(), madt_x86()], legacy=True))

    for name, table in [
        ("madt-x86", madt_x86()), ("madt-gic", madt_arm()), ("fadt", fadt()),
        ("hpet", hpet()), ("mcfg", mcfg(0xB0000000)), ("dmar", dmar()),
        ("iort", iort()), ("gtdt", gtdt()),
    ]:
        write(name, bytes(table))

    zero = bytearray(madt_x86())
    zero[44 + 9] = 0
    checksum_fix(zero, 9)
    write("madt-zero-length-entry", bytes(zero))

    cut = bytearray(madt_x86())
    cut[44 + 1] = 200
    write("madt-entry-past-end", bytes(cut))

    long = bytearray(hpet())
    struct.pack_into("<I", long, 4, 0x10000)
    write("length-past-memory", bytes(long))

    bad = bytearray(mcfg(0xB0000000))
    bad[9] ^= 0x55
    write("bad-checksum", bytes(bad))

    empty_structure = bytearray(dmar())
    struct.pack_into("<H", empty_structure, 48 + 2, 0)
    write("dmar-zero-length-structure", bytes(empty_structure))

    lying = bytearray(iort())
    struct.pack_into("<I", lying, 36, 0xFFFFFFFF)
    write("iort-node-count-lies", bytes(lying))

    broken_pointer = bytearray(machine([madt_x86()]))
    struct.pack_into("<Q", broken_pointer, 24, 0xFFFFFFFFFFFFFFF0)
    write("rsdp-xsdt-out-of-memory", bytes(broken_pointer))


if __name__ == "__main__":
    main()
