# init

`/sbin/init`, `/sbin/getty` and the getty generator: the service manager
`docs/INIT.md` designs, as far as landing L4 takes it.

The manager itself is `libs/svc`, a pure `no_std` crate of the Ferrix
workspace: unit files in systemd's syntax, the dependency graph, operations,
the slice tree and every kind's state machine, as `Manager::step(event, now)
-> actions`. What is here is everything around it that makes system calls:

* `init/` -- pid 1. It mounts `/run` and cgroup2, moves itself into
  `init.scope`, runs the generators, reads the three unit directories, and
  then waits in one `epoll_wait` on a signalfd, each child's exec report and
  each cgroup's `cgroup.events`. What it finds becomes the manager's events;
  the manager's actions become `mkdir`, `clone3(CLONE_INTO_CGROUP)`, `kill`,
  `cgroup.kill` and, at the end, `reboot(2)`. `src/sys.rs` holds every
  `unsafe` block.
* `getty/` -- `getty TTY` gives a terminal a session and a login shell;
  `getty-generator DIR` links a getty into `multi-user.target` for each
  console.
* `units/` -- the targets, `getty@.service` and `rescue.service`, installed
  in `/lib/ferrix/units`.

It is a workspace of its own, as zinc and the compositor are, because these
are Linux programs built with std, and the Ferrix workspace's dependency
policy is written for a kernel.

## Building and testing

`cargo xtask test-init --arch all` builds it for each architecture, boots it
as pid 1 and types at the shell its getty gives (`docs/INIT.md` §15).
`cargo xtask check` runs its formatting, clippy and host tests. To build it
alone, from this directory:

```
cargo build --release --target x86_64-unknown-linux-musl
```
