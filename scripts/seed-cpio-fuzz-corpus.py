#!/usr/bin/env python3
"""Write the seed corpus for the `cpio_parse` fuzz target.

Three kinds of seed, all small:

* The archive every boot image carries, byte for byte as
  `xtask/src/initramfs.rs` writes it without a program, and the same archive
  with a stand-in program and a few of its applet links, which is the shape
  `--init` produces. The kernel unpacks exactly these.
* Archives GNU cpio wrote from a small tree -- a directory, files, a symbolic
  link, a hard link, an empty file -- in both `newc` and `crc` form, so the
  corpus holds what a foreign producer emits and not only what ours does.
  Skipped, with a message, when `cpio` is not installed.
* Hand-made shapes the reader must refuse or bound: a missing trailer, data
  cut short, a name with an interior NUL, an absurd name size, lowercase hex,
  device nodes and garbage after the trailer.

Usage:
    python3 scripts/seed-cpio-fuzz-corpus.py
"""

import os
import pathlib
import shutil
import subprocess
import tempfile

OUT = pathlib.Path(__file__).resolve().parent.parent / "fuzz" / "corpus" / "cpio_parse"

# The constants xtask/src/initramfs.rs writes with.
FIXED_MTIME = 1_767_225_600
MARKER_PATH = "etc/ferrix/initramfs"
MARKER = b"unpacked by the kernel from a cpio archive the loader handed it\n"

S_IFIFO, S_IFCHR, S_IFDIR, S_IFBLK, S_IFREG, S_IFLNK = (
    0o010000, 0o020000, 0o040000, 0o060000, 0o100000, 0o120000,
)


class Newc:
    """The writer in xtask/src/initramfs.rs, field for field."""

    def __init__(self, magic=b"070701", digits="{:08X}"):
        self.bytes = bytearray()
        self.next_ino = 1
        self.magic = magic
        self.digits = digits

    def pad(self):
        while len(self.bytes) % 4:
            self.bytes.append(0)

    def entry(self, name, mode, ino, nlink, data=b"", rdev=(0, 0), check=0, namesize=None):
        name = name.encode() if isinstance(name, str) else name
        fields = [
            ino, mode, 0, 0, nlink, FIXED_MTIME, len(data), 0, 0, rdev[0], rdev[1],
            len(name) + 1 if namesize is None else namesize, check,
        ]
        self.bytes += self.magic
        for field in fields:
            self.bytes += self.digits.format(field).encode()
        self.bytes += name + b"\0"
        self.pad()
        self.bytes += data
        self.pad()

    def ino(self):
        self.next_ino += 1
        return self.next_ino - 1

    def directory(self, name, permissions):
        self.entry(name, S_IFDIR | permissions, self.ino(), 2)

    def file(self, name, permissions, data):
        self.entry(name, S_IFREG | permissions, self.ino(), 1, data)

    def symlink(self, name, target):
        self.entry(name, S_IFLNK | 0o777, self.ino(), 1, target.encode())

    def hard_linked(self, names, permissions, data):
        ino = self.ino()
        for at, name in enumerate(names):
            body = data if at + 1 == len(names) else b""
            self.entry(name, S_IFREG | permissions, ino, len(names), body)

    def finish(self):
        self.entry("TRAILER!!!", 0, 0, 1)
        return bytes(self.bytes)


def boot_archive(program=None, applets=()):
    """xtask's `build_with`: the archive the kernel's stage 8 check reads."""
    archive = Newc()
    archive.directory(".", 0o755)
    for name, permissions in [
        ("bin", 0o755), ("dev", 0o755), ("etc", 0o755),
        ("etc/ferrix", 0o755), ("proc", 0o555), ("tmp", 0o1777),
    ]:
        archive.directory(name, permissions)
    archive.file("etc/hostname", 0o644, b"ferrix\n")
    archive.hard_linked([MARKER_PATH, MARKER_PATH + ".link"], 0o644, MARKER)
    archive.symlink(MARKER_PATH + ".symlink", "initramfs")
    if program is not None:
        archive.file("etc/passwd", 0o644, b"root:x:0:0:root:/:/bin/sh\n")
        archive.file("etc/group", 0o644, b"root:x:0:\n")
        archive.file("bin/busybox", 0o755, program)
        for applet in applets:
            archive.symlink("bin/" + applet, "busybox")
    return archive.finish()


def gnu_cpio(format_name):
    """An archive GNU cpio writes from a small tree, or None without cpio."""
    if shutil.which("cpio") is None:
        return None
    with tempfile.TemporaryDirectory() as root:
        tree = pathlib.Path(root)
        (tree / "etc").mkdir()
        (tree / "etc" / "motd").write_bytes(b"hello from GNU cpio\n")
        (tree / "empty").write_bytes(b"")
        os.link(tree / "etc" / "motd", tree / "motd.link")
        os.symlink("etc/motd", tree / "motd.symlink")
        names = [".", "empty", "etc", "etc/motd", "motd.link", "motd.symlink"]
        for name in names:
            os.utime(tree / name, (FIXED_MTIME, FIXED_MTIME), follow_symlinks=False)
        result = subprocess.run(
            ["cpio", "--quiet", "-o", "-H", format_name, "--reproducible", "-R", "0:0"],
            cwd=tree,
            input="\n".join(names).encode() + b"\n",
            capture_output=True,
            check=True,
        )
        return result.stdout


def write(name, data):
    (OUT / name).write_bytes(data)
    print(f"  {name}: {len(data)} bytes")


def main():
    OUT.mkdir(parents=True, exist_ok=True)

    write("boot-initramfs", boot_archive())
    write(
        "boot-initramfs-with-program",
        boot_archive(b"\x7fELF" + bytes(60), ["sh", "ls", "cat", "[["]),
    )

    for format_name in ("newc", "crc"):
        archive = gnu_cpio(format_name)
        if archive is None:
            print(f"  gnu-cpio-{format_name}: skipped, no cpio on PATH")
        else:
            write(f"gnu-cpio-{format_name}", archive)

    # Device nodes and a FIFO, which only a root-run producer writes.
    nodes = Newc()
    nodes.directory("dev", 0o755)
    nodes.entry("dev/console", S_IFCHR | 0o600, nodes.ino(), 1, rdev=(5, 1))
    nodes.entry("dev/vda", S_IFBLK | 0o660, nodes.ino(), 1, rdev=(254, 0))
    nodes.entry("dev/initctl", S_IFIFO | 0o600, nodes.ino(), 1)
    write("device-nodes", nodes.finish())

    lower = Newc(magic=b"070702", digits="{:08x}")
    lower.file("lowercase", 0o644, b"abcdef")
    write("crc-lowercase-hex", lower.finish())

    whole = boot_archive()
    write("missing-trailer", whole[: whole.rfind(b"070701")])
    write("data-cut-short", whole[: whole.find(MARKER) + 10])
    write("garbage-after-trailer", whole + b"\xde\xad\xbe\xef" * 8)

    nul = Newc()
    nul.file(b"etc/pass\0wd", 0o644, b"x")
    write("interior-nul-name", nul.finish())

    huge = Newc()
    huge.entry("n", S_IFREG | 0o644, 1, 1, b"", namesize=0xFFFFFFFF)
    write("absurd-name-size", bytes(huge.bytes))

    write("bad-magic", b"070707" + whole[6:120])
    write("empty", b"")


if __name__ == "__main__":
    main()
