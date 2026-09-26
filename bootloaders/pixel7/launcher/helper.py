#!/usr/bin/env python3
"""Boot the Pixel 7 into Ferrix when the launcher app on it asks.

The phone cannot start Ferrix on its own: its kernel has no kexec, and
nothing may be flashed (HANDOVER.md, "Never write anything that survives a
reset"). What does start it is `fastboot boot` from a PC. This helper is that
PC half, and the app on the phone is a button for it.

It listens on 127.0.0.1 only, and keeps `adb reverse` pointing the phone's
127.0.0.1:PORT at it, so the app reaches it over the USB cable and nothing
else can. A reboot drops the reverse, so it is put back whenever the phone is
seen without it.

    GET  /status   what the helper would boot, what it is doing, and how the
                   last run ended
    POST /boot     answer at once, then: adb reboot bootloader, fastboot stage
                   vendor_boot.img, fastboot boot the image, wait for Android,
                   and save the ramoops record the run left
    POST /boot?stats=N
                   the same, with Ferrix's stat service (`statd/`) as pid 1
                   for N seconds: a copy of the image whose boot image header
                   carries `ferrix.init=/sbin/ferrix-statd
                   ferrix.statd.seconds=N`, which ABL puts in the device
                   tree's bootargs and the loader hands the kernel

Nothing is written to the phone's partitions. `fastboot boot` runs the image
from RAM, and Ferrix's watchdog brings Android back about 75 seconds later.

For the app's other button, which runs Ferrix as a guest of the phone's own
crosvm and needs no PC, the helper also keeps the raw loader `Image` from the
same run directory at /data/local/tmp/ferrix-vm/ferrix.Image, pushing it
whenever the phone's copy differs: an ordinary file, as an app's data is.

Usage:  python3 bootloaders/pixel7/launcher/helper.py [--image boot.img]
"""

from __future__ import annotations

import argparse
import hashlib
import http.server
import urllib.parse
import json
import pathlib
import subprocess
import threading
import time

PORT = 47707
SERIAL = "28171FDH2001RC"
RUNS = pathlib.Path.home() / ".local/share/ferrix/pixel7"
HERE = pathlib.Path(__file__).resolve().parent
MKBOOTIMG = HERE.parent / "mkbootimg.py"
AVBTOOL = RUNS / "avbtool.py"
VM_DIR = "/data/local/tmp/ferrix-vm"
VM_IMAGE = f"{VM_DIR}/ferrix.Image"


def run(*command: str, timeout: float = 60) -> subprocess.CompletedProcess[str]:
    """Run a command, capturing its output, never raising on its status."""
    return subprocess.run(command, capture_output=True, text=True, timeout=timeout, check=False)


class Phone:
    """The one phone this helper boots, and what it last did with it."""

    def __init__(self, serial: str, image: pathlib.Path | None, vendor_boot: pathlib.Path):
        self.serial = serial
        self.fixed_image = image
        self.vendor_boot = vendor_boot
        self.lock = threading.Lock()
        self.phase = "idle"
        self.last: dict[str, str] = {}
        self.pushed: str | None = None

    def image(self) -> pathlib.Path | None:
        """The image given, or else the newest boot.img a run directory holds."""
        if self.fixed_image is not None:
            return self.fixed_image
        images = sorted(RUNS.glob("*/boot.img"), key=lambda path: path.stat().st_mtime)
        return images[-1] if images else None

    def adb(self, *arguments: str, timeout: float = 60) -> subprocess.CompletedProcess[str]:
        return run("adb", "-s", self.serial, *arguments, timeout=timeout)

    def in_android(self) -> bool:
        return self.adb("get-state", timeout=10).stdout.strip() == "device"

    def in_fastboot(self) -> bool:
        return self.serial in run("fastboot", "devices", timeout=10).stdout

    def vm_image(self) -> pathlib.Path | None:
        """The raw loader `Image` beside the boot image, for the guest."""
        image = self.image()
        vm = image.parent / "Image" if image else None
        return vm if vm and vm.is_file() else None

    def keep_reverse(self) -> None:
        """Point the phone's 127.0.0.1:PORT here whenever adb can reach it,
        and keep the guest's image current there."""
        while True:
            try:
                if self.in_android():
                    if f"tcp:{PORT}" not in self.adb("reverse", "--list").stdout:
                        self.adb("reverse", f"tcp:{PORT}", f"tcp:{PORT}")
                    if not self.lock.locked():
                        self.keep_vm_image()
            except (OSError, subprocess.TimeoutExpired):
                pass
            time.sleep(3)

    def keep_vm_image(self) -> None:
        """Push the guest's image when the phone's copy is not this one."""
        local = self.vm_image()
        if local is None:
            return
        digest = hashlib.sha256(local.read_bytes()).hexdigest()
        if digest == self.pushed:
            return
        remote = self.adb("shell", f"sha256sum {VM_IMAGE} 2>/dev/null").stdout.split()
        if not remote or remote[0] != digest:
            self.adb("shell", f"mkdir -p {VM_DIR} && chmod 777 {VM_DIR}")
            if self.adb("push", str(local), VM_IMAGE, timeout=120).returncode != 0:
                return
            print(f"pushed {local} to {VM_IMAGE}", flush=True)
        self.pushed = digest

    def status(self) -> dict[str, object]:
        image = self.image()
        return {
            "phase": self.phase,
            "image": str(image) if image else None,
            "image_time": time.strftime(
                "%Y-%m-%d %H:%M", time.localtime(image.stat().st_mtime)
            )
            if image
            else None,
            "last": self.last,
            "vm_image": str(self.vm_image()) if self.pushed else None,
        }

    def boot(self, stats: int | None = None) -> str | None:
        """Start a boot in the background, with the stat service for `stats`
        seconds if that is given; a reason it cannot, or None."""
        image = self.image()
        if image is None or not image.is_file():
            return "no boot.img to boot"
        if not self.vendor_boot.is_file():
            return f"no {self.vendor_boot}"
        if stats is not None:
            if not 5 <= stats <= 300:
                return "stats must be 5 to 300 seconds on the phone: the watchdog is fed while Ferrix runs"
            if not (image.parent / "Image").is_file() or not AVBTOOL.is_file():
                return "no raw Image beside the boot image, or no avbtool, to add the stat service with"
        if not self.lock.acquire(blocking=False):
            return "a boot is already under way"
        threading.Thread(target=self.cycle, args=(image, stats), daemon=True).start()
        return None

    def with_options(self, image: pathlib.Path, options: str) -> pathlib.Path:
        """A copy of `image`'s boot image with `options` in its header."""
        out = RUNS / "launcher-options" / "boot.img"
        out.parent.mkdir(parents=True, exist_ok=True)
        steps = (
            ("python3", str(MKBOOTIMG), str(image.parent / "Image"), str(out), "--cmdline", options),
            ("python3", str(AVBTOOL), "add_hash_footer", "--image", str(out),
             "--partition_size", "67108864", "--partition_name", "boot", "--algorithm", "NONE"),
        )
        for step in steps:
            result = run(*step, timeout=120)
            if result.returncode != 0:
                raise RuntimeError(f"{pathlib.Path(step[1]).name} failed: {result.stderr.strip()[-200:]}")
        return out

    def wait(self, what: str, done, seconds: float) -> None:
        self.phase = what
        deadline = time.monotonic() + seconds
        while not done():
            if time.monotonic() > deadline:
                raise RuntimeError(f"timed out: {what}")
            time.sleep(1)

    def cycle(self, image: pathlib.Path, stats: int | None = None) -> None:
        started = time.strftime("%Y%m%d-%H%M%S")
        record = RUNS / f"launcher-{started}"
        booted = image
        try:
            if stats is not None:
                self.phase = f"adding the stat service for {stats} s"
                booted = self.with_options(
                    image, f"ferrix.init=/sbin/ferrix-statd ferrix.statd.seconds={stats}"
                )
            self.phase = "rebooting to the bootloader"
            self.adb("reboot", "bootloader")
            self.wait("waiting for fastboot", self.in_fastboot, 90)
            self.phase = "sending Ferrix"
            for step in (("stage", str(self.vendor_boot)), ("boot", str(booted))):
                result = run("fastboot", "-s", self.serial, *step, timeout=120)
                if result.returncode != 0:
                    raise RuntimeError(f"fastboot {step[0]} failed: {result.stderr.strip()}")
            began = time.monotonic()
            self.wait("Ferrix is running", lambda: not self.in_fastboot(), 60)
            self.wait(
                "Ferrix is running, then Android boots",
                lambda: self.adb("shell", "getprop", "sys.boot_completed", timeout=10).stdout.strip()
                == "1",
                300,
            )
            seconds = int(time.monotonic() - began)
            record.mkdir(parents=True)
            log = self.adb(
                "exec-out", "su -c 'cat /sys/fs/pstore/console-ramoops-0'", timeout=60
            ).stdout
            (record / "run.log").write_text(log)
            ended = next(
                (line for line in log.splitlines() if line.startswith(("FERRIX-BOOT-OK", "FERRIX-PANIC"))),
                "no FERRIX-BOOT-OK or FERRIX-PANIC in the record",
            )
            self.last = {
                "when": started,
                "image": str(image),
                "stats": str(stats) if stats is not None else "",
                "result": ended.strip(),
                "seconds": str(seconds),
                "record": str(record / "run.log"),
            }
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            self.last = {"when": started, "image": str(image), "result": f"helper: {error}"}
        finally:
            self.phase = "idle"
            self.lock.release()
            print(json.dumps(self.last), flush=True)


def handler(phone: Phone):
    class Handler(http.server.BaseHTTPRequestHandler):
        def reply(self, code: int, body: dict[str, object]) -> None:
            data = json.dumps(body).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self) -> None:
            if self.path == "/status":
                self.reply(200, phone.status())
            else:
                self.reply(404, {"error": "no such path"})

        def do_POST(self) -> None:
            url = urllib.parse.urlsplit(self.path)
            if url.path != "/boot":
                self.reply(404, {"error": "no such path"})
                return
            query = urllib.parse.parse_qs(url.query)
            stats = None
            if "stats" in query:
                try:
                    stats = int(query["stats"][0])
                except ValueError:
                    self.reply(400, {"error": "stats is a number of seconds"})
                    return
            refused = phone.boot(stats)
            if refused:
                self.reply(409, {"error": refused})
            else:
                self.reply(202, {"started": True, "image": str(phone.image())})

        def log_message(self, format: str, *args: object) -> None:
            print(f"{self.address_string()} {format % args}", flush=True)

    return Handler


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--image", type=pathlib.Path, help="boot.img to boot (default: the newest run's)")
    parser.add_argument("--vendor-boot", type=pathlib.Path, default=RUNS / "vendor_boot.img")
    parser.add_argument("--serial", default=SERIAL)
    args = parser.parse_args()
    phone = Phone(args.serial, args.image, args.vendor_boot)
    threading.Thread(target=phone.keep_reverse, daemon=True).start()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), handler(phone))
    print(f"launcher helper on 127.0.0.1:{PORT}, booting {phone.image()}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
