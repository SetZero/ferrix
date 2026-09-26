# Authentication: who a person is, proven to Ferrix

> **Approved by the customer, 2026-09-26: decisions 1-11 as recommended.**
> It was written the same day by the `auth-design` stream, which is now the
> `auth` stream. The customer had asked for "a true authentication mechanism
> for Ferrix that is both secure and somewhat still versatile, fitting to
> the current Ferrix design with a semi-microkernel layout". §9 is kept as
> the record of what was decided and why. Phase 1 (§7) is being built; P0
> belongs to ferrix-15, who works from §7's P0 row.
>
> **Paths.** The document was written before the repository relayout
> (`fddfc32d`, `docs/LAYOUT.md`) and its paths have since been rewritten to
> the new layout. Line numbers were read at `1a8bea54`, before the move,
> which moved no line inside a file.

## 0. The proposal in one page

One ring-3 service, **`authd`**, holds every credential on the machine and
is the only program that ever reads one. Everything that needs to know
whether a person is who they say they are asks `authd` and gets a verdict
back. That covers the lock screen, the console's login, `su`, `passwd`, a
privilege prompt, and later ssh. None of these programs reads a hash, links
a hash function, or keeps a failure count of its own.

```
  keyboard ─> input driver (ring 3) ─> kernel evdev ─> hyprix ─> hyprlock ┐
                                                                          │ AF_UNIX seqpacket
  console  ─> kernel tty ─> getty ─> login ───────────────────────────────┤ /run/ferrix/auth
  su, passwd, a PAM program (later) ──────────────────────────────────────┤ SO_PEERCRED says who
                                                                          v
                                  ┌──────────────────────────────────────────────┐
                                  │ authd   (uid `auth`, its own cgroup)         │
                                  │  policy per service   /etc/ferrix/auth/...   │
                                  │  methods: password (argon2id); later TOTP,   │
                                  │           FIDO2, fingerprint                 │
                                  │  throttle per account, audit log             │
                                  │  store /var/lib/ferrix/auth  (0700 auth)     │
                                  └───────────────┬──────────────────────────────┘
                                                  │ native channel, routed by init:
                                                  │ `ferrix.auth.seat` (phase 2)
                                                  v
                                  hyprix: unlocks the screen only on authd's GRANT
```

What makes this fit Ferrix rather than any Unix:

* **It is a service like a driver is.** The design line of
  `docs/ARCHITECTURE.md` §1 puts code that needs no privilege in ring 3 in a
  process of its own. `authd` runs under init in its own cgroup, as its own
  user, with no root. A fault in it costs a verdict, not the kernel.
* **The kernel does not grow an authentication function.** The certified
  item claims no identification or authentication (FIA) and no audit (FAU),
  on purpose (`docs/certification/SECURITY-TARGET.md` §9.1, lines 303-311).
  This design keeps it so. Authentication lives in the operational
  environment, and relies on the item only for isolation (O.ISOLATE),
  handles (O.CAPABILITY) and scrubbed frames (O.SCRUB). §8.1 is the
  amendment to the Security Target that says so.
* **Identity travels in the kernel's words.** Linux programs are identified
  by `SO_PEERCRED`, which Ferrix answers today
  (`kernel/src/fs/socket.rs:394-400`). Native programs and services are
  identified by the unit init routed them from, through the directory
  (`docs/INIT.md` §6), whose CONNECT names the client unit.
* **Policy is files in init's syntax.** A service's rules are an INI file in
  the same three layered directories as units, read by `libs/init/svc`'s parser
  (`libs/init/svc/src/ini.rs`). Adding a service is like adding a unit.

Phase 1 (§7) builds `authd` with passwords, `passwd`, the store and a gate,
and gives hyprlock a real backend. It is about 27 points and needs nothing
from the rest of the plan. Phase 2 moves the desktop off root and makes the
compositor, not the lock client, the judge of an unlock.

---

## 1. What Ferrix does today

Each fact below was read from the tree at `1a8bea54`.

| Area | What is there | Where |
|---|---|---|
| Ids | Four user ids, four group ids and supplementary groups per process. `fork` copies them, `execve` keeps them, the `set*id` calls follow Linux's rules. | `kernel/src/syscall/credentials.rs:1-28`, `:148-155`; `kernel/src/syscall/process.rs:433` |
| Privilege | An effective uid of 0 is all privilege. There are no capability sets: `capget` reports all or nothing, and `capset` narrows nothing. | `credentials.rs:15-28`, `:177-181`, `:485-556` |
| Set-id programs | A file with mode `04000` runs as its owner, and `02010` as its group, unless `PR_SET_NO_NEW_PRIVS` is set. `AT_SECURE` is set when the ids differ. | `kernel/src/fs/mod.rs:289-298`; `kernel/src/syscall/exec.rs:335-345`, `:721-725` |
| `nosuid` | Accepted as a mount flag and not enforced: "there is nothing yet for any of them to switch off". | `kernel/src/syscall/fsctl.rs:91-93` |
| File permissions | Enforced, from `libs/fs/vfs`'s `access` functions, against the filesystem ids. | `docs/ROADMAP.md:1424-1440` |
| Kernel-made processes | Every process the kernel starts is root's. That includes a **native process made with `process_create`, whoever made it**: the loader builds it with `Process::new`, which starts from `Credentials::root()`, and `process_create` uses its caller only for the job and image handles. | `kernel/src/syscall/exec.rs:181-195`; `kernel/src/syscall/process.rs:308-314`, `:369-371`; `kernel/src/syscall/native.rs:1290-1325` |
| Peer identity, Linux | `SO_PEERCRED` answers pid and **effective** uid and gid. They are the ids of whoever *made* the connecting socket, taken when it was made, not at `connect` as on Linux. `SCM_CREDENTIALS` is stamped only when asked for, and root may name any ids in it. | `kernel/src/fs/socket.rs:21-31`, `:394-400`, `:475-485`, `:773-779`, `:1724`; `kernel/src/syscall/sockets.rs:1053-1056` |
| Peer identity, native | A channel carries bytes and handles, and nothing about who wrote them. The directory's CONNECT carries the client *unit's* name, which init fills in. | `libs/proto/native-abi/src/rights.rs:1-9`; `docs/INIT.md:1395-1405` |
| Jobs from paths | `job_for_cgroup` gives `MANAGE` to a caller who may write that cgroup's `cgroup.procs`, and delegation chowns that file to a user. | `libs/proto/native-abi/src/nr.rs:201-211`; `docs/CGROUPS.md` §3.1, §5 |
| `/proc/<pid>` | Owned by the process's effective ids, always. `PR_SET_DUMPABLE` is recorded and changes nothing. `fd` entries are plain symbolic links, not Linux's magic links. | `kernel/src/fs/procfs.rs:797-808`, `:327-331`, `:980`; `kernel/src/syscall/attributes.rs:88-89`, `:308-314` |
| `ptrace` | Not in the call tables: no process can read another's memory, except through a VMO both hold. | `libs/proto/linux-abi` has no `Ptrace`; `SECURITY-TARGET.md` FDP_IFC.1 |
| Memory | No swap and no core dumps: a secret's page never leaves RAM. Frames are zeroed when handed to a new owner, not when freed. The kernel heap is not zeroed. `mlock` is `ENOSYS`. | `kernel/src/syscall/memory.rs:383-390`; `SECURITY-TARGET.md:144`, `:261`; `docs/BACKLOG.md` |
| Randomness | ChaCha20 seeded from firmware, the CPU's instruction and jitter. A machine with neither of the first two says at boot that it is not seeded. | `kernel/src/random.rs:1-33` |
| Device nodes | `/dev/console` is `0600` root. Cards and `event*` are `0660` root:root, and there is no `input` or `video` group. | `kernel/src/fs/devfs.rs:229`; `kernel/src/display/mod.rs:447-450`; `kernel/src/input/evdev.rs:237-241` |
| DMA | Contained by an IOMMU on x86-64 and AArch64. On ARMv7-A `virt` and the DK1, any ring-3 driver can read all memory, and the boot says so. | `docs/ARCHITECTURE.md:326-357` |
| Accounts | Busybox images carry `root:x:0:0` and `ferrix:x:1000:1000` with a home. `test-init` carries the same two. The foot compositor image carries only root. No image has `/etc/shadow`. | `xtask/src/initramfs.rs:536-546`; `xtask/src/init.rs:311-314`; `xtask/src/compositor.rs:4360` |
| The btrfs root | The first boot unpacks the initramfs onto the volume. Later boots re-unpack only a changed archive. **A file the archive carries is replaced; a file it does not carry is kept.** | `kernel/src/fs/root_disk.rs:25-31` |
| getty | `setsid`, takes the terminal, then execs `$SHELL` as a login shell. "There is no `login` yet." | `userland/init/getty/src/main.rs:7-11`, `:78-84`; `userland/init/units/getty@.service` |
| init | Reads `User=` from `/etc/passwd`. Its control socket is `0666`, and `SO_PEERCRED` decides who may change state. A user may make a scope only under their own `user-<uid>.slice`. | `userland/init/init/src/spawn.rs:446-487`; `userland/init/init/src/control.rs:5-6`, `:67`; `docs/INIT.md` §10 |
| The desktop | hyprix is linked into the kernel as its init, so it and every client run as uid 0. Moving it under init is `docs/INIT.md` L10, not started. | `xtask/src/compositor.rs:1237-1239`; `docs/INIT.md:810`, `:887` |
| The lock | `ext-session-lock-v1`. Any client may take the lock. Only the client that holds it may unlock. A lock whose client died stays locked, and **a second client is refused even then**, so a crashed locker needs a reboot. | `userland/compositor/server/src/client.rs:3670-3686`, `:3745-3749`; `userland/compositor/hyprix/src/state.rs:1048-1058`, `:5351-5361`, `:5406-5417` |
| hyprlock (unlanded) | Authentication through one trait, `auth::Backend::check(secret) -> Verdict`. On Ferrix today its backend is `Missing`, and only `SIGUSR1` unlocks. | branch `hyprlock`, `userland/compositor/hyprlock/src/auth.rs:1-23`, `:56-61`; its `docs/DESKTOP-CLIENTS.md` §5.2 |
| ferrousli | `getspnam_r` reads `/etc/tcb/<name>/shadow` or `/etc/shadow`, as musl does. `crypt` does DES, MD5, `$5$` and `$6$`. Blowfish gives `"*"`. There is no yescrypt and no Argon2. | `userland/ferrousli/src/shadow.rs:1-12`; `userland/ferrousli/src/crypt.rs:1-27` |
| Busybox | Built without PAM, with shadow passwords and libc's `crypt`, sha512 by default. `login`, `su`, `passwd`, `chpasswd`, `vlock` and `adduser` are built, and all are linked in `/bin`. | `~/.local/share/ferrix/busybox/ferrousli/src/busyboxconfig`; `xtask/src/initramfs.rs:237`, `:243`, `:253`, `:260` |
| ssh | `sshdt`, key-only. "`sshdt` given no key and no password accepts anyone", which is why every boot authorizes a key. | `xtask/src/ssh.rs:10-31`; `docs/ROADMAP.md:3491-3526` |

Two rows need action whatever the customer decides about the rest.

**`process_create` makes root processes for anyone.** Read together, the
rows on kernel-made processes and on jobs from paths say this. A uid-1000
program in a `Delegate=yes` service can get `MANAGE` on its own job and
make a VMO. `process_create` in that job then gives it a process running as
root. `test-init` already runs such a service (`docs/INIT.md:1263`). I have
read this and not booted it. Slice P0 (§7) is the fix: the child takes its
creator's credentials, as a fork child does (`process.rs:433`). It comes
with a boot check that makes the escalation and requires the refusal.

**An empty second field in `/etc/passwd` means no password** to busybox's
`login` and `su`. Every `/etc/passwd` Ferrix writes must keep `x` there, and
the store (§5) must never write a hash that matches an empty string.

---

## 2. Threat model

### 2.1 What is protected

| Id | Asset | Where it lives |
|---|---|---|
| AA.STORE | Credentials at rest: password hashes, and later TOTP seeds and FIDO2 public keys | `/var/lib/ferrix/auth`, on the btrfs root |
| AA.TRANSIT | A secret in transit between processes: keyboard to compositor to lock screen to `authd`, console to `login` to `authd` | kernel evdev and tty buffers, the Wayland socket, the auth socket |
| AA.MEMORY | A secret in the memory of `authd`, `login`, `hyprlock` and `passwd` while it is checked | their address spaces |
| AA.SESSION | A locked session: what is on the screen and what the keyboard reaches | hyprix |
| AA.VERDICT | The link between "authd accepted" and what is then allowed: an unlock, a uid change | the auth socket, and the seat channel (phase 2) |

These are the environment's assets, not the certified item's. The
Security Target's assets (`SECURITY-TARGET.md` §3.1) are memory, the
kernel, handles, devices and processor time, and this design relies on all
five.

### 2.2 Who attacks, and what stops them

The Security Target's threat agent is "unprivileged code running on the
TOE" (§3.2, lines 105-107). This design adds people, and it splits code by
the uid it runs as. The phase 1 and phase 2 columns differ because phase 1
leaves the desktop running as root.

| Agent | Wants | Phase 1 (desktop is root) | Phase 2 (desktop is a user) |
|---|---|---|---|
| **TA.WALKUP** A person at a *locked* screen, with the keyboard, the pointer and a USB port | In | The lock takes the keyboard (hyprix, stage 18). Only `authd`'s verdict opens it. The throttle (§3.5) makes guessing slow. A USB keyboard that types guesses gets the same throttle. | Same, and the compositor unlocks only on `authd`'s grant (§3.7). Crashing the locker leaves the screen locked, and a new locker may take over. |
| **TA.WALKUP'** The same person at an *unlocked*, unattended screen | Keep access later | `passwd` asks for the old password first, so they cannot change it. They can do anything else root can: phase 1 does not defend this. | `passwd` and becoming root both ask for a password. They can run anything as the user, and that is out of scope (§2.3). |
| **TA.CLIENT** A compromised desktop client | The password, or an unlock | It is root and can read the store. Phase 1 does not defend this, and says so. | Same uid as the session. It cannot read the store (`0700 auth`) or `authd`'s memory (no `ptrace`, §1). It cannot forge the grant (the seat channel is init-routed, §3.7). It can guess only at the throttle's rate. It can kill hyprix, which ends the session (§6.4) rather than unlocking it. **It can draw a fake lock screen and phish**, which no design on a same-uid desktop prevents (§2.3). |
| **TA.NET** A network attacker, once ssh or a network login exists | A shell | `sshdt` stays key-only (`xtask/src/ssh.rs:29-31`). `authd` listens on no network socket. | Password or keyboard-interactive ssh goes through `authd` (phase 3), with the same throttle and audit. Until then it stays off. |
| **TA.ROOT** A Linux-ABI program running as root | Everything | Out of reach by design. Root reads any file and can replace `authd`. What still holds: the hashes are Argon2id, so a stolen store costs a lot of work per guess (§5.1). | Same. Phase 2 makes root rarer: no desktop client runs as root. |
| **TA.OFFLINE** Someone with a copy of the disk (`build/root.img`, the DK1's SD card) | Passwords, which people reuse | Argon2id with a per-user salt. Nothing else: there is no disk encryption. | Same. |
| **TA.DRIVER** A compromised ring-3 driver | Keystrokes, secrets in memory | x86-64 and AArch64: its IOMMU domain confines it (`ARCHITECTURE.md` §7). ARMv7-A and the DK1: it can read all memory, including `authd`'s, and the boot says so (`ARCHITECTURE.md:340-357`). The input driver sees every keystroke by its nature: it is in the trusted base of any password typed. | Same. |

### 2.3 Out of scope, said so

* **Phishing by code that runs as the user.** A program that runs as you
  can draw a window that looks like the lock and ask for your password.
  Only a trusted path, a key combination the compositor alone answers, can
  fix that, and even that only helps people who use it. §6.5 leaves room for
  one.
* **Root.** Nothing on a Unix defends against root. This design keeps root
  rare, and its audit log is only as trustworthy as root is.
* **Disk encryption, secure boot and measured boot.** `A.FIRMWARE` and
  `A.PHYSICAL` (`SECURITY-TARGET.md` §3.3) carry these, as they do today. A
  credential-sealed disk key is a later design, and §5.1's store format
  leaves room for it.
* **Side channels between processes on one core** (`SECURITY-TARGET.md`
  §9.5, V-06). Argon2id's first pass does not depend on the data, which is
  the reason to prefer `id` over `d`. Nothing more is claimed.
* **Denial of service by locking.** Anyone in a session may lock it. Locking
  is harmless: the owner types their password.

---

## 3. Architecture

### 3.1 Three ways to do it, weighed

| | **A. PAM-style modules in each program** | **B. A pam_unix-compatible `/etc/shadow`** | **C. A dedicated service (recommended)** |
|---|---|---|---|
| Who reads hashes | Every program that authenticates, or a set-uid helper it runs (`unix_chkpwd`) | Every program that authenticates | `authd` alone |
| Ported programs | Need a PAM library and its modules. Ferrix's programs are static, so the modules would be linked in, not loaded. | busybox `login`, `su` and `passwd` work unchanged, and so does any `getspnam` program | Ferrix's own `login`, `su` and `passwd`. Ported PAM programs through a shim (§4.3). |
| Hash | Any the module links | Only what the C library's `crypt` reads: `$6$` (sha512crypt) on musl and ferrousli (`crypt.rs:6-14`). That is not memory-hard. | Argon2id, memory-hard (§5.1) |
| Throttle and lockout | Each program's own. `pam_faillock` needs a shared, writable tally file. | None | One, per account, across every service |
| Audit | Each program's own log lines | None | One log, one format |
| A lock screen running as the user | Needs a set-uid helper, because the user cannot read the hashes | Same | Asks over a socket. Needs no privilege. |
| A new method (TOTP, FIDO2) | A module in every program, and its secrets readable by all of them | Not possible | One method in `authd`. Clients see only prompts. |
| Fits Ferrix's layout | No: it spreads the most sensitive code into every program | No: it is Linux's layout, not Ferrix's | Yes: a ring-3 service in its own cgroup, identified by the kernel, reached like any other |
| Cost | Largest: a PAM ABI, modules, and policy files that most people get wrong | Smallest: about a day | About 27 points for phase 1 (§7) |

B is what hyprlock's interim `Shadow` backend already does, and the
customer declined it as a policy. It is also a dead end as a mechanism. It
fixes the hash to the one the C library knows, and it needs every checker to
be root. It has nowhere to put a throttle. And it cannot grow a second
factor.

A is Linux's answer, and its reason for existing does not apply here. PAM
lets one binary load another vendor's module at run time, and every Ferrix
program is built from this tree. Its costs do apply: secrets in every
process's memory, a set-uid helper, and a per-program throttle.

C costs a daemon, a protocol and a store format. In return, the secret goes
to one process and the verdict comes back. The hash never leaves `authd`, a
new method is one change, and the lock screen needs no privilege. It is how
macOS (`opendirectoryd`), Windows (LSASS) and systemd-homed arrange the same
thing, and it is the shape `docs/ARCHITECTURE.md` gives any function that
needs no ring 0. **Recommended.**

### 3.2 `authd`

| | |
|---|---|
| Program | `/sbin/authd`, std Rust on `*-linux-musl`, like init (`docs/INIT.md` §2) |
| Crate | `userland/auth/authd`, in a workspace `userland/auth/` beside `userland/init/` |
| Runs as | its own user, `auth`, with a fixed system uid (§9, decision 10). It needs no root: it reads its own store, and the programs that change uid (`login`, `su`) are the ones that are root. |
| Unit | `auth.service` (`Type=notify`, `User=auth`, `MemoryMax=` enough for one hash and a little more, `NoNewPrivileges=yes` once L13 lands) and `auth.socket` (`ListenSequentialPacket=/run/ferrix/auth`, `SocketMode=0666`), which init already supports (`libs/init/svc/src/kind/socket.rs:19-24`, `:83`) |
| Without init | Under the phase 1 desktop, where hyprix is pid 1, `exec-once = /sbin/authd` starts it as root. It binds the socket and then drops to `auth` with `setresuid`. It is the same binary and the same socket, so no client can tell the difference. |
| Offers | `ferrix.auth.seat` in the directory (phase 2, §3.7) |

One connection carries one conversation, as one PAM handle does. A client
that wants two, such as fingerprint and password at once (hyprlock's
upstream does this), opens two connections.

### 3.3 The protocol

Records on a `SOCK_SEQPACKET` socket, one per packet, at most 4 KiB. They
are fixed little-endian layouts in `libs/proto/auth-proto`, which allocates
nothing, as the directory's records do (`libs/proto/native-abi/src/directory.rs`),
so a native client could use it later. The conversation is PAM's own, so
the PAM shim of §4.3 is a direct translation:

```
client -> authd
  HELLO     version
  BEGIN     service, account (empty: the peer's own), method hint (empty: policy's)
  RESPOND   bytes                       an answer to the last PROMPT
  CANCEL

authd -> client
  PROMPT    secret | visible, text      PAM_PROMPT_ECHO_OFF / _ON
  INFO      text                        PAM_TEXT_INFO   ("2 attempts left before a 30 s wait")
  ERROR     text                        PAM_ERROR_MSG
  ACCEPTED  account, uid                the conversation is over
  FAILED    text, retry_after_ms        the conversation is over; ask again after the delay
  UNAVAILABLE text                      no verdict could be had: no such service, no credential set,
                                        the store unreadable. Never a silent yes.
```

`passwd` is a service like the others. Its conversation asks for the old
secret, the new one, and the new one again, and ends ACCEPTED once the store
is written. `authctl status <account>` is a one-record request, answered
only to root and to the account itself. It says whether a credential is set,
which methods exist, and until when the account is throttled. It never
reveals a hash.

What a verdict does not carry: a token for the client to hand on. `login`
and `su` are root, and they act on ACCEPTED themselves. The lock is the one
place where the verdict must reach a third party, and there `authd` tells
that party directly (§3.7) rather than trusting the client to carry it.

### 3.4 How a client proves who it is

**Over the socket: `SO_PEERCRED`.** `authd` reads the peer's pid, effective
uid and effective gid once, at accept. The uid decides what the peer may
ask. The pid is used only for the audit line and to read
`/proc/<pid>/cgroup` for it, because a pid can be reused and is never
evidence. The rules:

* **Any peer** may authenticate *as its own uid* for a service whose policy
  says `Account=self`: the lock screen, `passwd` on one's own password.
* **Only a root peer** may name another account. That covers `login` (root,
  from getty) and `su` (set-uid root, so its effective uid is 0).
* **Only a root peer or `auth` itself** may set another account's
  credential, reset a throttle, or read another account's status.

The Ferrix difference in §1 matters here. The peer's ids are the ones its
*socket was made with*, not the ones it had at `connect`
(`kernel/src/fs/socket.rs:773-779`). A program that makes the socket as
root, drops to uid 1000 and then connects is still root to `authd`. So is a
uid-1000 program that was handed a socket a root process made. Both need
root's help first, so neither gives an attacker anything it could not do
with root. But the rule above is written so that being root lets a peer
*ask*, and never lets it *skip* a check: a root peer still types the
password. The kernel should take the ids at `connect`, as Linux does.
That is filed as slice K-E, which is small and not needed first.

**Over a native channel: the unit.** A channel says nothing about its
writer, so a native client gets no uid. What it gets is better for a
service: init routes an OPEN only from a unit whose file lists the name in
`Uses=`, and its CONNECT names that unit (`docs/INIT.md` §6, `:1395-1405`).
`authd` uses this for exactly one thing, the seat channel (§3.7), which
only `hyprix.service` may open.

### 3.5 Throttling, not lockout

Every failure is counted per **account**, not per peer, so a thousand
connections guess no faster than one:

* Each failed attempt costs a fixed `FailDelaySec=` (2 s, pam_unix's) before
  FAILED is sent. hyprlock already waits for it, and will use
  `retry_after_ms` instead of its own fixed delay. Until that FAILED has gone
  out, no attempt on the account is looked at, from any connection.
* From the fourth consecutive failure, the next attempt is refused before it
  is checked until `2^(n-3)` seconds have passed, capped at 300 s. INFO says
  so in words ("wait 16 s"), and hyprlock shows it as `$PAMFAIL`, as it
  shows `pam_faillock`'s text today.
* A success resets the count. The count is kept in the store (§5.2) with
  `fsync`, so restarting `authd` or rebooting does not reset it.
* An unknown account is checked against a dummy hash with the same
  parameters, and fails with the same text in the same time. It is also
  counted and throttled as an account is, in a bounded table in memory keyed
  by a keyed hash of the name, so that from the fourth failure on it answers
  "wait" just as an account does (ferrix-55's review). A caller learns nothing
  from the answers about which accounts exist, and naming accounts makes no
  file. A restart of `authd` forgets those tallies and not an account's, and
  only root can restart it.
* Hashing is serialized: one Argon2id at a time for the whole machine. That
  bounds `authd`'s memory to one hash's (§5.1), and makes guessing through
  many services no faster.

**No permanent lockout by default.** On a desktop with one person, a lockout
is a denial of service against that person, and anyone who can reach the
lock screen can trigger it. The delay cap achieves what a lockout is for: at
one attempt per 300 s, a 10,000-word guess list takes a month. `authctl
reset <account>` (root) clears a throttle. A policy may still ask for a hard
lockout per service with `LockoutAfter=` (decision 6).

### 3.6 Audit

One line per conversation's end, and one per credential change:

```
auth: service=hyprlock account=ferrix peer=pid:812,uid:1000 unit=session-1.scope method=password result=failed failures=4 wait=2s
auth: service=passwd account=ferrix peer=pid:903,uid:1000 unit=session-1.scope result=changed
```

The line never holds the secret, its length or its hash. It goes to
`authd`'s standard output, which init keeps in its per-unit log
(`svc log auth`, `docs/INIT.md` §10). It is also appended, with `fsync`, to
`/var/log/ferrix/auth.log` (`0600 auth`), capped at 1 MiB and rotated once.
Root can edit both, which §2.3 already says. This is FAU outside the item,
as the Security Target's §9.1 expects.

### 3.7 Who may unlock: the compositor decides, on `authd`'s word

Today the client that holds the lock decides alone: `unlock_and_destroy`
from it gives the screen back (`userland/compositor/server/src/client.rs:3745-3749`,
`userland/compositor/hyprix/src/state.rs:5406-5417`). While every client is root,
that costs nothing, because any client could do worse anyway. Once the
desktop is a user's, it matters: the lock holder would be the one process
between a same-uid attacker and the session.

So in phase 2:

1. `authd.service` has `Offers=ferrix.auth.seat`, and `hyprix.service` has
   `Uses=ferrix.auth.seat`. Init routes the channel (`docs/INIT.md` §6), so
   hyprix knows the other end is `authd` without taking anyone's word for
   it, and no other unit can open the name.
2. When a conversation for a service with `Grant=seat` ends ACCEPTED,
   `authd` sends `GRANT { uid, at }` down the seat channel. "The lock
   screen's user typed their password at 12:00:03."
3. hyprix honours `unlock_and_destroy` only when it has a grant for the
   session's own uid that arrived after the lock was taken and in the last
   30 s. It then uses the grant up. An unlock without a grant is refused:
   the lock stays, the client is told it lost it (`finished`), and the
   screen stays locked. That is the state a dead locker leaves today.
4. **A lock whose client died, or which was refused, may be taken over by a
   new lock client.** Today hyprix refuses every second lock, even one
   replacing a dead client (`state.rs:5351-5361`), so a crashed hyprlock
   means a reboot. With grants, a new client that takes over still cannot
   unlock without the password. This is Hyprland's
   `misc:allow_session_lock_restore`, made safe.

hyprlock's two other ways out map onto this:

* **`--grace N`** (any key unlocks within N s of locking) becomes hyprix's
  own setting, `misc:lock_grace`, which hyprix enforces from when *it* took
  the lock. Its default is 0. hyprlock's `--grace` works up to hyprix's
  limit and no further, so the client cannot extend it.
* **`SIGUSR1`** (unlock from a script) becomes `authctl unlock-seat`, which
  only root may send. It makes `authd` send a grant, and the audit log says
  who asked. An admin who reached the machine by ssh can still let a
  locked screen go, and the log shows they did.

In phase 1 there is no seat channel, because there is no init above hyprix,
and hyprix keeps honouring the lock client. §2.2's phase 1 column says what
that leaves open, which is nothing that root could not already do.

### 3.8 Secrets in memory

* **One secret type.** `libs/proto/auth-proto` gives `Secret`: a
  fixed-capacity buffer (256 bytes, the most any method needs) that is never
  `Clone` or `Debug`. It is zeroed with volatile writes and a compiler fence
  on drop. `authd`, `login`, `passwd`, `su` and hyprlock hold typed secrets
  only in it. A fixed buffer is not moved by a `Vec` growing, which is how
  copies of a secret usually survive in Rust.
* **Argon2id's working memory** is zeroed after each hash, before it is
  freed. It is the largest copy of anything derived from the password.
* **No swap, no core dumps** (`kernel/src/syscall/memory.rs:387`), so
  nothing writes a secret's page to disk. `mlock` is `ENOSYS`
  (`docs/BACKLOG.md`). `authd` calls it anyway and ignores the error,
  so it is right the day the kernel grows swap. Accepting it as a no-op is
  slice K-D.
* **The kernel's copies.** A password passes through the tty's line buffer
  or evdev's queue, then the Wayland socket's buffer, then the auth
  socket's. A frame is zeroed when it is handed out again (O.SCRUB). A
  kernel heap allocation is not, so a freed socket buffer holds its bytes
  until reused. Nothing in user space can read it back without a kernel
  bug. Zeroing the socket, pipe and tty buffers when they are freed is slice
  K-C, as defence in depth.
* **`/proc/<pid>`** of hyprlock, `login` and `authd` should not be readable
  by the same uid. `PR_SET_DUMPABLE 0` is how Linux programs ask for that,
  and Ferrix records it and ignores it (§1). Slice K-B makes procfs honour
  it, and makes a set-id `execve` or an id change clear it, as Linux does.
  It matters little today, because `fd` entries are plain links (§1). It
  matters as soon as anything like `/proc/<pid>/mem`, `environ` or Linux's
  magic `fd` links arrives, and the rule is cheaper before them than after.

---

## 4. Versatility

### 4.1 Methods

A method is one Rust trait in `authd`: given the account's stored
parameters, run a sub-conversation (prompts in, responses out) and say
accepted or not. The store holds a line per method an account has (§5.2),
and the service policy says which are needed.

| Method | Stored | Phase | Needs |
|---|---|---|---|
| `password` | Argon2id PHC string | 1 | nothing |
| `password` legacy import | `$6$` / `$5$` string | 1 | the SHA-2 crypt code hyprlock already tested against Drepper's vectors (branch `hyprlock`, `userland/compositor/hyprlock/src/crypt.rs`), moved into `authd`. It verifies an imported hash, then rewrites it as Argon2id on the first success. |
| `totp` | RFC 6238 seed, digits, period | 3 | HMAC-SHA-1 and a clock that is right, which on a board without a battery means NTP first (`ntpd` is in busybox) |
| `fido2` | credential id and COSE public key, per key | 3 | CTAP2 over USB HID. `native/drivers/usbhid` exists, and the DK1 has USB host (`kernel/src/platform/st/stm32mp1/usb.rs`). A hidraw-style path from that driver to `authd` is the unsized part. |
| `fingerprint` | a reader's template handle | later | a reader driver. There is none. |
| `sshkey` | nothing: ssh keys stay in `~/.ssh/authorized_keys` | 3 | `authd` only records and throttles an ssh login's verdict, which `sshdt` makes itself. |

### 4.2 Policy per service

A service is a file named after it, in the three layers units use:
`/lib/ferrix/auth/services/` from the image, `/etc/ferrix/auth/services/`
from the admin, and `/run/ferrix/auth/services/` at run time. A file in a
higher layer replaces one of the same name, and a `<name>.d/*.conf` drop-in
changes keys in it (`docs/INIT.md` §4.1). The parser is `libs/init/svc`'s
(`libs/init/svc/src/ini.rs`), so a policy with a mistake is a warning in the same
words as a unit with one.

```ini
# /lib/ferrix/auth/services/hyprlock
[Service]
Description=Unlock the screen
Account=self          # only the peer's own uid
Methods=password      # later: "password totp" (both), "fido2|password" (either)
Grant=seat            # tell the seat owner (§3.7)
FailDelaySec=2

# /lib/ferrix/auth/services/login
[Service]
Description=Log in on a terminal
Account=any
Callers=root          # getty's login is root; nobody else may name an account
Methods=password
FirstPassword=local   # an account with no credential may set one here, on a local console only (§5.4)

# /lib/ferrix/auth/services/su
[Service]
Description=Become root
Account=caller        # authenticate the person asking, not the target (decision 5)
Callers=root          # su is set-uid root
Methods=password
TargetGroup=wheel     # the caller must be in wheel to become root

# /lib/ferrix/auth/services/passwd
[Service]
Account=self
Methods=password      # the old one first, then the new one twice
```

| Service | Who asks | Account | Methods | Phase |
|---|---|---|---|---|
| `hyprlock` (or `auth:pam:module`) | the lock client | self | password; later + fingerprint or FIDO2 | 1 |
| `passwd` | Ferrix's `passwd` | self; root for anyone | password | 1 |
| `login` | Ferrix's `login` on a getty | any, from a root caller | password | 2 |
| `su` | Ferrix's `su` | the caller, for a wheel member | password | 2 |
| `greeter` | a graphical login | any, from `sessiond` | password | 3 |
| `polkit`-style prompts | a privileged service asks for the session user's consent (§4.5) | the session user | password | 3 |
| `sshd` | `sshdt`, keyboard-interactive | any, from a root caller | password + TOTP | 3 |

An unknown service name is UNAVAILABLE, never a default policy, so a typo
cannot open a door.

### 4.3 Programs that expect PAM or `/etc/shadow`

Three kinds of program, and a different answer for each:

1. **Programs this tree writes.** Ferrix's own `login`, `su` and `passwd`
   (§7) speak the protocol. The busybox applet links of those three names in
   `/bin` (`xtask/src/initramfs.rs:237`, `:243`, `:253`) are replaced by
   Ferrix's programs, as uutils and zinc already replace busybox applets
   (`docs/UUTILS.md`). The same goes for `chpasswd`, `cryptpw`, `mkpasswd`,
   `adduser` and `vlock`, which would otherwise write or read hashes
   themselves: they are unlinked, or, for `vlock`, replaced with a small
   client.
2. **Ported programs that use PAM**: `sudo`, OpenSSH, `swaylock`, upstream
   hyprlock's C++. A **PAM shim in ferrousli** gives them `pam_start`,
   `pam_authenticate`, `pam_acct_mgmt`, `pam_chauthtok`,
   `pam_open_session`, `pam_close_session`, `pam_end`, `pam_get_item`,
   `pam_set_item` and `pam_strerror`, with Linux-PAM's ABI. Its one "module"
   is the protocol: `pam_start(service, user, conv)` is BEGIN, and each
   PROMPT, INFO and ERROR is one message to the program's `conv` callback.
   It loads no modules and reads no `/etc/pam.d`, because `authd`'s policy
   is the policy. The work is about 5 points, in phase 3 (ferrousli stream).
3. **Ported programs that read `/etc/shadow` directly**, through
   `getspnam` and `crypt`, and static musl programs (Alpine's busybox)
   whatever they call. **Refused, safely**: there is no `/etc/shadow`, so
   `getspnam` finds nothing and the check fails. No empty password is ever
   the result, because `/etc/passwd` says `x` (§1). A shim that answers
   `getspnam` with a marker hash, which ferrousli's `crypt` then checks with
   `authd`, would make busybox's `login` and `su` work over the service
   (busybox is built against ferrousli, `docs/BACKLOG.md` "The busyboxes").
   It is possible, and about 2 points. I recommend against it (decision 7):
   it keeps a second, quieter path into authentication, and Ferrix's own
   programs cover the three that matter.

A native program (no libc) links `libs/proto/auth-proto` and speaks the same
records over the socket, or over a directory channel if its unit is given
`Uses=ferrix.auth`. That name is reserved and not needed yet.

### 4.4 hyprlock's configuration, mapped

The hyprlock stream answered the questions this section depends on
(2026-09-26). Its `check()` already runs on its own thread, and its
`$PAMPROMPT` is hard-coded today. It asks for this interface:

```rust
pub trait Backend: Send + Sync {
    /// Start a conversation. The first prompt's text is known before the
    /// person types, which is what `$PAMPROMPT` shows at lock time.
    fn begin(&self) -> Result<Prompt, Verdict>;
    /// Answer the last prompt.
    fn respond(&self, secret: &Secret) -> Next;   // Prompt | Accepted | Failed | Unavailable
}
```

| hyprlock.conf | On Ferrix |
|---|---|
| `auth { pam { enabled = true } }` | the `Service` backend: a connection to `/run/ferrix/auth`, BEGIN with the service name below and the peer's own account |
| `auth:pam:module = hyprlock` | the service name, so the policy is `/etc/ferrix/auth/services/hyprlock`. A module with no policy file is UNAVAILABLE, and hyprlock says so. |
| `$PAMPROMPT` | the first PROMPT's text, fetched at lock time by `begin()`. hyprlock re-prompts only when the text changes, as upstream's `Pam.cpp` conversation does. |
| `$PAMFAIL`, `$FAIL` | FAILED's text, or an INFO received since the last prompt. "wait 16 s" is shown the way `pam_faillock`'s "left to unlock" is. |
| the 2 s refusal hold | `retry_after_ms` from FAILED, in place of hyprlock's fixed delay |
| `$ATTEMPTS` | hyprlock's own count, unchanged |
| `auth { fingerprint { enabled = true } }` | a second connection, BEGIN with method hint `fingerprint`, run beside the password conversation. Whichever is ACCEPTED first unlocks, as upstream does. Until a reader exists it is UNAVAILABLE and hyprlock logs it and turns it off, as it already does. |
| `fingerprint:ready_message`, `present_message` | INFO texts the fingerprint method sends |

Phase 1 replaces `Missing` with `Service`. `Missing` stays only as the
verdict when the socket is absent: UNAVAILABLE, "no authentication service
is running". `Shadow` leaves the binary, and its tested rules move into
`authd`'s legacy import. The gate's `Hashed` and `/etc/hyprlock/gate.hash`
are replaced by a gate image that seeds a real store entry (§5.3), so the
gate tests the path people use.

### 4.5 Privilege prompts (phase 3)

A privileged service sometimes needs the session user's consent: changing
the network, mounting a disk, or a non-root `svc stop`, which init refuses
today (`docs/INIT.md` §10). polkit's shape fits Ferrix's directory well:

1. The service sends `authd` `ASK { account: the session user, action,
   reason }` over its own directory channel, `Uses=ferrix.auth.ask`.
2. `authd` sends the prompt to the session's *agent*, a small client in the
   session that registered with `Offers=`-like consent. It is drawn by
   hyprix's session, like hyprlock is. The agent runs a normal conversation
   (§3.3) for service `polkit`.
3. `authd` answers the service yes or no. The service never sees the
   password, and the agent never gets the privilege.

The actions and who may consent to them are policy files, like services.

---

## 5. Credentials at rest

### 5.1 The KDF: Argon2id

| | Argon2id | yescrypt (`$y$`) | sha512crypt (`$6$`) |
|---|---|---|---|
| Memory-hard | yes | yes | no: a GPU runs many at once |
| Specified by | RFC 9106 (2021), with test vectors | a reference implementation and a draft | Drepper's 2008 text |
| On Ferrix today | nothing | nothing: ferrousli's `crypt` gives `*` for it | ferrousli's `crypt` (`crypt.rs:11-12`); hyprlock's Rust copy |
| Readable by `crypt(3)` programs | no | glibc's libxcrypt only | yes |
| Data-independent first pass (§2.3) | yes (the `id` part) | no | not applicable |

Being readable by `crypt(3)` is sha512crypt's only advantage, and §4.3 says
no program reads the hash. Of the two memory-hard hashes, Argon2id has the
RFC, the test vectors, and the side-channel property. **Argon2id**, written
as a PHC string (`$argon2id$v=19$m=…,t=…,p=…$salt$hash`), with a 16-byte salt
from `getrandom` and a 32-byte output.

**Written here, in `libs/crypto/argon2`**: BLAKE2b and Argon2id, `no_std`,
no `unsafe`, host-tested against RFC 9106 §5's vectors and BLAKE2's, under
Miri, with a fuzzer on the PHC-string parser. That is `docs/ARCHITECTURE.md`
§9's rule (byte logic in `libs/`, where every tool reaches it), and it is
what hyprlock already did for SHA-2. The alternative is RustCrypto's
`argon2` crate, which userland is free to use (the compositor workspace
already pulls 42 external crates), and is decision 2.

**Parameters, chosen when a password is set, and stored with it.**
`authd` measures the machine it runs on and picks the memory cost that takes
about the target time, between a floor and a ceiling:

| | x86-64, AArch64 | ARMv7-A (DK1: 2 × Cortex-A7, 512 MiB) |
|---|---|---|
| Target time for one check | 0.5 s | 1 s |
| Floor (OWASP's minimum) | m = 19 MiB, t = 2, p = 1 | the same |
| Expected choice | m = 64 MiB, t = 3, p = 1 (RFC 9106's second recommendation, with p = 1) | about m = 19-32 MiB, t = 2 |
| Ceiling | m = 256 MiB | m = 64 MiB |

**What one check took**, measured with `libs/crypto/argon2`'s
`examples/timing` (the fastest of three runs):

| Where | Floor (19 MiB, t = 2) | 64 MiB, t = 3 |
|---|---|---|
| The build host, natively (Ryzen 9 9900X), which a KVM guest runs at | 38 ms | 213 ms |
| `qemu-aarch64` as a Cortex-A72, under TCG | 146 ms | 734 ms |
| `qemu-arm` as a Cortex-A7, under TCG | 201 ms | 1064 ms |
| The DK1 (2 × Cortex-A7 at 800 MHz) | **estimate:** 0.4 to 0.6 s | **estimate:** 2 to 3 s |

Read on 2026-09-26, with the host's load between 30 and 40 from other
sessions, so the emulated rows are upper bounds. The emulator rows are
the emulator's speed, not a Cortex-A7's: TCG on a fast host runs 32-bit
Arm code far quicker than an 800 MHz core does. The DK1 row is worked out,
not measured, and stays an estimate until the board is free to time. One
block is about 6,000 instructions of 64-bit arithmetic done in 32-bit
halves, at about one instruction a cycle, and the floor is 38,912 blocks.
So an x86-64 or AArch64 machine lands well above the floor at 0.5 s, and
the DK1 at about the floor for its 1 s. That was the table's prediction,
and the floor holds on every target. Each `authd` also says what the floor
took when it first sets a password (`authd: argon2id at the floor ...`), and
`cargo xtask test-auth` prints that line for every architecture it boots.

`p = 1` because `authd` checks one password at a time (§3.5), so a second
lane buys nothing. Because each hash carries its own parameters, a store
copied from x86-64 to the DK1 still verifies there, only more slowly. When
the floor rises, `authd` rehashes on the next success, as it does for a
`$6$` import.

**Emulation.** Gates run under TCG, which can be tens of times slower. A
gate image's seeds (§5.3) use the floor parameters, so a check under
emulation takes seconds, not minutes, and only those seeds use them.

### 5.2 The store

```
/var/lib/ferrix/auth/            0700 auth:auth
    users/<name>                 0600 auth:auth   the credential record
    state/<name>                 0600 auth:auth   failure count, throttle deadline, last success
/var/log/ferrix/auth.log         0600 auth:auth   §3.6
```

**On the root volume, never in the initramfs.** The btrfs root replaces
every file the archive carries whenever the archive changes
(`kernel/src/fs/root_disk.rs:25-31`). A password set with `passwd` would be
lost at the next rebuild if the archive carried the store. Because nothing in
the archive lives under `/var/lib/ferrix/auth`, the volume's copy survives
every rebuild, as a user's other files do. On a tmpfs root (test boots,
`--tmpfs-root`) the store starts empty every boot, which is right for a
machine that forgets everything.

**The record, a line per fact**, written only by `authd`:

```
format 1
account ferrix 1000
password $argon2id$v=19$m=65536,t=3,p=1$c2FsdHNhbHRzYWx0c2FsdA$…
changed 2026-09-26T17:40:00Z
```

Later methods add lines: `totp <label> <seed> <digits> <period>`,
`fido2 <label> <credential-id> <cose-key>`. An account may be `locked` (a
line `locked <why>`), in which case no method opens it. One whose record is
absent has **no credential**, which is not the same as an empty one and
never opens anything (§5.4).

**Checked against `/etc/passwd` at every use.** The record names the account
and its uid. If `/etc/passwd` gives that name another uid, `authd` refuses
with UNAVAILABLE and logs it. A uid given to a new account never inherits an
old account's password.

**Written atomically**: a new file beside the old one, `fsync`, `rename`,
`fsync` of the directory. Failure counts go in `state/`, not `users/`, so a
wrong guess never rewrites the credential file. One file per account, as
tcb's `/etc/tcb/<name>/shadow` (`userland/ferrousli/src/shadow.rs:4-5`), so one
account's change cannot damage another's.

### 5.3 Provisioning

| Moment | How a credential comes to exist |
|---|---|
| **Building an image** | `cargo xtask run … --auth-seed <account>` prompts on the host, hashes the password there with the same `libs/crypto/argon2` at the target's parameters, and places the PHC string in the archive at `/lib/ferrix/auth/seed/<account>` (`0600 root`). `--auth-seed-file <account>=<file>` does the same without a prompt, for CI. |
| **First boot** | `authd` imports each seed whose account has no record yet, and never one that has a record. So a seed sets the first password, and a later `passwd` change survives the next build even though the archive still carries the seed. The import is audited. |
| **A running system** | `passwd` for your own account (the old password, then the new twice). As root, `passwd <account>` for anyone, with no old password. |
| **A fresh image with no seed** | §5.4 |
| **Gates** | A gate image seeds a known test password at the floor parameters. Only gate images carry it, as only the gate image carries `hyprlock-gate` today. |

The seed is a hash and not a password, but a hash is still worth guessing
at. It is `0600 root` in the archive, and it is on disk only where the
image is.

### 5.4 Before any password exists

**No password is never "any password".** An account without a record cannot
be opened by any method.

* **The lock screen.** hyprlock asks `authd` for its own account's status
  (the record `authctl status` uses) at start. With no credential set it does not take the lock, and says why on
  screen and on standard error: "no password is set for ferrix: run
  `passwd` first". The customer's `SUPER+L` then does nothing visible
  instead of locking them out (decision 4).
* **The console.** `login` (phase 2) finds the account has no credential.
  Only on a **local console**, the kind named by `FirstPassword=local` (the
  getty's `TTYPath=`, `/dev/console`, never a `pts`), it offers to set one:
  "ferrix has no password. Choose one now:". This is the first password,
  set by the person with the machine in front of them. It is the same
  physical-presence assumption `A.PHYSICAL` already makes.
* **ssh** never offers a first password. Until a credential exists it is key
  only, as today.
* **root** has no record on a fresh image, so it is locked: nobody logs in
  as root by password. Phase 1's desktop is the exception (decision 3).

---

## 6. The session as a user, not root

### 6.1 Where it stands

hyprix is linked into the kernel as its init (`xtask/src/compositor.rs:1239`),
so it and every program it starts are uid 0. It opens the card and the
`event*` nodes itself, and it can because they are `0660 root`
(`kernel/src/display/mod.rs:447-450`, `kernel/src/input/evdev.rs:237-241`).
`docs/INIT.md` L10 (6 points, not started) moves hyprix under init as
`hyprix.service`, with a scope per client (`docs/INIT.md` §5.6). L10 is
necessary. It is not enough on its own, because hyprix under init still runs
as root unless something gives it the devices.

### 6.2 What it takes

1. **`login` on the console** (P2.3). getty execs `/bin/login` instead of
   the shell (`userland/init/getty/src/main.rs:78-84`). `login` runs the `login`
   conversation. On ACCEPTED it does `initgroups`, `setresgid` and
   `setresuid`, asks init for `session-N.scope` under `user-<uid>.slice`
   (`svc scope`, `docs/INIT.md` §5.6), and execs the account's shell. The
   first-password offer of §5.4 lives here.
2. **A session manager, `sessiond`** (P2.4): root, a unit, the one owner of
   **seat0** (the machine's screens, keyboard, pointer and sound). It is
   logind's device half and seatd's whole job, and no more:
   * It opens the card, the `event*` nodes and `/dev/snd/*`, and passes the
     descriptors to the session's compositor with `SCM_RIGHTS`, which works
     today (`kernel/src/fs/socket.rs:34-40`). The nodes stay `0660 root`.
     **No user or group is given the input nodes**: with read access to
     `event*`, any program in the session could read the lock screen's
     keystrokes. Ferrix has no `input` group today (`evdev.rs:237`), and
     this design keeps it that way.
   * It starts the graphical session: `hyprix` as the account's uid, in
     `user-<uid>.slice/session-N.scope`, after the `login` or `greeter`
     conversation, or at once for an image configured to log a named user in
     automatically (decision 8).
   * It knows which session is active on the seat, which is what a second
     session and user switching would need later.
3. **hyprix takes its devices from `sessiond`** instead of opening
   `/dev/dri/card0` and `/dev/input/event*`, and uses the seat channel of
   §3.7 (P2.5).
4. **Kernel prerequisites**: P0 (native processes take their creator's
   credentials) before any non-root desktop, and K-B (dumpable) with it.
5. **`su`** (P2.6): Ferrix's own, set-uid root, running the `su`
   conversation (§4.2) and then the target shell.

### 6.3 Seats, and who may lock and unlock

* **One seat, one active graphical session**, in phase 2. More than one is
  later, and nothing here assumes it cannot happen.
* **Anyone in the session may lock it.** It is harmless (§2.3).
* **Only the session's own account unlocks it**, through `authd`'s grant.
  An administrator does not unlock another user's session by typing *their
  own* password. `authctl unlock-seat` (root, audited) is the one override
  (§3.7).

### 6.4 When the compositor dies

Under init with `Restart=`, a hyprix that is killed comes back as a *fresh,
unlocked* desktop running as the same user. Killing it would then be the
way past a locked screen, and any same-uid client can send that signal
(`kernel/src/syscall/credentials.rs:70-82`). So `hyprix.service` does not
restart into the same session. **The session ends with its compositor**:
`sessiond` stops the scope (`cgroup.kill`) and the seat goes back to login.
This is what GNOME does on Wayland, and it is the only safe answer.

### 6.5 Now, and later

| Now (phase 2) | Later |
|---|---|
| console `login`, and a desktop started by `sessiond` for one account | a graphical greeter (hyprlock's widgets and layout, speaking the `greeter` service) |
| one seat | several sessions, and switching between them, with device revocation (`EVIOCREVOKE`, dropping DRM master) in the kernel |
| hyprix is the only client of the seat channel | a trusted path: a key combination only hyprix answers, which always shows the real lock or greeter |

---

## 7. Phased plan

In story points, each slice landed and gated on its own, owners by stream.
The gates are the ones `docs/BACKLOG.md`'s "What a landing runs" names for
the area touched, plus the boot named here. Every new boot stage has a
negative control that must be seen to fire, per the repository's rule.

### P0: the kernel hole (2 points, first, independent)

| | Slice | Owner | Gate | Points |
|---|---|---|---|---|
| P0 | `process_create` gives the child its creator's credentials, as `fork` does. A boot check makes a native process as uid 1000 in a delegated job and requires `getuid` in it to be 1000. Its negative control is the old `Credentials::root()`, which must fail that line. | ferrix-15 (was: kernel, native ABI) | the kernel row of the gate table; `test-init --arch all` | 2 |
| P0a | Init refuses `User=` and `Group=` on a `Type=native` unit, failing closed. Init makes a native service's process itself, so such a unit ran as root and the keys were silently ignored (found by ferrix-15 beside P0). **Done 2026-09-26**: the unit, `SupplementaryGroups=` too, loaded as `bad-setting`; P0b replaced the refusal the same day. | ferrix-15 | `test-init --arch all` | with P0 |
| P0b | With init's L11, a native service's process is made by a forked child that has already become the unit's user, so `process_create` (after P0) gives it that user's credentials. **Done 2026-09-26**: `test-init` runs `pong-as-user.service` (`User=ferrix`) and reads uid and gid 1000 in every role from its `/proc/<pid>/status`; the helper left root reads 0. | ferrix-15 | `test-init --arch all` | with L11 |
| P0c | A `Delegate=yes` unit with `User=1000` gets `MANAGE` on its own job through `job_for_cgroup` (`kernel/src/fs/cgroupfs.rs:356-357`), and native `job_set_limit` (`kernel/src/syscall/native.rs:1244`) asks for `MANAGE` alone, so the unit can lift its own `MemoryMax=` or `TasksMax=` to unlimited. Ancestor slices still bound it. The fix: a `SET_LIMIT` job right, granted only to a caller that may write `memory.max`. Found by ferrix-15 and confirmed by ferrix-2c in the code; after ferrix-55's OK. **Done 2026-09-26** (`54cba422`, F-40 closed): the `limits` boot line. | ferrix-15 | `test-init --arch all` | 2 |

P0 and P0c have one shape: `MANAGE` on a job is too coarse a right. A
delegated user holds it for its own subtree, as it must to move its own
processes, and it then reached everything `MANAGE` guards: making a root
process (P0) and lifting its own limits (P0c). Any new call that takes a
job should ask for the narrowest right it needs, not for `MANAGE`.

### Phase 1: `authd`, passwords, and a real hyprlock (27 points)

`authd` does not wait for P0b. It is a Linux-ABI program (std on musl),
started by init's Linux spawn path, which already sets `User=` before
`execve` (`docs/INIT.md` §16, "Spawn"). So `auth.service` runs as `auth`
under init from its first boot. Under today's desktop, where hyprix is pid 1
and there is no init, `authd` starts as root from `exec-once`, binds its
socket, and drops to `auth` itself with `setgroups`, `setresgid` and
`setresuid` before it reads the store (§3.2).

Phase 1 gives hyprlock the path it keeps. The socket, the protocol and the
store are the ones phase 2 uses, and phase 2 changes who runs hyprlock, not
what hyprlock does.

| | Slice | Owner | Gate | Points |
|---|---|---|---|---|
| P1.1 | `libs/crypto/argon2`: BLAKE2b, Argon2id, PHC strings; RFC 9106 and BLAKE2 vectors; Miri; a fuzzer on the parser; the timing table of §5.1 measured on x86-64 KVM, AArch64 and the DK1, and written in | auth | `cargo xtask check`, Miri, fuzz | 5 |
| P1.2 | `libs/proto/auth-proto`: the records of §3.3, their framing, `Secret`; host tests; a fuzzer on the decoder | auth | `cargo xtask check`, Miri, fuzz | 3 |
| P1.3 | `authd`: the socket, `SO_PEERCRED` rules (§3.4), policy files over `libs/init/svc`'s parser (§4.2), the `password` method with Argon2id and `$6$`/`$5$` import-and-rehash, the store (§5.2), seeds (§5.3), throttle (§3.5), audit (§3.6), zeroing (§3.8), `Type=notify`. Host tests run a real `authd` over a temporary root, as hyprlock's `Store::at` does. | auth | `cargo xtask check` | 8 |
| P1.4 | `passwd` and `authctl` (status, reset, unlock-seat, which is inert until phase 2). They replace busybox's `passwd`, `chpasswd`, `cryptpw` and `mkpasswd` links (§4.3). | auth | `cargo xtask check` | 3 |
| P1.5 | hyprlock's `Service` backend and the conversational trait (§4.4). `Shadow` and `Hashed` go. hyprlock refuses to lock an account with no credential (§5.4). | hyprlock | the hyprlock stream's gate | 2 |
| P1.6 | Images and the gate. `--auth-seed` and `--auth-seed-file`. `auth.service` and `auth.socket` in images with init, `exec-once = /sbin/authd` in the desktop's. **`cargo xtask test-auth --arch all`**: a seeded account, a wrong password refused after the delay, the fourth failure throttled, the right one accepted, `passwd` changing it, and the change surviving a reboot on the btrfs root (x86-64, which attaches one). Also: the audit lines are there, no line contains the password, an unknown account fails like a wrong password, and a uid-1000 peer naming another account is refused. **`test-hyprlock`** moves onto the real `authd`. | auth, with hyprlock | `test-auth`, `test-hyprlock` | 5 |
| P1.7 | Documents: the Security Target amendment (§8.1), roadmap and backlog rows, `docs/sysml/` | auth | `cargo xtask check` | 1 |

**What phase 1 gives the customer.** `SUPER+L` locks the desktop, and the
screen opens only for the account's password, checked by Argon2id, throttled
and audited. The password is set at build time or with `passwd`, and survives
rebuilds. What it does not give yet: while hyprix is init, the locked account
is root (hyprlock asks for its own uid's password, `getuid() == 0`), and a
compromised client is root. Decision 3 is whether that is acceptable for
phase 1. hyprlock does not change when phase 2 moves the session to
`ferrix`: its `getuid()` does.

### Phase 2: the desktop as a user (31 points, plus L10's 6)

| | Slice | Owner | Needs | Gate | Points |
|---|---|---|---|---|---|
| L10 | hyprix under init (`docs/INIT.md` §13, already planned) | init | | `test-compositor` under init | (6) |
| P2.1 | K-B: procfs honours `PR_SET_DUMPABLE`, and set-id `execve` and id changes clear it | kernel | | kernel gate | 2 |
| P2.2 | K-C: zero socket, pipe and tty buffers when freed | kernel | | kernel gate | 1 |
| P2.3 | `login`, and getty execs it. First password on a local console. `test-init` gains a stage: log in as `ferrix`, a wrong password refused, `id` says 1000, the session's scope is `user-1000.slice/session-1.scope`. | auth, init | P1 | `test-init --arch all` | 5 |
| P2.4 | `sessiond`: seat0, device descriptors by `SCM_RIGHTS`, starts hyprix as the account in its scope, ends the session with its compositor | session (new) | L10, P0 | `test-compositor` as uid 1000 | 10 |
| P2.5 | hyprix: devices from `sessiond`, the seat channel and grants (§3.7), a new locker taking over a dead lock, `misc:lock_grace` | compositor | P2.4, P1.3 | `test-compositor`, `test-hyprlock` | 6 |
| P2.6 | `su`, set-uid root, the wheel rule | auth | P1 | `test-vfs` (it already becomes `ferrix` with `su`) | 3 |
| P2.7 | Adversary controls in the gates. A client that calls `unlock_and_destroy` with no grant leaves the screen locked. A client that kills hyprix lands at `login`, not on a desktop. A uid-1000 program cannot read `/var/lib/ferrix/auth`. Each has a sabotage that must make it fail. | auth, compositor | P2.4, P2.5 | `test-compositor`, `test-auth` | 4 |

### Phase 3: versatility (about 32 points sized, plus unsized items)

| | Slice | Owner | Points |
|---|---|---|---|
| P3.1 | PAM shim in ferrousli (§4.3) | ferrousli | 5 |
| P3.2 | TOTP method, and `authctl totp enrol` with a QR code (`libs/kernel/qr` exists) | auth | 3 |
| P3.3 | `sshdt` keyboard-interactive and password through `authd`, the `sshd` service | auth, net | 4 |
| P3.4 | Privilege prompts: `ferrix.auth.ask`, an agent in the session (§4.5), and the first user, a non-root `svc stop` | auth, init | 6 |
| P3.5 | Accounts: `useradd`/`userdel`, with `/etc/passwd` generated from `/lib/ferrix/sysusers` plus the store's accounts. The archive stops carrying `/etc/passwd` (§5.2 says why it must). | auth | 4 |
| P3.6 | A graphical greeter on hyprlock's widgets | desktop clients | 8 |
| P3.7 | K-E: `SO_PEERCRED` taken at `connect`, as Linux does | kernel | 1 |
| P3.8 | K-D: `mlock` accepted as a no-op within `RLIMIT_MEMLOCK` (`docs/BACKLOG.md`) | kernel | 1 |
| — | FIDO2 over USB HID (needs a hidraw path from `native/drivers/usbhid`), fingerprint (needs a reader), a trusted path, several seats | | unsized |

### Order

```
P0 ──────────────────────────────┐
P1.1 ─┐                          │
P1.2 ─┼─> P1.3 ─> P1.4 ─> P1.6 ──┼─> P2.3 ─> P2.6
      │       └──> P1.5 ─┘       │
L10 ──┴──────────────────────────┴─> P2.4 ─> P2.5 ─> P2.7 ─> phase 3
P2.1, P2.2: any time before P2.4
```

`docs/CONVENTIONS.md`'s first splitting rule is to start the critical path
first. Here that is P1.3, which can be written against P1.1's and P1.2's
interfaces while they are being built, and L10, which the init stream owns
already.

---

## 8. What changes elsewhere, once approved

### 8.1 The Security Target

The item's boundary does not move. `docs/certification/SECURITY-TARGET.md`
gains, in the words it already uses:

* **§4.2**, an objective for the environment: **OE.AUTH**, "people are
  identified and authenticated by the ring-3 authentication service of
  `docs/AUTH.md`, which alone holds credentials. It relies on the TOE for
  O.ISOLATE, O.CAPABILITY and O.SCRUB, and on the Linux personality's
  credentials for the uid a process runs as." A matching assumption
  **A.AUTH**: the authentication service and the programs that act on its
  verdict (`login`, `su`, `sessiond`, hyprix) are competently built,
  which is `A.ADMIN`'s shape. It also relies on the Linux personality's uid
  model and on its `SO_PEERCRED`, both in the uncertified load ring. No
  organisational security policy is needed for this (ferrix-55's review,
  2026-09-26; the TOE claims no FIA or FAU, F-21b).
* **§9.1**, a sentence: FIA and FAU are still absent from the TOE. Their
  environment counterparts are `authd` and its audit log, and on ARMv7-A and
  the DK1 a ring-3 driver can read that service's memory
  (`ARCHITECTURE.md` §7).
* **§2.3** stays as it is. An OS Protection Profile still cannot be
  claimed, because its FIA would be the environment's, not the TOE's.

### 8.2 The other documents

* `docs/INIT.md` §6: `ferrix.auth.seat` and `ferrix.auth.ask` as
  directory names; §4.4: `auth.service` and `auth.socket` among the shipped
  units; L10's gate names the session ending with its compositor (§6.4).
* `docs/ROADMAP.md`: authentication as a section of stage 15 ("a real
  userland", whose exit is a shell a person can use), with phase 3 as its
  own row (decision 9).
* `docs/BACKLOG.md`: a row per phase with its owner, and P0 as a row of its
  own today, since it is a hole whatever else is decided.
* `docs/sysml/`: the service, its store and its channels, in the model's
  maturity terms.

### 8.3 Known weaknesses of the environment

Listed here and in `docs/certification/VULNERABILITY-ANALYSIS.md`'s
"What this analysis does not cover", until each is fixed:

* **E-01, `SO_PEERCRED` names who made a socket, not who connected it**
  (§1, §3.4). A root-made socket used by a process that dropped to another
  uid reads as root. That gains a question, not an answer, since root never
  skips a password. K-E fixes it: the kernel takes the ids at `connect` and
  at `listen`, as Linux does. It is about 3 points: a listener records its
  credentials when it listens (`Socket::listen`, `kernel/src/fs/socket.rs`
  near 651); `connect_stream` (near 755-802) gives the server's end the
  connecting process's current ids, where it now uses `self.credentials`,
  and the client's end the listener's listen-time ids, where it now uses
  `target.credentials`. The caller in `kernel/src/syscall/sockets.rs`
  (near 331-335) passes the process in, and a boot check beside the
  existing `SO_PEERCRED` one (`kernel/src/syscall/check.rs` near 9084)
  changes a socket's ids between making it and connecting it. Its negative
  control is the old creation-time ids. The review asked for it in phase 1,
  as its own kernel slice before `authd` lands and after ferrix-55's OK.

---

## 9. Decisions for the customer

All eleven were taken as recommended on 2026-09-26. The text below is as
it was put to the customer.

1. **A dedicated service (`authd`) rather than a shadow file or PAM
   modules.** *Recommended: yes* (§3.1). It is the only one of the three
   that keeps hashes out of every client, lets the lock screen need no
   privilege, and can grow a second factor.
2. **Argon2id, written in `libs/crypto/argon2`, or RustCrypto's `argon2`
   crate.** *Recommended: Argon2id, written here* (§5.1). It is about 5
   points, the vectors are public, and it keeps the most sensitive
   arithmetic in code the tree has read. RustCrypto's crate can check it in
   a host test.
3. **Phase 1 locks the desktop with root's password**, because the desktop
   is root until phase 2. *Recommended: accept it for phase 1.* hyprlock
   asks for its own uid's password and does not change when phase 2 makes
   that uid `ferrix`'s. The alternative is to wait for phase 2 (about 37
   points more) before the lock screen can be used.
4. **What a lock screen does when no password is set.** *Recommended:
   refuse to lock, and say why* (§5.4). The alternatives are to lock anyway,
   which locks the person out, or to let an empty password through, which
   the customer ruled out.
5. **How someone becomes root.** *Recommended: root stays locked, and a
   member of `wheel` becomes root with their own password* (sudo's rule,
   §4.2 `su`). Classic `su` asks for root's password, which means root must
   have one.
6. **Throttle or lockout.** *Recommended: a growing delay capped at 5
   minutes, no permanent lockout, and `LockoutAfter=` for a service that
   wants one* (§3.5).
7. **Programs that read `/etc/shadow`.** *Recommended: refuse them safely,
   and give PAM programs a shim in phase 3* (§4.3). No `getspnam` shim.
8. **Starting the desktop in phase 2**: log in at the console and start the
   desktop from there, or have `sessiond` start it for one named account at
   boot. *Recommended: log in first, with an automatic login configurable
   per image* (for the gate images and for the customer's own machine if
   wanted). An automatically logged-in desktop still asks for the
   password at the lock, so its account must have one (decision 4).
9. **Where it goes on the roadmap.** *Recommended: stage 15, "a real
   userland"*, with phases 1 and 2 as its rows and phase 3 after the stage.
   P0 goes into the backlog now.
10. **Names and ids**: `authd`, `/run/ferrix/auth`, `/var/lib/ferrix/auth`,
    `/lib/ferrix/auth/services`, the `auth` system user with a fixed uid
    below 100 (proposed 90), `sessiond`, `authctl`. *Recommended: as
    written.*
11. **The unlock override.** *Recommended:* hyprlock's `SIGUSR1` becomes
    root's audited `authctl unlock-seat`, and `--grace` is capped by
    hyprix's `misc:lock_grace`, default 0 (§3.7).

---

## 10. Where it stands (2026-09-26)

| Slice | State |
|---|---|
| P0 | on `main` as `f84a8d3c` (ferrix-15): a native process runs as the process that made it, checked in every `test-init` boot |
| P0a, P0b, P0c | done 2026-09-26 (ferrix-15): P0a refused `User=` on a native unit until P0b made it run as that user; P0c is `54cba422`, the `SET_LIMIT` right |
| P1.1 `libs/crypto/argon2` | on `main` with this section: RFC 9106's vector and five RustCrypto ones, Miri in CI, the `argon2_phc` fuzz target, the timings of §5.1 |
| P1.2 `libs/proto/auth-proto` | on `main` with this section: the records of §3.3, `Secret`, the `auth_proto` fuzz target |
| P1.3 `authd`, P1.4 `passwd` and `authctl` | written on branch `auth-authd` (`userland/auth/`), with host tests over a root of their own and one over a real socket |
| P1.6 `cargo xtask test-auth` | written on branch `auth-authd` (`xtask/src/auth.rs`), with `--sabotage NAME` for the four negative controls; not yet booted |
| P1.5 hyprlock's backend | the hyprlock stream's, after `authd` lands |
| P1.7 the Security Target's OE.AUTH | with the last landing of phase 1 |

**One rule found while building it**, and now written into §3.5: after any
failure, no attempt on that account is looked at, from any connection,
until its FAILED has gone out. A guesser with a hundred connections gets
one guess per delay, like one with a single connection.

**Left for the desktop.** `authd` reaches an image with init through
`auth.socket`. The desktop's image, where hyprix is still pid 1, needs
`exec-once = /sbin/authd` and the `auth` account in its `/etc/passwd`. That
is to be agreed with hyprlock's P1.5 landing, so that `test-compositor`'s
boots do not grow a userland build before a lock screen uses it.
