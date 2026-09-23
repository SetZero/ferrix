#!/usr/bin/env python3
"""Move the pointer over a VNC server and say what a viewer was sent.

`hyprix` times its own frames, and that is half of what a person at a
viewer feels. The other half is everything after the frame: the flush to
the host, QEMU's encoder, the wire. This is a viewer that does nothing but
keep one update request outstanding, as TigerVNC does, while it moves the
pointer in a circle sixty times a second, and it reports

* how many updates arrived a second, and how many bytes they carried;
* how long after a pointer event the first update touching the pointer's
  new place arrived (median and 95th percentile), which is the pointer lag
  a person sees when the cursor is drawn into the frame;
* whether the server sent a cursor shape, which is the tell that the
  cursor is a plane the viewer draws itself -- then the lag above is not a
  lag anybody sees, because the viewer draws the arrow where the mouse is.

    cargo xtask run-compositor --accel kvm --vnc :18
    compositor/hyprix/probe/vncbench.py 127.0.0.1 5918 --seconds 20

`--encoding zrle` makes QEMU compress, as a real viewer's does, and costs
QEMU's encoder what a real viewer costs it; `raw` (the default) measures
the guest and the flush with the encoder out of the way. No Pillow needed.

usage: vncbench.py <host> <port> [--seconds N] [--encoding raw|zrle]
                   [--radius R] [--rate HZ]
"""
import argparse
import math
import select
import socket
import struct
import time

RAW = 0
ZRLE = 16
CURSOR = -239
DESKTOP_SIZE = -223
EXTENDED_DESKTOP_SIZE = -308


class Reader:
    """A socket read in whole messages, without blocking the pointer."""

    def __init__(self, sock):
        self.sock = sock
        self.buffer = bytearray()

    def fill(self, timeout):
        ready, _, _ = select.select([self.sock], [], [], timeout)
        if not ready:
            return False
        chunk = self.sock.recv(1 << 20)
        if not chunk:
            raise SystemExit("the server closed the connection")
        self.buffer += chunk
        return True

    def have(self, n):
        return len(self.buffer) >= n

    def take(self, n):
        data = bytes(self.buffer[:n])
        del self.buffer[:n]
        return data

    def need(self, n):
        while not self.have(n):
            self.fill(5)
        return self.take(n)


def handshake(sock, reader, encodings):
    version = reader.need(12)
    if not version.startswith(b"RFB "):
        raise SystemExit(f"not a VNC server: {version!r}")
    sock.sendall(b"RFB 003.008\n")
    count = reader.need(1)[0]
    if count == 0:
        length = struct.unpack(">I", reader.need(4))[0]
        raise SystemExit(f"refused: {reader.need(length)!r}")
    kinds = reader.need(count)
    if 1 not in kinds:
        raise SystemExit(f"the server wants authentication: {list(kinds)}")
    sock.sendall(bytes([1]))
    if struct.unpack(">I", reader.need(4))[0] != 0:
        raise SystemExit("the server refused the connection")
    sock.sendall(bytes([1]))
    width, height = struct.unpack(">HH", reader.need(4))
    reader.need(16)
    name = struct.unpack(">I", reader.need(4))[0]
    reader.need(name)
    sock.sendall(
        struct.pack(
            ">BBBBBBBBHHHBBBBBB",
            0, 0, 0, 0, 32, 24, 0, 1, 255, 255, 255, 16, 8, 0, 0, 0, 0,
        )
    )
    sock.sendall(struct.pack(f">BBH{len(encodings)}i", 2, 0, len(encodings), *encodings))
    return width, height


def request(sock, width, height, incremental):
    sock.sendall(struct.pack(">BBHHHH", 3, incremental, 0, 0, width, height))


def read_update(reader, width, height):
    """One FramebufferUpdate: its rectangles, bytes, and any cursor shape."""
    reader.need(1)
    count = struct.unpack(">H", reader.need(2))[0]
    rects, carried, cursor = [], 4, False
    for _ in range(count):
        x, y, w, h, encoding = struct.unpack(">HHHHi", reader.need(12))
        carried += 12
        if encoding == RAW:
            reader.need(w * h * 4)
            carried += w * h * 4
            rects.append((x, y, w, h))
        elif encoding == ZRLE:
            length = struct.unpack(">I", reader.need(4))[0]
            reader.need(length)
            carried += 4 + length
            rects.append((x, y, w, h))
        elif encoding == CURSOR:
            size = w * h * 4 + (w + 7) // 8 * h
            reader.need(size)
            carried += size
            cursor = True
        elif encoding == DESKTOP_SIZE:
            width, height = w, h
        elif encoding == EXTENDED_DESKTOP_SIZE:
            screens = reader.need(4)[0]
            reader.need(16 * screens)
            width, height = w, h
        else:
            raise SystemExit(f"an encoding this does not read: {encoding}")
    return rects, carried, cursor, width, height


def skip_other(reader, kind):
    if kind == 1:  # SetColourMapEntries
        _, _, n = struct.unpack(">BHH", reader.need(5))
        reader.need(6 * n)
    elif kind == 2:  # Bell
        pass
    elif kind == 3:  # ServerCutText
        reader.need(3)
        reader.need(struct.unpack(">I", reader.need(4))[0])
    else:
        raise SystemExit(f"a server message this does not read: {kind}")


def covers(rect, point):
    x, y, w, h = rect
    return x <= point[0] < x + w and y <= point[1] < y + h


def percentile(values, fraction):
    if not values:
        return float("nan")
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(fraction * len(ordered)))]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("host")
    parser.add_argument("port", type=int)
    parser.add_argument("--seconds", type=float, default=20)
    parser.add_argument("--encoding", choices=["raw", "zrle"], default="raw")
    parser.add_argument("--radius", type=int, default=200)
    parser.add_argument("--rate", type=float, default=60)
    args = parser.parse_args()

    first = ZRLE if args.encoding == "zrle" else RAW
    encodings = [first, RAW, CURSOR, DESKTOP_SIZE, EXTENDED_DESKTOP_SIZE]
    sock = socket.create_connection((args.host, args.port), timeout=20)
    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    reader = Reader(sock)
    width, height = handshake(sock, reader, encodings)

    # The whole screen once, which is not measured: a viewer's first update
    # is every pixel, and it says nothing about a desktop in use.
    request(sock, width, height, 0)
    while True:
        kind = reader.need(1)[0]
        if kind == 0:
            _, _, _, width, height = read_update(reader, width, height)
            break
        skip_other(reader, kind)

    centre = (width // 2, height // 2)
    period = 1.0 / args.rate
    pending = []  # (sent_at, point) pointer events not yet seen drawn
    lags, updates, carried, shapes = [], 0, 0, 0
    begun = time.monotonic()
    next_move, step = begun, 0
    request(sock, width, height, 1)
    while (now := time.monotonic()) - begun < args.seconds:
        if now >= next_move:
            angle = step * 2 * math.pi / args.rate
            point = (
                int(centre[0] + args.radius * math.cos(angle)),
                int(centre[1] + args.radius * math.sin(angle)),
            )
            sock.sendall(struct.pack(">BBHH", 5, 0, point[0], point[1]))
            pending.append((now, point))
            step += 1
            next_move += period
        # Whatever has arrived, but never so long that a move is late.
        if not reader.have(1) and not reader.fill(max(0.0, next_move - time.monotonic())):
            continue
        while reader.have(1):
            kind = reader.buffer[0]
            if kind != 0:
                reader.take(1)
                skip_other(reader, kind)
                continue
            reader.take(1)
            rects, size, cursor, width, height = read_update(reader, width, height)
            arrived = time.monotonic()
            updates += 1
            carried += size
            shapes += cursor
            # A move is drawn once an update touches where the pointer went.
            # Any older move not yet seen is superseded by a newer one that
            # was, and is not counted: the pointer is where the last one put it.
            seen = None
            for index, (_, point) in enumerate(pending):
                if any(covers(rect, point) for rect in rects):
                    seen = index
            if seen is not None:
                lags.append(arrived - pending[seen][0])
                del pending[: seen + 1]
            request(sock, width, height, 1)

    spent = time.monotonic() - begun
    ms = [lag * 1000 for lag in lags]
    print(
        f"vncbench: {args.encoding} {width}x{height} {spent:.1f} s: "
        f"{updates / spent:.1f} updates/s, {carried / spent / 1e6:.1f} MB/s, "
        f"pointer seen {len(lags)} times of {step} moves, lag median "
        f"{percentile(ms, 0.5):.1f} ms p95 {percentile(ms, 0.95):.1f} ms, "
        f"cursor shapes {shapes}"
    )


main()
