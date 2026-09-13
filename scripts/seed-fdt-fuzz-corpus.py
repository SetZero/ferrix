#!/usr/bin/env python3
"""Write the seed corpus for the `fdt_parse` fuzz target.

Two kinds of seed:

* Device trees QEMU generates for the machines `xtask` boots, dumped with
  `-machine ...,dumpdtb=` and packed with `dtc` so the corpus holds the tree
  and not QEMU's megabyte of padding. Pass `--qemu` to (re)write these; it runs
  QEMU, so it is opt-in.
* Trees compiled with `dtc` from sources below, each holding a binding the
  crate decodes -- GICv2 and GICv3, the architected timer, PSCI, `/chosen`
  and `/aliases`, an ECAM host with an `iommu-map`, an SMMUv3, a GICv2m frame,
  `virtio,mmio`, `/cpus` with a failed processor and cache nodes, memory
  reservations -- and a few built byte by byte for shapes `dtc` will not
  write: NOP tokens, a property after a subnode, nesting at and past the depth
  limit, an unknown token and a tree that never ends.

Usage:
    python3 scripts/seed-fdt-fuzz-corpus.py [--qemu]
"""

import pathlib
import shutil
import struct
import subprocess
import sys
import tempfile

OUT = pathlib.Path(__file__).resolve().parent.parent / "fuzz" / "corpus" / "fdt_parse"

GICV2_VIRT = """/dts-v1/;
/memreserve/ 0x40000000 0x100000;
/ {
    #address-cells = <2>;
    #size-cells = <2>;
    compatible = "linux,dummy-virt";
    model = "ferrix seed: gicv2 virt";
    interrupt-parent = <&gic>;

    chosen {
        bootargs = "console=ttyAMA0";
        stdout-path = "/pl011@9000000";
    };
    aliases {
        serial0 = "/pl011@9000000";
    };
    memory@40000000 {
        device_type = "memory";
        reg = <0x0 0x40000000 0x0 0x20000000>;
    };
    psci {
        compatible = "arm,psci-1.0", "arm,psci-0.2", "arm,psci";
        method = "hvc";
    };
    cpus {
        #address-cells = <1>;
        #size-cells = <0>;
        cpu@0 {
            device_type = "cpu";
            compatible = "arm,cortex-a15";
            reg = <0x0>;
            enable-method = "psci";
        };
        cpu@1 {
            device_type = "cpu";
            compatible = "arm,cortex-a15";
            reg = <0x1>;
            enable-method = "psci";
            status = "disabled";
        };
    };
    gic: intc@8000000 {
        compatible = "arm,cortex-a15-gic";
        #interrupt-cells = <3>;
        interrupt-controller;
        reg = <0x0 0x8000000 0x0 0x10000>, <0x0 0x8010000 0x0 0x10000>;
        phandle = <0x8001>;
        #address-cells = <2>;
        #size-cells = <2>;
        ranges;
        v2m@8020000 {
            compatible = "arm,gic-v2m-frame";
            msi-controller;
            reg = <0x0 0x8020000 0x0 0x1000>;
            arm,msi-base-spi = <64>;
            arm,msi-num-spis = <64>;
        };
    };
    timer {
        compatible = "arm,armv7-timer";
        interrupts = <1 13 0xf08>, <1 14 0xf08>, <1 11 0xf08>, <1 10 0xf08>;
        always-on;
    };
    pl011@9000000 {
        compatible = "arm,pl011", "arm,primecell";
        reg = <0x0 0x9000000 0x0 0x1000>;
        interrupts = <0 1 4>;
        clock-names = "uartclk", "apb_pclk";
    };
    virtio_mmio@a000000 {
        compatible = "virtio,mmio";
        reg = <0x0 0xa000000 0x0 0x200>;
        interrupts = <0 16 1>;
        dma-coherent;
    };
    virtio_mmio@a000200 {
        compatible = "virtio,mmio";
        reg = <0x0 0xa000200 0x0 0x200>;
        interrupts = <0 17 1>;
        status = "disabled";
    };
    smmu: iommu@9050000 {
        compatible = "arm,smmu-v3";
        reg = <0x0 0x9050000 0x0 0x20000>;
        interrupts = <0 74 1>, <0 75 1>, <0 76 1>, <0 77 1>;
        interrupt-names = "eventq", "priq", "cmdq-sync", "gerror";
        #iommu-cells = <1>;
        phandle = <0x8002>;
    };
    pcie@10000000 {
        compatible = "pci-host-ecam-generic";
        device_type = "pci";
        reg = <0x40 0x10000000 0x0 0x10000000>;
        bus-range = <0x0 0xff>;
        linux,pci-domain = <0>;
        #address-cells = <3>;
        #size-cells = <2>;
        ranges = <0x1000000 0x0 0x0 0x0 0x3eff0000 0x0 0x10000>,
                 <0x2000000 0x0 0x10000000 0x0 0x10000000 0x0 0x2eff0000>;
        iommu-map = <0x0 &smmu 0x0 0x10000>;
        iommu-map-mask = <0xfff8>;
        msi-map = <0x0 &gic 0x0 0x10000>;
    };
};
"""

GICV3_AARCH64 = """/dts-v1/;
/memreserve/ 0x48000000 0x1000;
/memreserve/ 0x0 0x0;
/ {
    #address-cells = <2>;
    #size-cells = <2>;
    model = "ferrix seed: gicv3 aarch64";

    chosen {
        stdout-path = "serial0:115200n8";
        linux,stdout-path = "/uart@9000000";
    };
    aliases {
        serial0 = "/uart@9000000";
    };
    memory {
        reg = <0x0 0x40000000 0x0 0x40000000>, <0x1 0x0 0x0 0x40000000>;
    };
    firmware {
        psci {
            compatible = "arm,psci-0.2";
            method = "smc";
        };
    };
    cpus {
        #address-cells = <2>;
        #size-cells = <0>;
        cpu-map {
            cluster0 {
                core0 { cpu = <&cpu0>; };
                core1 { cpu = <&cpu1>; };
            };
        };
        idle-states {
            entry-method = "psci";
        };
        cpu0: cpu@0 {
            device_type = "cpu";
            reg = <0x0 0x0>;
            enable-method = "psci";
            l2-cache {
                compatible = "cache";
                cache-level = <2>;
            };
        };
        cpu1: cpu@100 {
            device_type = "cpu";
            reg = <0x0 0x100>;
            enable-method = "psci";
        };
        cpu@200 {
            device_type = "cpu";
            reg = <0x0 0x200>;
            status = "fail";
        };
    };
    interrupt-controller@8000000 {
        compatible = "arm,gic-v3";
        #interrupt-cells = <3>;
        interrupt-controller;
        reg = <0x0 0x8000000 0x0 0x10000>, <0x0 0x80a0000 0x0 0xf60000>;
        linux,phandle = <0x1>;
    };
    timer {
        compatible = "arm,armv8-timer";
        interrupts = <1 13 4>, <1 14 4>, <1 11 4>, <1 10 4>;
    };
    uart@9000000 {
        compatible = "arm,pl011";
        reg = <0x0 0x9000000 0x0 0x1000>;
        interrupts = <0 1 4>;
    };
    pcie@3f000000 {
        compatible = "pci-host-ecam-generic";
        reg = <0x0 0x3f000000 0x0 0x1000000>;
        bus-range = <0x10 0x1f>;
    };
};
"""

# Cells wider than a u64, a zero size-cells, and a string list with an empty
# string in it.
ODD_CELLS = """/dts-v1/;
/ {
    #address-cells = <3>;
    #size-cells = <0>;
    compatible = "", "ferrix,odd", "";
    wide@1 {
        reg = <0x1 0x2 0x3>, <0x0 0x0 0x4>;
        #address-cells = <5>;
        #size-cells = <1>;
        child@0 {
            reg = <0 0 0 0 1 2>;
        };
    };
    memory@0 {
        device_type = "memory";
        reg = <0x0 0x0 0x80000000>;
    };
};
"""


class Tree:
    """A device tree written token by token, for shapes dtc will not emit."""

    def __init__(self):
        self.structs = bytearray()
        self.strings = bytearray()
        self.offsets = {}

    def token(self, value):
        self.structs += struct.pack(">I", value)
        return self

    def begin(self, name):
        self.token(1)
        self.structs += name.encode() + b"\0"
        self.pad()
        return self

    def end(self):
        return self.token(2)

    def nop(self):
        return self.token(4)

    def prop(self, name, value):
        if name not in self.offsets:
            self.offsets[name] = len(self.strings)
            self.strings += name.encode() + b"\0"
        self.token(3)
        self.structs += struct.pack(">II", len(value), self.offsets[name])
        self.structs += value
        self.pad()
        return self

    def pad(self):
        while len(self.structs) % 4:
            self.structs.append(0)

    def blob(self, finish=True, reservations=()):
        if finish:
            self.token(9)
        rsvmap = b"".join(struct.pack(">QQ", *pair) for pair in reservations)
        rsvmap += bytes(16)
        off_rsvmap = 40
        off_structs = off_rsvmap + len(rsvmap)
        off_strings = off_structs + len(self.structs)
        total = off_strings + len(self.strings)
        header = struct.pack(
            ">10I", 0xD00DFEED, total, off_structs, off_strings, off_rsvmap,
            17, 16, 0, len(self.strings), len(self.structs),
        )
        return header[:40] + rsvmap + bytes(self.structs) + bytes(self.strings)


def u32(value):
    return struct.pack(">I", value)


def dtc(source):
    """Compile a DTS source to a blob."""
    return subprocess.run(
        ["dtc", "-q", "-I", "dts", "-O", "dtb", "-o", "-", "-"],
        input=source.encode(), capture_output=True, check=True,
    ).stdout


def qemu_trees():
    """The trees QEMU builds for the machines xtask boots, packed."""
    machines = {
        "qemu-virt-armv7a": ["qemu-system-arm", "-machine", "virt", "-cpu", "cortex-a15", "-smp", "2"],
        "qemu-virt-aarch64": ["qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a72", "-smp", "4"],
        "qemu-virt-aarch64-gicv3-smmuv3": [
            "qemu-system-aarch64", "-machine", "virt,gic-version=3,iommu=smmuv3",
            "-cpu", "cortex-a72", "-smp", "2",
        ],
    }
    with tempfile.TemporaryDirectory() as scratch:
        for name, command in machines.items():
            raw = pathlib.Path(scratch) / f"{name}.dtb"
            machine = command.index("-machine") + 1
            command = list(command)
            command[machine] += f",dumpdtb={raw}"
            subprocess.run(command + ["-m", "512", "-display", "none", "-nodefaults"],
                           check=True, capture_output=True)
            packed = subprocess.run(
                ["dtc", "-q", "-I", "dtb", "-O", "dtb", "-o", "-", str(raw)],
                capture_output=True, check=True,
            ).stdout
            yield name, packed


def write(name, data):
    (OUT / name).write_bytes(data)
    print(f"  {name}: {len(data)} bytes")


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    if shutil.which("dtc") is None:
        sys.exit("dtc is needed: the seeds are compiled device tree sources")

    if "--qemu" in sys.argv[1:]:
        for name, blob in qemu_trees():
            write(name, blob)

    write("gicv2-virt", dtc(GICV2_VIRT))
    write("gicv3-aarch64", dtc(GICV3_AARCH64))
    write("odd-cells", dtc(ODD_CELLS))

    # NOPs everywhere a token may be, and a property after a subnode.
    shapes = Tree().nop().begin("").nop().prop("#address-cells", u32(1)).nop()
    shapes.prop("#size-cells", u32(1)).begin("memory@0").prop("device_type", b"memory\0")
    shapes.prop("reg", u32(0) + u32(0x1000)).end().nop()
    shapes.prop("late", b"after a subnode\0").end().nop()
    write("nops-and-a-late-property", shapes.blob(reservations=[(0x1000, 0x1000)]))

    # Nesting exactly at the limit, and one past it.
    for depth, name in ((64, "depth-64"), (65, "depth-65")):
        deep = Tree()
        for level in range(depth):
            deep.begin("" if level == 0 else "n")
        for _ in range(depth):
            deep.end()
        write(name, deep.blob())

    write("unknown-token", Tree().begin("").token(7).end().blob())
    write("no-end-token", Tree().begin("").end().blob(finish=False))
    whole = dtc(GICV2_VIRT)
    write("truncated", whole[: len(whole) // 2])


if __name__ == "__main__":
    main()
