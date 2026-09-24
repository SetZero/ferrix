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

## Splitting one piece of work across several agents

On 2026-09-24 one session split the init and stage 13's cgroups across five
agents. Six landings went in (22 points) in about two hours, and the one
that mattered most never started. The nazuna gate summaries, kept under
`~/.local/share/ferrix/logs/init-gate-*`, show where the time went. Each
rule below comes from something seen there.

1. **Start the critical path first.** The largest landing was the init
   program itself. It waited until the landings it builds on were in, then
   got a quarter of an hour before the day's wind-down, and ended with 0 of
   its 10 points. Its design needed only the interface of the library being
   written beside it, and that was readable on the other branch from the
   start. Start the landing everything else leads to in the first round.
   Let it design against in-progress work, and rebase it as that work lands.
2. **Read another branch; don't copy it.** Use `git show <branch>:<path>`
   and `git log <branch>`. One agent unpacked another branch's crate into its
   own worktree, then was refused both the cherry-pick and the reset that
   would have undone it. The worktree was left with 34 uncommitted files for
   the person to clear by hand.
3. **Don't re-gate for docs, or for areas the change doesn't reach.** 14 gate
   runs went to 6 landings. `main` moved about ten times in those two hours,
   and one landing was re-gated only because another landing had changed the
   roadmap. Re-gate when the rebase changed code the landing touches or
   depends on. Otherwise the gate still applies, and saying so in the report
   is enough.
4. **Keep shared-doc edits to your own lines.** Every agent edits
   `ROADMAP.md`, `BACKLOG.md` and the design doc, so every rebase conflicts
   there. Edit your own row, paragraph or "where it stands" entry, and never
   reflow a neighbour's. On a conflict, take `main`'s side and add your lines
   again. Recount a total such as the roadmap's host-test count against
   `main`'s number, rather than merging two sums.
5. **Give a new failure a row the day it is seen.** Four of the 14 runs
   failed on something the change didn't touch and passed on rerun. Each
   cost a full row, and two of them had no backlog row until the coordinator
   filed them afterwards. Copy the log aside before rerunning, rerun once,
   and file the row with the log's path and the commit. A flake that keeps
   firing is cheaper to fix than to rerun.
6. **Name what you put in shared places.** Parallel agents share the
   session's scratch directory and nazuna's home. One agent copied another's
   `adhoc.sh` to nazuna by mistake, because both scripts had the same name.
   Prefix your scripts, worktrees, refs, target directories and `TMPDIR`
   with your stream's name, and remove only those, by exact name.
7. **Make agent worktrees by hand.** The Agent tool's `isolation: worktree`
   refuses this checkout: the session's path is `f:\…`, git reports `F:/…`,
   and the tool sees a redirect. Run `git worktree add -b <branch>
   .claude/worktrees/<name> main` yourself, and give the agent that path.
   Tell every agent to run gates and boots in the foreground, because a
   subagent is not woken by its own background task.
8. **Wind down to a state the next session can start from.** When asked to
   stop, an agent lands only what is already gated or still gating. Anything
   else stays on its branch as a WIP commit, never on `main`. What the next
   session needs goes into the design document's "where it stands" section
   on `main`, not only into a report. The landing that stopped before it
   wrote any code still left its findings there (`docs/INIT.md` §16).
9. **Record estimate against spend for each landing.** Put the points
   estimated beside the points spent, and the gate summary's times beside
   both, in each agent's report. That is what shows whether a stream is slow
   because of its code, its gates, or its reruns.

## Before calling a change done

```
cargo xtask check
```

It runs the gate set CI runs, cheapest-first, in one command.
