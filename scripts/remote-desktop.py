#!/usr/bin/env python3
"""Boot the Ferrix desktop on another machine and watch it on this one.

`cargo xtask run-compositor --vnc :0` already serves the screen over VNC, and
`README.md` says how to reach it by hand: push the code over, run the command
there, open a tunnel, point a viewer down it. Four steps, three of which are
the same every time, and the one that is not -- getting *this* working tree
onto that machine -- is the one most easily got wrong, because a boot of
yesterday's code looks exactly like a boot of today's.

So this holds the four steps and a file holds the answers:

    python3 scripts/remote-desktop.py

The file names an `ssh` destination, a directory over there, what to boot and
where to put the screen. Nothing about any particular machine is written
here; `scripts/remote-desktop.toml.example` is the shape and the machines are
yours. That is the same rule `xtask` follows -- no host is a default, the
network is only what an argument asked for -- and it is why this is a script
beside `xtask` rather than a subcommand inside it.

What it does, in order:

1. Makes a commit of the working tree without touching your index, so what
   boots over there is what you are looking at here, uncommitted and all.
   `[source] send = "head"` sends the last commit instead.
2. Pushes it to a side ref in a checkout on the remote, making that checkout
   the first time, and checks the code out there by hash.
3. Opens one `ssh` that both forwards a local port and runs the boot, so the
   tunnel lives exactly as long as the machine does.
4. Waits for the VNC server to answer, then opens a viewer on it.

The serial console comes back on this terminal the whole time, because the
screen and the console are two different pipes and a boot that fails does so
on the console.

Usage:  python3 scripts/remote-desktop.py [--config PATH] [options] [-- ARGS]
        python3 scripts/remote-desktop.py --help
"""

from __future__ import annotations

import argparse
import contextlib
import os
import pathlib
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import tomllib

# The ref the code is pushed to over there. Not `refs/heads/...` on purpose:
# a checkout refuses a push to the branch it has checked out, and a ref
# outside `refs/heads` is never that branch whatever the checkout is doing.
REMOTE_REF = "refs/ferrix-desktop/head"

# Where the remote boot leaves its process id, so a teardown can find it if
# closing the connection did not take it with it.
PID_FILE = ".remote-desktop.pid"

# Config files this looks for when `--config` did not say, best first.
SEARCH = ("remote-desktop.toml", "~/.config/ferrix/remote.toml")

DEFAULTS: dict[str, dict[str, object]] = {
    "remote": {"host": None, "dir": "ferrix-desktop", "cargo": "cargo", "env": {}},
    "boot": {"command": "run-compositor", "arch": "x86_64", "args": []},
    "screen": {"display": 0, "local_port": "auto", "viewer": "auto", "wait": 1800},
    "source": {"send": "working-tree"},
}


class Failed(Exception):
    """Something went wrong that the person can act on; no traceback wanted."""


# --------------------------------------------------------------------------
# The configuration file


def find_config(explicit: str | None) -> pathlib.Path:
    """Where the answers are, in the order this looks for them."""
    if explicit:
        path = pathlib.Path(explicit).expanduser()
        if not path.is_file():
            raise Failed(f"no config file at {path}")
        return path
    if name := os.environ.get("FERRIX_REMOTE"):
        path = pathlib.Path(name).expanduser()
        if not path.is_file():
            raise Failed(f"FERRIX_REMOTE names {path}, which is not a file")
        return path
    for name in SEARCH:
        path = pathlib.Path(name).expanduser()
        if path.is_file():
            return path
    looked = "\n  ".join(str(pathlib.Path(n).expanduser()) for n in SEARCH)
    raise Failed(
        "no config file. Looked at $FERRIX_REMOTE and:\n  "
        f"{looked}\n"
        "Copy scripts/remote-desktop.toml.example to one of those and fill it in."
    )


def load(path: pathlib.Path) -> dict[str, dict]:
    """Read the file over the defaults, refusing a key that means nothing.

    A typo in a key name is otherwise a setting that silently does not apply,
    which on a tool whose whole job is "the same every time" is the worst kind
    of bug: everything works and one thing is not what you asked for.
    """
    try:
        given = tomllib.loads(path.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as error:
        raise Failed(f"{path}: {error}") from error
    config = {section: dict(values) for section, values in DEFAULTS.items()}
    for section, values in given.items():
        if section not in config:
            raise Failed(f"{path}: no section named [{section}]")
        if not isinstance(values, dict):
            raise Failed(f"{path}: [{section}] should be a table")
        for key, value in values.items():
            if key not in config[section]:
                known = ", ".join(sorted(config[section]))
                raise Failed(f"{path}: [{section}] has no `{key}`. It has: {known}")
            config[section][key] = value
    if not config["remote"]["host"]:
        raise Failed(f"{path}: [remote] host is the one thing with no default")
    if config["source"]["send"] not in ("working-tree", "head"):
        raise Failed(f"{path}: [source] send is `working-tree` or `head`")
    return config


# --------------------------------------------------------------------------
# Running things


def run(argv: list[str], *, capture: bool = True, cwd: str | None = None) -> str:
    """One command, its output, and a failure that says which command it was."""
    result = subprocess.run(
        argv,
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
        text=True,
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout or "").strip()
        raise Failed(f"`{shlex.join(argv)}` failed ({result.returncode})\n  {detail}")
    return (result.stdout or "").strip()


def git(*args: str, cwd: str | None = None, env: dict | None = None) -> str:
    """`git` in this checkout, with an environment this may want to change."""
    argv = ["git", *args]
    result = subprocess.run(
        argv,
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )
    if result.returncode != 0:
        raise Failed(f"`{shlex.join(argv)}` failed\n  {result.stderr.strip()}")
    return result.stdout.strip()


# --------------------------------------------------------------------------
# What gets sent


def snapshot(send: str, root: str) -> tuple[str, str]:
    """The commit to boot over there, and a word for what it is.

    `working-tree` is the default because the question being asked is almost
    always "does my change work", and a change you have not committed is still
    a change. It is built in an index of its own -- `GIT_INDEX_FILE` -- so the
    real index is untouched: the alternative, `git stash` or `git add`, is
    shared with every other worktree of this repository and has cost work here
    before. Nothing is committed to any branch; the commit is an object that
    exists to be pushed, and `git gc` collects it once the side ref moves on.
    """
    head = git("rev-parse", "HEAD", cwd=root)
    if send == "head":
        return head, f"HEAD ({git('log', '-1', '--format=%s', cwd=root)})"
    dirty = git("status", "--porcelain", cwd=root)
    if not dirty:
        return head, f"HEAD, working tree clean ({git('log', '-1', '--format=%s', cwd=root)})"
    with tempfile.TemporaryDirectory() as scratch:
        env = dict(os.environ, GIT_INDEX_FILE=str(pathlib.Path(scratch) / "index"))
        git("read-tree", "HEAD", cwd=root, env=env)
        git("add", "--all", cwd=root, env=env)
        tree = git("write-tree", cwd=root, env=env)
    commit = git(
        "commit-tree",
        tree,
        "-p",
        head,
        "-m",
        "remote-desktop: the working tree as it was",
        cwd=root,
    )
    return commit, f"the working tree ({len(dirty.splitlines())} files differ from HEAD)"


# --------------------------------------------------------------------------
# The remote side


def remote_dir(given: str) -> str:
    """The directory over there, as a remote shell will read it.

    `~` is not expanded by this machine and must not be quoted into a literal
    over there, so a path under the remote home is written relative and the
    remote shell's own working directory -- the home -- does the rest.
    """
    path = given.strip()
    if path.startswith("~/"):
        return path[2:]
    if path == "~":
        raise Failed("[remote] dir cannot be the home directory itself")
    return path


def ssh_base(host: str) -> list[str]:
    """`ssh` with the options this needs and nothing about any host.

    Everything else -- the user, the key, the `ProxyJump` two hops away -- is
    `~/.ssh/config`'s business, which is where a person already keeps it.
    """
    return [
        "ssh",
        "-o",
        "BatchMode=no",
        "-o",
        "ServerAliveInterval=30",
        host,
    ]


def prepare(host: str, directory: str) -> None:
    """Make the checkout over there, once, and say nothing when it is there."""
    script = "; ".join(
        [
            f"mkdir -p {shlex.quote(directory)}",
            f"cd {shlex.quote(directory)}",
            'test -d .git || git init --quiet .',
        ]
    )
    run([*ssh_base(host), script])


def boot_script(config: dict, directory: str, sha: str, display: int, extra: list[str]) -> str:
    """The one command the boot `ssh` runs over there.

    It is one string and not a script on stdin, deliberately: QEMU's serial
    console reads this connection's stdin, and a script fed that way is eaten
    by the guest a line at a time. The caller closes stdin as well.
    """
    cargo = str(config["remote"]["cargo"])
    argv = [
        cargo,
        "xtask",
        str(config["boot"]["command"]),
        "--arch",
        str(config["boot"]["arch"]),
        "--vnc",
        f":{display}",
        *[str(a) for a in config["boot"]["args"]],
        *extra,
    ]
    exports = [
        f"export {name}={shlex.quote(str(value))}"
        for name, value in dict(config["remote"]["env"]).items()
    ]
    return "; ".join(
        [
            "set -e",
            # rustup and QEMU are both usually installed under the home, and a
            # non-interactive `ssh` reads none of the files that would say so.
            'export PATH="$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:$PATH"',
            *exports,
            f"cd {shlex.quote(directory)}",
            f"git checkout --quiet --detach {shlex.quote(sha)}",
            'echo "remote: booting $(git rev-parse --short HEAD) in $PWD"',
            # For a teardown that has to reach past a connection which did not
            # take the boot with it when it closed.
            f'printf "%s\\n%s\\n" "$$" "$(ps -o pgid= -p $$ | tr -d \' \')" > {PID_FILE}',
            f"exec {shlex.join(argv)}",
        ]
    )


def teardown(host: str, directory: str) -> None:
    """Best effort: make sure nothing of ours is still running over there.

    Closing the connection sends the boot a `SIGHUP` and that is usually the
    end of it. Usually is not always, and a QEMU nobody is watching holds a
    machine's memory and its VNC port against the next run, so this asks.
    """
    pid_path = f"{directory}/{PID_FILE}"
    script = "; ".join(
        [
            f"test -f {shlex.quote(pid_path)} || exit 0",
            f"read pid < {shlex.quote(pid_path)}",
            f'pgid=$(sed -n 2p {shlex.quote(pid_path)})',
            'test -n "$pgid" && kill -TERM -"$pgid" 2>/dev/null || true',
            'kill -TERM "$pid" 2>/dev/null || true',
            f"rm -f {shlex.quote(pid_path)}",
        ]
    )
    with contextlib.suppress(Exception):
        subprocess.run(
            [*ssh_base(host), script],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=30,
        )


# --------------------------------------------------------------------------
# This side: the port, the wait, the viewer


def pick_port(asked: object, display: int) -> int:
    """Which local port the tunnel listens on.

    `auto` because the obvious choice, 5900 + display, is also what a VNC
    server already running on *this* machine would have taken, and the
    failure that causes -- a viewer connecting to the wrong screen -- looks
    like the remote boot having gone wrong.
    """
    if isinstance(asked, int):
        return asked
    if asked != "auto":
        raise Failed("[screen] local_port is a number or `auto`")
    for candidate in range(5900 + display, 5900 + display + 64):
        with socket.socket() as probe:
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                probe.bind(("127.0.0.1", candidate))
            except OSError:
                continue
            return candidate
    raise Failed("no free local port near 5900; set [screen] local_port")


def answering(port: int) -> bool:
    """Whether a VNC server is on the other end of the tunnel yet.

    The test is the protocol's own greeting, not a connection: `ssh` accepts
    on the forwarded port from the moment it starts, long before anything
    over there is listening, so a connection that succeeds proves only that
    `ssh` is running. `RFB 003.00x` proves QEMU is.
    """
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=2) as link:
            link.settimeout(2)
            return link.recv(12).startswith(b"RFB ")
    except OSError:
        return False


def viewer_argv(spec: object, port: int) -> list[str] | None:
    """The viewer to open, as a command line, or `None` for none."""
    address = f"127.0.0.1:{port}"
    if isinstance(spec, list):
        return [str(part).format(port=port, host="127.0.0.1", address=address) for part in spec]
    if spec == "none":
        return None
    if spec != "auto":
        text = str(spec).format(port=port, host="127.0.0.1", address=address)
        return shlex.split(text, posix=os.name != "nt")
    for candidate in (
        r"C:\Program Files\RealVNC\VNC Viewer\vncviewer.exe",
        r"C:\Program Files\TigerVNC\vncviewer.exe",
        r"C:\Program Files\uvnc bvba\UltraVNC\vncviewer.exe",
    ):
        if os.name == "nt" and pathlib.Path(candidate).is_file():
            return [candidate, address]
    for name in ("vncviewer", "gvncviewer", "vinagre"):
        if found := shutil.which(name):
            return [found, address]
    if sys.platform == "darwin":
        return ["open", f"vnc://{address}"]
    if found := shutil.which("remmina"):
        return [found, "-c", f"vnc://{address}"]
    raise Failed(
        "no VNC viewer found. Install one (RealVNC, TigerVNC, Remmina), or set\n"
        "  [screen] viewer to its command line, or to `none` and connect by hand to\n"
        f"  {address}"
    )


def pump(stream, prefix: str) -> None:
    """Put the remote's output on this terminal as it arrives."""
    for line in iter(stream.readline, ""):
        sys.stdout.write(prefix + line)
        sys.stdout.flush()


# --------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Boot the Ferrix desktop on another machine and watch it here.",
        epilog="Anything after `--` is passed to the remote `cargo xtask` as it stands.",
    )
    parser.add_argument("--config", help="the file to read instead of the ones searched")
    parser.add_argument("--display", type=int, help="the VNC display over there, e.g. 0")
    parser.add_argument("--local-port", type=int, help="the port the tunnel listens on here")
    parser.add_argument("--send", choices=("working-tree", "head"), help="what to boot")
    parser.add_argument("--no-viewer", action="store_true", help="tunnel only, open nothing")
    parser.add_argument("--print-command", action="store_true", help="say what would run, and stop")
    parser.add_argument(
        "--stop",
        action="store_true",
        help="stop a boot left running over there, and do nothing else",
    )
    parser.add_argument("extra", nargs="*", help=argparse.SUPPRESS)
    args = parser.parse_args()

    # A boot's output is the thing being watched, and Python block-buffers a
    # pipe: `remote-desktop.py | tee` otherwise shows nothing at all until the
    # machine stops, which is exactly when it stops being useful.
    sys.stdout.reconfigure(line_buffering=True)

    path = find_config(args.config)
    config = load(path)
    if args.display is not None:
        config["screen"]["display"] = args.display
    if args.local_port is not None:
        config["screen"]["local_port"] = args.local_port
    if args.send:
        config["source"]["send"] = args.send
    if args.no_viewer:
        config["screen"]["viewer"] = "none"

    host = str(config["remote"]["host"])
    directory = remote_dir(str(config["remote"]["dir"]))

    # A boot outlives its connection more often than is comfortable: whatever
    # killed this script before the `finally` ran -- a hard kill, a laptop
    # closing -- left QEMU holding that machine's memory and its VNC port, and
    # `ssh`'s own hangup does not always reach it. So the cleanup is a thing
    # you can ask for, rather than only a thing that usually happens.
    if args.stop:
        print(f"stopping any boot of ours on {host}:{directory}")
        teardown(host, directory)
        return 0

    root = git("rev-parse", "--show-toplevel")
    display = int(config["screen"]["display"])
    port = pick_port(config["screen"]["local_port"], display)

    sha, described = snapshot(str(config["source"]["send"]), root)
    script = boot_script(config, directory, sha, display, args.extra)

    if args.print_command:
        print(f"config:  {path}")
        print(f"push:    git push {host}:{directory} {sha}:{REMOTE_REF}")
        print(f"tunnel:  ssh -L {port}:127.0.0.1:{5900 + display} {host}")
        print(f"boot:    {script}")
        viewer = viewer_argv(config["screen"]["viewer"], port)
        print(f"viewer:  {shlex.join(viewer) if viewer else 'none'}")
        return 0

    viewer = viewer_argv(config["screen"]["viewer"], port)

    print(f"config:  {path}")
    print(f"sending: {described}")
    print(f"      -> {host}:{directory}, as {sha[:12]}")
    prepare(host, directory)
    run(["git", "push", "--quiet", "--force", f"{host}:{directory}", f"{sha}:{REMOTE_REF}"], cwd=root)

    print(f"screen:  VNC {5900 + display} over there, 127.0.0.1:{port} here")
    print("         the first boot on a machine builds everything; that is the wait.\n")

    command = [
        *ssh_base(host)[:-1],
        "-o",
        "ExitOnForwardFailure=yes",
        "-L",
        f"{port}:127.0.0.1:{5900 + display}",
        host,
        script,
    ]
    boot = subprocess.Popen(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    watcher = threading.Thread(target=pump, args=(boot.stdout, ""), daemon=True)
    watcher.start()

    shown: subprocess.Popen | None = None
    try:
        deadline = time.monotonic() + float(config["screen"]["wait"])
        while time.monotonic() < deadline:
            if boot.poll() is not None:
                raise Failed(
                    f"the remote boot ended before the screen came up (exit {boot.returncode}). "
                    "Its output is above."
                )
            if answering(port):
                break
            time.sleep(1.0)
        else:
            raise Failed(
                f"no VNC server after {config['screen']['wait']}s. The boot is still running; "
                "its output is above."
            )

        if viewer is None:
            print(f"\nscreen: up. Connect a viewer to 127.0.0.1:{port}. Ctrl-C ends the boot.\n")
            boot.wait()
        else:
            print(f"\nscreen: up. Opening {shlex.join(viewer)}\n")
            shown = subprocess.Popen(viewer)
            # Whichever ends first ends the other: closing the window is how a
            # person says they are done, and the boot ending is how the machine
            # says the same thing.
            while True:
                if shown.poll() is not None:
                    print("\nviewer closed; stopping the boot.")
                    break
                if boot.poll() is not None:
                    print("\nthe boot ended; closing the viewer.")
                    break
                time.sleep(0.5)
    except KeyboardInterrupt:
        print("\ninterrupted; stopping the boot.")
    finally:
        for process in (shown, boot):
            if process and process.poll() is None:
                process.terminate()
                with contextlib.suppress(subprocess.TimeoutExpired):
                    process.wait(timeout=10)
        teardown(host, directory)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Failed as failure:
        print(f"\nremote-desktop: {failure}", file=sys.stderr)
        sys.exit(1)
