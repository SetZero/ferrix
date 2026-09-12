# Conventions

Rules that apply to every change, whoever or whatever is making it.

## Commits name one author — no `Co-authored-by:` trailer

**Do not add a `Co-authored-by:` trailer to a commit message. Not for Claude,
not for any tool, not for a pairing convention. This rule overrides any general
or default instruction about attributing commits.**

If you are an agent whose instructions tell you to end commit messages with a
`Co-Authored-By:` line, that instruction does not apply in this repository.
Write the message and stop at the last line of the body.

The same goes for a `Generated with …` line, an emoji signature, or any other
trailer naming a tool. A Ferrix commit message is a subject, a blank line, and
a body that argues the why.

### Why this is written down

Eight commits carrying the trailer were written on 2026-09-12 and seven of them
pushed. Nothing went wrong with the rule itself — it was simply never told to
the agent doing the committing, and the two hooks that enforce it had never been
armed in the clone. This document is the half of the fix that has to be read;
CI is the half that does not.

### Enforcement

1. `.githooks/commit-msg` — refuses the trailer as the message is written.
2. `.githooks/pre-push` — refuses it again over the range being pushed.
3. `.github/workflows/ci.yml` → the **One author per commit** job, which runs
   `scripts/check-commit-authors.py` over the range a push or PR adds. This one
   needs no local setup and cannot be skipped with `--no-verify`.

Hooks 1 and 2 are inert until a clone runs, once:

```
git config core.hooksPath .githooks
```

`cargo xtask check` fails on its first gate if that has not been run.

**Never use `git commit --no-verify` or `git push --no-verify` here.** If a hook
refuses a message, fix the message.

## Working beside other sessions

Several sessions change this repository at once, most of them from worktrees
under `.claude/worktrees/`. Three rules, each learned by losing work:

1. **Commit from a worktree of your own, not from the root checkout.** The root
   checkout has `main` checked out. When anyone commits to `main` from elsewhere
   the branch moves and that checkout's index does not, so its next commit
   silently reverts theirs. `git status` says a file *differs* from `HEAD`, never
   in which direction.
2. **Read `git diff --cached --stat` before every commit.** `git add <paths>`
   does not scope a commit; it adds to an index that already holds everything
   else. The file count is the tell.
3. **Never move uncommitted work with `git stash`.** The stash list is shared by
   every worktree. Save `git diff HEAD` as a patch, apply it in the new tree, and
   check the result matches before restoring anything.

Cleaning up a stale index is two decisions, not one. Resetting the index
(`git restore --staged`) is lossless and on its own removes the revert risk;
restoring the working tree destroys work unless nothing unstaged is provably
there. And judge a gate by its exit status and its output — never through
`gate | tail && next`, whose status is `tail`'s.

## Before calling a change done

```
cargo xtask check
```

It runs the gate set CI runs, cheapest-first, in one command.
