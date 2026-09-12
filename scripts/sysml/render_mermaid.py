"""Write a figure as Mermaid, which is how the Markdown draws it.

The Markdown output is read on GitHub, and GitHub renders a ```mermaid fence
itself: theme-aware, selectable, zoomable, and no image file to keep in step
with the model. The HTML output cannot use it -- that page has to open from a
file:// path with no network -- so it draws the same figure with the layout
engine next door. One figure, two renderings, and neither is a picture of the
other.

Where the SVG says a thing with a shape, Mermaid says it with a word: a hollow
triangle is `specializes` on the edge, a filled diamond is `part of`. A reader
who does not know UML loses nothing, and the two outputs still agree about what
the model says, which is the only agreement that matters.

Everything here is escaping. A Mermaid label is parsed twice -- once by Mermaid,
once as HTML -- so `~EfiHandoffPort`, `[0..*]` and `#[expect]` each have a way
of ending the diagram early if they go in unquoted.
"""

from __future__ import annotations

import re

from .figure import Figure, Node

# Mermaid reads `#` as the start of an entity and the label as HTML, so the
# characters that can end a label early are replaced by entities it decodes
# back to the original text.
_ENTITIES = {
    "#": "#35;",
    '"': "#quot;",
    "<": "#lt;",
    ">": "#gt;",
    "{": "#123;",
    "}": "#125;",
    "|": "#124;",
    "(": "#40;",
    ")": "#41;",
    "[": "#91;",
    "]": "#93;",
}

_ID = re.compile(r"[^A-Za-z0-9]+")

# The maturity palette, written out rather than inherited: a Mermaid diagram on
# GitHub is themed by GitHub, and a fill with no colour beside it is a box a
# dark-theme reader cannot read. Backgrounds are the light ones from the page's
# palette, with the ink pinned to match.
CLASS_DEFS = {
    "implemented": "fill:#dceae2,stroke:#2c6e4e,color:#16191d",
    "inProgress": "fill:#dae5f0,stroke:#2a5f8f,color:#16191d",
    "writtenAhead": "fill:#e5dff0,stroke:#6b4fa0,color:#16191d",
    "planned": "fill:#e4e7ea,stroke:#6a737e,color:#16191d",
    "deferred": "fill:#f3e8ce,stroke:#87600b,color:#16191d",
}

# What an edge says when its shape cannot. `flow` says it with the arrow.
EDGE_WORDS = {
    "composition": "part of",
    "specialization": "specializes",
    "connect": "connects",
    "dependency": "depends on",
    "satisfy": "satisfied by",
    "verify": "verified by",
    "allocate": "allocated to",
}
DASHED = ("dependency", "satisfy", "verify", "allocate")


def escape(text: str) -> str:
    return "".join(_ENTITIES.get(char, char) for char in text)


def node_id(figure: Figure, node: Node) -> str:
    """A Mermaid-safe identifier, unique within the figure.

    The model's names are already identifiers, but a node id here can be a
    qualified name or a port pair, so everything outside `[A-Za-z0-9]` folds to
    an underscore and the index keeps two folded names apart.
    """
    index = figure.nodes.index(node)
    return f"n{index}_{_ID.sub('_', node.id).strip('_')[:40]}"


def _label(node: Node) -> str:
    lines: list[str] = []
    if node.kind == "interface":
        lines.append("«interface»")
    lines.append(node.label)
    if node.sublabel:
        lines.append(node.sublabel)
    lines.extend(node.rows)
    return "<br>".join(escape(line) for line in lines)


def _shape(node: Node, label: str) -> str:
    if node.kind == "terminal":
        return f'(["{label}"])'
    if node.kind == "choice":
        return f'{{"{label}"}}'
    if node.kind in ("state", "action"):
        return f'("{label}")'
    if node.kind == "note":
        return f'>"{label}"]'
    return f'["{label}"]'


def render(figure: Figure) -> str:
    """The figure as Mermaid source, without the fence around it."""
    if figure.kind == "state":
        return _state_diagram(figure)
    return _flowchart(figure)


def _flowchart(figure: Figure) -> str:
    lines = [f"flowchart {figure.direction}"]
    ids = {node.id: node_id(figure, node) for node in figure.nodes}

    clustered = {cluster.id: figure.members(cluster.id) for cluster in figure.clusters}
    loose = [node for node in figure.nodes if node.cluster not in clustered]

    for node in loose:
        lines.append(f"  {ids[node.id]}{_shape(node, _label(node))}")
    for cluster in figure.clusters:
        members = clustered.get(cluster.id) or []
        if not members:
            continue
        title = escape(cluster.label + (f" — {cluster.sublabel}" if cluster.sublabel else ""))
        lines.append(f'  subgraph {node_id(figure, members[0])}_g["{title}"]')
        for node in members:
            lines.append(f"    {ids[node.id]}{_shape(node, _label(node))}")
        lines.append("  end")

    for edge in figure.edges:
        label = edge.label or EDGE_WORDS.get(edge.kind, "")
        arrow = "-.->" if edge.kind in DASHED else "-->"
        if label:
            middle = "-. " if edge.kind in DASHED else "-- "
            tail = " .->" if edge.kind in DASHED else " -->"
            lines.append(
                f'  {ids[edge.source]} {middle}"{escape(label)}"{tail} {ids[edge.target]}'
            )
        else:
            lines.append(f"  {ids[edge.source]} {arrow} {ids[edge.target]}")

    used = figure.maturities()
    for maturity in used:
        lines.append(f"  classDef {maturity} {CLASS_DEFS[maturity]}")
    for maturity in used:
        members = [ids[node.id] for node in figure.nodes if node.maturity == maturity]
        if members:
            lines.append(f"  class {','.join(members)} {maturity}")

    for node in figure.nodes:
        if node.href:
            lines.append(f'  click {ids[node.id]} "{escape(node.href)}"')
    return "\n".join(lines)


def _state_diagram(figure: Figure) -> str:
    """A state machine, in the notation Mermaid has for exactly this."""
    lines = ["stateDiagram-v2"]
    ids = {node.id: node_id(figure, node) for node in figure.nodes}
    starts = {edge.target for edge in figure.edges}

    for node in figure.nodes:
        if node.kind == "terminal":
            continue
        lines.append(f'  state "{escape(node.label)}" as {ids[node.id]}')
        if node.doc:
            first = _first_sentence(node.doc)
            if first:
                lines.append(f"  {ids[node.id]} : {escape(first)}")

    for node in figure.nodes:
        if node.kind == "terminal" or node.id in starts:
            continue
        lines.append(f"  [*] --> {ids[node.id]}")

    for edge in figure.edges:
        source = figure.node(edge.source)
        target = figure.node(edge.target)
        if source is None or target is None:
            continue
        left = "[*]" if source.kind == "terminal" else ids[edge.source]
        right = "[*]" if target.kind == "terminal" else ids[edge.target]
        suffix = f" : {escape(edge.label)}" if edge.label else ""
        lines.append(f"  {left} --> {right}{suffix}")
    return "\n".join(lines)


def _first_sentence(text: str, limit: int = 90) -> str:
    flat = " ".join(text.split())
    cut = flat.find(". ")
    if cut != -1:
        flat = flat[: cut + 1]
    if len(flat) > limit:
        flat = flat[: limit - 1].rstrip() + "…"
    return flat
