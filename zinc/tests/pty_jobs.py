#!/usr/bin/env python3
"""Job control in zinc, driven through a pseudo-terminal.

Usage: pty_jobs.py PATH/TO/zinc

Runs zinc interactively on a pty, starts jobs, suspends and resumes them,
and checks both what the shell says and what the kernel says about the
process groups behind it -- the part a transcript cannot show. Exits 1 with
the screen text on the first mismatch. Linux only: it needs the pty module.

What is checked, and why each one is here rather than taken on trust:

* A background job is announced as `[1] <pid>` and listed by `jobs`.
* Every process of a pipeline is in one process group, and that group is not
  the shell's. This is the rule a Ctrl-C depends on, and the one that a
  shell which merely forks looks identical without.
* The foreground group of the terminal is the job's while it runs and the
  shell's again afterwards.
* Ctrl-Z suspends the foreground job, `jobs` calls it suspended, `bg` sets it
  running again and `fg` brings it back.
* Ctrl-C interrupts a foreground job and leaves the shell alive.
* `exit` with a suspended job is refused once and obeyed the second time.
"""

import os
import pty
import re
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def main():
    if len(sys.argv) != 2:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    zinc = os.path.abspath(sys.argv[1])
    work = tempfile.mkdtemp(prefix="zinc-jobs-")
    try:
        return run(zinc, work)
    finally:
        shutil.rmtree(work, ignore_errors=True)


def run(zinc, work):
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(work)
        env = {"PATH": "/usr/bin:/bin", "HOME": work, "PS1": "% ", "TERM": "dumb"}
        os.execve(zinc, [zinc, "-f", "-i"], env)

    failures = []
    seen = []

    def read(seconds):
        out = b""
        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([fd], [], [], 0.05)
            if not ready:
                continue
            try:
                chunk = os.read(fd, 4096)
            except OSError:
                break
            if not chunk:
                break
            out += chunk
        text = re.sub(rb"\x1b\[[0-9;]*[A-Za-z]", b"", out).replace(b"\r", b"")
        shown = text.decode(errors="replace")
        seen.append(shown)
        return shown

    def type_in(keys, seconds=1.0):
        os.write(fd, keys)
        return read(seconds)

    def check(label, ok, shown):
        if not ok:
            failures.append(f"{label}:\n{shown}")

    def stat_field(child, index):
        """One field of /proc/<pid>/stat, counted from 1 as proc(5) does."""
        try:
            with open(f"/proc/{child}/stat", "rb") as handle:
                text = handle.read()
        except OSError:
            return None
        after = text.rsplit(b")", 1)[-1].split()
        # Field 3 is the state, the first after the command, so field N is
        # at index N - 3 here.
        if len(after) < index - 2:
            return None
        return int(after[index - 3])

    def children_of(parent):
        try:
            out = subprocess.run(
                ["ps", "-o", "pid=", "--ppid", str(parent)],
                capture_output=True,
                check=False,
            ).stdout
        except OSError:
            return []
        return [int(line) for line in out.split()]

    read(1.0)

    # A background job: announced, listed, and in a group of its own.
    shown = type_in(b"sleep 30 &\n")
    check("a background job is announced as [n] pid", re.search(r"\[1\] \d+", shown), shown)
    shown = type_in(b"jobs\n")
    check(
        "jobs lists it as running",
        re.search(r"\[1\]\s+\+\s+running\s+sleep 30", shown),
        shown,
    )

    # The group behind it, which the screen cannot show. The sleep is the
    # shell's grandchild: `&` runs the list in a subshell, which is the
    # group's leader.
    kids = children_of(pid)
    check("the shell has a child for the job", len(kids) >= 1, str(kids))
    shell_pgid = stat_field(pid, 5)
    groups = {stat_field(kid, 5) for kid in kids}
    check(
        "the job's group is not the shell's",
        groups and shell_pgid not in groups,
        f"shell group {shell_pgid}, job groups {groups}",
    )
    for kid in kids:
        for grandchild in children_of(kid):
            check(
                "a job's own children share its group",
                stat_field(grandchild, 5) == stat_field(kid, 5),
                f"{grandchild} in {stat_field(grandchild, 5)}, "
                f"parent in {stat_field(kid, 5)}",
            )

    # The terminal comes back to the shell after a foreground command.
    check(
        "the shell has the terminal at the prompt",
        os.tcgetpgrp(fd) == shell_pgid,
        f"foreground group {os.tcgetpgrp(fd)}, shell group {shell_pgid}",
    )

    # A pipeline is one job, and one process group.
    type_in(b"kill %1\n")
    read(0.3)
    type_in(b"sleep 30 | cat &\n")
    shown = type_in(b"jobs\n")
    check(
        "a pipeline is one job",
        re.search(r"\[\d\]\s+\+\s+running\s+sleep 30 \| cat", shown),
        shown,
    )

    # Ctrl-Z suspends the foreground job, and the terminal comes back.
    os.write(fd, b"sleep 30\n")
    read(0.5)
    foreground_while_running = os.tcgetpgrp(fd)
    check(
        "the job has the terminal while it runs",
        foreground_while_running != shell_pgid,
        f"foreground group {foreground_while_running}, shell group {shell_pgid}",
    )
    shown = type_in(b"\x1a")
    check("Ctrl-Z says the job is suspended", "suspended" in shown, shown)
    check(
        "the shell has the terminal again",
        os.tcgetpgrp(fd) == shell_pgid,
        f"foreground group {os.tcgetpgrp(fd)}, shell group {shell_pgid}",
    )
    shown = type_in(b"jobs\n")
    check(
        "jobs calls it suspended",
        re.search(r"\+\s+suspended\s+sleep 30", shown),
        shown,
    )

    # bg sets it running again, without giving it the terminal.
    shown = type_in(b"bg\n")
    check("bg says it continued", "continued" in shown, shown)
    check(
        "bg leaves the terminal with the shell",
        os.tcgetpgrp(fd) == shell_pgid,
        f"foreground group {os.tcgetpgrp(fd)}",
    )
    shown = type_in(b"jobs\n")
    check("bg made it run", re.search(r"running\s+sleep 30", shown), shown)

    # fg brings it back: it prints the command and takes the terminal.
    os.write(fd, b"fg\n")
    shown = read(0.5)
    check("fg prints the command", "sleep 30" in shown, shown)
    check(
        "fg gives the job the terminal",
        os.tcgetpgrp(fd) != shell_pgid,
        f"foreground group {os.tcgetpgrp(fd)}, shell group {shell_pgid}",
    )

    # Ctrl-C ends it, and the shell survives.
    type_in(b"\x03")
    shown = type_in(b"echo alive\n")
    check("the shell is alive after Ctrl-C", "alive" in shown, shown)
    check(
        "the terminal is the shell's after Ctrl-C",
        os.tcgetpgrp(fd) == shell_pgid,
        f"foreground group {os.tcgetpgrp(fd)}",
    )

    # exit with a suspended job is refused once.
    os.write(fd, b"sleep 30\n")
    read(0.4)
    type_in(b"\x1a")
    shown = type_in(b"exit\n")
    check("exit is refused while a job is suspended", "suspended jobs" in shown, shown)
    type_in(b"exit\n")

    ended = wait_for_exit(pid, 3.0)
    check("the second exit leaves the shell", ended, "the shell was still running")
    if not ended:
        os.kill(pid, signal.SIGKILL)
        wait_for_exit(pid, 1.0)

    for failure in failures:
        print(failure, file=sys.stderr)
    if failures:
        print("--- the whole session ---", file=sys.stderr)
        print("".join(seen), file=sys.stderr)
        return 1
    print("zinc job control: all checks passed")
    return 0


def wait_for_exit(pid, seconds):
    end = time.time() + seconds
    while time.time() < end:
        try:
            done, _ = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            return True
        if done == pid:
            return True
        time.sleep(0.05)
    return False


if __name__ == "__main__":
    sys.exit(main())
