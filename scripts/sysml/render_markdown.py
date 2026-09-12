"""Render a `document.Doc` as GitHub-flavoured Markdown.

Two rules carry the file. Every string that came from the model is escaped,
because the model's prose contains `*`, `_`, `[0..*]`, `#[expect]` and
`|`, and an unescaped pipe silently eats a table column. And nothing is
wrapped: a generated document is diffed, and reflowing prose turns a one-word
change into a twelve-line hunk.
"""

from __future__ import annotations

import re

from . import render_mermaid
from .document import (
    Bullets,
    Code,
    Defs,
    Diagram,
    Doc,
    Heading,
    Note,
    Ordered,
    P,
    Steps,
    Table,
    Tree,
    plain,
    runs,
)

# Escaping is kept to what GitHub-flavoured Markdown actually reacts to.
# Escaping everything punctuation-shaped is tempting and wrong: it turns
# `docs/ROADMAP.md` into `docs/ROADMAP\.md` in a file people read as text, and
# makes every diff of this document a wall of backslashes.
_ALWAYS = set("\\`*[]<")
_ENTITY = re.compile(r"&(?=[A-Za-z#][A-Za-z0-9]*;)")
_LEADING = re.compile(r"^(\s*)([#>+=-]|\d+[.)])")


def escape(text: str, line_start: bool = True) -> str:
    out: list[str] = []
    for index, char in enumerate(text):
        if char in _ALWAYS:
            out.append("\\" + char)
        elif char == "_":
            # Intraword underscores are literal in GFM, so `set_tid_address`
            # needs nothing; only one that could *open* emphasis is escaped.
            before = text[index - 1] if index else " "
            out.append("_" if before.isalnum() else "\\_")
        elif char == "~":
            # Only a doubled tilde is strikethrough; `~2 GiB` is prose.
            following = text[index + 1 : index + 2]
            out.append("\\~" if following == "~" else "~")
        else:
            out.append(char)
    escaped = _ENTITY.sub(r"\\&", "".join(out))
    if line_start:
        escaped = _LEADING.sub(r"\1\\\2", escaped)
    return escaped


def escape_cell(text: str) -> str:
    """Table cells cannot hold a newline, and a pipe would eat a column."""
    return escape(text, line_start=False).replace("\n", " ").replace("|", "\\|")


def inline(content, in_table: bool = False) -> str:
    escaper = escape_cell if in_table else escape
    out: list[str] = []
    for run in runs(content):
        kind = run[0]
        if kind == "text":
            out.append(escaper(run[1]))
        elif kind == "code":
            # A code span needs no escaping, only a fence long enough to hold
            # whatever backticks the value itself contains.
            value = run[1].replace("\n", " ")
            fence = "`"
            while fence in value:
                fence += "`"
            padding = " " if value.startswith("`") or value.endswith("`") else ""
            out.append(f"{fence}{padding}{value}{padding}{fence}")
        elif kind == "strong":
            out.append(f"**{escaper(run[1])}**")
        elif kind == "em":
            out.append(f"_{escaper(run[1])}_")
        elif kind == "link":
            out.append(f"[{escaper(run[1])}]({run[2]})")
    return "".join(out)


def render(doc: Doc, contents: bool = True) -> str:
    lines: list[str] = []
    lines.append(f"# {escape(doc.title)}")
    lines.append("")
    if doc.subtitle:
        lines.append(f"_{escape(doc.subtitle)}_")
        lines.append("")

    banner = doc.meta.get("banner")
    if banner:
        lines.append(f"> {escape(banner)}")
        lines.append("")

    if contents:
        headings = doc.headings(2)
        if headings:
            lines.append("## Contents")
            lines.append("")
            for heading in headings:
                indent = "  " * (heading.level - 1)
                lines.append(f"{indent}- [{escape(heading.text)}](#{heading.anchor})")
            lines.append("")

    for block in doc.blocks:
        lines.extend(_block(block))
    # One trailing newline, and no more: the line-endings gate looks at this.
    return "\n".join(lines).rstrip("\n") + "\n"


def _block(block) -> list[str]:
    if isinstance(block, Heading):
        return [f"{'#' * (block.level + 1)} {escape(block.text)}", ""]

    if isinstance(block, P):
        return [inline(block.parts), ""]

    if isinstance(block, Note):
        body = inline(block.parts).replace("\n", "\n> ")
        return [f"> **{escape(block.label)}** — {body}", ""]

    if isinstance(block, Bullets):
        return [f"- {inline(item)}" for item in block.items] + [""]

    if isinstance(block, Ordered):
        return [f"{n}. {inline(item)}" for n, item in enumerate(block.items, 1)] + [""]

    if isinstance(block, Steps):
        out = []
        for index, item in enumerate(block.items, 1):
            out.append(f"{index}. {inline(item)}")
        return out + [""]

    if isinstance(block, Defs):
        out = []
        for term, description in block.items:
            out.append(f"- **{inline(term)}** — {inline(description)}")
        return out + [""]

    if isinstance(block, Tree):
        out = ["```text"]
        for depth, content in block.lines:
            out.append("  " * depth + plain(content))
        out.append("```")
        return out + [""]

    if isinstance(block, Code):
        return [f"```{block.language}", block.text.rstrip("\n"), "```", ""]

    if isinstance(block, Diagram):
        return _diagram(block)

    if isinstance(block, Table):
        return _table(block)

    raise TypeError(f"no Markdown rendering for {type(block).__name__}")


def _diagram(block: Diagram) -> list[str]:
    """A Mermaid fence and a caption.

    GitHub renders the fence itself, so the Markdown carries a real diagram
    rather than a link to a picture of one -- and a reader looking at the raw
    file gets the graph as text, which is the next best thing. The caption
    links the standalone SVG for everywhere else.
    """
    figure = block.figure
    out = ["```mermaid", render_mermaid.render(figure), "```", ""]
    caption = f"**Figure {block.number} — {escape(figure.title, line_start=False)}.**"
    if figure.caption:
        caption += " " + _caption_runs(figure.caption)
    if block.file:
        caption += f" [SVG]({block.file})"
    if figure.source:
        caption += f" Source: `{figure.source}`."
    out.extend([caption, ""])
    return out


def _caption_runs(caption: str) -> str:
    """A caption written with `backticks` keeps them; everything else escapes.

    The captions come from `diagrams.py`, which writes the model's own names in
    backticks the way the rest of the document does.
    """
    out: list[str] = []
    for index, part in enumerate(caption.split("`")):
        out.append(f"`{part}`" if index % 2 else escape(part, line_start=False))
    return "".join(out)


def _table(block: Table) -> list[str]:
    width = len(block.head)
    align = block.align or ["left"] * width
    separators = []
    for column in range(width):
        separators.append("---:" if align[column] == "right" else "---")
    out = [
        "| " + " | ".join(escape_cell(head) for head in block.head) + " |",
        "| " + " | ".join(separators) + " |",
    ]
    for row in block.rows:
        cells = [inline(cell, in_table=True) for cell in row]
        cells += [""] * (width - len(cells))
        out.append("| " + " | ".join(cells) + " |")
    out.append("")
    if block.caption:
        out.append(inline(block.caption))
        out.append("")
    return out
