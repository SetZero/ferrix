#!/usr/bin/env python3
"""Tab completion in zinc's line editor, driven through a pseudo-terminal.

Usage: pty_completion.py PATH/TO/zinc

Runs zinc interactively on a pty in a scratch directory, types lines with
Tabs in them, and checks what the completed commands print. Exits 1 with the
screen text on the first mismatch. Linux only: it needs the pty module.
"""

import os
import pty
import re
import select
import shutil
import sys
import tempfile
import time


def main():
    if len(sys.argv) != 2:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    zinc = os.path.abspath(sys.argv[1])
    work = tempfile.mkdtemp(prefix="zinc-pty-")
    try:
        return run(zinc, work)
    finally:
        shutil.rmtree(work, ignore_errors=True)


def run(zinc, work):
    for name in ["alpha.txt", "alpine.log"]:
        open(os.path.join(work, name), "w").close()
    os.mkdir(os.path.join(work, "beta dir"))

    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(work)
        env = {"PATH": "/usr/bin:/bin", "HOME": work, "PS1": "% ", "TERM": "xterm"}
        os.execve(zinc, [zinc, "-f", "-i"], env)

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
        return out

    def screen(raw):
        text = re.sub(rb"\x1b\[[0-9;]*[A-Za-z]", b"", raw).replace(b"\r", b"")
        return text.decode(errors="replace")

    failures = []

    def expect(keys, wanted, label):
        os.write(fd, keys)
        shown = screen(read(1.0))
        if wanted not in shown:
            failures.append(f"{label}: typed {keys!r}, wanted {wanted!r} in:\n{shown}")

    read(1.0)
    expect(b"ech\thi\r", "\nhi\n", "a command name completes with a space")
    expect(b"echo al\t", "echo alp", "an ambiguous file name inserts the common prefix")
    expect(b"\t", "alpha.txt   alpine.log", "the next Tab lists the matches")
    expect(b"\t\r", "\nalpha.txt\n", "the Tab after the listing inserts the first match")
    expect(b"echo be\t\r", "\nbeta dir/\n", "a directory completes with a slash, quoted")
    expect(b"FOOBAR=1 FOOBAZ=2\r", "% ", "assignments")
    expect(b"echo $FOOB\t\t", "FOOBAR  FOOBAZ", "parameters list without their dollar")
    os.write(fd, b"\x15exit\r")
    read(1.0)
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass

    for failure in failures:
        print(failure, file=sys.stderr)
    if failures:
        return 1
    print("zinc completion: all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
