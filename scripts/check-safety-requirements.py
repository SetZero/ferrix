#!/usr/bin/env python3
"""Hold the safety manual to its evidence.

`docs/certification/SAFETY-MANUAL.md` is an out-of-context safety argument: an
integrator reads it, designs against its assumed safety requirements, and
discharges its assumptions of use. That makes a stale claim in it worse than no
manual at all, because a manual is believed and a missing one is not.

So every claim names its evidence in `scripts/safety-requirements.json`, and
this asserts three things about the pair.

  1. **The manual and the register agree on which ids exist.** An `ASR-9` added
     to one and not the other fails, in either direction. A requirement that
     exists only in prose has no evidence; one that exists only in the register
     is claiming nothing.

  2. **Every named piece of evidence exists.** A file path must resolve, and a
     `path::symbol` reference must actually name something in that file. This
     is the check that earns its place: the manual cites `zero_frame`,
     `check_stacks`, `enable_user_access_protection` and a dozen others, and a
     rename that left the manual behind would leave an argument pointing at
     nothing.

  3. **Every requirement has evidence at all.** An ASR with an empty
     `demonstrated_by` is an assertion, and the manual's whole claim is that it
     does not make those.

What it deliberately does not check: whether the evidence actually *supports*
the claim. No script can. It checks that the citation resolves, which is the
part that rots silently.

    python3 scripts/check-safety-requirements.py
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REGISTER = ROOT / "scripts" / "safety-requirements.json"
MANUAL = ROOT / "docs" / "certification" / "SAFETY-MANUAL.md"

ID = re.compile(r"\b(ASR-\d+|FM-\d+|AoU-\d+)\b")


def ids_in_manual() -> set[str]:
    return set(ID.findall(MANUAL.read_text(encoding="utf-8")))


def evidence_exists(reference: str) -> str | None:
    """`None` if the reference resolves, else why it does not."""
    if "::" in reference:
        path_part, symbol = reference.split("::", 1)
    else:
        path_part, symbol = reference, None

    path = ROOT / path_part
    if not path.is_file():
        return f"no such file: {path_part}"
    if symbol is None:
        return None

    text = path.read_text(encoding="utf-8", errors="replace")
    # A definition, not a mention: the symbol must appear after `fn`, `struct`,
    # `const`, `static` or `enum`, or as a Python `def`. A doc comment that
    # names it is not evidence that it exists.
    defined = re.search(
        rf"(?:fn|struct|const|static|enum|type|def)\s+{re.escape(symbol)}\b", text
    )
    return None if defined else f"{path_part} defines no `{symbol}`"


def main() -> int:
    register = json.loads(REGISTER.read_text(encoding="utf-8"))
    manual = ids_in_manual()
    status = 0

    registered: set[str] = set()
    registered |= {r["id"] for r in register["assumed_safety_requirements"]}
    registered |= {f["id"] for f in register["failure_modes"]}
    registered |= set(register["assumptions_of_use"])

    missing_from_manual = sorted(registered - manual)
    missing_from_register = sorted(manual - registered)

    if missing_from_manual:
        print(
            "safety-requirements: recorded but not stated in the manual. A claim\n"
            "  with evidence and no text says nothing to an integrator:",
            file=sys.stderr,
        )
        for name in missing_from_manual:
            print(f"    {name}", file=sys.stderr)
        status = 1

    if missing_from_register:
        print(
            "safety-requirements: stated in the manual with no entry here. The\n"
            "  manual's claim is that it cites evidence for everything; an id\n"
            "  with none is the claim it exists to avoid:",
            file=sys.stderr,
        )
        for name in missing_from_register:
            print(f"    {name}", file=sys.stderr)
        status = 1

    checked = 0
    for requirement in register["assumed_safety_requirements"]:
        references = requirement["implemented_by"] + requirement["demonstrated_by"]
        if not requirement["demonstrated_by"]:
            print(
                f"safety-requirements: {requirement['id']} has no evidence",
                file=sys.stderr,
            )
            status = 1
        for reference in references:
            checked += 1
            if problem := evidence_exists(reference):
                print(
                    f"safety-requirements: {requirement['id']} cites {problem}",
                    file=sys.stderr,
                )
                status = 1

    for mode in register["failure_modes"]:
        for reference in mode["mitigated_by"]:
            checked += 1
            if problem := evidence_exists(reference):
                print(
                    f"safety-requirements: {mode['id']} cites {problem}",
                    file=sys.stderr,
                )
                status = 1

    print(
        f"safety-requirements: {len(register['assumed_safety_requirements'])} "
        f"assumed requirements, {len(register['failure_modes'])} failure modes, "
        f"{len(register['assumptions_of_use'])} assumptions of use, "
        f"{checked} evidence references resolved"
    )
    return status


if __name__ == "__main__":
    sys.exit(main())
