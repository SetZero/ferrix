#!/usr/bin/env python3
"""Tests for split-roadmap.py.

The splitter is rerun on whatever `main` holds when it lands, and `reapply`
carries other sessions' edits across, so the properties that matter are the
ones a person would not notice going wrong: that nothing of the text is lost
or reordered (the round trip), that a second run changes nothing
(idempotence), that a link still lands on its heading, and that a branch's
edit arrives in the right file.

Usage:  python3 scripts/gen/test_split_roadmap.py
"""

from __future__ import annotations

import filecmp
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

_spec = importlib.util.spec_from_file_location("split_roadmap", HERE / "split-roadmap.py")
sr = importlib.util.module_from_spec(_spec)
sys.modules["split_roadmap"] = sr
_spec.loader.exec_module(sr)

ROADMAP = """\
# Ferrix — roadmap

The rules. See [the design](ARCHITECTURE.md) and stage 1, [below](#stage-1--boot----week).

**Status and estimates.**

| what | points | current state |
|---|---|---|
| ~~Stage 0, foundation~~ | ~~3~~ | done |
| Stage 1, boot | 5 | in progress |
| Stage 12, later | 8 | not started |

### Burndown

![Burndown](img/burndown.svg)

---

## Stage 0 — Foundation ✅

Workspace. `[not a link](#nowhere)` and a real [link](#burndown).

```
## not a heading, in a fence
```

**Exit:** met.

## Stage 1 — Boot  ·  *week*

Boot it. Back to [stage 0](#stage-0--foundation-).

### Done

One.

## ARMv7-A — a third architecture ✅

A port.

### Done

Two, and [the first done](#done).

## Written ahead of their stage

Crates.
"""

OTHER = """\
# Elsewhere

[Stage 1](../ROADMAP.md#stage-1--boot----week), [the roadmap](../ROADMAP.md) and
[stage 0 from the top](docs/ROADMAP.md#stage-0--foundation-) (broken, and mended).
`[in code](../ROADMAP.md#stage-1--boot----week)`
"""

TOP = "[Roadmap](docs/ROADMAP.md#burndown), [ARM](docs/ROADMAP.md#armv7-a--a-third-architecture-)\n"


def make_repo(d: Path, roadmap: str = ROADMAP) -> None:
    (d / "docs" / "sub").mkdir(parents=True)
    (d / "docs" / "img").mkdir()
    (d / "docs" / "img" / "burndown.svg").write_text("<svg/>")
    (d / "docs" / "ARCHITECTURE.md").write_text("# Architecture\n")
    (d / "docs" / "ROADMAP.md").write_text(roadmap)
    (d / "docs" / "sub" / "OTHER.md").write_text(OTHER)
    (d / "README.md").write_text(TOP)
    (d / "docs" / "roadmap").mkdir()
    (d / "docs" / "roadmap" / "HOW-TO-EDIT.md").write_text("# How to edit the roadmap\n")


def run(d: Path, *args: str) -> int:
    return sr.main(["--root", str(d), *args])


def snapshot(d: Path) -> dict[str, str]:
    return {
        str(p.relative_to(d)): p.read_text()
        for p in sorted(d.rglob("*"))
        if p.is_file() and ".git" not in p.parts
    }


class Quiet(unittest.TestCase):
    def setUp(self):
        self._stdout = sys.stdout
        sys.stdout = open(os.devnull, "w")
        self.tmp = Path(tempfile.mkdtemp(prefix="split-roadmap-test-"))

    def tearDown(self):
        sys.stdout.close()
        sys.stdout = self._stdout
        shutil.rmtree(self.tmp)


class Names(unittest.TestCase):
    def test_filenames(self):
        cases = {
            "Stage 7 — The Linux syscall ABI ✅": "stage-07-linux-syscall-abi.md",
            "Stage 12 — btrfs, write ✅  ·  *≈ 60 points, spent*": "stage-12-btrfs-write.md",
            "Stage 16 — `rustc`  ·  *the goal*  ·  *≈ 40 guessed, 8 spent*": "stage-16-rustc.md",
            "Stage 9 — The native ABI: handles, channels, ports, VMOs ✅": "stage-09-native-abi.md",
            "ARMv7-A — a third architecture ✅": "armv7a.md",
            "Dynamic linking — PIE, `PT_INTERP`, a loader  ·  *done 2026-09-23*": "dynamic-linking.md",
            "sysfs — the device tree ✅  ·  *26 points, spent*": "sysfs.md",
            "Written ahead of their stage": "written-ahead.md",
            "Continuously, from stage 1": "continuously.md",
        }
        for heading, name in cases.items():
            self.assertEqual(sr.filename_for(heading), name, heading)

    def test_anchor_matches_github(self):
        self.assertEqual(sr.anchor_of("Stage 7 — The Linux syscall ABI ✅"), "stage-7--the-linux-syscall-abi-")
        self.assertEqual(
            sr.anchor_of("Dynamic linking — PIE, `PT_INTERP`, a loader  ·  *done 2026-09-23: 39*"),
            "dynamic-linking--pie-pt_interp-a-loader----done-2026-09-23-39",
        )
        self.assertEqual(sr.anchors("# A\n## Done\n## Done\n```\n## Done\n```\n## Done\n"), ["a", "done", "done-1", "done-2"])

    def test_status(self):
        rows = sr.status_rows(ROADMAP)
        self.assertEqual(sr.status_of("Stage 0 — Foundation ✅", rows), sr.DONE)
        self.assertEqual(sr.status_of("Stage 1 — Boot  ·  *week*", rows), sr.PROGRESS)
        self.assertEqual(sr.status_of("Stage 12 — Later", rows), sr.NOT_STARTED)
        self.assertEqual(sr.status_of("Stage 2 — Unlisted", rows), sr.NOT_STARTED)
        self.assertEqual(sr.status_of("X — y  ·  *done 2026-09-23*", rows), sr.DONE)
        self.assertIsNone(sr.status_of("Written ahead of their stage", rows))
        self.assertEqual(sr.classify_state("init done (L1 to L10); phase 1 is not started"), sr.PROGRESS)
        self.assertEqual(sr.classify_state("planned when bare-metal work is requested"), sr.PLANNED)


class Split(Quiet):
    def test_layout_and_promotion(self):
        make_repo(self.tmp)
        self.assertEqual(run(self.tmp, "split"), 0)
        out = self.tmp / "docs" / "roadmap"
        names = sorted(p.name for p in out.glob("*.md"))
        self.assertEqual(
            names,
            ["HOW-TO-EDIT.md", "README.md", "SUMMARY.md", "armv7a.md", "stage-00-foundation.md",
             "stage-01-boot.md", "written-ahead.md"],
        )
        s1 = (out / "stage-01-boot.md").read_text()
        self.assertTrue(s1.startswith("# Stage 1 — Boot  ·  *week*\n"))
        self.assertIn("\n## Done\n", s1)
        s0 = (out / "stage-00-foundation.md").read_text()
        self.assertIn("## not a heading, in a fence", s0)
        readme = (out / "README.md").read_text()
        self.assertIn("\n## Burndown\n", readme)
        self.assertIn("| 1 | [Boot](stage-01-boot.md) | ◐ in progress | week |", readme)
        summary = (out / "SUMMARY.md").read_text()
        self.assertIn("- [✓ Stage 0 — Foundation](stage-00-foundation.md)", summary)
        self.assertIn("[How to edit the roadmap](HOW-TO-EDIT.md)", summary)
        stub = (self.tmp / "docs" / "ROADMAP.md").read_text()
        self.assertTrue(sr.is_stub(stub))
        self.assertIn("## Stage 1 — Boot  ·  *week*\n\nNow [`docs/roadmap/stage-01-boot.md`](roadmap/stage-01-boot.md).", stub)
        self.assertEqual(run(self.tmp, "check"), 0)

    def test_links_rewritten(self):
        make_repo(self.tmp)
        run(self.tmp, "split")
        out = self.tmp / "docs" / "roadmap"
        readme = (out / "README.md").read_text()
        self.assertIn("[the design](../ARCHITECTURE.md)", readme)
        self.assertIn("[below](stage-01-boot.md#stage-1--boot----week)", readme)
        self.assertIn("![Burndown](../img/burndown.svg)", readme)
        s0 = (out / "stage-00-foundation.md").read_text()
        self.assertIn("`[not a link](#nowhere)`", s0)
        self.assertIn("a real [link](README.md#burndown)", s0)
        s1 = (out / "stage-01-boot.md").read_text()
        self.assertIn("[stage 0](stage-00-foundation.md#stage-0--foundation-)", s1)
        # The second `### Done` was #done-1 in the old file and is #done in its own.
        arm = (out / "armv7a.md").read_text()
        self.assertIn("[the first done](stage-01-boot.md#done)", arm)
        other = (self.tmp / "docs" / "sub" / "OTHER.md").read_text()
        self.assertIn("[Stage 1](../roadmap/stage-01-boot.md#stage-1--boot----week)", other)
        self.assertIn("[the roadmap](../roadmap/README.md)", other)
        # Written from the top of the repository, which was broken here; it
        # is the path the task names, so it is mended rather than left.
        self.assertIn("(../roadmap/stage-00-foundation.md#stage-0--foundation-) (broken, and mended)", other)
        self.assertIn("`[in code](../ROADMAP.md#stage-1--boot----week)`", other)
        top = (self.tmp / "README.md").read_text()
        self.assertEqual(
            top,
            "[Roadmap](docs/roadmap/README.md#burndown), "
            "[ARM](docs/roadmap/armv7a.md#armv7-a--a-third-architecture-)\n",
        )

    def test_idempotent(self):
        make_repo(self.tmp)
        run(self.tmp, "split")
        first = snapshot(self.tmp)
        self.assertEqual(run(self.tmp, "split"), 0)
        self.assertEqual(run(self.tmp, "index"), 0)
        self.assertEqual(snapshot(self.tmp), first)

    def test_round_trip(self):
        make_repo(self.tmp)
        run(self.tmp, "split")
        self.assertEqual(sr.join_text(self.tmp), ROADMAP)

    def test_split_of_join_is_the_same_tree(self):
        make_repo(self.tmp)
        run(self.tmp, "split")
        again = self.tmp / "again"
        make_repo(again, sr.join_text(self.tmp))
        run(again, "split")
        a = self.tmp / "docs" / "roadmap"
        b = again / "docs" / "roadmap"
        cmp = filecmp.dircmp(a, b)
        self.assertEqual((cmp.left_only, cmp.right_only, cmp.diff_files), ([], [], []))

    def test_check_finds_breakage(self):
        make_repo(self.tmp)
        run(self.tmp, "split")
        out = self.tmp / "docs" / "roadmap"
        (out / "stray.md").write_text("# Stray\n\n[gone](stage-01-boot.md#no-such-heading)\n")
        with open(out / "stage-00-foundation.md", "a") as f:
            f.write("\n[missing](nope.md)\n")
        problems, _ = sr.check(self.tmp)
        text = "\n".join(problems)
        self.assertIn("stray.md is not listed in SUMMARY.md", text)
        self.assertIn("no heading #no-such-heading", text)
        self.assertIn("nope.md does not exist", text)

    def test_index_follows_a_heading(self):
        make_repo(self.tmp)
        run(self.tmp, "split")
        p = self.tmp / "docs" / "roadmap" / "stage-01-boot.md"
        p.write_text(p.read_text().replace("# Stage 1 — Boot  ·  *week*", "# Stage 1 — Boot ✅  ·  *week*", 1))
        problems, _ = sr.check(self.tmp)
        self.assertTrue(any("stale" in x for x in problems))
        run(self.tmp, "index")
        # The index is fresh again; the old anchor's links are now broken, and
        # check says so, as it should.
        problems = sr.check(self.tmp)[0]
        self.assertFalse(any("stale" in x for x in problems))
        self.assertTrue(any("no heading #stage-1--boot----week" in x for x in problems))
        self.assertIn("## Stage 1 — Boot ✅  ·  *week*", (self.tmp / "docs" / "ROADMAP.md").read_text())

    def test_real_roadmap_round_trips(self):
        real = (ROOT / "docs" / "ROADMAP.md").read_text()
        make_repo(self.tmp)
        if sr.is_stub(real):
            # Already split: the tree must reassemble into something that
            # splits back into the same tree.
            (self.tmp / "docs" / "ROADMAP.md").write_text(sr.join_text(ROOT))
            run(self.tmp, "split")
            for name in sr.summary_order((ROOT / "docs" / "roadmap" / "SUMMARY.md").read_text()):
                self.assertEqual(
                    (self.tmp / "docs" / "roadmap" / name).read_text(),
                    (ROOT / "docs" / "roadmap" / name).read_text(),
                    name,
                )
        else:
            (self.tmp / "docs" / "ROADMAP.md").write_text(real)
            run(self.tmp, "split")
            self.assertEqual(sr.join_text(self.tmp), real)


def git(d: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(d), "-c", "user.name=t", "-c", "user.email=t@example.invalid",
         "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", *args],
        check=True, capture_output=True, text=True,
    ).stdout


@unittest.skipUnless(shutil.which("git"), "needs git")
class Reapply(Quiet):
    def test_branch_edit_lands_in_its_file(self):
        d = self.tmp
        make_repo(d)
        git(d, "init", "-q", "-b", "main")
        git(d, "add", "-A")
        git(d, "commit", "-q", "-m", "base")
        # The branch edits stage 1 and the preamble in the old single file.
        git(d, "checkout", "-q", "-b", "feature")
        p = d / "docs" / "ROADMAP.md"
        p.write_text(p.read_text().replace("Boot it.", "Boot it, on three architectures.")
                     .replace("The rules.", "The rules, two of them."))
        git(d, "commit", "-q", "-am", "feature")
        # main splits, then someone edits stage 0 in its new file.
        git(d, "checkout", "-q", "main")
        run(d, "split")
        git(d, "add", "-A")
        git(d, "commit", "-q", "-m", "split")
        s0 = d / "docs" / "roadmap" / "stage-00-foundation.md"
        s0.write_text(s0.read_text().replace("Workspace.", "Workspace, and CI."))
        git(d, "commit", "-q", "-am", "main moves on")
        # The branch after its rebase: main's tree, the old tip kept aside.
        git(d, "checkout", "-q", "-b", "rebased")
        self.assertEqual(run(d, "reapply", "feature"), 0)
        out = d / "docs" / "roadmap"
        self.assertIn("Boot it, on three architectures.", (out / "stage-01-boot.md").read_text())
        self.assertIn("The rules, two of them.", (out / "README.md").read_text())
        self.assertIn("Workspace, and CI.", s0.read_text())
        self.assertEqual(sr.check(d)[0], [])

    def test_conflict_is_marked_not_dropped(self):
        d = self.tmp
        make_repo(d)
        git(d, "init", "-q", "-b", "main")
        git(d, "add", "-A")
        git(d, "commit", "-q", "-m", "base")
        git(d, "checkout", "-q", "-b", "feature")
        p = d / "docs" / "ROADMAP.md"
        p.write_text(p.read_text().replace("Boot it.", "Boot it the branch's way."))
        git(d, "commit", "-q", "-am", "feature")
        git(d, "checkout", "-q", "main")
        run(d, "split")
        s1 = d / "docs" / "roadmap" / "stage-01-boot.md"
        s1.write_text(s1.read_text().replace("Boot it.", "Boot it main's way."))
        git(d, "add", "-A")
        git(d, "commit", "-q", "-m", "split")
        git(d, "checkout", "-q", "-b", "rebased")
        self.assertEqual(run(d, "reapply", "feature"), 1)
        text = s1.read_text()
        self.assertIn("<<<<<<<", text)
        self.assertIn("Boot it the branch's way.", text)
        self.assertIn("Boot it main's way.", text)


if __name__ == "__main__":
    unittest.main()
