# Apps: optional programs, each in a folder of its own

Written on 2026-09-30 at the customer's asking: *a folder where all
standalone apps go, which xtask adds by itself, so that an app, when added,
only ever changes things in its own folder*. The same day the customer asked
that the design leave room for a package manager later, and agreed to the
answers §9 records.

## 1. The problem

Before this, a program outside the system's own touched up to nine places
beyond its folder: the root `Cargo.toml`, `native.rs`'s `PROGRAMS`, an xtask
module of its own (`statd.rs`, `zinc.rs`), `check.rs`'s `userland`, `ports.rs`'s
`PORTS` and `FILES`, three audits' `ROOTS`, `check-crate-layering.sh`,
`ci.yml`'s target caches and `LAYOUT.md`. Moving or deleting one meant
finding all of them. A fetch program was the first to ask for this, and it
needed none of the kernel.

## 2. The layout

```
src/user/
  system/            first party: what Ferrix needs to be useful
    native/          the runtime, devmgr, the drivers
    linux/           init, auth, zinc, ferrousli, the compositor, adbd, the installer
  apps/              optional, each self-contained, found by xtask
    <name>/
      app.toml       the one contract with xtask (§3)
      Cargo.toml     a workspace of its own, with its own Cargo.lock
      README.md
      src/
```

`src/user/system/` is phase 3's move of today's `src/user/native/` and
`src/user/linux/` (§8); until then they stay where they are, and "the
system" in this document means them.

The line between the two is whether Ferrix is still useful without it. The
runtime, devmgr, the drivers, init, auth, the shell, the C library and the
compositor are the system. A fetch program, Bad Apple!!'s player, the stat
service and the ported programs (curl, btop, git, foot, vkgears, ALSA's
utilities) are apps. The native test programs `pong` and `channel-echo` are
neither: they are tests, and belong in `src/tests/`.

## 3. The contract: `app.toml`

```toml
[package]
name = "example"                # the folder's name
version = "0.1.0"
description = "One line: what it is."
abi = "native"                  # native | linux
arches = ["x86_64", "aarch64", "armv7a"]
depends = []                    # "name", or "name >= 1.2"

[[package.files]]
from = "example"                # a cargo binary, or a path in a script's output
to = "bin/example"              # where it goes, from the root
mode = "755"

[build]
kind = "cargo"                  # cargo | script

[image]
default = true                  # in every image a person runs (§5)

[check]
host-tests = true               # the crate's lib target, tested on the host

[[smoke]]
run = "example --version"       # a command line, run in the guest
expect = "example 0.1.0"        # a line of its output starts with this
```

`[package]` is what travels: it goes into the built package and into the
record on the installed system (§6). `[build]`, `[image]`, `[check]` and
`[[smoke]]` stay in the tree; they are how the package is made and judged.

The manifest is a small subset of TOML -- tables, arrays of tables, and
string, boolean and array-of-string values -- read by `ferrix-pkg`'s own
reader, because xtask takes nothing from crates.io and the package manager
on Ferrix will read the same records.

### 3.1 Building

* `kind = "cargo"`, `abi = "native"`: `cargo build --release --target
  <the kernel's target>` in the folder. The root's `.cargo/config.toml`
  applies, as it does to the system's native programs, and the ELF is held
  to the same shape (`native.rs`'s check) before it goes anywhere.
* `kind = "cargo"`, `abi = "linux"`: a static program against the target's
  musl, as zinc is built. xtask gives the flags and names `rust-lld` as the
  linker, so the app needs no `.cargo/config.toml`.
* `kind = "script"`: `bash build.sh <arch> <out>`, which installs into
  `<out>` the paths `from` names. For C ports, which source ferrousli's port
  toolkit (`src/user/linux/ferrousli/tools/ports/common.sh`) as a native app
  links the runtime; it needs a Linux host. `check` runs `bash -n` over it.

Each app builds into `target/apps/<name>/`, never the system's target
directory. An app whose toolchain is missing, or that does not list an
architecture, is skipped with a line saying so, as a port is today.

A script build is a download and minutes of C, which no image starts on its
own, as no image starts a port: `run` and `run-compositor` take the package
built last, and say how to build it when there is none. `cargo xtask
build-apps` builds every app's package, or `--app`'s; `test-apps` builds
what it boots. A cargo build is incremental, and every image makes it.

### 3.2 What an app may depend on

A native app links the runtime, `src/user/native/rt` -- the SDK -- by a
relative path. That path is the one reference an app makes outside its
folder, and apps never refer to each other's folders. A Linux app needs
nothing of the tree.

An app never has to grow the SDK to make a system call.
`ferrix_rt::linux::call` makes any Linux call by number, and
`ferrix_rt::linux::numbers` is the running architecture's table of them, from
`ferrix-linux-abi`. The app spells the calls it makes in its own folder.

## 4. The rules

1. **An app changes only its folder.** Adding one is adding a folder;
   deleting the folder removes it. `cargo xtask check` fails when a file
   outside `src/user/apps/<name>/` names that path.
2. **No `asm!` in an app.** The assembly allow-list and the unsafe and panic
   audits are the system's, and they never learn an app's name. An app's
   `unsafe` is its system calls, each with a `SAFETY:` comment, which the
   app lints below require.
3. **The lints are xtask's.** An app's manifest carries no lint table: xtask
   passes the same set to every app's clippy, so a new app is held to the
   rules without copying a hundred lines.
4. **Declarative installs.** An app's files are exactly its
   `[[package.files]]`, and nothing runs when it is installed. What needs
   registering -- a unit, a font -- is a file in a directory the system reads.
   That is what makes removing an app, later, a list of deletions.

## 5. What xtask does, by discovery

`tools/common/xtask/src/apps.rs` reads `src/user/apps/*/app.toml`, and nothing
in xtask names an app.

| Command | For each app |
|---|---|
| `cargo xtask apps` | lists it, and checks its manifest |
| `cargo xtask check` | formatting (`bash -n` for a script); clippy on the host's lib target and the programs' targets; the host tests; rule 1 |
| `cargo xtask run`, `run-compositor` | builds the `default` ones for the architecture and installs their packages into the image |
| `cargo xtask build`, `test-boot` | installs only the ones `--app` names |
| `cargo xtask build-apps` | builds its package, a script's too |
| `cargo xtask test-apps` | one boot that runs every `[[smoke]]` line and wants each `expect` |
| `cargo xtask new-app --app NAME [--abi linux]` | writes a new app's folder, which passes the rows above as it is |

`--app NAME` adds an app that is not `default`; `--no-apps` leaves them all
out. The images the test rows boot are unchanged: they are the system's
tests, and an app's is `test-apps`. `--statd` is `--app statd`, kept because
the phone's scripts say it.

## 6. Packages

Building an app makes a package, and an image is packages installed into a
root. This is the package manager's engine from the start (§7), so that
every image build exercises it.

A package is a newc cpio archive, `<name>-<version>-<arch>.fxpkg`, holding:

* each file at its `to` path, with its mode;
* its record, `lib/ferrix/packages/<name>.toml`: the manifest's
  `[package]`, and a `[[files]]` entry for each file with its path, mode,
  size and BLAKE2b-256 digest (`ferrix-argon2`'s).

Installing is unpacking, so the record lands with the files, and a running
Ferrix knows what it was built with. `ferrix-pkg` (`src/lib/proto/pkg`)
holds the manifest reader, the record and the installer's plan -- the
dependencies met, no two packages owning a path, no path outside the root --
where `cargo test` reaches it. xtask builds with it on the host; the package
manager will be the same code on Ferrix.

## 7. Later: a package manager

What this leaves room for, and deliberately does not build yet:

* **The tool.** `pkg install`, `remove`, `upgrade`, `list`, on Ferrix,
  over `ferrix-pkg`. First party, not an app: it manages the apps.
* **A repository.** An index of packages per architecture, built by CI and
  served statically (GitHub's releases or Pages), fetched with the curl
  port that already runs.
* **Signatures.** The index and every package signed; this needs an
  Ed25519 the tree does not have yet. Digests come first, and are in the
  records from phase 1.
* **Resolution.** Minimum versions and no solver, as apk has, until
  something needs more.
* **Atomic upgrades.** Ferrix writes btrfs; a snapshot before a transaction
  is the natural rollback.
* **Building on Ferrix.** Stage 20 wants the recipes run in the guest.
  `cargo` recipes can be; `script` recipes need gcc on the host until the
  ports build natively.
* **The system as packages.** Only once a broken install of init or the
  shell can be recovered from.

## 8. The phases

1. **The mechanism**: `apps.rs`, `ferrix-pkg`, the SDK's `linux::call`,
   the `check` steps, image installs, `test-apps`, rule 1, and the fetch
   program `ferrofetch` as the first app. Landed.
2. **The optional programs move in**: the stat service and btop first, one
   of each build that had no app, with `build-apps` and `new-app`; then Bad
   Apple!!'s player and the other ports -- which empties `ports.rs`'s
   `PORTS` and `FILES` of programs, and gives the ports' dependencies (git
   on zlib and curl) the `depends` they have implicitly today. A library
   only built against, as libcxx is for btop, stays a port: it installs
   nothing an image carries.
3. **`src/user/native` and `src/user/linux` move under `src/user/system/`**,
   a mechanical move of paths in xtask, the generators, CI and
   `LAYOUT.md`, done apart from the rest so it collides with as little
   other work as it can.
4. **The package manager** (§7).

## 9. The customer's decisions

Agreed on 2026-09-30, as the draft proposed them:

1. The names `src/user/system/` and `src/user/apps/`.
2. The split of §2; adbd and the installer stay in the system.
3. One generic `ferrix_rt::linux::call` is the one change to the system an
   app needs.
4. The test rows' images do not carry apps; `test-apps` does.

## 10. Where it stands

2026-09-30: phase 1. `ferrix-pkg`, `apps.rs`, `ferrix_rt::linux::call`,
and `ferrofetch` as the first app. On x86-64 `test-apps` boots zinc as init
and passes the app's three smoke checks, the first a native program started
from a shell by `execve` and reading `uname` and `/proc`; with one `expect`
changed to a line the program never prints it fails and names the check.

Phase 2's first half, the same day: the stat service is the `statd` app,
`abi = "linux"`, and `statd.rs` is gone; btop is the `btop` app, `kind =
"script"`, out of `ports.rs`; `build-apps` builds packages on demand, and
`new-app` writes a folder for either ABI.

Not yet: Bad Apple!!'s player and the other ports.
