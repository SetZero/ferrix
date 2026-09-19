#!/usr/bin/env python3
"""Grab one frame from a VNC server as a PNG: what a person would see.

QEMU cannot `screendump` a GL console (`docs/GPU.md` §3.1), and a 3D card
has no other kind, so the boot that draws on the GPU is judged from inside
the guest. That says whether the pixels are right; it does not let anybody
*look* at the screen -- and the first GPU scanout was upside down, which a
person sees in a second and a digest of the guest's own screenshot cannot
see at all.

A VNC server beside `egl-headless` is handed the same frames a viewer would
show, so this is the screen. Run the desktop and grab it:

    cargo xtask run-compositor --gl --accel kvm --vnc :18
    compositor/hyprix/probe/vncshot.py 127.0.0.1 5918 screen.png

Needs Pillow. Reads the whole framebuffer once, raw, with no authentication,
which is what `-vnc` without a password offers.

usage: vncshot.py <host> <port> <out.png>
"""
import socket
import struct
import sys

from PIL import Image


def readn(sock, n):
    data = b""
    while len(data) < n:
        chunk = sock.recv(n - len(data))
        if not chunk:
            raise SystemExit(f"the server closed after {len(data)} of {n} bytes")
        data += chunk
    return data


def main():
    host, port, out = sys.argv[1], int(sys.argv[2]), sys.argv[3]
    sock = socket.create_connection((host, port), timeout=20)
    version = readn(sock, 12)
    if not version.startswith(b"RFB "):
        raise SystemExit(f"not a VNC server: {version!r}")
    sock.sendall(b"RFB 003.008\n")
    count = readn(sock, 1)[0]
    if count == 0:
        reason_len = struct.unpack(">I", readn(sock, 4))[0]
        raise SystemExit(f"refused: {readn(sock, reason_len)!r}")
    kinds = readn(sock, count)
    if 1 not in kinds:
        raise SystemExit(f"the server wants authentication: {list(kinds)}")
    sock.sendall(bytes([1]))
    if struct.unpack(">I", readn(sock, 4))[0] != 0:
        raise SystemExit("the server refused the connection")
    sock.sendall(bytes([1]))  # shared
    width, height = struct.unpack(">HH", readn(sock, 4))
    _pixel_format = readn(sock, 16)
    name_len = struct.unpack(">I", readn(sock, 4))[0]
    readn(sock, name_len)

    # 32 bits a pixel, little-endian, red at 16: what the guest draws.
    sock.sendall(
        struct.pack(
            ">BBBBBBBBHHHBBBBBB",
            0, 0, 0, 0,          # SetPixelFormat and its three padding bytes
            32, 24, 0, 1,        # bpp, depth, big-endian, true-colour
            255, 255, 255,       # maxima
            16, 8, 0,            # shifts
            0, 0, 0,             # padding
        )
    )
    sock.sendall(struct.pack(">BBHi", 2, 0, 1, 0))  # SetEncodings: raw
    sock.sendall(struct.pack(">BBHHHH", 3, 0, 0, 0, width, height))

    image = Image.new("RGB", (width, height))
    seen = 0
    while seen < width * height:
        kind = readn(sock, 1)[0]
        if kind != 0:
            raise SystemExit(f"a message that is not an update: {kind}")
        readn(sock, 1)
        rects = struct.unpack(">H", readn(sock, 2))[0]
        if rects == 0:
            sock.sendall(struct.pack(">BBHHHH", 3, 0, 0, 0, width, height))
            continue
        for _ in range(rects):
            x, y, w, h, encoding = struct.unpack(">HHHHi", readn(sock, 12))
            if encoding != 0:
                raise SystemExit(f"an encoding this does not read: {encoding}")
            pixels = readn(sock, w * h * 4)
            part = Image.frombytes("RGBX", (w, h), pixels)
            image.paste(part, (x, y))
            seen += w * h
    image.save(out)
    print(f"{out}: {width}x{height}")


main()
