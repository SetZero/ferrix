# Contributing to Ferrix

Boot Ferrix, try something, and tell us where it breaks. A clear report is as
useful as a patch.

## Report what you find

- **Something broke:** open a bug with the command, the commit and the serial
  log. A boot that fails its own checks says which one.
- **A Linux program that should run:** open a "Linux program" issue. The
  kernel logs every system call it does not answer, and that line is usually
  the whole bug.
- **A security problem:** see [SECURITY.md](SECURITY.md) and do not open a
  public issue.

## Changing the code

1. Read [docs/CONVENTIONS.md](docs/CONVENTIONS.md) first. Every change follows
   it, including the rule that a commit names one author and carries no
   `Co-authored-by:` trailer.
2. Run `git config core.hooksPath .githooks` once in your clone.
3. `cargo xtask check` runs every gate CI runs. A change to the kernel should
   also pass `cargo xtask test-boot --arch all`.
4. A commit message is a subject and a body that argues why, not what.

[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) is the map of the tree, and
[the roadmap](docs/roadmap/README.md) says what each stage still needs.

## How Ferrix is made

Most code is written in Claude sessions. The project owner chooses what to
build and reviews the results. Contributions from other people go through the
same checks.
