# Hooks

Version-controlled hooks, so the rule they enforce lives with the code rather
than in each clone's `.git/hooks`. Git does not pick them up on its own — one
line per clone, once:

```
git config core.hooksPath .githooks
```

`core.hooksPath` is local configuration and cannot be committed, which is the
one thing about this arrangement that has to be remembered rather than enforced.
That is why the rule is checked in two places rather than one.

## `commit-msg` — no `Co-authored-by`

Ferrix commits name one author. A `Co-authored-by:` trailer is refused at the
moment it is written, which is where the error is cheap and the fix is to edit
the message.

Two things are deliberately *not* matched, because both would be false
positives that teach people to reach for `--no-verify`:

* the phrase inside a sentence — only a trailer at the start of a line counts;
* anything below the scissors line, which is the diff `git commit -v` appends —
  including the diff of this directory, whenever a hook changes.

## `pre-push` — the same rule, over what is actually being published

`commit-msg` runs when a message is written. That misses a `--no-verify`, a
rebase that replays a message without reopening it, and any commit that arrived
from somewhere else. So the check is repeated over every commit in the range
being pushed, which is the last point at which a rewrite is still free.

For a new branch the range is everything not already reachable from a
remote-tracking ref, so pushing a branch does not re-examine — or re-reject —
history that is already public.
