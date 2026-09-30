# Steam's window on Ferrix

Valve's Steam client, unchanged, draws its sign-in window on hyprix through
yserver, on Ferrix, in a guest under KVM. This is the first step of stage
22's exit ("the Steam client starts on Ferrix, logs in and shows its store,
with the browser helper drawing"): the client and its browser helper start
and draw, and nobody has signed in yet. Input has not been tried.

It runs with launch-side workarounds: stand-ins and flags, each for
something Ferrix does not do yet. The table in §3 lists every one, the real
fix that retires it, and who owns that fix. None of them is in the kernel,
and no library is preloaded into the client any more.

## 1. Running it

```
tools/common/fetch/fetch-steam-window.sh     # once: the volume, about 7 GB sparse
cargo xtask test-steam-window           # the gate: waits for the window, judges the screen
cargo xtask run-steam                   # the same boot, screens dumped until the timeout
cargo xtask test-steam-store            # the --everything desktop's Steam, on ferrousli: sign-in, then the store
```

Both need KVM and the internet, as `test-steamcmd` does, and neither is in
`cargo xtask check`. The volume is attached under `snapshot=on`, so every
boot starts from Valve's bootstrap: the client downloads and installs itself
(about 500 MB), restarts, and opens its window about twelve minutes after the
boot on the gate host. The guest has 16 GiB (`--memory` changes it; see §3,
`SIGBUS`).

`test-steam-window` passes when hyprix lists a window titled "Sign in to
Steam" and a screen dump twenty seconds later has the colours of a drawn one;
the dump is `build/x86_64/steam/login-window.ppm`. `run-steam` keeps every
screen that differs from the last in `build/x86_64/steam/` until the
timeout (`--timeout`, 2400 s by default). The serial transcript's
`steam-window:` lines are the guest's: the client's output, the titles of
hyprix's windows as they change, and the client's logs at the end.

What shows: the sign-in window ("SIGN IN WITH ACCOUNT NAME", the password
field, "Sign in", and the QR code for the mobile app), top left in a tile of
hyprix's that fills the screen, the rest of the tile black.

**On the desktop.** `cargo xtask run-compositor --everything` makes the
volume when it is missing (by running `fetch-steam-window.sh`, and a failed
fetch stops the run), merges its tree into the desktop's volume and
starts Steam beside Chrome and a terminal: `tools/common/steam/desktop.sh` waits
for the desktop's yserver on `:0` and runs the client's half as uid 1000,
its output in the guest's `/tmp/steam.log`. The guest has 16 GiB then,
unless `--memory` says otherwise. Steam's tree carries its own yserver, so
the desktop takes it and never merges yserver's own volume. The first start installs the client, as above, and the
desktop's volume is attached under `snapshot=on` too, so every boot does.
fuzzel lists Steam too (`steam.desktop`, which runs `desktop.sh` again), so
a client that was closed can be started again; the entry is there only when
the volume is merged.

**The store, gated.** `test-steam-store` boots that desktop's Steam as
`run-compositor --everything` does -- the merged volume, the same archive
with ferrousli's loader at `/lib64`, `desktop.sh` and `client.sh`, 16 GiB --
without the host's `hyprland.conf`, the terminal, Chrome's window, the
wallpaper, the clipboard and the 3D card (QMP cannot dump its screen), and
with `tools/common/steam/store-watch.sh` listing hyprix's windows. It passes
its first step when hyprix lists "Sign in to Steam" and the screen has the
colours of a drawn window, as `test-steam-window` judges it, and the
window its two fields (`build/x86_64/steam-store/sign-in.ppm`); an empty
frame, which has enough colours for `test-steam-window`, does not pass. The
second step needs a Steam account kept for the gate, with Steam Guard off:
a mobile authenticator needs the phone, and email Steam Guard sends a code
for every new machine, which every boot is. Put its name and its password,
one a line, in `~/.config/ferrix/steam-test-account` on the machine that
runs the gate, readable by its owner alone (`chmod 600`), or name another
file with `FERRIX_STEAM_ACCOUNT_FILE`; never in a checkout. Then the gate
clicks into the sign-in window's fields, types both through QMP on a US
layout, presses Sign in, waits for the main window titled "Steam", and
passes when that window shows the store: at least 15% of it the store's
dark blues (`#171d25` to `#1b2838`) and at least 10,000 colours, which its
art brings and an empty page does not. Without the file it says the store
step was skipped, and how to enable it, and passes on the first step. A
sign-in window that shows red fails as a sign-in not taken, with the
connection log's answers: a wrong name or password, Steam refusing the
address after several failures, or email Steam Guard's code prompt, which
has red enough to count (the test account's first run met that one). One
still up three minutes after Sign in with its account name field gone
fails as Steam Guard too. With the test account and Guard off, the gate
signed in and found the store on 2026-09-30. The account is typed into the guest and nowhere
else: every line of the transcript and the gate's error are redacted of
both, and a screen dumped after the typing is kept shrunk eight times, too
small to read (`store.ppm`, `not-signed-in.ppm`, `after-sign-in.ppm`).

## 2. How the pieces fit

| Piece | Where | What |
|---|---|---|
| The volume | `tools/common/fetch/fetch-steam-window.sh` | yserver's tree (`fetch-yserver.sh`, the fork at its pinned commit), Valve's bootstrap and the Debian tools under it (`fetch-steam.sh`), i386 Mesa with llvmpipe for the 32-bit client's own GL UI, i386 libstdc++, and Debian's amd64 `lsof` |
| The boot | `tools/common/xtask/src/compositor/steam_window.rs` | hyprix, the links the volume's programs need, the scripts below, a `uname` that says `Linux` |
| The root half | `tools/common/steam/run.sh` | yserver on `:0` as a Wayland client of hyprix, a lease, then the client's half as uid 1000; a watcher for the window's title |
| The client's half | `tools/common/steam/client.sh` | `ubuntu12_32/steam` started directly with `steam.sh`'s environment, again while it exits 42 |
| Stand-ins | `tools/common/steam/_v2-entry-point`, `logger-0.bash`, `lsof` | see §3 |

The client runs as uid 1000: run as root, it moves its effective uid to the
home's owner partway through, and GTK2's setuid check then exits
(`docs/I386.md`, I5b). That is how Linux behaves too, and is not a
workaround.

The flags `-no-cef-sandbox -cef-disable-gpu -cef-disable-gpu-compositing`
are the host spike's: Chromium's sandbox needs user namespaces, and its GPU
process would look for DRI3, which yserver on a guest without a render node
does not offer; it renders in software instead.

## 3. The workarounds

| Workaround | Why | Real fix | Owner |
|---|---|---|---|
| `_v2-entry-point` stand-in | Valve's steamrt64 entry point starts pressure-vessel, which needs user and mount namespaces for bubblewrap | namespaces | N2–N6 (`docs/NAMESPACES.md`) |
| `-no-cef-sandbox` | Chromium's sandbox is two layers: user and pid namespaces (network ones optional), and a seccomp-bpf filter in every child. Measured on the host (`docs/SECCOMP.md` §1.2): with `CLONE_NEWPID` refused, Chrome with its sandbox on does not start at all | user namespaces, pid namespaces and seccomp together | N4 (`docs/NAMESPACES.md`); pid namespaces and a loopback-only network namespace, os-98 after N4 (the customer's decision of 2026-09-30); seccomp S1–S6, os-7c, and S7–S8, Steam's own filters (`docs/SECCOMP.md`) |
| `-cef-disable-gpu -cef-disable-gpu-compositing` | yserver on the Wayland backend offers no DRI3, so the web helper's GL is llvmpipe, which CEF 126 rejects: its GPU process falls back to SwiftShader after three or four restarts | ANGLE on Vulkan through Venus, presenting with `MESA_VK_WSI_DEBUG=sw`: user copies through a device window's own mapping (F-55's second landing), the render node opened to a `render` group, and the helper's flags (§6) | not started; os-9f's conditions are in §6 |
| `logger-0.bash` stand-in | The Steam Runtime's logger failed on Ferrix under `steamwebhelper.sh`; this one logs nothing | whatever the logger meets: `/dev/fd` through process substitution, and the `/proc` gaps below | steam-proc-gaps |
| (not worked around) `lsof` warns "unsupported format" for `/proc/net/tcp6` and `udp6`, and cannot identify Unix sockets | the IPv6 tables' columns differ from Linux's, and `/proc/net/unix` names no inodes | Linux's formats | steam-proc-gaps |
| 16 GiB guest | At 8 GiB several processes died of `SIGBUS` on execute faults of mapped library pages while Chromium started | find and fix the refault | steam-sigbus |
| the window fills its tile, black around the login | hyprix tiled a window of a fixed size, and yserver did not pass on its size hints (`WM_NORMAL_HINTS`) | a floating window of the size Steam asks for: hyprix ec4ce6b2 and the yserver pin c5b5935; and a floating window that follows the size its program gives it later, without which Steam's dialogs float as 130x70 miniatures of themselves ("Steamwebhelper is not responding" did) | done: the sign-in window floats at its own size on the `--everything` desktop (2026-09-30), and a dialog that resizes itself afterwards takes its new size (`follow_own_size`, with `a_floating_dialog_takes_a_size_its_program_gives_it_later`) |

Five things the sprint needed are no longer workarounds. A shim that
retried `pipe2` without `O_DIRECT`: the kernel makes packet pipes now, each
write a packet and each read one packet at most, which the client's
`controllerxinput_linux.cpp` asserts it gets. And yserver's own
patch making a client's socket blocking before its setup (the pinned fork
has 14197fb, and the kernel no longer passes a listener's `O_NONBLOCK` to
`accept`, 18388c70); a `getresuid` shim, not needed once the client runs as
uid 1000; and two shims for `/proc`, retired when `test-procfs` landed.
One was preloaded into `lsof`, because `stat` through `/proc/<pid>/fd/<n>`
of a socket was `ENOENT`, so `lsof` could not tie the client's websocket to
the web helper and the client rejected it ("Unexpected Transport Error
0x3008"). The other was preloaded into the client, because `/proc`'s inode
numbers and its entries' offsets did not fit 32 bits, so the client's
`readdir` there was `EOVERFLOW` and it found no web helper process at all.
The inode numbers were fixed first (f577b9b1); the offsets only after the
client, without the shim, still logged `Checked: <pid>/<pid>` and rejected
the connection, since the shim had truncated `d_off` as well (a46797f7).
cgroupfs and sysfs have offsets past 2³¹ too; nothing 32-bit lists them yet.

## 4. What the sprint found, in order (2026-09-28 and 29)

1. The updater chooses its UI by `dlopen`: `libX11.so.6`, then `libGLX.so`
   by its development name, then `libXrandr.so.2`; without `libGLX.so` it
   uses its console UI.
2. GTK2's setuid check exits under root (above).
3. `pipe2(O_DIRECT)`.
4. yserver's RandR output lost its CRTC, and Chromium found no display
   (fixed in the fork).
5. The web helper's websocket was rejected: `accept` gave the new socket the
   listener's `O_NONBLOCK` (fixed, 18388c70); `/proc/net/tcp` named the
   stack's socket id as the inode (fixed, 5b14b91b); `stat` through a socket's
   descriptor link, and 32-bit `readdir` on `/proc`, both its inode numbers
   and its offsets (worked around, then fixed; `test-procfs` checks both).
6. The client looks for `lsof` only at `/sbin`, `/bin`, `/usr/sbin` and
   `/usr/bin`, and says so nowhere visible when it finds none.

## 5. On the `--everything` desktop, under ferrousli (2026-09-30)

`cargo xtask run-compositor --everything` starts Steam beside Chrome and a
terminal (`tools/common/steam/desktop.sh`). There, `/lib64`'s loader is
ferrousli's, so the client's 64-bit side runs on ferrousli's C library, not
glibc's; the 32-bit client itself still runs on the volume's i386 glibc. On
2026-09-30 the sign-in window came up there, the customer signed in with the
Steam app's QR code, and the client showed its store. What the 64-bit side
needed of ferrousli, in the order it was met:

1. `libGL.so.1` reads its TLS by initial-exec: a `dlopen`ed library's TLS
   image is now copied into every running thread, as glibc does.
2. No GLX visual: Mesa's software renderer needs LLVM, which needs
   `libstdc++`, whose `STB_GNU_UNIQUE` symbols the loader did not take for
   definitions; and `logf128`, `pthread_mutex_clocklock`, `iopl` and a few
   `_chk` names.
3. "steamwebhelper is not responding": `libc.so.6` was loaded where
   `libdl.so.2` is named, ahead of `libcef.so`, so Chromium's
   `dlsym(RTLD_NEXT, "localtime")` found nothing, and logging that
   deadlocked in its own `localtime_r` wrapper. `libc.so.6` now loads where
   its own name puts it.
4. "futex robust_list not initialized by pthreads", then "…is corrupt":
   the web helper links its robust mutexes into the thread's list the way
   glibc does. Every thread now registers a list, laid out as glibc's (a
   `robust_prev` word before the head), and the kernel keeps it per thread.
5. Scout's `setup.sh` exited 127: `client.sh` ran it with the runtime's own
   `zenity` on `PATH`, which does not load (`_IO_getc`).

Each was found by running the program under ferrousli on nazuna first
(`patchelf --set-interpreter` to ferrousli's loader, Xvfb), which is
minutes where a desktop boot is ten. The workarounds of §3 are unchanged:
the helper still runs without its sandbox and outside pressure-vessel, and
its GPU process is disabled (`-cef-disable-gpu`), so the store draws in
software.

Most of what the client printed was its libudev failing, two lines at a
time and over and over: `udev_monitor_new_from_netlink_fd: error getting
socket: Protocol not supported` and `udev_has_devtmpfs: name_to_handle_at
on /dev: Function not implemented`. Since 2026-09-30 a
`NETLINK_KOBJECT_UEVENT` socket opens (and hears no event, since none is
sent yet), and `name_to_handle_at` answers `EOPNOTSUPP`, the one failure
that libudev takes quietly. A `test-steam-window` run went from 291 such
lines to none.

## 6. The GPU process: what is left (2026-10-01)

The store draws in software. To draw it on the GPU, the one route that
needs no DRI3 from yserver is ANGLE on Vulkan through Venus, with Mesa
presenting to X by copying each frame (`MESA_VK_WSI_DEBUG=sw`); plain Chrome
got that far on 2026-09-30, and the copy was F-55's kernel page fault.
F-55's first landing (c5c92781) turned that fault into `EFAULT`, and F-55
is closed (ef206bb2). Three pieces are left, 14 to 23 points in all; none
has code yet. The certification consultant (os-9f) set the conditions for
the first two on 2026-10-01, and each diff goes to os-9f before landing.

**User copies through a device window, 5 to 8 points.** Mesa's copy
`writev`s the frame straight from a Venus window to the X socket. The
conditions: a per-CPU mapping slot per copy (Linux's `kmap_local`), not a
`vmap` and unmap (a machine-wide shootdown per page) and no standing
mapping per region; preemption off for a copy of at most one page, a local
invalidation only, and no interrupt handler in the slot; the slot's
attributes exactly the user mapping's, and only normal memory copied, since
a device-type window keeps `EFAULT`; the page checked to lie in the
region's own recorded device range, with the region held for the copy; no
direct-map address and no sleeping lock; a requirement beside L.user.107, a
stage 9 check that bytes written through the copy read back through a
second mapping, both ways, across a page boundary and within a page, and
negative controls with the attribute check and the range check dropped.
What the tree has for it: `Backing::Device` records only `cached`
(`src/lib/kernel/vma`); `map_device` always stores `false` and `map_window`
what the device says, so a `cached` region is normal write-back and the
rest device-type, and nothing maps write-combining yet. The space's
`inner` lock is a spinlock and holds the region across the copy. There is
no per-CPU temporary mapping area: slots can go after `DEMAND_WINDOW` below
the vmap arena (mind armv7a's 64 MiB reserve and stage 3's probes there),
their tables built once at bring-up, with a new helper that writes one leaf
without allocating and invalidates one address on this CPU (`invlpg`,
`tlbi vale1`, `TLBIMVA`); `mm::unmap_kernel` shoots down every CPU and
cannot be used. Landing 1's stage 9 check then changes: cached windows
copy, device-type ones stay `EFAULT`.

**The render node for a `render` group, 5 to 8 points.** Mode 0660, group
`render`, the desktop's user in it, not 0666. Before the mode changes: what
a user can allocate through the node bounded, and an audit of its calls.
What the reading found: the node's metadata is `Renderer::metadata`
(`src/kernel/src/interfaces/render/mod.rs`), gid 0 like every device node so
far; `/etc/group` is written in four places in xtask (`initramfs.rs`,
`init.rs`, `auth.rs`, and `compositor/apps.rs` to check), gid 90 is taken.
The renderer's session limits, 16 contexts and 256 objects
(`src/lib/proto/renderctl/src/session.rs`), are shared by every open, so a
user could take the compositor's GPU away: the bound has to be per open
(one context, an object limit) with a per-job limit on opens, and each
object's heap charged with a kmem `Charge` as F-37 does elsewhere. For the
audit, in `interfaces/render/node.rs`: `mapping_at`'s offset against the
object's size, `transfer`'s box, level and offset, `resource_create`'s
32-bit size with no page bound of its own, and handles that stop advancing
at `u32::MAX`. A fuzz target for the request parser, and `docs/GPU.md` to
say that the host's virglrenderer is a guest-to-host surface for the host
to secure. The boot check needs a renderer, which test-boot's machine does
not have: a stand-in driver, or a gate under `--venus`.

**The helper, 4 to 7 points.** `client.sh` without `-cef-disable-gpu`,
with `--use-angle=vulkan` and Vulkan's features, `MESA_VK_WSI_DEBUG=sw`,
and without the llvmpipe variables it sets for the 32-bit client. Untested:
whether CEF 126 accepts Venus and presents this way. The host's
`__GLX_VENDOR_LIBRARY_NAME=nvidia`, from the carried `hyprland.conf`,
reaches every guest program too.
