# adb for Ferrix: handover

**Status, 2026-09-26 (ferrix-9c): step 1, adbd over TCP, is built and
gated (§6); steps 2 and 3 follow.** The rest is the brief as it was
written: why adb, what it has to do, what Ferrix already has for it, and the
order to build it in. `docs/PIXEL7-USB-HANDOVER.md` is the USB stack it goes on top of;
read its §8 first, and `boot/pixel7/HANDOVER.md` before touching the phone.

## 1. Why adb, and what done looks like

The Pixel 7 running Ferrix natively has a live log over USB and nothing else.
The owner wants a debug interface: a shell, files in and out, a reboot, and
later port forwarding for a debugger. Four were weighed with the owner on
2026-09-26:

* **fastboot** is a bootloader's protocol (`flash`, `erase`, `boot`,
  `getvar`). In a running system it offers a reboot and little else. Dropped.
* **A shell on a serial port** is the embedded board's UART console. It is
  cheap, but it moves no files and shares its line with whatever else is
  there.
* **SSH over USB networking** (CDC-NCM plus `sshdt`) is what embedded Linux
  usually does. It needs an IP on both ends, and `sshdt` is only in x86-64
  images so far.
* **adb** needs no network setup on the PC. It is what the phone's tooling
  already speaks: `tools/pixel7/monitor` reads the phone's `/proc` through one
  `adb shell`, and the helper and the launcher are adb. It covers shell,
  files, reboot and forwarding in one tool, and it runs over TCP as well as
  USB. **Chosen.**

Done means: with Ferrix running natively on the phone, `adb devices` on nazuna
lists it, and `adb shell`, `adb push`, `adb pull`, `adb reboot` and
`adb forward` work. The same works over TCP (`adb connect`) against Ferrix in
QEMU and in the phone's crosvm guest, and a gate proves it on every change.

Until then, the monitor's "Boot Android" button sends `usbdev` one line over
the serial port (`ferrix-usbdev: reboot`). `adb reboot` replaces it.

## 2. The protocol, as much as is needed

AOSP's `packages/modules/adb/protocol.txt`, `SYNC.TXT` and
`shell_protocol.h` are the authority. The summary below is from memory, so
check each value against those files before relying on it. The host's
`adb` (platform-tools 36 on nazuna) is the real test of every step.

* **Messages.** A 24-byte header, little-endian: `command`, `arg0`, `arg1`,
  `data_length`, `data_crc32`, and `magic` (`command ^ 0xFFFFFFFF`), then
  `data_length` bytes of payload. Commands are four ASCII bytes: `CNXN`,
  `AUTH`, `OPEN`, `OKAY`, `WRTE`, `CLSE` (and `STLS`, which Ferrix need not
  offer). With version `0x01000001` and up, the CRC is not checked and may be
  zero. The payload limit is agreed in `CNXN` (1 MiB on current hosts).
* **Connecting.** The host sends `CNXN(version, max_payload, "host::")`. A
  device that does not require authentication (a debug build's
  `ro.adb.secure=0`) answers with its own `CNXN` and a banner such as
  `device::ro.product.name=ferrix;ro.product.model=Pixel 7;features=shell_v2,cmd`.
  Authentication (`AUTH` with an RSA token, signature and public key) comes
  later, once keys have somewhere to live.
* **Streams.** `OPEN(local_id, 0, "service:args")` opens one; the device
  answers `OKAY(its_id, local_id)`, or `CLSE` to refuse. Then `WRTE` carries
  bytes each way, each acknowledged with `OKAY` before the next (one write in
  flight per stream), and `CLSE` closes it.
* **Services, first cut:**
  * `shell:` — a command, or with no command an interactive shell, on a
    pseudo-terminal. `shell,v2:` adds framed stdout, stderr and exit status
    (packets of id, length and bytes: 0 stdin, 1 stdout, 2 stderr, 3 exit,
    4 close stdin, 5 window size), which `adb shell cmd` needs to report an
    exit code.
  * `sync:` — `push` and `pull`: requests of a 4-byte id and a 4-byte length
    (`STAT`, `LIST`, `SEND`, `RECV`, `DATA`, `DONE`, `OKAY`, `FAIL`,
    `QUIT`).
  * `reboot:` — restart. On the phone, Ferrix's restart is the watchdog reset
    that brings Android back.
  * `tcp:PORT` — a stream to a local TCP port, which is `adb forward`, and
    what gdbserver needs later.
* **Transports.** Over TCP the messages are the byte stream on port 5555.
  Over USB they go on one interface of class `0xFF`, subclass `0x42`,
  protocol `0x01`, with a bulk IN and a bulk OUT endpoint. The host finds it
  by that class, whatever the vendor ID.

## 3. What Ferrix already has

* **Sockets, ptys, processes:** `AF_INET` TCP (`test-net`), `AF_UNIX`,
  pseudo-terminals and job control (`test-pty`, `test-jobs`),
  `fork`/`execve`, and zinc as the shell. `/proc` is what `ps` and the
  monitor's script read.
* **A place for the program:** `userland/`, beside `userland/statd`, built
  for the Linux personality and put in the initramfs the same way.
* **The USB device stack:** `libs/drivers/dwc3`, `libs/drivers/usb-device`
  and `native/drivers/usbdev` present a CDC-ACM port today (§8 of the USB
  handover).
* **The pattern for the USB half:** `native/drivers/vport` moves a device's
  bytes to and from a Unix socket, and a Linux program on the other side
  speaks the protocol (`docs/CLIPBOARD.md` §6). adb's USB transport is the
  same shape: `usbdev` moves the adb interface's bulk transfers to a socket,
  and `adbd` holds the protocol.

## 4. The plan

Each step lands on its own, gated, and says where it stands here.

1. **adbd over TCP** (`userland/adbd`, a Linux program). The message layer
   and the stream table go in a host-tested library, as the USB pieces are,
   tested against captured `adb` exchanges. Then `CNXN` without auth,
   `shell:` on a pty, `reboot:`, `sync:` push and pull, and `tcp:`. Started
   by init where there is a network, or as `ferrix.init=` for a test. About
   13 points.
2. **A gate**, `xtask test-adb`: boot with `--net --forward 5555:5555`, run
   the host's `adb connect 127.0.0.1:5555`, then `adb shell echo`, a
   `push`/`pull` round trip compared byte for byte, and `adb reboot`.
   Without `adb` on the host the step says so and passes as skipped, as
   other optional tools' steps do. About 3 points.
3. **The USB transport.** A composite device in `libs/drivers/usb-device`
   (ACM plus the adb interface, with an interface association), and room for
   the extra endpoints in `libs/drivers/dwc3`'s layout (`MAX_ENDPOINTS` is 4
   and ACM uses 3). Then `usbdev`'s bridge to a socket `adbd` connects to.
   About 8 points. **The new endpoint registers (`0xC860` and up) are a new
   write list: the product owner session approves it before the first boot**
   (§8 of the USB handover has the standing list and the rules).
4. **The host.** A udev rule so `adb` may open `1209:0001`:
   `SUBSYSTEM=="usb", ATTR{idVendor}=="1209", ATTR{idProduct}=="0001",
   TAG+="uaccess"`, installed by the owner as the ModemManager rule was
   (`/etc/udev/rules.d/70-ferrix-console.rules` is where that one lives).
   Then the monitor's "Boot Android" button becomes `adb reboot`, and its
   phone script can run against Ferrix, where `/proc` answers it.

## 5. Threats to keep in view

* **Whoever holds the cable holds the phone.** An adbd without
  authentication gives a root shell to anyone who plugs in, as a debug
  Android build does. The first cut runs that way on purpose, on a phone
  that is a development device. Authentication (§2) is the step that ends
  it, and it should come before adbd is started by default rather than on
  request.
* **The stopgap is the same, smaller.** Today `usbdev` restarts the machine
  when the serial port's host sends `ferrix-usbdev: reboot`. It is
  unauthenticated too, for anyone with a USB cable to the phone. It reboots
  and does nothing else. It goes when adb's `reboot:` replaces it.
* **Opt-in, because of the first point.** adbd is in an image only when it
  is built with `--adbd`, and nothing starts it there: running it is always
  something somebody asked for (the product owner's condition,
  2026-09-26). Under QEMU its port is reachable only through a `--forward`
  on the host's loopback.
* **Keys, when they come.** adbd keeps its own list of allowed public keys,
  as `sshdt` keeps `authorized_keys`, and checks the RSA token itself. The
  auth design's owner (ferrix-d5, `docs/AUTH.md` §4) settled that authd
  stores no adb keys and verifies no signatures; from authd's phase 3,
  adbd reports each verdict to it (service `adbd`, the target uid) for
  throttling and the audit log.
* **Forwarding reaches inside.** `tcp:` opens connections from inside Ferrix
  to whatever listens there, so it answers to the same authentication.

Not in the first cut: authentication, `adb install`, `logcat`, `adb root`
and `remount`, and `reboot bootloader`. On this phone that last one is a
secure-firmware call plus a persistent write Ferrix must never make:
`docs/BACKLOG.md` has the row.

## 6. Where it stands

### Step 1, adbd over TCP (2026-09-26)

* `libs/proto/adb` (`ferrix-adb`): the 24-byte messages, the banner, and the
  first sync protocol's requests and replies, with tests on the host. The
  constants match `protocol.txt`, and the host's `adb` 37 is the proof.
* `userland/adbd`: a static musl program, built as `statd` is. `shell:` runs
  a command under `/bin/sh -c`, or an interactive shell on a pty. `sync:`
  does `STAT`, `LIST`, `SEND` and `RECV`. `reboot:` restarts the machine,
  or ends adbd under `--test`. `tcp:PORT` is a stream to a port inside. No
  features are offered, so the host uses `shell:` v1: `adb shell` does not
  pass back the command's exit status.
* `cargo xtask build --adbd` puts it at `/bin/adbd`, which nothing starts.
  Run it by hand or from a unit: `adbd [--port N]`, 5555 by default.
* `cargo xtask test-adb --init <busybox with {arch}>` is the gate. The
  kernel brings up `eth0` by DHCP and runs `adbd --test`. This machine's
  `adb` then connects through a `--forward` on the loopback and runs
  `shell echo`, a 300 000-byte `push` and `pull` compared byte for byte,
  `ls /bin`, a `forward` that speaks `CNXN` to adbd through itself, and
  `reboot`. It passed on x86_64, aarch64 and armv7a on 2026-09-26. Without
  `adb` on the machine it is skipped, and says so. `cargo xtask check`
  holds adbd's formatting and clippy, and `ferrix-adb`'s tests.
* By hand: `cargo xtask run --arch x86_64 --init <busybox> --adbd --forward
  5555:5555`, then `udhcpc -i eth0` and `adbd &` in the guest, then
  `adb connect 127.0.0.1:5555` on the host.

### Next

Step 3, the USB transport, needs the product owner's OK for the new DWC3
endpoint registers before any phone boot (§4). Step 4 is the host's udev
rule. Authentication follows §5.
