"""The document in between: what every renderer walks, and no renderer parses.

The generator emits Markdown and HTML from one source. The obvious way to do
that is to write Markdown and convert it, and it is the wrong way: converting
means writing a Markdown parser, and every escaping bug then lands in the HTML
rather than in the Markdown where it could be seen. So the sections build this
structure instead, and each renderer walks it once.

The consequence worth knowing when adding a section: **text is never markup**.
A string handed to `P()` is literal, and the renderer escapes it for its own
target. Emphasis, code spans and links are `runs` -- `c("libs/sched")` rather
than a string with backticks in it -- so a doc comment lifted out of the model
cannot accidentally be read as formatting. The model's prose is full of `*`,
`_`, `[0..*]` and `#[expect]`, and every one of them survives intact because
nothing here ever looks at a character and wonders what it means.
"""

from __future__ import annotations

import dataclasses
import re

# A run of inline content. The first element names the kind; a link carries a
# label and a target, everything else carries one string.
Run = tuple
Cell = "str | list[Run] | None"


def t(value: str) -> Run:
    """Literal text."""
    return ("text", value)


def c(value: str) -> Run:
    """A code span."""
    return ("code", value)


def b(value: str) -> Run:
    """Bold."""
    return ("strong", value)


def i(value: str) -> Run:
    """Italic."""
    return ("em", value)


def a(label: str, target: str) -> Run:
    """A link."""
    return ("link", label, target)


def runs(value) -> list[Run]:
    """Normalise a cell into a list of runs. A bare string is literal text."""
    if value is None:
        return []
    if isinstance(value, str):
        return [t(value)] if value else []
    if isinstance(value, tuple):
        return [value]
    return list(value)


def plain(value) -> str:
    """The run list as unformatted text, for anchors and sorting."""
    return "".join(run[1] for run in runs(value))


# ---------------------------------------------------------------------------
# Blocks
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class Heading:
    level: int
    text: str
    anchor: str = ""

    def __post_init__(self) -> None:
        if not self.anchor:
            self.anchor = slug(self.text)


@dataclasses.dataclass
class P:
    """A paragraph. A bare string is literal; pass runs for formatting."""

    content: object

    @property
    def parts(self) -> list[Run]:
        return runs(self.content)


@dataclasses.dataclass
class Note:
    """A callout: something the reader should not skim past."""

    content: object
    label: str = "Note"

    @property
    def parts(self) -> list[Run]:
        return runs(self.content)


@dataclasses.dataclass
class Bullets:
    items: list


@dataclasses.dataclass
class Ordered:
    items: list


@dataclasses.dataclass
class Defs:
    """Term / description pairs."""

    items: list


@dataclasses.dataclass
class Table:
    head: list[str]
    rows: list[list]
    caption: object = None
    align: list[str] | None = None
    """Per column: "left" or "right". Numbers read better right-aligned."""


@dataclasses.dataclass
class Tree:
    """An indented structure: (depth, content) in order."""

    lines: list


@dataclasses.dataclass
class Steps:
    """An ordered flow: a sequence whose order is the information."""

    items: list


@dataclasses.dataclass
class Code:
    text: str
    language: str = ""


@dataclasses.dataclass
class Diagram:
    """A figure, placed. The graph itself lives in `figure.Figure`.

    The block holds no drawing, only the graph and the two things the document
    adds to it: a number, so prose can refer to a figure, and a file name for
    the copy written beside the page. Each renderer draws it its own way --
    Markdown hands it to GitHub as Mermaid, HTML lays it out and inlines the
    SVG -- which is the whole reason it arrives here undrawn.
    """

    figure: object
    number: int = 0
    file: str = ""
    """Where the standalone SVG lives, relative to the document."""

    @property
    def anchor(self) -> str:
        return f"fig-{getattr(self.figure, 'name', '')}"


@dataclasses.dataclass
class Doc:
    title: str
    subtitle: str = ""
    blocks: list = dataclasses.field(default_factory=list)
    meta: dict = dataclasses.field(default_factory=dict)

    def add(self, *blocks) -> None:
        for block in blocks:
            if block is not None:
                self.blocks.append(block)

    def headings(self, max_level: int = 2) -> list[Heading]:
        return [
            block
            for block in self.blocks
            if isinstance(block, Heading) and block.level <= max_level
        ]


_SLUG_STRIP = re.compile(r"[^a-z0-9\s-]")
_SLUG_SPACE = re.compile(r"[\s-]+")


def slug(text: str) -> str:
    """A GitHub-compatible anchor, so the Markdown and the HTML agree."""
    lowered = text.strip().lower()
    lowered = _SLUG_STRIP.sub("", lowered)
    return _SLUG_SPACE.sub("-", lowered).strip("-")
