"""The figure in between: what a diagram is before anything draws it.

`document.py` exists because text is not markup; this exists for the same
reason one level up. A diagram is a graph -- nodes, edges, and the boxes drawn
around groups of them -- and the three things that draw one here disagree about
everything else:

    render_svg.py       lays the graph out and emits geometry
    render_mermaid.py   hands the graph to GitHub's renderer as text
    emit_json.py        hands the graph to whatever reads the model next

So nothing below mentions a pixel, a colour or a syntax. `diagrams.py` builds
these from the model, `sections.py` places them in the document, and a renderer
that cannot draw something (a compartment, a nested cluster) degrades rather
than fails -- which is why a node carries a *label* and a *kind* rather than a
shape, and an edge carries a *kind* rather than a line style.

One rule, inherited from `document.py`: **a label is literal text**. It comes
out of the model, where `[0..*]`, `~BootHandoffPort` and `#[expect]` are
ordinary, and each renderer escapes it for its own target.
"""

from __future__ import annotations

import dataclasses
import re

# Node kinds. A renderer maps these to a shape; one it does not know draws as a
# plain box, which is never wrong, only plainer.
NODE_KINDS = (
    "box",  # a part, a crate, anything with an identity
    "def",  # a definition: the box gets its compartments
    "interface",  # an interface definition
    "action",  # a step in a flow
    "state",  # a state in a state machine
    "terminal",  # start / done
    "choice",  # a branch
    "requirement",  # a requirement or a goal
    "test",  # a verification case
    "port",  # a port on the edge of a part
    "note",  # prose hung off the side
)

# Edge kinds, each of which means a different thing in SysML and so reads
# differently once drawn: composition gets a diamond, specialization a hollow
# triangle, dependency and the trace relations a dashed line.
EDGE_KINDS = (
    "flow",  # succession: first ... then
    "transition",  # a state transition, usually with a trigger
    "composition",  # a part within a part
    "specialization",  # :> -- the subtype points at the supertype
    "connect",  # a connection between two ports
    "dependency",  # dependency from ... to
    "satisfy",
    "verify",
    "allocate",
)

# What a figure is of. Only the layout cares: a flow wants its steps in a
# column, a block diagram wants its layers across the page.
FIGURE_KINDS = ("flow", "state", "block", "interface", "trace", "dependency")


@dataclasses.dataclass
class Node:
    id: str
    label: str
    """The main line: the name the model gives it, as the model spells it."""

    sublabel: str = ""
    """A second, quieter line: a type, a short name, a stage, a size."""

    kind: str = "box"
    maturity: str = ""
    """One of the lifecycle keywords, or `deferred`. The only colour a
    renderer is allowed to spend, because it is the one thing a reader scans
    a diagram for."""

    stage: int | None = None
    rows: list[str] = dataclasses.field(default_factory=list)
    """Compartment lines -- an interface's operations, a part's features.
    A renderer too coarse for compartments drops them and the diagram still
    says who talks to whom."""

    doc: str = ""
    """The model's prose for this element, for a tooltip. Never laid out."""

    cluster: str = ""

    href: str = ""
    """An anchor in the generated document, so a node in the HTML is a link
    to the table row that describes it."""


@dataclasses.dataclass
class Edge:
    source: str
    target: str
    label: str = ""
    kind: str = "flow"

    @property
    def is_trace(self) -> bool:
        return self.kind in ("dependency", "satisfy", "verify", "allocate")


@dataclasses.dataclass
class Cluster:
    id: str
    label: str
    sublabel: str = ""
    maturity: str = ""


@dataclasses.dataclass
class Figure:
    """One diagram: a graph, plus how it wants to be read."""

    name: str
    """A slug. It names the standalone file and the anchor, so it is stable."""

    title: str
    kind: str = "block"
    direction: str = "TB"
    """`TB` for something read as a sequence, `LR` for something read as a
    structure. A tall flow in a wide page is a scroll; a wide tree is a squint."""

    nodes: list[Node] = dataclasses.field(default_factory=list)
    edges: list[Edge] = dataclasses.field(default_factory=list)
    clusters: list[Cluster] = dataclasses.field(default_factory=list)

    flip: bool = False
    """Draw the layers in the other order: last at the top, first at the
    bottom. A generalisation is the case that needs it -- the arrow has to
    point from the subtype to the supertype, and UML has drawn the supertype
    above its subtypes since before it was UML."""

    caption: str = ""
    """One sentence under the figure, in the document's voice: how to read it."""

    source: str = ""
    """Where in the model it came from, `04-memory.sysml` style, so a reader
    who distrusts the picture can go and check the text."""

    # -- construction -----------------------------------------------------

    def add(self, node: Node) -> Node:
        """Add a node, or return the one already there with that id.

        Figures are built by walking the model from several directions at
        once -- a part reached as a member and again as the target of a
        connection -- and the second arrival should not make a second box.
        """
        existing = self.node(node.id)
        if existing is not None:
            if node.sublabel and not existing.sublabel:
                existing.sublabel = node.sublabel
            if node.maturity and not existing.maturity:
                existing.maturity = node.maturity
            if node.doc and not existing.doc:
                existing.doc = node.doc
            return existing
        self.nodes.append(node)
        return node

    def link(self, source: str, target: str, kind: str = "flow", label: str = "") -> None:
        """Add an edge, if both ends exist and it is not already there.

        Dropping an edge whose ends are missing is deliberate: the figures are
        views, every view is a subset, and a relation that leaves the subset
        is not an error in the model.
        """
        if source == target and kind != "transition":
            return
        if self.node(source) is None or self.node(target) is None:
            return
        for edge in self.edges:
            if edge.source == source and edge.target == target and edge.kind == kind:
                # The same two things joined twice -- `userland.posix` to the
                # kernel's Linux ABI and `userland.native` to its native one --
                # is two statements in the model and one line on the page, so
                # the line has to carry both labels or the second disappears.
                if label and label not in edge.label.split(" · "):
                    edge.label = f"{edge.label} · {label}" if edge.label else label
                return
        self.edges.append(Edge(source=source, target=target, kind=kind, label=label))

    def node(self, node_id: str) -> Node | None:
        for node in self.nodes:
            if node.id == node_id:
                return node
        return None

    # -- queries a renderer asks ------------------------------------------

    @property
    def is_empty(self) -> bool:
        """A figure with nothing to see. Sections check this and add nothing.

        One box is not a diagram, so a figure has to have an edge: a lone node
        says less than the sentence that would have introduced it.
        """
        return not self.edges

    def maturities(self) -> list[str]:
        """The lifecycle keywords actually used, for the legend."""
        seen: list[str] = []
        for node in self.nodes:
            if node.maturity and node.maturity not in seen:
                seen.append(node.maturity)
        return seen

    def edge_kinds(self) -> list[str]:
        seen: list[str] = []
        for edge in self.edges:
            if edge.kind not in seen:
                seen.append(edge.kind)
        return seen

    def cluster(self, cluster_id: str) -> Cluster | None:
        for cluster in self.clusters:
            if cluster.id == cluster_id:
                return cluster
        return None

    def members(self, cluster_id: str) -> list[Node]:
        return [node for node in self.nodes if node.cluster == cluster_id]

    def alt_text(self) -> str:
        """What the figure says, for a reader who cannot see it.

        Not a description of the picture -- a description of the graph, which
        is the content. Long enough to be useful and short enough to be read.
        """
        kind = {
            "flow": "Flow diagram",
            "state": "State machine",
            "block": "Block diagram",
            "interface": "Interface diagram",
            "trace": "Traceability diagram",
            "dependency": "Dependency diagram",
        }.get(self.kind, "Diagram")
        parts = [f"{kind}: {self.title}.", f"{len(self.nodes)} nodes, {len(self.edges)} edges."]
        if self.kind in ("flow", "state"):
            chain = _longest_chain(self)
            if chain:
                parts.append("Main path: " + " then ".join(chain) + ".")
        else:
            busiest = _busiest(self)
            if busiest:
                parts.append(
                    "Most connected: "
                    + ", ".join(f"{name} ({count})" for name, count in busiest)
                    + "."
                )
        return " ".join(parts)


def _longest_chain(figure: Figure, limit: int = 12) -> list[str]:
    """The longest path through a flow, by label. Used for the alt text."""
    successors: dict[str, list[str]] = {node.id: [] for node in figure.nodes}
    indegree = {node.id: 0 for node in figure.nodes}
    for edge in figure.edges:
        successors[edge.source].append(edge.target)
        indegree[edge.target] += 1
    best: list[str] = []
    for node in figure.nodes:
        if indegree[node.id]:
            continue
        path: list[str] = []
        seen: set[str] = set()
        current: str | None = node.id
        while current is not None and current not in seen and len(path) < limit:
            seen.add(current)
            element = figure.node(current)
            if element is not None:
                path.append(element.label)
            following = successors.get(current, [])
            current = following[0] if following else None
        if len(path) > len(best):
            best = path
    return best


def _busiest(figure: Figure, limit: int = 3) -> list[tuple[str, int]]:
    degree: dict[str, int] = {node.id: 0 for node in figure.nodes}
    for edge in figure.edges:
        degree[edge.source] += 1
        degree[edge.target] += 1
    ranked = sorted(degree.items(), key=lambda item: (-item[1], item[0]))
    out = []
    for node_id, count in ranked[:limit]:
        if count == 0:
            continue
        node = figure.node(node_id)
        out.append((node.label if node else node_id, count))
    return out


_SLUG = re.compile(r"[^a-z0-9]+")


def slugify(text: str) -> str:
    """A stable file name for a figure, from the model's own spelling."""
    spaced = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "-", text)
    return _SLUG.sub("-", spaced.lower()).strip("-")
