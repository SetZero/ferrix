# Init: a service manager for Ferrix

Version 2, a draft. Written on 2026-09-23 at the customer's asking: *a real
init, fitting to our kernel, usable if Ferrix later becomes a microkernel,
and somewhat extensible like systemd*. Version 2 follows the customer's
second order of the same day: *build stage 13's cgroups first, and plan the
init as if they exist*. §14 lists the decisions that
are the customer's and not this document's; until they are answered, the
draft answers are what the rest of the text assumes. The first, C8, was
answered the same day. §16 says what has been built since.

It finishes stage 15. `docs/ROADMAP.md` says what is left there: "nothing in
user space mounts `/proc` and `/dev`, reaps what a session orphans, gives a
shell a session and a controlling terminal of its own, respawns one that dies,
or brings the machine down", and a getty per terminal.

## 0. Prerequisite: stage 13's cgroups, built first

**Init is not started until the cgroup half of stage 13 has landed.** Every
section below assumes cgroup v2 exists as `docs/ARCHITECTURE.md` §6 designs
it: one unified hierarchy, exposed as cgroupfs, with the `cpu`, `memory`,
`io` and `pids` controllers. Stage 13's other two thirds, namespaces and
seccomp, are **not** prerequisites. Once they land, init gains sandboxing keys
for them (§4.4, landing L13).

Why first, rather than working around it. A service manager has to know what
belongs to a service, end all of it, know when it has ended, and bound what
it may use. A cgroup answers all four questions, and it is the answer every
Linux program that manages services already expects: `systemd-run`, a
container runtime, a nested manager with `Delegate=yes`. Version 1 of this
document answered the first three with native jobs, and it needed four kernel
additions to do it. Those additions made jobs into cgroups without the
filesystem, and without the resource limits that are the fourth answer.
Building the real thing first costs no more, and init then has nothing to
unlearn.

### 0.1 What init needs from stage 13

These are init's requirements on stage 13, not stage 13's design, which is
`docs/CGROUPS.md`; its landings G1 to G4 meet C1 to C5 and C7, and G5 meets
C8. Each is Linux's own interface, so a program written for Linux cgroups
works unchanged:

| | Interface | What init uses it for |
|---|---|---|
| C1 | `mount -t cgroup2` at `/sys/fs/cgroup`, with `mkdir` and `rmdir` making and removing cgroups. The mount point is sysfs's `fs/cgroup` (`docs/SYSFS.md`), which the kernel mounts on `/sys` at boot | the tree of slices and services (§5.1) |
| C2 | `cgroup.procs`: read to list members, write to move one; `fork` and `clone` put the child in the parent's cgroup | membership, and a service's forked children staying its own |
| C3 | `clone3` with `CLONE_INTO_CGROUP`, which answers `ENOSYS` today (`kernel/src/syscall/family.rs`) | starting a service already inside its cgroup, with no window outside it (§5.2) |
| C4 | `cgroup.events`, with `populated` and `frozen`, waking `poll`/`epoll` with `POLLPRI` when it changes (there is no `inotify`) | knowing a service has ended, all of it |
| C5 | `cgroup.kill` | ending a service whose processes do not stop when asked |
| C6 | `cgroup.subtree_control` and the controllers' files: `memory.max`, `memory.high`, `memory.events` (with `oom_kill`), `pids.max`, `cpu.weight`, `cpu.max`, `io.weight` | the resource keys of §5.5 |
| C7 | Ownership of a cgroup directory by `chown`, with writes to `cgroup.procs` checked against it, as Linux's delegation rules check them | handing a subtree to a nested manager (`Delegate=`) |
| C8 | **Every cgroup is backed by a `Job`.** A job created natively appears as a cgroup, and `job_for_cgroup(dirfd)` returns a handle to the job behind a cgroup. The job asserts a new `EMPTY` signal exactly when `populated` becomes 0 | native services in a cgroup, and the microkernel's view of the same tree (§7) |

Init strictly needs C1 to C5 to start at all. C6 comes in controller by
controller, and a resource key whose controller does not exist yet is a
warning, not an error. So `memory` can land before `io` without init waiting
for both. C7 is needed for `Delegate=`, and C8 for `Type=native` services.

**C8 is the one requirement that is not Linux's, and the customer accepted it
for stage 13 on 2026-09-23.** `docs/ARCHITECTURE.md` §3 already calls the job
"where resource limits and kill authority live", and §4 scopes the OOM kill
"by Job and cgroup". If a cgroup is a job with a filesystem view, there is
one container concept in the kernel rather than two, and three things
follow:

* `devmgr`'s per-driver jobs show up in cgroupfs, where a driver's memory can
  be read and limited;
* `process_create` in a job and `CLONE_INTO_CGROUP` put a process in the
  same kind of place;
* a microkernel that drops cgroupfs keeps the jobs underneath it, and with
  them everything init relies on (§7).

The alternative was to build cgroups as their own kernel object, as Linux
does. Then `Type=native` services would have run in a job beside their
cgroup instead of in it, and §7's microkernel column would have needed one
translation layer more.

## 1. What this is, and what it is not

`/sbin/init` is pid 1 and the service manager, one program as on systemd, for
the same reason systemd gives: the process that reaps everything is the one
that knows which exit belonged to which service. It:

* starts the system from *units*, declarative files with dependencies, in
  parallel where the dependencies allow;
* runs each service in a **cgroup** of its own, restarts it by policy, bounds
  what it may use, and stops it as a unit, whatever it forked;
* is the one writer of the cgroup tree, except for subtrees it delegates;
* reaps orphans, since it is pid 1;
* gives each terminal a getty, with its own session and controlling terminal;
* hands services the capabilities their unit names, and nothing else (§6);
* brings the machine down in order: services in reverse, then sync, then
  unmount, then `reboot(2)`;
* answers a control socket, through which `svc` starts, stops and reports.

It is **not** `devmgr`. Drivers stay `devmgr`'s, and in this version `devmgr`
is still started by the kernel before init (§7.3 is how that changes). It is
not a logging daemon, a login manager, a network configurator or an IPC bus:
systemd grew those in the same tree, and here each would be a service that
init starts.

## 2. The three requirements, read as constraints

**Fit the kernel.** Linux is the native ABI (`docs/ARCHITECTURE.md` §2), and
with stage 13's cgroups, everything init does to a *Linux* service is Linux's
own interface: `clone3`, `execve`, `wait4`, `setsid`, `TIOCSCTTY`, `mount`,
cgroupfs, `reboot`. So init is a Linux program, std Rust on musl like zinc.
It makes native calls only for what Linux cannot express: handing a service
its bootstrap channel, and starting a native program. §11 lists the six small
kernel additions it needs besides stage 13, each one also useful outside
init.

**Survive a microkernel.** One rule makes this hold: **the manager's logic
never names a system call.** It is a pure state machine in `libs/svc` that
takes events and returns actions (§3). Every effect goes through one of nine
*backends*, and §7 says for each who serves it today and who would serve it
in a microkernel. There is a second half to the rule. A microkernel's root
task is the process that hands out capabilities, so init is designed to be
that from the first version: services get handles from init, never by name
from the kernel (§6). With C8, the cgroup tree init builds is also a job
tree, which is what a native init would build. Moving to a microkernel then
changes backends and leaves unit files, the directory protocol and the
manager unchanged.

**Extensible like systemd.** What makes systemd extensible is not its size but
five mechanisms. This design takes all five, and adds one of its own:

| Mechanism | What it lets a newcomer do without touching init |
|---|---|
| Unit files in layered directories, with drop-ins | Override one key of a shipped unit |
| Templates (`getty@.service`) | One file for every terminal |
| Targets, slices and `[Install]` | Hook into boot at a named point, and into the resource tree at a named branch |
| Generators | Write units from the machine's state at boot (a getty per console) |
| Socket activation | Start a service on its first connection |
| **The directory** (Ferrix's own, §6) | Offer or use a named native service, started on first use |

Unit *kinds* are a trait in `libs/svc` (§4.2). A new kind, such as `timer` or
`path`, is a new implementation of the trait; the graph and the operations
do not change.

## 3. Shape

```
libs/svc         no_std + alloc; host-tested, Miri, fuzzed
                 unit files -> model, the dependency graph, operations,
                 the slice tree, restart policy, and
                 Manager::step(event, now) -> actions
libs/svc-proto   the control and notify wire formats, shared with `svc`
init/            its own workspace, std on *-linux-musl, like zinc/
  init           /sbin/init: the event loop and the Linux backends
  svc            /bin/svc: the control client
  getty          /sbin/getty
```

The core is one function:

```rust
impl Manager {
    /// Everything that happened, in; everything to do about it, out.
    /// `now` comes from the Clock backend, so a test replays time too.
    pub fn step(&mut self, event: Event, now: Instant) -> Actions;
    /// When `step` next wants a `Timer` event, if ever.
    pub fn deadline(&self) -> Option<Instant>;
}

pub enum Event {
    Spawned   { unit: UnitId, main: Pid },
    Exited    { pid: Pid, how: Exit },          // wait4, or a native status
    Emptied   { unit: UnitId },                 // cgroup.events: populated 0
    OomKilled { unit: UnitId },                 // memory.events: oom_kill grew
    Ready     { unit: UnitId, status: Option<String> },
    Timer,
    Request   { client: ClientId, request: Request },   // from `svc`
    Open      { from: UnitId, name: Name, end: Token }, // the directory, §6
    Mounted   { unit: UnitId, result: Result<(), Errno> },
}

pub enum Action {
    MakeGroup   { unit: UnitId, path: GroupPath, limits: Limits },
    SetLimits   { unit: UnitId, limits: Limits },
    RemoveGroup { unit: UnitId },
    Spawn   { unit: UnitId, spec: SpawnSpec },  // argv, env, fds, user, group path, grants
    Signal  { unit: UnitId, signal: Signal, whom: Whom },  // main, or every member
    KillGroup { unit: UnitId },                 // cgroup.kill
    Mount   { unit: UnitId, spec: MountSpec },
    Unmount { unit: UnitId },
    Route   { to: UnitId, name: Name, end: Token },
    Reply   { client: ClientId, reply: Reply },
    Log     { unit: Option<UnitId>, line: String },
    Power   (PowerAction),
}
```

`Token` is opaque, and so is `GroupPath` as far as the core is concerned:
the core never holds a handle, a descriptor or a directory, only names the
backend maps to them. That is what lets the same crate run inside a
Linux-ABI init today and a native-only root task later. It is `no_std` so
that `devmgr` (a `no_std` native program) can use its restart policy (§5.4),
and so that a native init is a new event loop, not a new manager.

Because `step` is pure, the manager's tests are host tests. A test is a unit
set plus a script of events, and it asserts the actions. That covers boot
order, cycle breaking, the slice tree, restart backoff and shutdown order
under `cargo test` and Miri. The unit-file parser gets a fuzzer, as the
hyprlang parser has.

## 4. Units

### 4.1 Files

The syntax is systemd's INI subset: `[Section]`, `Key=value`, `#` and `;`
comments, a trailing backslash to continue a line, and repeated keys adding
to a list, where an empty assignment clears it. It is chosen for familiarity
over elegance: anyone who has written a systemd unit can write a Ferrix one,
and a distribution's unit file mostly loads. A key init does not know is a
warning in the log, not an error, so such a file still loads.

Units are searched in three directories, each overriding the one below it:

| Directory | Who writes it | Lives on |
|---|---|---|
| `/run/ferrix/units` | generators, at every boot | tmpfs |
| `/etc/ferrix/units` | the administrator; `svc enable` | the root volume |
| `/lib/ferrix/units` | the image (`xtask`) | the initramfs, installed onto `/` |

A drop-in `name.service.d/*.conf`, in any of the three, overrides single keys
of `name.service`, with files applied in name order. A unit linked to
`/dev/null` in a higher directory is *masked*. A template `getty@.service` is
instantiated as `getty@console.service`, with `%i` naming the instance.

### 4.2 Kinds

A unit's suffix names its kind, and each kind implements one trait:

```rust
pub trait Kind {
    /// The keys of its own section, parsed; unknown keys are warnings.
    fn parse(&self, section: &Section, log: &mut Warnings) -> Result<Config, UnitError>;
    /// Dependencies it implies (a mount wants its mount point's parent
    /// mounted; a service wants its slice).
    fn implied(&self, config: &Config, graph: &mut Edges);
    /// Drive one unit toward `goal`, given what just happened to it.
    fn advance(&self, unit: &mut UnitState, goal: Goal, event: Option<&Event>, now: Instant) -> Actions;
}
```

| Kind | Version | What it is |
|---|---|---|
| `.service` | 1 | Processes init starts, in a cgroup of their own (§5) |
| `.slice` | 1 | A branch of the cgroup tree, with limits over everything beneath it |
| `.scope` | 1 | Processes init did *not* start, grouped on request: a login session, a compositor's client |
| `.target` | 1 | A named point in boot; no processes |
| `.mount` | 1 | A mount point, which init mounts and unmounts |
| `.socket` | 2 | A listening socket init holds; the service starts on the first connection |
| `.builtin` | 1 | Something the kernel provides, always active (§7.2) |
| `.timer` | later | Starts a unit on a schedule |
| `.path` | later | Starts a unit when a path changes (needs `inotify`, absent) |

### 4.3 Dependencies and operations

The dependency keys are systemd's, with systemd's meanings: `Requires=`,
`Wants=`, `BindsTo=`, `PartOf=`, `Conflicts=`, `After=`, `Before=`, and
`ConditionPathExists=` and its siblings, which skip a unit rather than fail it.

What systemd calls a *job*, a pending start or stop, is called an
**operation** here. The word *job* is the kernel's (§0.1, C8), and one word
must not mean two things in one design. A request is expanded into a
*transaction*: every operation it pulls in, ordered by `After=`/`Before=`.
A cycle is broken by dropping a `Wants=` edge, with a warning; a cycle made
only of `Requires=` refuses the whole transaction. A transaction that
conflicts with a running one replaces it, or is refused, by systemd's rules
for `replace` and `fail`. Operations that are not ordered against each other
run at once. That parallelism is the reason to have a graph at all.

Every service and socket gets `After=sysinit.target` and
`Before=shutdown.target Conflicts=shutdown.target` unless it sets
`DefaultDependencies=no`. That is what makes shutdown stop everything
without every unit saying so.

The targets shipped with the image:

```
sysinit.target     /run and /sys/fs/cgroup mounted, generators run, hostname set
basic.target       sysinit + the builtins (§7.2)
network.target     after net.builtin; udhcpc's unit is WantedBy it
multi-user.target  gettys, sshd
graphical.target   multi-user + hyprix
rescue.target      one shell on the console, nothing else
shutdown.target / poweroff.target / reboot.target
```

`default.target` is a link to one of them, and `ferrix.target=` on the kernel
command line overrides it, as `systemd.unit=` does.

### 4.4 Services

```ini
# /lib/ferrix/units/getty@.service
[Unit]
Description=Login prompt on %i
After=basic.target

[Service]
Type=exec
ExecStart=/sbin/getty %i
Restart=always
RestartSec=0
TTYPath=/dev/%i
TasksMax=512

[Install]
WantedBy=multi-user.target
```

| Key | Meaning here |
|---|---|
| `Type=` | `simple`, `exec` (ready once `execve` succeeded), `oneshot`, `forking`, `notify` (§5.3), `native` (a native program started with `process_create` in the cgroup's job, ready on its READY message) |
| `ExecStart=`, `ExecStartPre=`, `ExecStartPost=`, `ExecStop=`, `ExecReload=` | As systemd; `-` before a path ignores its failure |
| `Restart=` | `no`, `on-failure`, `on-abnormal`, `always` |
| `RestartSec=`, `StartLimitBurst=`, `StartLimitIntervalSec=` | Backoff and the budget (§5.4) |
| `KillMode=` | `control-group` (the default: every member of the cgroup), `mixed` (the signal to the main process, then `cgroup.kill` for the rest) or `process` |
| `KillSignal=`, `TimeoutStopSec=` | The polite signal, and how long before `cgroup.kill` |
| `Slice=` | Which slice the cgroup goes under; `system.slice` by default |
| `MemoryMax=`, `MemoryHigh=`, `TasksMax=`, `CPUWeight=`, `CPUQuota=`, `IOWeight=` | The resource limits of §5.5 |
| `OOMPolicy=` | `stop` (the default), `continue` or `kill`, when the kernel's OOM kill reaches the service |
| `Delegate=` | `yes` gives the service its cgroup subtree to manage (C7) |
| `User=`, `Group=`, `WorkingDirectory=`, `Environment=`, `EnvironmentFile=` | As systemd |
| `StandardInput=`, `StandardOutput=`, `StandardError=` | `null`, `tty`, `console`, `log` (§10) |
| `TTYPath=` | The terminal for `tty`; init makes it the controlling terminal of a new session |
| `Offers=`, `Uses=` | Names in the directory (§6) |

The sandboxing keys wait for the rest of stage 13 and are landing L13:
`PrivateTmp=`, `ProtectSystem=`, `PrivateNetwork=` (namespaces),
`SystemCallFilter=` (seccomp), and `NoNewPrivileges=`, which `prctl` answers
already. Until then init warns about them and runs the service without them.
It does not refuse the service, because a unit that loads on systemd should
load here.

`[Install]` takes `WantedBy=`, `RequiredBy=` and `Alias=`. `svc enable` makes
the links in `/etc/ferrix/units/<target>.wants/` that systemd makes.

## 5. Supervision: a service is a cgroup

### 5.1 The tree

Init mounts cgroup2 at `/sys/fs/cgroup` (C1) and builds systemd's layout,
because tools that read it already know it:

```
/sys/fs/cgroup/
  init.scope/                       pid 1 itself
  system.slice/
    getty@console.service/
    sshd.service/
    hyprix.service/
      app-foot-12.scope/            a client hyprix asked init to group (§5.6)
  user.slice/
    user-1000.slice/
      session-1.scope/              the shell a getty's login started
  drivers.slice/                    devmgr's jobs, as C8 makes them visible (§7.3)
```

Init moves itself into `init.scope` first, because cgroup v2 lets processes
live only in leaves once a cgroup's controllers are enabled for its children.
Then it enables the controllers that exist in the root's `subtree_control`,
then in each slice's as it creates the slice. A slice's limits bound the
whole branch beneath it. So `MemoryMax=` on `system.slice` keeps the
services, together, from starving the login sessions.

Init is the one writer of this tree. Everything else reads it, except a
service with `Delegate=yes`: its subtree is chowned to its `User=` (C7), and
init never writes beneath it.

### 5.2 Starting and stopping

A Linux service starts as follows:

1. Init `mkdir`s the service's cgroup under its slice, writes its limits, and
   opens the directory.
2. `clone3` with `CLONE_INTO_CGROUP` and that directory (C3). The child is in
   the service's cgroup from its first instruction, so nothing it does before
   `execve`, or at any time after, can land anywhere else. `fork` inherits
   the cgroup (C2), so a daemon that forks twice stays the service's.
3. The parent hands the child its bootstrap channel (K3), if the unit has a
   `Uses=` or `Offers=`, and then writes one byte on a pipe the child is
   blocked on.
4. The child sets up its credentials, session, terminal, descriptors and
   environment, then calls `execve`.
5. The pipe closes on exec (`O_CLOEXEC`), which is how the parent learns that
   `execve` succeeded (`Type=exec`), as `posix_spawn` implementations do.

A native service is `job_for_cgroup` on the directory (C8), then
`process_create` in that job and `process_start` with its bootstrap channel.
That is exactly what `devmgr` does for a driver, so a native service is in
the service's cgroup just as a Linux one is.

To stop a service, init runs `ExecStop=` if the unit has one. Then it sends
`KillSignal=` (`SIGTERM` by default) as `KillMode=` says: to the main process
only, or to every pid in `cgroup.procs`. It waits up to `TimeoutStopSec=` for
`cgroup.events` to say `populated 0` (C4), then writes `cgroup.kill` (C5),
and finally `rmdir`s the cgroup. So the service is stopped when its cgroup is
empty, not when its main pid exits. A service whose main process exits while
other processes remain is `deactivating` until they go.

### 5.3 Readiness

A service is *active* when it says so, not when it has started. Which of
these a service uses is set by its `Type=`:

* `exec`: `execve` succeeded (§5.2 step 5).
* `notify`: the service writes a line to descriptor `NotifyFd=`, a pipe init
  passed it. `READY=1` means ready and `STATUS=…` is shown by `svc status`.
  This is s6's readiness descriptor with sd_notify's words. It is not
  sd_notify itself, because that protocol sends a datagram to a named socket
  and Ferrix answers that with `EOPNOTSUPP` today. When the kernel sends
  datagrams to names, `NOTIFY_SOCKET` is a second transport for the same
  parser, and ported daemons that speak sd_notify work unchanged.
* `native`: the service's first message on its bootstrap channel is READY.
  A driver's HELLO is the same idea.
* `forking`: the parent exited 0. The main pid is the one the service writes
  to `PIDFile=`; failing that, it is the one process left in the cgroup.

### 5.4 Restarting

Restarting is a policy function in `libs/svc`: given the history of a
unit's exits and the clock, restart now, restart at `t`, or give up. The
defaults are systemd's: `RestartSec=100ms`, and at most five starts in ten
seconds, after which the unit is `failed` and stays so until
`svc reset-failed` or a new start. Each restart doubles the delay, up to
`RestartSec=` times 32. A restart always begins with an empty cgroup: a
service's leftovers from its last run are killed before it starts again.

`devmgr` has the same problem with no clock. It restarts a display driver at
most eight times, counted (`docs/DEVMGR.md` §4). The policy function takes
`Option<Instant>`: with none it falls back to a pure count, and with a clock
it uses the rate. So `devmgr` and init share one tested implementation, and
when `devmgr` becomes a unit under init (§7.3) its drivers' budget can
become a rate.

### 5.5 Resources

Each resource key writes one controller file in the service's cgroup (C6):

| Key | File | Controller |
|---|---|---|
| `MemoryMax=` | `memory.max` | memory |
| `MemoryHigh=` | `memory.high` | memory |
| `TasksMax=` | `pids.max` | pids |
| `CPUWeight=` | `cpu.weight` | cpu |
| `CPUQuota=` | `cpu.max` | cpu |
| `IOWeight=` | `io.weight` | io |

`svc set-property unit Key=value` writes the file at run time, and writes
the key to a drop-in only if `--persistent` is given. A key whose controller
the kernel does not have is a warning and is skipped, so a unit written for
the full set loads on a kernel that has built only `memory` and `pids`.

When the kernel's OOM kill takes a process in a service, `memory.events`'
`oom_kill` count grows, and `poll` reports it like `cgroup.events`. Init
records the result `oom-kill`, and `OOMPolicy=` decides the rest: `stop`
takes the whole service down, since a service missing one process is often
worse than a stopped one, and `Restart=` then applies as for any failure.
This is where stage 13's exit, an OOM kill scoped to one cgroup, becomes
something a person sees: `svc status` says which service it was.

### 5.6 Scopes: processes init did not start

A login shell is started by getty's `login`, not by init. A terminal window's
shell is started by hyprix's `exec`. Both should be accountable, limitable
and killable as a unit. A **scope** is a cgroup for processes that already
exist. A program asks init for one over the control socket, with the pids to
move in and the slice to put the scope under:

```
svc scope --slice user-1000.slice --unit session-1.scope --pid 4242
```

Init makes the cgroup, moves the pids into it (C2) and supervises it from
then on like a service it did not start. A scope has no `ExecStart=`, is
stopped by signal and `cgroup.kill`, and is removed once empty. `getty`
creates `session-N.scope` for the login it runs. hyprix creates
`app-<name>-<n>.scope` for each program it starts, so `svc status` answers
which window a runaway process came from, and `svc stop` closes all of it.

### 5.7 pid 1's other duties

Init reaps every orphan the kernel reparents to it. An exit that matches no
unit's main process is reaped and forgotten, because the orphan is still in
its service's cgroup, and `populated` is what init listens for. Init sets
`PR_SET_CHILD_SUBREAPER` anyway, so a future nested manager (a
`Delegate=yes` session manager) behaves the same. It ignores every signal but
the ones it acts on (§8.2). If it panics, it does not unwind: it is built with
`panic = "abort"`, and §8.3 says what the kernel does next.

## 6. Capabilities and the directory

This is the part of the design that a microkernel needs and systemd does not
have. The cgroup tree says what a service may *use*; the directory says what
it may *reach*.

**The kernel gives init its capabilities (K2).** The kernel starts init with
a bootstrap channel, just as it starts `devmgr`. Before init runs, the kernel
writes one message on its end carrying the handles init may pass on. The
first version carries none that init needs to start: its jobs come from
cgroupfs through C8. The channel exists so that later messages can carry a
power handle, and in a microkernel the device root and physical memory, which
`devmgr` receives today (§7.3). How a Linux program finds its bootstrap
handle is **K3**: the `process_bootstrap` call returns it once, then nothing.

**Init gives each service a bootstrap channel.** Init keeps the other end.
On that channel the service can:

```
READY    service -> init   Type=native readiness (§5.3)
OFFER    service -> init   name; one handle: a channel end init sends OPENs down
OPEN     service -> init   name; one handle: the client's end of a new channel
CONNECT  init -> provider  name, the client unit; one handle: that end, forwarded
REFUSED  init -> service   name; why
```

A unit declares what it offers and uses:

```ini
# /lib/ferrix/units/clipboard.service
[Service]
Type=native
ExecStart=/sbin/vdagent
Offers=ferrix.clipboard

# /lib/ferrix/units/hyprix.service
[Unit]
Wants=clipboard.service
[Service]
ExecStart=/bin/hyprix --config /etc/hyprland.conf
Uses=ferrix.clipboard
Delegate=yes
```

The client creates a channel pair, keeps one end and sends the other in OPEN.
Init checks that the client's unit lists the name in `Uses=`. It starts the
provider if the provider is not running, since an OPEN is an activation just
as a connection to a `.socket` is, and forwards the end in CONNECT. From then
on the two talk directly; init is out of the path. A name nobody offers, or
that the unit does not declare, gets REFUSED.

Four things follow:

* **The unit files are the policy.** What a service can reach is what its unit
  says, and nothing reaches a service except through a channel init routed. No
  global namespace exists to search. A compromised service can open only the
  names it declared.
* **Handles never pass through the core.** An OPEN arrives as `Event::Open`
  carrying a `Token`, and the backend moves the real handle when the core
  returns `Action::Route`.
* **Linux services use it the same way.** A musl program may make native calls
  (`docs/ARCHITECTURE.md` §2), and `process_bootstrap` gives it its channel. A
  Linux service that makes no native call ignores the channel, and closing it
  costs nothing.
* **Sockets and the directory do the same job for two worlds.** A `.socket`
  unit gives a Linux daemon `LISTEN_FDS` and is activated on connect. An
  `Offers=` name gives a native service CONNECT and is activated on open.
  Both come from one manager with one dependency graph.

## 7. The backends, and who serves each

### 7.1 The table

| Backend | Today (Linux-ABI init, monolithic kernel) | In a microkernel |
|---|---|---|
| **Spawn** | `clone3(CLONE_INTO_CGROUP)`, `execve`; `process_create` in the cgroup's job for `Type=native` | the process server's spawn, or `process_create` everywhere |
| **Groups** | cgroupfs: `mkdir`, `rmdir`, `cgroup.procs`, `cgroup.kill` | the jobs behind them (C8): `job_create`, `job_kill` |
| **Resources** | cgroupfs controller files | limits on the job, which `docs/ARCHITECTURE.md` §3 already puts there |
| **Supervise** | `wait4` on `SIGCHLD`; `cgroup.events` and `memory.events` by `POLLPRI`; process `TERMINATED` on a port | the job's `EMPTY` (C8) and the process's `TERMINATED`, on a port; statuses from K6 |
| **Clock** | `clock_gettime`; the event loop's timeout (§9) | a timer object on the port |
| **Filesystem** | `mount(2)`, `umount2(2)`, `sync(2)` | a channel to the VFS server, from the directory |
| **Terminal** | `open` of `/dev/*`, `setsid`, `TIOCSCTTY` | a channel to the console server |
| **Power** | `reboot(2)` | the power handle from K2 |
| **Directory** | init's own, over bootstrap channels | unchanged: it is already what a root task is |

Each backend is a Rust trait in `init/`, and the Linux implementations are
the only ones built in version 1. The table's right-hand column is not
promised work. It is the check that nothing in `libs/svc` would have to change
if that column were built. It holds for the Groups, Resources and Supervise
rows only because of C8. Without C8, the microkernel column of those three
rows would need a cgroup server rebuilt from nothing.

### 7.2 What the kernel provides is a unit too

The filesystem, the network stack, the block core and `devmgr` are in the
kernel today (`devmgr` is started by it). In a microkernel they would be
services. So init has units for them now, of kind `.builtin`: always active,
started by nobody, stopped by nobody.

```
vfs.builtin   net.builtin   block.builtin   devmgr.builtin
```

A unit written today says `After=net.builtin` or `Uses=ferrix.vfs`, and it is
correct on both kernels. The day the network stack becomes a server,
`net.builtin` is replaced by `net.service` with `Alias=net.builtin`, and no
unit that depended on it changes. The names are the contract. Whether a kernel
subsystem or a process serves one is an implementation detail, which is the
whole point.

### 7.3 `devmgr`, and the one step a microkernel changes

Today the kernel starts `devmgr` before init, because the root volume needs a
ring-3 block driver before any file on it can be read (`docs/ARCHITECTURE.md`
§7, "Bootstrap"). Init starts from the initramfs copy, as `devmgr` does, so
nothing in this design has to change that order. With C8, `devmgr`'s root
job and each driver's job are cgroups. Init adopts them read-only as
`drivers.slice`, so a driver's memory shows in `svc status devmgr.builtin`,
though init neither starts nor stops them.

The step toward a microkernel is to reverse it. The kernel starts only init,
and hands it on K2's channel what it now hands `devmgr`: the device nodes and
the driver images. Init then starts `devmgr.service` (`Type=native`,
`Slice=drivers.slice`) with them. The DEVICES protocol (`docs/DEVMGR.md` §2)
stays the same; init writes it instead of the kernel. `devmgr.builtin`
becomes `devmgr.service`, as in §7.2. `DEVMGR.md` §5's rule, that no driver
may fault on the disk it serves, still holds, because init starts `devmgr`
from the initramfs before any mount it depends on. This is landing L12,
marked *later*, and it is the rehearsal that proves the backend table true.

## 8. Boot and shutdown

### 8.1 Boot

The kernel runs its boot checks, starts `devmgr`, switches the root, then
starts **the program `ferrix.init=` names** (K0, built in L3). The file may
be a `#!` script, run under its interpreter as `execve` runs one. A file that
is missing or will not start is said on one line (`init     ferrix.init=…
could not be started: …; falling back to the built-in program`), and the
built-in program runs as before. With nothing named, the order is the
built-in program, then `/sbin/init` if the image has one, then nothing. The
built-in program goes first so that no gate that embeds a shell or hyprix
can change: none of today's images carries a `/sbin/init`, and one that
starts to must not take a gate's boot from it. `test-init` (§15) embeds
nothing and names `/sbin/init`, so it reaches init either way.

Init then:

1. Mounts `/run` (tmpfs), then cgroup2 at `/sys/fs/cgroup`. It moves itself
   into `init.scope` and enables the controllers the kernel has (§5.1).
   `/proc`, `/dev`, `/sys` and `/tmp` are already mounted by the kernel
   today; they have `.mount` units, whose mount
   is skipped when the kernel already mounted them. That way a kernel that
   stops mounting them changes nothing above it.
2. Receives K2's first message.
3. Runs the generators in `/lib/ferrix/generators/`, each with
   `/run/ferrix/units` as its argument, with a deadline. The first one shipped
   is `getty-generator`, which writes a `getty@<name>.service` link into
   `multi-user.target.wants` for every terminal the kernel command line names
   in `console=`, and for `/dev/console` otherwise.
4. Loads the units and starts `default.target`, or `ferrix.target=`.
5. Prints one boot-log line per unit that becomes active or fails, in the
   `  init     …` format `xtask` reads.

If `default.target`'s transaction fails, init starts `rescue.target`, a shell
on the console, so a broken unit file is fixable from the machine itself.

### 8.2 Shutdown

`svc poweroff`, `svc reboot`, or `SIGTERM`/`SIGINT` to pid 1 (Ctrl-Alt-Del's
signal, and what a QEMU `system_powerdown` will become) start
`poweroff.target` or `reboot.target`. Starting either stops everything that
`Conflicts=shutdown.target`, which is everything by default (§4.3), in
reverse dependency order, each unit by its own `KillMode=`. Then init:

1. writes `cgroup.kill` in every cgroup left beside `init.scope`, deepest
   first, and waits for each to report `populated 0`;
2. calls `sync`, remounts `/` and `/data` read-only, and unmounts the rest in
   reverse order;
3. calls `reboot(2)`.

A process in no service does not survive step 1: it is in some cgroup, and
every cgroup but init's is killed. The cgroup tree is what makes "stop
everything" mean everything.

`reboot(2)` commits `/` and `/data` before it acts (**K7**, built in L3), as
`power::finish` does, so a program calling it directly cannot lose a btrfs
transaction either. Init's own `sync` in step 2 then leaves it nothing to
commit.

### 8.3 When init dies

Linux panics. The kernel here does what it does today when pid 1 exits: it
prints `init     … exited with N` and runs `power::finish`, which syncs and
powers off. That is what every gate relies on, so it stays. The kernel option
`ferrix.onexit=panic`, beside `reset`, gives Linux's behaviour to anyone who
wants it (L3): the disks are committed, and then the kernel panics with
`FX-1501`.

## 9. The event loop

Init waits in one `epoll_wait`. Its descriptors are:

* a self-pipe that the handlers for `SIGCHLD`, `SIGTERM` and `SIGINT` write to
  (there is no `signalfd`);
* each cgroup's `cgroup.events`, and each `memory.events` where the memory
  controller is on, with `EPOLLPRI` (C4);
* the control socket, and each connected `svc` client;
* each `Type=notify` service's readiness pipe;
* each `.socket` unit's listening socket;
* **the port**, through K4's `port_fd`: a descriptor that is readable while
  the port has packets. On the port, init watches every native service's
  process for `TERMINATED`, and every bootstrap channel for READABLE and
  PEER_CLOSED.

The timeout is `Manager::deadline()`, so no `timerfd` is needed.

K4 is needed because native waits and file descriptors cannot see each other
today. The bridge goes this way round, a port becoming a descriptor, because
a descriptor that polls is one `File` implementation, while a port that
watches descriptors would reach into every file type's wakeups. hyprix and
the terminal have the same problem as soon as they use a native service, so
K4 is not init's alone.

A native init, the microkernel column of §7.1, would wait on the port alone.
It would watch each job's `EMPTY` in place of `cgroup.events`, and bind the
signals it needs to the port. It would be a different loop over the same
`Manager`.

## 10. Control and logs

`/run/ferrix/control` is a stream socket. Requests and replies are
length-prefixed records in `libs/svc-proto`, not text, so `svc`'s output can
change without breaking another client. `SO_PEERCRED` says who is asking:
anyone may ask for status. Only root may change state, except that a user
may make a scope for their own processes under their own `user-<uid>.slice`.
`svc` takes the verbs people already know:

```
svc status [unit]      svc start|stop|restart|reload unit
svc list [--failed]    svc enable|disable|mask|unmask unit
svc log unit           svc daemon-reload   svc isolate target
svc poweroff|reboot    svc reset-failed [unit]
svc scope …            svc set-property unit Key=value [--persistent]
svc top                (cgroup usage per unit, from the controller files)
```

There is no journal. A service's standard output and error default to `log`:
a pipe init reads, prefixes each line with the unit's name, writes to the
console, and keeps in a ring of the last 256 lines per unit for
`svc log unit`. A journal, if one is ever wanted, is a service that takes
`Uses=ferrix.log`, and nothing here needs to be written differently for it.

## 11. What the kernel needs, besides stage 13

Stage 13's cgroups are §0's prerequisite, sized by stage 13 and not here.
Beyond them, init needs these six small items. Each is argued by something
outside init as well:

| | Change | Also wanted by | Points |
|---|---|---|---|
| K0 | `ferrix.init=<path>`: start pid 1 from a file, the embedded program as the fallback. **Done in L3** | any image that is not a gate | 2 |
| K2 | Init started with a bootstrap channel, as `devmgr` is | §7.3 | 2 |
| K3 | `process_give(pid, handle)`: a parent installs one handle in its own child that has not yet called `execve`; `process_bootstrap()` returns that handle once, to the child | any Linux program that starts a native-aware one | 2 |
| K4 | `port_fd(port)`: a descriptor readable while the port has packets | hyprix and the terminal, once they use a native service | 3 |
| K6 | Read a process's exit status and signal from its handle (in the reserved `0x1032..0x1037`) | `devmgr` reports 137 for every death today | 1 |
| K7 | `reboot(2)` syncs `/` and `/data` first, as `power::finish` does. **Done in L3** | any program calling it | 1 |

Together that is 11 points. None of the six changes the ABI of an existing
call. Version 1's K1 (jobs inherited by `fork`) and K5 (a signal to every
member of a job) are gone, because C2 and `cgroup.procs` are the same things
in Linux's own words. The numbers are kept so that version 1's references
still resolve.

A **second terminal** is a separate item, outside these points. devfs has
`console`, `tty` and `ptmx`, and no second serial device. A getty per terminal
is built here and gated on the console. A second terminal needs a second UART
node or `hvc0` over the virtio-console library that `vport` already uses, and
that belongs to the rest of stage 15's word *ttys*.

## 12. The order

```
stage 13, cgroups: C1-C5 ──┬──> L3 ──> L4 ──┬──> L5 (with C6, C7) ──> L10
                  (then C6,│                ├──> L6, L7, L9
                   C7, C8) │                └──> L8 (with C8) ──> L12 (later)
L1 ──> L2 ─────────────────┘
stage 13, namespaces + seccomp ─────────────────> L13
```

L1 and L2 are host-only and need nothing from the kernel, so they can be
built while stage 13 is. Init's first boot, L4, waits for C1 to C5.

## 13. Landings and points

In story points, each landing gated by the row it names. Stage 13's own
points are not here; the roadmap sizes that stage at about 60, and its
cgroup half is what §0 asks for first.

| | Landing | Needs | Gate | Points |
|---|---|---|---|---|
| L1 | `libs/svc`: the unit-file parser, drop-ins, templates, the model; a fuzzer | | host tests, Miri, fuzz | 5 |
| L2 | `libs/svc`: the graph, transactions, operations, the slice tree, the restart policy, `step` | L1 | host tests replaying event scripts | 8 |
| L3 | K0, K7. **Done 2026-09-24** (§16) | | `test-boot`, `test-shell` | 3 |
| L4 | `init` minimal: pid 1, reaping, `/run` and cgroupfs, `init.scope`, a cgroup per service, `simple`/`exec`/`oneshot`, `KillMode=`, restart, shutdown by `cgroup.kill`; `getty` and the generator | L2, L3, C1-C5 | `test-init` stages one and two (§15) | 10 |
| L5 | Slices and scopes; the resource keys; `OOMPolicy=`; `Delegate=` | L4, C6, C7 | `test-init` stage three | 6 |
| L6 | `svc` and the control socket; `log` output; `set-property`, `top` | L4 | `test-init` stage four | 5 |
| L7 | `notify` readiness; `forking` | L4 | host tests + `test-init` | 3 |
| L8 | K2, K3, K4, K6; `Type=native` in the cgroup's job; the directory (§6) | L5, C8 | `test-init` stage five | 16 |
| L9 | `.socket` units | L4 | `test-init`: sshd activated on connect | 5 |
| L10 | Move the images over: `cargo xtask run` and `run-compositor` boot init with `multi-user.target` / `graphical.target`; hyprix stops being pid 1 and makes a scope per client | L5, L6 | `test-compositor` under init | 6 |
| L11 | `devmgr` shares the restart policy | L2 | `test-restart` | 2 |
| L12 | *later*: the kernel starts init alone and init starts `devmgr` (§7.3) | L8 | `test-boot` on all three architectures | 8 |
| L13 | *after the rest of stage 13*: `PrivateTmp=`, `ProtectSystem=`, `PrivateNetwork=`, `SystemCallFilter=`, `NoNewPrivileges=` | L4, stage 13 | `test-init` stage six | 8 |

The kernel items of §11 are counted inside the landings that carry them.
L1 to L10 add up to 67 points, and L11 to L13 to 18 more. L1 to L4 are what
the roadmap calls "a working init". L5 is what makes it a resource manager.
L8 is what makes it one a microkernel could keep.

## 14. What the customer decides

1. ~~**C8, cgroups built over jobs.**~~ **Decided 2026-09-23: yes.** It is
   the one choice here that is stage 13's design and not init's, and the
   microkernel requirement rests on it (§7.1).
2. **Stage 13's order inside itself.** Draft: cgroups (C1 to C5, then the
   controllers, `memory` and `pids` first), then namespaces, then seccomp.
   Init waits for only the first of these.
3. **The unit syntax.** Draft: systemd's INI subset (§4.1), for familiarity.
   The alternative is TOML, which is cleaner but has no serde here; its parser
   would be hand-written like hyprlang's.
4. **Whether hyprix stops being pid 1** (L10). Draft: yes. Today a compositor
   crash powers the machine off, and under init it is a restart.
5. **Whether `devmgr` moves under init** (L12), and when. Draft: after the
   directory has been used for real, not before.
6. **What happens when init dies** (§8.3). Draft: keep `power::finish`, and
   add `ferrix.onexit=panic`.
7. **The names**: `/sbin/init`, `svc`, `/lib/ferrix/units`, `ferrix.*`
   directory names, systemd's slice names. Draft: as written.
8. **Where the design goes on the roadmap.** Draft: stage 15, whose remaining
   item it is, with stage 13's cgroups as its prerequisite. L12 belongs with
   whatever stage first takes the microkernel question up again
   (`docs/BACKLOG.md`, 2026-09-16).

## 15. The test

`cargo xtask test-init` boots `/sbin/init` from the image, with
`ferrix.init=` and no embedded program, on all three architectures. Each
landing adds a stage, and each stage requires its lines:

1. **Boot and a terminal** (L4). `multi-user.target` becomes active. The
   getty on the console gives a zinc prompt whose session and controlling
   terminal are its own, read from `/proc/self/stat`, not taken from the
   transcript. A test service that exits 1 is restarted by its budget and then
   reported `failed`. `svc poweroff` from the prompt ends with every unit
   stopped in reverse order, `sync`, and a `btrfs check` of the volume that
   finds it clean.
2. **Groups** (L4). A service forks twice and its main process exits. The
   grandchild's pid, read from a file the service wrote, is in the service's
   `cgroup.procs`, and stopping the service ends it.
3. **Resources** (L5). A service with `MemoryMax=64M` that allocates past it
   is OOM-killed and reported `oom-kill`, while a sibling service with no
   limit keeps running. A `TasksMax=` service's `fork` fails with `EAGAIN` at
   the limit.
4. **Control** (L6). The session types `svc status`, `svc restart`,
   `svc log`, and a non-root `svc stop` that must be refused.
5. **The directory** (L8). A native test service is offered and started on
   first OPEN, and is found in its unit's cgroup. A unit that does not declare
   the name is REFUSED.

Every stage has a negative control, per this repository's rule. It must show
it fired: a marker line, a sabotage that matches exactly one line, then that
stage's own failure. Stage two's control, for example, is `KillMode=process`
on the forking service, which must leave the grandchild alive and fail that
stage's check. `test-jobs` moves onto a getty under init, and then it tests
the system people will actually use.

## 16. Where it stands (2026-09-24)

| | State | On `main` as |
|---|---|---|
| L3 | done, 2026-09-24 | "Start pid 1 from the file ferrix.init= names, and commit the disks in reboot(2)" |
| L1 | done, 2026-09-24 | "Read unit files in systemd's syntax" |
| L2 | being built on `init/svc` | |
| L4 to L13 | not started | |

59 of L1 to L10's 67 points are left.

**L1, as built (5 points).** `libs/svc` is on `main`: `no_std` with
`alloc`, `forbid(unsafe_code)`, 52 host tests, a Miri step in CI and in
`cargo xtask check --miri`, and the `svc_unit` fuzz target with a seed
corpus. What it does, module by module:

* `ini`: §4.1's syntax, read as systemd's `conf-parser.c` reads it,
  including the corners: a comment line in the middle of a continued line is
  dropped, an escaped backslash does not continue, a continuation at the end
  of the file is kept, CRLF and a byte-order mark are read, and a section
  header without its bracket refuses the file. Every other fault is a
  warning in systemd's words.
* `source`: the three directories as a `Source` the backend fills with
  `add(layer, path, Entry)`, where an entry is a file's bytes, `Masked` (a
  link to `/dev/null`) or `Alias(name)`. `Source::load(name)` does the rest:
  the highest file under the name, then under its template; aliases,
  instantiated through templates; drop-ins of the unit, its aliases and its
  template, a higher layer's file hiding a lower one of the same name,
  applied in file-name order; `.wants/` and `.requires/` links; specifiers.
* `name`, `specifier`, `value`, `exec`: names with templates and slice
  parents, path escaping for mounts, `%i %I %n %N %p %P %j %J %f %t %S %C %L
  %E %%`, booleans, time spans, base-1024 sizes, percentages, signals,
  quoted and C-escaped words, and `Exec…=` lines with their `-@:+!` prefixes
  and `;` separators.
* `unit` and `kind`: `[Unit]` (the nine dependency keys, seventeen
  conditions and assertions with `!` and `|`, start limits) and `[Install]`,
  then the `Kind` trait with the six kinds of version 1 and `.socket`, and
  every key of §4.4.

**What building L1 changed.**

* `Kind::parse` takes the unit's name as well as its section, because a
  mount's `Where=` must match its name. The trait gained `section()` and
  `needs_file()`; `implied` and `advance` come with L2.
* Slices, scopes and builtins load with no file. A builtin's name is its
  contract (§7.2), and the manager makes every slice a `Slice=` path names.
* An empty unit file masks, as it does in systemd.
* Specifiers are expanded in every value when the unit loads, except in the
  six resource keys, whose `%` is a percentage. systemd expands none there
  either. `%H`, `%m`, `%u` and the other specifiers that need the machine are
  refused by name, and the assignment carrying one is dropped with a
  warning.
* A link to a unit file under the link's own name is the backend's to
  follow: it hands in the file's bytes. Only a link to another name, an
  alias, or to `/dev/null` reaches the crate as a link.
* `StandardOutput=journal`, `kmsg` and their `+console` forms read as
  `log`, since the log reaches the console (§10). `console` is Ferrix's own
  value.
* `NotifyFd=` is a service key: the descriptor §5.3's readiness line is
  written to.
* The sandboxing keys of L13 warn by name. Type-wide drop-in directories
  (`service.d/`) are not read.
* Conditions are parsed into `Condition` values, and `conditions_hold`
  combines results. The tests themselves look at the machine, so they are
  the backend's to run.

**L3, as built (3 points).** K0: `ferrix.init=<path>` on the kernel command
line (`CMDLINE.TXT`, or U-Boot's `bootargs` on a board) starts pid 1 from
that file in the switched root, with the path as its only argument and
`PATH=/bin HOME=/ TERM=dumb` as its environment; a `#!` script runs under
its interpreter. The kernel prints `init     starting <argv>`, then
`init     <path> exited with <status>`. A missing or unstartable file prints
`init     ferrix.init=<path> could not be started: <why>; falling back to
the built-in program`, and a path that is not absolute is refused when the
option is read. `cargo xtask build`, `run` and `test-boot` take
`--init-path <PATH>`, which writes `ferrix.init=<PATH>` into `CMDLINE.TXT`;
`qemu::init_option` builds the same option for a test (§8.1 for the order
of the defaults). K7: `reboot(2)` calls `power::sync_disks` before power-off,
halt and restart. `ferrix.onexit=panic` makes init's exit panic with
`FX-1501` after the disks are committed (§8.3).

`cargo xtask test-shell` is the gate (`xtask/src/init_file.rs`). After its
built-in boot it boots the same shell and script from files,
`ferrix.init=/etc/shell-test`, and requires the kernel's `starting` and
`exited with 7` lines and no fallback. Under busybox (`--init`) it then
writes `/data/k7` on a fresh volume and runs `poweroff -f -n`, which skips
busybox's own `sync`. A second boot of that volume, under
`ferrix.onexit=panic`, must read the file back and then panic with
`FX-1501`. A test boot has no root disk and so no committer, so only the
call can have committed the file. The three negative controls fired by the
checks' own messages: a wrong path (the fallback line, then "the kernel
fell back to the built-in shell"), no `sync_disks` in `reboot(2)` ("/data/k7
did not survive poweroff -f -n"), and no `ferrix.onexit=panic` ("did not
panic with FX-1501"). Under zinc only the first boot runs, because zinc
cannot make the call.

**What the next session does first.** L2 is being built on `init/svc`, in
the same crate: `Kind::implied`, the graph, transactions, the slice tree,
the restart policy, `Manager::step` and `deadline`, and the names the init
program maps (`UnitId`, `GroupPath`, `Token`, `ClientId`). Then L4, once
L2 and C1 to C5 are in
(`docs/CGROUPS.md` §7.1 has G4 for C3). `cargo xtask test-init` builds an
image with no program in the kernel and `/sbin/init` in the initramfs, and
puts `qemu::init_option("/sbin/init")` into `CMDLINE.TXT`, as
`init_file::Parts::image` does. It judges the kernel's `init     …` lines
as `init_file` does.
