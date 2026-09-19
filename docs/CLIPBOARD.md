# Clipboard: the host's selection and the guest's, joined over vdagent

Version 1, drafted. Written by ferrix-d3 on 2026-09-19 at the user's asking
("shared copy and paste between QEMU Ferrix and the host"). **Not yet approved
by a product owner:** §7 is the list of decisions that are the owner's and not
this document's, and no owner session was running when this was written --
`ferrix-32`, whom `docs/BACKLOG.md` names, has ended. Until one answers, §7's
draft answers are what the code assumes and each is marked where it is used.

It has the shape of `docs/INPUT.md` on purpose, because it is the same kind of
thing: a small, rare byte stream between a ring-3 virtio driver and a program
that wants it, with a kernel core in between and a host-tested crate for every
protocol. Where a rule here differs from that document's, the item says why.

## 1. What this is, and what it is not

A person watching Ferrix in a QEMU window copies a line in their host editor
and pastes it into a Ferrix program, and copies in a Ferrix program and pastes
it on the host. This document specifies the whole path:

* **the transport**, virtio-serial: a `virtio-console` device with the
  multiport feature, whose port named `com.redhat.spice.0` is the wire every
  SPICE guest agent has used since 2010;
* **the protocol on it**, SPICE's vdagent, of which this implements the
  clipboard messages and nothing else;
* **the kernel's port core**, which owns `/dev/vport0p1` and the channel to
  the ring-3 driver;
* **the agent**, a program that is a `ext-data-control` client on one side and
  the port's reader on the other;
* **the test**, which needs no window and no person: QEMU's own host half of
  vdagent, driven from `xtask` over a socket.

It is **not** the SPICE display, the SPICE server, or a SPICE client. QEMU's
`qemu-vdagent` chardev (since QEMU 6.1) is the host half of the agent protocol
with no SPICE anywhere: it bridges the port straight to whatever clipboard the
active UI has — GTK's, SDL's, or a VNC client's through the RFB extended
clipboard. So Ferrix speaks vdagent and gets every viewer QEMU has, and this
project does not gain a SPICE dependency.

It is not the mouse, the monitors configuration, the file transfer or the
audio volume of the same protocol. It is not images on the clipboard: §7(b)
asks the owner whether `image/png` is in version 1 or not. It is not a
clipboard between two Ferrix machines, and it is not the guest's clipboard
persisting after the program that owns it exits, which is Wayland's rule and
stays Wayland's rule (`docs/ROADMAP.md`, stage 18: "Wayland's clipboard is a
promise and not a buffer").

**Exit:** on x86-64 and AArch64, with `cargo xtask test-clipboard`, a string
copied by the host half arrives in the guest's selection and is printed by
`clip paste`; a string given to `clip copy` is grabbed, requested by the host
half, and arrives there byte for byte. `cargo xtask run --display --clipboard`
gives a person the same thing with their own keyboard.

## 2. Why this transport and not another

Three were considered and two rejected, and the reasons are worth keeping
because the rejected ones look cheaper until the viewer is asked about.

1. **vdagent over virtio-serial** — this document. It costs a device driver
   Ferrix does not have. It is the only one of the three that reaches the
   clipboard of a viewer that is not on the QEMU host: the person watching
   Ferrix from another machine, over VNC or from a QEMU window on a host whose
   build has GTK, has their clipboard joined by QEMU itself.
2. **A bridge over the network**, guest TCP to a listener in `xtask`, which
   already terminates the guest's TCP on the host (`xtask/src/gateway/`). It
   needs no driver and would have been days rather than weeks — but the
   clipboard it joins is the *QEMU host's*, which is the wrong machine for
   anyone whose viewer is elsewhere. It also works only on a boot `xtask`
   started, with `--net`, which is not a clipboard a system has.
3. **A shim in `xtask`**, shuttling text through a channel that exists. A
   development convenience, not a clipboard programs share.

The decision, on 2026-09-19, was the user's, told the viewer question: their
QEMU window is on another host. (1) is the only one that reaches it.

## 3. The transport

### 3.1 The device

`-device virtio-serial-pci` with one port:

```
-chardev qemu-vdagent,id=vdagent,name=vdagent,clipboard=on
-device virtio-serial-pci,disable-legacy=on,iommu_platform=on
-device virtserialport,chardev=vdagent,name=com.redhat.spice.0
```

`clipboard=on` is what makes the chardev a clipboard peer of QEMU's UI;
without it the chardev exists and carries nothing. `mouse=off` is left as it
defaults, since the agent announces no mouse capability and QEMU then sends no
mouse state (`ui/vdagent.c`, `have_mouse`).

virtio-console is device id 3, so the modern PCI device id is `0x1043` and the
transitional one `0x1003` — the pair `devmgr`'s table takes (`docs/DEVMGR.md`
§3). The features the driver takes are `VIRTIO_CONSOLE_F_MULTIPORT` (bit 1),
without which there is no port naming and no control queue and the device is
one nameless console; `VERSION_1`; and `ACCESS_PLATFORM`, without which a
device behind the machine's IOMMU refuses `FEATURES_OK`, as every other Ferrix
virtio driver takes it. `F_SIZE` and `F_EMERG_WRITE` are not asked for.

### 3.2 The queues, and why the numbering is the awkward part

Virtio 1.2 §5.3.2. Port 0 is queues 0 (receive) and 1 (transmit). With
multiport, queues 2 and 3 are the control receive and control transmit. Every
port *N* above 0 is queues `2N + 2` and `2N + 3`. So the one port this device
has — port 1, the only one `virtserialport` adds — is queues 4 and 5, and
queues 0 and 1 exist, must be set up, and are never used. A driver that
assumes its port's queues are 0 and 1 gets a device that accepts everything
and delivers nothing, which is the failure this paragraph exists to prevent.

### 3.3 The control conversation

The control message is `struct virtio_console_control` — `le32 id`, `le16
event`, `le16 value` — verified against QEMU 9.2.4's
`include/standard-headers/linux/virtio_console.h`. The opening exchange, in
the order it happens:

1. driver → `DEVICE_READY` (0) with `value` 1, once the queues are up;
2. device → `PORT_ADD` (1) for port 1;
3. driver → `PORT_READY` (3) with `value` 1;
4. device → `PORT_NAME` (7), whose payload after the header is the name
   `com.redhat.spice.0`, and `CONSOLE_PORT` (4) it does not send for this one;
5. device → `PORT_OPEN` (6) with `value` 1 when the host end is ready;
6. driver → `PORT_OPEN` (6) with `value` 1, and only now does data flow.

The name is how the port is identified, not the number: a port's number is
QEMU's to choose, and a guest that hardcodes 1 is a guest that breaks the day
somebody adds a second port before it. The driver matches the name and calls
the port it found "the agent port"; §7(c) asks whether a second port is worth
carrying at all in version 1.

`PORT_OPEN` with `value` 0 from the device means the host end went away — the
viewer's window closed, the chardev disconnected — and the agent must forget
its grabs and stop offering a selection it can no longer satisfy.

## 4. The protocol on the port

SPICE's vdagent, from `/usr/include/spice-1/spice/vd_agent.h` (BSD-licensed
header; the numbers are cited here, none of its code is copied) and checked
against what QEMU 9.2.4 actually sends and accepts in `ui/vdagent.c`.

### 4.1 Framing

Two layers, both little-endian:

```
VDIChunkHeader   port: u32, size: u32          the port field; the bytes after
VDAgentMessage   protocol: u32 = 1, type: u32, opaque: u32, size: u32, data
```

A message is cut into chunks of at most **1024** bytes of payload each, which
is what QEMU does when it sends (`vdagent_send_msg`) and what a guest should
do when it sends, because the host reassembles by `chunk.size` and a chunk
larger than its buffer is a dropped connection rather than a big message. The
chunk's `port` is `VDP_CLIENT_PORT` (1) on the way out of the guest;
QEMU ignores the field when reading, and this is written down so that nobody
"fixes" it to 2 and wonders why nothing changes.

`opaque` is 0 for every clipboard message.

### 4.2 The capability exchange

`VD_AGENT_ANNOUNCE_CAPABILITIES` (6): `u32 request`, then the capability
bitmap as `u32`s. QEMU with `clipboard=on` announces
`CLIPBOARD_BY_DEMAND` (5), `CLIPBOARD_SELECTION` (6) and
`CLIPBOARD_GRAB_SERIAL` (17), and nothing else unless `mouse=on`.

The agent announces exactly those three and no more, and it must, because each
changes the shape of a later message:

* **`CLIPBOARD_BY_DEMAND`** is the modern clipboard at all: a grab says only
  *what types are available*, and the data moves when the other side asks. The
  old behaviour — sending the data with the grab — is what its absence means,
  and nothing here implements it.
* **`CLIPBOARD_SELECTION`** puts a one-byte selection number, then three bytes
  of padding, at the front of `CLIPBOARD`, `CLIPBOARD_GRAB`,
  `CLIPBOARD_REQUEST` and `CLIPBOARD_RELEASE`. Without it every message is
  four bytes shorter and there is only the one selection. This is the single
  most common way to get a vdagent implementation wrong, so the codec has no
  way to spell a message without saying which of the two shapes it is.
* **`CLIPBOARD_GRAB_SERIAL`** adds a `u32` serial after the selection in a
  grab, which settles a grab race: whoever's serial is newer wins, and a grab
  arriving with an older serial than the one already held is dropped rather
  than fought over.

Either side may send `ANNOUNCE_CAPABILITIES` with `request` 1, which asks the
other to answer with its own. The agent sends its own unprompted when the port
opens, with `request` 1, and answers any it receives with `request` 0.

### 4.3 The clipboard messages

Selections: `CLIPBOARD` (0) and `PRIMARY` (1). `SECONDARY` (2) exists in the
protocol, QEMU maps it to nothing, and the agent refuses it.

Types: `UTF8_TEXT` (1) is version 1's whole vocabulary, mapped to the MIME
type `text/plain;charset=utf-8` that the compositor's clipboard already
carries (`compositor/clip`). `IMAGE_PNG` (2) is §7(b).

| Message | Number | Meaning |
|---|---|---|
| `CLIPBOARD_GRAB` | 7 | I now own this selection; here are the types |
| `CLIPBOARD_REQUEST` | 8 | send me this selection as this type |
| `CLIPBOARD` | 4 | here is the data, in answer to a request |
| `CLIPBOARD_RELEASE` | 9 | I no longer own it |

Four rules that are the protocol's and not obvious:

1. **A request is answered, always.** A `CLIPBOARD` message with type
   `NONE` (0) and no data is how a side says "I could not". A request left
   unanswered hangs the other side's paste until its own timeout, which on the
   host is a viewer that appears frozen.
2. **Line endings.** The guest announces neither `GUEST_LINEEND_LF` nor
   `GUEST_LINEEND_CRLF`, so no conversion happens in either direction and
   bytes are carried as they are. A Ferrix program that copies `\r\n` pastes
   `\r\n` on the host.
3. **A regrab is not a release.** Without
   `CLIPBOARD_NO_RELEASE_ON_REGRAB` (16), which neither side announces here,
   a new grab replaces the old with no release in between.
4. **Size.** Neither side announces `MAX_CLIPBOARD` (10), so no maximum is
   negotiated. The agent has one of its own anyway — §7(a) — because a
   selection is read into memory whole and a guest that will allocate whatever
   the host names is a guest the host can exhaust.

## 5. The kernel's port core

The one piece of new kernel. It is deliberately the smallest thing that can
work, and it follows `docs/INPUT.md` §3 rather than `docs/BLOCK-RING.md`: like
input events and unlike disk traffic, clipboard bytes are small and rare, so
there is **no shared-memory ring**. One control channel per port carries fixed
little-endian messages in `libs/displayctl`'s shape, in a new host-tested
crate `libs/portctl`:

```
HELLO    driver -> core    the port's name, its number, its maximum chunk
OPEN     core   -> driver  the port is wanted; open it on the device
DATA     either direction  a length and up to MAX_CHUNK bytes
CLOSED   driver -> core    the host end went away
STOP     core   -> driver  give the device back
STOPPED  driver -> core    it is given back
```

The core publishes one character device per port it accepts,
`/dev/vport0p1` — Linux's name for port 1 of virtio-serial device 0, so that a
program written against Linux finds it where it expects. It is a character
device in devfs beside `/dev/console` and `/dev/pts` (`kernel/src/fs/devfs.rs`
today registers block nodes only; this adds the character half it has always
been shaped for). It answers `open`, `read`, `write`, `poll` and `close`, and
nothing else: no `ioctl`, since the one Linux has on these nodes is about the
console size, which this device does not have.

Reads and writes are bounded by a per-port buffer, and a write that would
overflow it blocks or returns `EAGAIN` by the file's flags, as a pipe does.
A read on a port whose host end is closed returns 0 at end of stream, which is
what makes the agent's loop terminate without a special message.

## 6. The agent, and where it lives

The agent is an `ext-data-control` client. That protocol is already in the
compositor and complete for this (`compositor/protocol/src/generated/ext_data_control.rs`,
served by `compositor/hyprix/src/clipboard.rs`): a manager can watch the
selection change, read it through a pipe, and set it from a source of its own —
which is exactly a clipboard manager, and exactly the two directions needed.
So nothing in the compositor changes for this feature. That is the reason for
choosing a separate program over teaching `hyprix` to open the port itself:

* host → guest is `create_data_source`, `offer("text/plain;charset=utf-8")`,
  `set_selection`, then answering the `send` event by writing what the host
  gave into the pipe;
* guest → host is the `selection` event naming an offer, `receive` on it with
  a pipe, and the bytes that come back.

§7(d) asks the owner to confirm the program's name and where it starts. The
draft assumes `/bin/vdagent`, started by the session that starts the
compositor, exiting quietly with status 0 when `/dev/vport0p1` does not exist
so that a boot without the device is a boot without a clipboard and not a
boot with an error.

## 7. What the product owner decides

* **(a) The maximum selection.** The draft says 1 MiB, refusing anything
  larger in either direction with `CLIPBOARD`/`NONE` and a line on the
  console. Large enough for any text a person copies, small enough that a
  hostile host cannot exhaust the guest.
* **(b) Images.** `image/png` both ways is perhaps 150 lines more and no new
  concepts — the type number exists, the compositor carries any MIME type
  already. In or out of version 1?
* **(c) More than one port.** The core is written for many ports and the
  driver for one. Carrying *n* ports costs little now and cannot be added
  later without changing the devfs naming. The draft carries *n*.
* **(d) The agent's name and its start.** `/bin/vdagent`, started beside the
  compositor?
* **(e) Where it lands.** This is a stage 19 feature by subject and a stage 10
  feature by machinery. The draft assumes stage 19, with the port core noted
  as a stage 10 debt paid late.

## 8. The order of the landings

Each row is one landing and each is gated on its own. The first two need
nothing from the kernel and are pure host-tested logic.

| # | What | Where | State |
|---|---|---|---|
| 1 | this document | `docs/CLIPBOARD.md` | landed |
| 2 | the vdagent protocol, encode and decode | `libs/vdagent` | landed |
| 3 | the virtio-console device protocol | `libs/virtio/src/console.rs` | landed |
| 4 | the port control protocol | `libs/portctl` | to do |
| 5 | the kernel's port core and `/dev/vport0p1` | `kernel/src/port/`, `kernel/src/fs/devfs.rs` | to do |
| 6 | the driver, and `devmgr`'s table | `user/vport`, `user/devmgr` | to do |
| 7 | the agent | `user/vdagent` or `compositor/vdagent` | to do |
| 8a | `--clipboard`: the device on the bus | `xtask` | landed |
| 8b | `test-clipboard` | `xtask` | to do |

Landings 2, 3 and 8a are on `clipboard-vdagent`. The three that are not yet
done are the ones that need a decision in §7 or a new piece of kernel, and
8a is worth its place in the order after all: `test-boot --arch x86_64
--clipboard` reaches `FERRIX-BOOT-OK` with 9 PCI functions and 4 virtio
transports where a plain boot has 8 and 3, and `devmgr` starts the same two
drivers and fails none. So the device is enumerated, a node is published for
it, and nothing claims it -- which is exactly the state landing 6 begins
from, proven rather than assumed.

## 9. The test

`cargo xtask test-clipboard`, headless, on x86-64 and AArch64. The host half is
QEMU's own: `-chardev qemu-vdagent` bridges to the UI's clipboard, and with no
UI there is nothing to bridge to — so the test does not use the UI at all. It
attaches a second chardev, a Unix socket `xtask` listens on, and speaks vdagent
on it as a viewer would, which makes the whole guest path — device, driver,
core, node, agent, compositor — the thing under test and the host clipboard not
part of the test at all.

Two directions, one boot:

1. `xtask` sends `CLIPBOARD_GRAB` for `CLIPBOARD`/`UTF8_TEXT`; the guest's
   `clip paste` must print the string, which requires the agent to have
   requested it and the compositor to have served it.
2. The guest runs `clip copy <string>`; `xtask` must receive a grab, and the
   `CLIPBOARD` answer to its request must hold the string.

A person's own check, which no gate can make, is
`cargo xtask run --display --clipboard` and their own keyboard.
