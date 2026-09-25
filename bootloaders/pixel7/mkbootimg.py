#!/usr/bin/env python3
"""Wrap the Pixel 7 loader in an Android boot image ABL will boot.

The Pixel 7 launched on Android 13 and its ABL takes a version 4 boot image:
a 4 KiB header page, then the kernel, then the ramdisk, each padded to 4 KiB.
The generic ramdisk lives in `init_boot` on this phone, so the image carries
none; ABL adds the vendor ramdisk from `vendor_boot` and passes the device
tree, and the loader ignores both except for the tree. The header is
`struct boot_img_hdr_v4` from AOSP's `system/tools/mkbootimg`.

Usage:
    python3 bootloaders/pixel7/mkbootimg.py <Image> <boot.img> [--cmdline TEXT]

Then, with the bootloader unlocked, `fastboot boot boot.img` runs it once
without writing anything to the phone.
"""

from __future__ import annotations

import argparse
import pathlib
import struct
import sys

MAGIC = b"ANDROID!"
PAGE = 4096
HEADER_VERSION = 4
# magic, kernel_size, ramdisk_size, os_version, header_size, reserved[4],
# header_version, cmdline[1536], signature_size.
HEADER = struct.Struct("<8sIIII16sI1536sI")
ARM64_IMAGE_MAGIC = b"ARM\x64"
# The legacy LZ4 frame the stock kernel is packed in, and ABL unpacks.
LZ4_LEGACY_MAGIC = b"\x02\x21\x4c\x18"


def pad(data: bytes) -> bytes:
    """Pad to the next page."""
    return data + b"\0" * (-len(data) % PAGE)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("image", type=pathlib.Path, help="the loader as an arm64 Image, raw or legacy LZ4")
    parser.add_argument("output", type=pathlib.Path, help="the boot image to write")
    parser.add_argument("--cmdline", default="", help="kernel command line (at most 1535 bytes)")
    arguments = parser.parse_args()

    kernel = arguments.image.read_bytes()
    if kernel[0x38:0x3C] != ARM64_IMAGE_MAGIC and kernel[:4] != LZ4_LEGACY_MAGIC:
        print(f"{arguments.image} is neither an arm64 Image nor legacy LZ4", file=sys.stderr)
        return 1
    cmdline = arguments.cmdline.encode()
    if len(cmdline) >= 1536:
        print("the command line does not fit the header", file=sys.stderr)
        return 1

    header = HEADER.pack(
        MAGIC,
        len(kernel),
        0,
        0,
        HEADER.size,
        b"\0" * 16,
        HEADER_VERSION,
        cmdline.ljust(1536, b"\0"),
        0,
    )
    arguments.output.write_bytes(pad(header) + pad(kernel))
    print(f"{arguments.output}: {len(kernel)} byte kernel, header v{HEADER_VERSION}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
