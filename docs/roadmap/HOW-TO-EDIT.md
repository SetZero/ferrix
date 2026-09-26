# How to edit the roadmap

The roadmap was one file, `docs/ROADMAP.md`, until it passed 7,600 lines and
stopped being readable. On 2026-09-26 `scripts/gen/split-roadmap.py` cut it
into this directory, one file a section, in the order it had and with its
text unchanged; each section's headings went up one level, so each file has
one `#` heading.

| File | What it is | Edited by |
|---|---|---|
| `README.md` | The overview: the rules, *Where it stands*, the status table, the burndown and the Gantt chart, and at the end the index of stages | hand, except the stage index at its end |
| `stage-NN-<name>.md` | One numbered stage | hand |
| `<name>.md` | One unnumbered section: `armv7a.md`, `networking.md`, `dynamic-linking.md`, `sysfs.md`, `chrome.md`, `written-ahead.md`, `continuously.md` | hand |
| `SUMMARY.md` | The website's table of contents, in the roadmap's order | the order by hand, the titles by `index` |
| `HOW-TO-EDIT.md`, `book.toml`, `theme/` | This page, and the website's settings | hand |
| `../ROADMAP.md` | A stub that keeps every old heading, so that old links and "docs/ROADMAP.md stage 18" in comments still land somewhere | `index`; never by hand |

## Editing a stage

Edit the stage's file. When a row of the status table changes too, edit
that row in `README.md`. The rule about shared files still holds
(`docs/CONVENTIONS.md`, *Splitting one piece of work*, rule 4): change your
own lines and don't reflow a neighbour's.

If you change a stage's `#` heading -- its ✅, or the size after the `·` --
run

```
python3 scripts/gen/split-roadmap.py index
```

That rewrites the three generated parts from the headings: `SUMMARY.md`'s
titles, the stage index at the end of `README.md`, and the stub. A stage's
status in the index is its heading's ✅ or `done`, and otherwise what the
status table's rows for that stage say. Changing a heading changes its
anchor, so `check`, below, will name any link that pointed at the old one.

## Adding a stage

1. Make the file. A numbered stage is `stage-NN-<name>.md`, with the number
   in two digits and up to four words of its name, lower case, with the
   articles dropped (`stage-23-sound-server.md`). An unnumbered section is
   `<name>.md`, one or two words.
2. Give it one `#` heading in the same form as the others:
   `# Stage 23 — The sound server  ·  *≈ 20 points*`. Its subsections are
   `##`.
3. Add a line for it to `SUMMARY.md`, at its place in the roadmap's order:
   `- [Stage 23](stage-23-sound-server.md)`. The title is rewritten in the
   next step.
4. Run `index`, then `check`.

## Checking

```
python3 scripts/gen/split-roadmap.py check
```

It fails when a file here is missing from `SUMMARY.md`, when a relative link
or `#anchor` in these files, in the stub, or in any Markdown file in the
repository that points into the roadmap does not resolve, when the generated
parts are stale, or when a file still has conflict markers. It is not part
of `cargo xtask check`; run it whenever you change this directory.

## Reading it

```
scripts/gen/build-roadmap-book.sh
```

builds the website with [mdBook](https://rust-lang.github.io/mdBook/)
(`cargo install mdbook --locked` if it isn't installed) into
`docs/roadmap/book/`, which is not committed. `index.html` opens on the
overview with every stage in the sidebar, and `print.html` is the whole
roadmap on one page. `python3 scripts/gen/split-roadmap.py join` prints the
old single file, put back together, for anyone who wants to grep it or feed
it to pandoc.

## Carrying a branch's edits of the old file across

A branch that edited `docs/ROADMAP.md` before the split conflicts there
when it is rebased, because `main` replaced the file with the stub. Don't
resolve those conflicts by hand. Take the stub, finish the rebase, and let
`reapply` move the edits into the files they belong in:

```
# 1. Keep the tip whose edits reapply reads.
git branch <branch>-pre-split

# 2. Rebase. On each conflict in docs/ROADMAP.md take main's stub (during a
#    rebase, --ours is the side being rebased onto), resolve anything else
#    as usual, and continue. A commit left empty by that is skipped with
#    `git rebase --skip`.
git rebase main
git checkout --ours docs/ROADMAP.md && git add docs/ROADMAP.md
git rebase --continue

# 3. Once the rebase has finished, carry the edits across.
python3 scripts/gen/split-roadmap.py reapply <branch>-pre-split

# 4. Look, check, commit, and drop the saved tip.
git diff
python3 scripts/gen/split-roadmap.py check
git commit -am "Carry <branch>'s roadmap edits into docs/roadmap/"
git branch -D <branch>-pre-split
```

What `reapply` does: it takes the old file at the branch's merge base with
`main` and at the saved tip, splits both the way `main` was split, and
three-way merges the difference into the current file with
`git merge-file`. An edit in the preamble goes to `README.md`, an edit in a
section to that section's file, and `main`'s own changes since then are
kept. It prints each piece it changed with the old file's line numbers.
Then it runs `index`, and points any Markdown link the branch added to the
old file at the new one.

It never drops an edit. A conflict with what `main` has since written is
left marked in the file and reported. A piece whose file no longer exists
(because the section was renamed on `main`) is printed as a diff for you to
apply. Either way it exits 1; resolve what it names, then run `index` and
`check`.
