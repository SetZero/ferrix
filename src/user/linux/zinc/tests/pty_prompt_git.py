#!/usr/bin/env python3
"""oh-my-zsh's agnoster prompt in a git repository, driven through a
pseudo-terminal.

Usage: pty_prompt_git.py PATH/TO/zinc

agnoster builds its git segment with `vcs_info`, which formats every piece
with `zformat`. Before zinc had that builtin, every prompt inside a git
repository printed `VCS_INFO_formats:87: command not found: zformat` above
it (ferrix-df, 2026-09-27). This starts zinc interactively on a pty with the
oh-my-zsh checkout `cargo xtask omz` installs, as the image's /etc/zshrc
does, enters a fresh repository on a branch named `ferrix-prompt-check`,
and requires a prompt naming the branch with no error anywhere on the
screen.

The checkout is `$FERRIX_OMZ`, else ~/.local/share/ferrix/oh-my-zsh. A
machine without one -- CI -- has nothing to check this against, and the
script says so and passes. Exits 1 with the screen on a mismatch.
"""

import os
import pty
import select
import subprocess
import sys
import tempfile
import time

BRANCH = "ferrix-prompt-check"


def read(fd, seconds):
    """Everything the shell writes within `seconds`."""
    out = b""
    end = time.time() + seconds
    while time.time() < end:
        ready, _, _ = select.select([fd], [], [], 0.1)
        if ready:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            out += chunk
    return out


def main():
    zinc = sys.argv[1]
    omz = os.environ.get("FERRIX_OMZ", os.path.expanduser("~/.local/share/ferrix/oh-my-zsh"))
    if not os.path.isfile(os.path.join(omz, "oh-my-zsh.sh")):
        print(f"pty_prompt_git: no oh-my-zsh at {omz}; skipped")
        return 0
    with tempfile.TemporaryDirectory(prefix="zinc-prompt-git-") as home:
        repo = os.path.join(home, "repo")
        os.mkdir(repo)
        git = ["git", "-C", repo, "-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=" + BRANCH]
        subprocess.run(git + ["init", "-q"], check=True)
        with open(os.path.join(repo, "f"), "w") as f:
            f.write("x\n")
        subprocess.run(git + ["add", "f"], check=True)
        subprocess.run(git + ["commit", "-q", "-m", "c"], check=True)
        # What the image's /etc/zshrc says, with this machine's checkout and
        # a cache of the test's own.
        rc = os.path.join(home, "omzrc")
        with open(rc, "w") as f:
            f.write(
                f"export ZSH={omz}\n"
                "ZSH_THEME=agnoster\nplugins=(git)\nZSH_DISABLE_COMPFIX=true\n"
                f"export ZSH_CACHE_DIR={home}/cache\nexport ZSH_COMPDUMP={home}/cache/zcompdump\n"
                "mkdir -p $ZSH_CACHE_DIR/completions\nsource $ZSH/oh-my-zsh.sh\n"
            )
        env = {
            "PATH": "/usr/bin:/bin",
            "HOME": home,
            "TERM": "xterm-256color",
            "LANG": "C.UTF-8",
        }
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(home)
            os.execve(zinc, [zinc, "-f", "-i"], env)
        read(fd, 1.0)
        os.write(fd, f"source {rc}\r".encode())
        read(fd, 8.0)
        os.write(fd, f"cd {repo}\r".encode())
        screen = read(fd, 6.0)
        os.write(fd, b"exit\r")
        read(fd, 1.0)
        try:
            os.kill(pid, 9)
        except ProcessLookupError:
            pass
        os.waitpid(pid, 0)
    text = screen.decode("utf-8", "replace")
    problems = []
    if "command not found" in text or "zformat" in text or "Updated" in text:
        problems.append("an error came with the prompt")
    if BRANCH not in text:
        problems.append(f"the prompt does not name the branch {BRANCH}")
    if problems:
        print("pty_prompt_git: " + "; ".join(problems))
        print("--- what the shell wrote after `cd` ---")
        print(text)
        return 1
    print(f"pty_prompt_git: agnoster's prompt in a repository names {BRANCH}, with no error")
    return 0


if __name__ == "__main__":
    sys.exit(main())
