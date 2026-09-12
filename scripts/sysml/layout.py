"""Place a figure's nodes and route its edges. The only module with geometry.

There is no Graphviz here for the same reason there is no SysML parser: a gate
that needs an apt or a pip is a gate that does not run on a stock checkout. So
this is a layered layout -- the one Sugiyama described and every block diagram
since has used -- in standard library Python:

  1. break cycles, remembering which edges were reversed, so the rest of the
     pass can assume a DAG and a back edge still draws pointing backwards;
  2. assign each node to a layer by longest path from a source;
  3. insert a dummy node per layer an edge crosses, so an edge that spans four
     layers has four places to bend and nothing to cross through;
  4. order each layer by the average position of its neighbours, repeatedly,
     which is what stops edges crossing;
  5. give every node a coordinate across the layer, pulling each towards its
     neighbours and then pushing boxes apart until none overlap.

Two properties matter more than prettiness. It is **deterministic**: every
iteration count is fixed, every sort is stable and keyed on something that
cannot tie, and nothing consults a random number or a dictionary order. The
generated document is compared byte for byte, so a layout that wandered would
fail CI on an unrelated commit. And it **measures text honestly**: every label
is drawn in a monospace face at a known size, so a box's width is arithmetic
rather than a guess at what a proportional font will do on somebody else's
machine.

The layout is computed in *layer* and *cross* axes -- along the reading
direction and across it -- and transposed at the end, which is why `TB` and
`LR` are one code path rather than two.
"""

from __future__ import annotations

import dataclasses

from .figure import Edge, Figure, Node

# Text metrics. The face is monospace, declared in the styles beside the
# renderer, so one character is exactly 0.6 em wide and these are facts rather
# than estimates. Everything else on the diagram is sized from them.
CHAR = 0.6
LABEL_SIZE = 12.0
SUB_SIZE = 10.5
ROW_SIZE = 10.5
STEREOTYPE_SIZE = 9.5
EDGE_LABEL_SIZE = 10.0
EDGE_LABEL_CHARS = 44
"""Edge labels run longer than node labels: a connection carries two port
names, and a guard is a condition rather than a name."""

LABEL_LINE = 16.0
SUB_LINE = 14.0
ROW_LINE = 14.5

PAD_X = 11.0
PAD_Y = 9.0
MIN_WIDTH = 58.0
MAX_CHARS = 34
"""Labels longer than this are elided. The model's names are identifiers, and
the handful that run long are long because they are compounds; the full text
survives in the tooltip and in the table the figure sits beside."""

GAP_CROSS = 22.0
"""Minimum space between two boxes in the same layer."""
GAP_DUMMY = 8.0
"""And between two lines passing through it. A long edge is given a waypoint
in every layer it crosses, and a fan of thirty leaves means eighteen lines
threading the first column; charging each of those the width of a box is what
turns a diagram of thirty boxes into a diagram a metre tall."""
GAP_LAYER = 46.0
"""Space between one layer and the next, before edge labels are considered."""
GAP_LAYER_LABELLED = 62.0
"""Layers need more room when the edges crossing them carry text."""

CLUSTER_PAD = 13.0
CLUSTER_TITLE = 17.0

MARGIN = 14.0

LAYER_LIMIT = 12
"""How many nodes a layer may hold before the ones at the end of the graph are
dealt a second row. A fan is the common shape in this model -- a part with
twenty members, a stage with eight parts allocated to it -- and a layered
layout puts a fan in one line, which on a page is one very long line."""

ORDER_PASSES = 6
PLACE_PASSES = 8
"""Fixed, not "until it converges": a fixed count is reproducible and these
two numbers are where the layouts here stop visibly improving."""


# ---------------------------------------------------------------------------
# Geometry
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class Box:
    """A placed node. `x`, `y` is the top left corner."""

    node: Node
    x: float = 0.0
    y: float = 0.0
    width: float = 0.0
    height: float = 0.0
    lines: list[str] = dataclasses.field(default_factory=list)
    """The label, elided and split, as it will be drawn."""
    rows: list[str] = dataclasses.field(default_factory=list)
    stereotype: str = ""

    @property
    def centre_x(self) -> float:
        return self.x + self.width / 2

    @property
    def centre_y(self) -> float:
        return self.y + self.height / 2


@dataclasses.dataclass
class ClusterBox:
    id: str
    label: str
    sublabel: str
    x: float = 0.0
    y: float = 0.0
    width: float = 0.0
    height: float = 0.0


@dataclasses.dataclass
class Route:
    """A placed edge: the points its line passes through, in drawing order."""

    edge: Edge
    points: list[tuple[float, float]] = dataclasses.field(default_factory=list)
    label: str = ""
    label_x: float = 0.0
    label_y: float = 0.0
    label_anchor: str = "middle"
    """Which end of the text sits at `label_x`. A guard on a branch hangs off
    the side its own line is heading for, so two guards out of one `decide`
    lean away from each other instead of meeting in the middle."""

    reversed: bool = False
    """True when layering turned this edge around to break a cycle. The points
    are still in the model's order -- the chain is put back the right way round
    when the dummies go in -- so this is for anything that wants to draw a back
    edge differently, not for the arrowhead."""


@dataclasses.dataclass
class Layout:
    figure: Figure
    width: float = 0.0
    height: float = 0.0
    boxes: list[Box] = dataclasses.field(default_factory=list)
    clusters: list[ClusterBox] = dataclasses.field(default_factory=list)
    routes: list[Route] = dataclasses.field(default_factory=list)

    def box(self, node_id: str) -> Box | None:
        for box in self.boxes:
            if box.node.id == node_id:
                return box
        return None


# ---------------------------------------------------------------------------
# Text
# ---------------------------------------------------------------------------


def text_width(text: str, size: float = LABEL_SIZE) -> float:
    return len(text) * size * CHAR


def elide(text: str, limit: int = MAX_CHARS) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1].rstrip() + "…"


STEREOTYPES = {
    "interface": "«interface»",
    "def": "«def»",
    "requirement": "«requirement»",
    "test": "«verification»",
    "port": "«port»",
}


def _measure(node: Node) -> Box:
    """Size one box from its text. Nothing else decides how big a box is."""
    box = Box(node=node)
    box.stereotype = STEREOTYPES.get(node.kind, "")
    box.lines = [elide(node.label)]
    box.rows = [elide(row, 38) for row in node.rows]

    widths = [text_width(line, LABEL_SIZE) for line in box.lines]
    if node.sublabel:
        widths.append(text_width(elide(node.sublabel, 38), SUB_SIZE))
    if box.stereotype:
        widths.append(text_width(box.stereotype, STEREOTYPE_SIZE))
    widths.extend(text_width(row, ROW_SIZE) for row in box.rows)

    box.width = max(MIN_WIDTH, max(widths) + 2 * PAD_X)
    height = 2 * PAD_Y + LABEL_LINE * len(box.lines)
    if box.stereotype:
        height += SUB_LINE
    if node.sublabel:
        height += SUB_LINE
    if box.rows:
        # A rule separates the compartment from the name above it.
        height += 5 + ROW_LINE * len(box.rows)
    box.height = height

    if node.kind == "choice":
        # A diamond needs its text to fit inside the inscribed rectangle.
        box.width += box.width * 0.35
        box.height += 14
    elif node.kind == "terminal":
        box.width += 16
    return box


# ---------------------------------------------------------------------------
# The pass
# ---------------------------------------------------------------------------


class _Graph:
    """The working graph: real nodes, dummy nodes, and the layering over them."""

    def __init__(self, figure: Figure) -> None:
        self.figure = figure
        self.order: list[str] = [node.id for node in figure.nodes]
        self.boxes: dict[str, Box] = {node.id: _measure(node) for node in figure.nodes}
        self.layer: dict[str, int] = {}
        self.cross: dict[str, float] = {}
        self.dummy: dict[str, tuple[str, int]] = {}
        """Dummy id -> (the edge's key, its index along the edge)."""
        self.layers: list[list[str]] = []
        self.points: dict[str, tuple[float, float]] = {}
        """Where each dummy ended up, once the layers have coordinates."""
        self.segments: list[tuple[Edge, list[str], bool]] = []
        """Each edge, the chain of ids its line runs through, and whether it
        was reversed to break a cycle."""

    # -- 1. cycles --------------------------------------------------------

    def break_cycles(self) -> list[tuple[Edge, bool]]:
        """Return every edge with a flag for whether layering reversed it.

        Depth first from the nodes in the order the figure declared them: an
        edge into a node still on the stack closes a cycle, and is the one
        reversed. Which edge that is depends only on that declaration order,
        so it is the same on every machine.
        """
        successors: dict[str, list[tuple[str, Edge]]] = {key: [] for key in self.order}
        for edge in self.figure.edges:
            if edge.source == edge.target:
                continue
            successors[edge.source].append((edge.target, edge))

        state: dict[str, int] = {key: 0 for key in self.order}
        reversed_edges: set[int] = set()

        def visit(start: str) -> None:
            stack: list[tuple[str, int]] = [(start, 0)]
            state[start] = 1
            while stack:
                node_id, index = stack[-1]
                following = successors[node_id]
                if index >= len(following):
                    state[node_id] = 2
                    stack.pop()
                    continue
                stack[-1] = (node_id, index + 1)
                target, edge = following[index]
                if state[target] == 1:
                    reversed_edges.add(id(edge))
                elif state[target] == 0:
                    state[target] = 1
                    stack.append((target, 0))

        for node_id in self.order:
            if state[node_id] == 0:
                visit(node_id)

        return [(edge, id(edge) in reversed_edges) for edge in self.figure.edges]

    # -- 2. layers --------------------------------------------------------

    def assign_layers(self, edges: list[tuple[Edge, bool]]) -> None:
        incoming: dict[str, list[str]] = {key: [] for key in self.order}
        outgoing: dict[str, list[str]] = {key: [] for key in self.order}
        for edge, is_reversed in edges:
            if edge.source == edge.target:
                continue
            source, target = (edge.target, edge.source) if is_reversed else (edge.source, edge.target)
            incoming[target].append(source)
            outgoing[source].append(target)

        # Longest path from a source, computed over a topological order so the
        # loop is linear and needs no recursion depth.
        remaining = {key: len(incoming[key]) for key in self.order}
        queue = [key for key in self.order if remaining[key] == 0]
        layer = {key: 0 for key in self.order}
        visited: list[str] = []
        while queue:
            node_id = queue.pop(0)
            visited.append(node_id)
            for target in outgoing[node_id]:
                layer[target] = max(layer[target], layer[node_id] + 1)
                remaining[target] -= 1
                if remaining[target] == 0:
                    queue.append(target)
        for node_id in self.order:
            if node_id not in visited:
                # Unreachable only if cycle breaking missed something; keep it
                # on the graph rather than dropping it silently.
                layer.setdefault(node_id, 0)

        self.layer = layer
        self._split_wide_layers()

    def _split_wide_layers(self) -> None:
        """Deal the sinks of an over-full layer into further layers.

        Only sinks: a node with something after it has to stay where the
        layering put it or its edge would run backwards. That is enough,
        because the layers that overflow here are the ones full of leaves.
        """
        counts: dict[int, list[str]] = {}
        for node_id, index in self.layer.items():
            counts.setdefault(index, []).append(node_id)

        outgoing = set()
        for edge in self.figure.edges:
            if edge.source != edge.target:
                outgoing.add(edge.source)

        for index in sorted(counts):
            members = [node_id for node_id in self.order if node_id in set(counts[index])]
            if len(members) <= LAYER_LIMIT:
                continue
            sinks = [node_id for node_id in members if node_id not in outgoing]
            keep = len(members) - len(sinks)
            if len(sinks) <= LAYER_LIMIT - keep:
                continue
            # How many rows the sinks need, and how deep everything below has
            # to move to make room for them.
            room = max(1, LAYER_LIMIT - keep)
            extra = (len(sinks) + room - 1) // room - 1
            if extra <= 0:
                continue
            for node_id in self.order:
                if self.layer.get(node_id, 0) > index:
                    self.layer[node_id] += extra
            for position, node_id in enumerate(sinks):
                self.layer[node_id] = index + position // room

    # -- 3. dummies -------------------------------------------------------

    def insert_dummies(self, edges: list[tuple[Edge, bool]]) -> None:
        for index, (edge, is_reversed) in enumerate(edges):
            if edge.source == edge.target:
                self.segments.append((edge, [edge.source], False))
                continue
            source, target = (edge.target, edge.source) if is_reversed else (edge.source, edge.target)
            start, end = self.layer[source], self.layer[target]
            chain = [source]
            for step, middle in enumerate(range(start + 1, end)):
                dummy_id = f"\x01{index}.{step}"
                self.dummy[dummy_id] = (dummy_id, step)
                self.layer[dummy_id] = middle
                self.order.append(dummy_id)
                chain.append(dummy_id)
            chain.append(target)
            if is_reversed:
                chain.reverse()
            self.segments.append((edge, chain, is_reversed))

    # -- 4. ordering ------------------------------------------------------

    def build_layers(self) -> None:
        depth = max(self.layer.values()) + 1 if self.layer else 0
        self.layers = [[] for _ in range(depth)]
        for node_id in self.order:
            self.layers[self.layer[node_id]].append(node_id)

    def _cluster_rank(self) -> dict[str, int]:
        """Which group a node sorts into, so a cluster stays contiguous.

        Cluster boxes are drawn as the bounding box of their members, so the
        members have to be neighbours in every layer they appear in. Sorting on
        the cluster before the barycentre is the cheapest way to guarantee a
        box that contains nothing it should not.
        """
        ranks = {cluster.id: index + 1 for index, cluster in enumerate(self.figure.clusters)}
        out: dict[str, int] = {}
        for node in self.figure.nodes:
            out[node.id] = ranks.get(node.cluster, 0)
        for dummy_id in self.dummy:
            out[dummy_id] = 0
        return out

    def order_layers(self) -> None:
        neighbours_down: dict[str, list[str]] = {key: [] for key in self.order}
        neighbours_up: dict[str, list[str]] = {key: [] for key in self.order}
        for _, chain, _ in self.segments:
            for left, right in zip(chain, chain[1:]):
                if self.layer[left] == self.layer[right]:
                    continue
                upper, lower = (left, right) if self.layer[left] < self.layer[right] else (right, left)
                neighbours_down[upper].append(lower)
                neighbours_up[lower].append(upper)

        cluster_rank = self._cluster_rank()
        position = {
            node_id: float(index)
            for layer in self.layers
            for index, node_id in enumerate(layer)
        }

        for iteration in range(ORDER_PASSES):
            downward = iteration % 2 == 0
            indices = range(1, len(self.layers)) if downward else range(len(self.layers) - 2, -1, -1)
            for index in indices:
                source = neighbours_up if downward else neighbours_down
                layer = self.layers[index]
                keys: list[tuple[int, float, int, str]] = []
                for slot, node_id in enumerate(layer):
                    fixed = [position[other] for other in source[node_id]]
                    # No neighbour in the layer being read from: keep the slot
                    # it has, rather than drifting to one end.
                    centre = sum(fixed) / len(fixed) if fixed else position[node_id]
                    keys.append((cluster_rank[node_id], centre, slot, node_id))
                keys.sort()
                self.layers[index] = [key[3] for key in keys]
                for slot, node_id in enumerate(self.layers[index]):
                    position[node_id] = float(slot)

    # -- 5. coordinates ---------------------------------------------------

    def place(self) -> None:
        cross_size = {node_id: self._cross_size(node_id) for node_id in self.order}
        separation = {
            node_id: (GAP_DUMMY if node_id in self.dummy else GAP_CROSS)
            for node_id in self.order
        }
        cluster_of = {node.id: node.cluster for node in self.figure.nodes}

        def pack(layer: list[str], desired: dict[str, float]) -> None:
            """Left to right, honouring every gap, as close to desired as it can.

            The forward sweep makes the layer legal; the shift afterwards puts
            it back under the neighbours it was pulled towards, because a legal
            layer that has drifted right is a diagram with a lean.
            """
            if not layer:
                return
            centres: list[float] = []
            for index, node_id in enumerate(layer):
                want = desired[node_id]
                if index:
                    previous = layer[index - 1]
                    gap = (separation[previous] + separation[node_id]) / 2
                    if cluster_of.get(previous, "") != cluster_of.get(node_id, ""):
                        # Two clusters need room for both their borders.
                        gap += CLUSTER_PAD * 1.5
                    floor = centres[-1] + cross_size[previous] / 2 + gap + cross_size[node_id] / 2
                    want = max(want, floor)
                centres.append(want)
            drift = sum(centres[index] - desired[node_id] for index, node_id in enumerate(layer))
            drift /= len(layer)
            for index, node_id in enumerate(layer):
                self.cross[node_id] = centres[index] - drift

        # Start from a simple packing in the order the layers now hold.
        for layer in self.layers:
            running = 0.0
            for node_id in layer:
                self.cross[node_id] = running + cross_size[node_id] / 2
                running += cross_size[node_id] + GAP_CROSS

        neighbours: dict[str, list[str]] = {key: [] for key in self.order}
        for _, chain, _ in self.segments:
            for left, right in zip(chain, chain[1:]):
                if self.layer[left] == self.layer[right]:
                    continue
                neighbours[left].append(right)
                neighbours[right].append(left)

        for iteration in range(PLACE_PASSES):
            indices = (
                range(len(self.layers))
                if iteration % 2 == 0
                else range(len(self.layers) - 1, -1, -1)
            )
            for index in indices:
                layer = self.layers[index]
                desired: dict[str, float] = {}
                for node_id in layer:
                    linked = [self.cross[other] for other in neighbours[node_id]]
                    desired[node_id] = (
                        sum(linked) / len(linked) if linked else self.cross[node_id]
                    )
                pack(layer, desired)

            # A long edge should be a straight line, so its dummies agree on one
            # coordinate before the next pass pulls the boxes back into line.
            for _, chain, _ in self.segments:
                middle = [node_id for node_id in chain if node_id in self.dummy]
                if len(middle) < 2:
                    continue
                ends = [self.cross[chain[0]], self.cross[chain[-1]]]
                target = sum(ends) / 2
                for node_id in middle:
                    self.cross[node_id] = target
            for layer in self.layers:
                pack(layer, {node_id: self.cross[node_id] for node_id in layer})

        self._lanes(cross_size)

        lowest = min(self.cross.values()) if self.cross else 0.0
        for node_id in self.cross:
            self.cross[node_id] -= lowest

    def _lanes(self, cross_size: dict[str, float]) -> None:
        """Give each cluster a band of its own, across the whole diagram.

        Keeping a cluster's members next to each other *within* a layer is not
        enough to draw a box round them: the members sit in different layers,
        so two clusters can interleave across the page and their two boxes then
        overlap -- which says the two contain each other, and neither does.

        A band is the fix, and it is also how such a diagram is drawn by hand:
        one strip per container, in the order the figure declares them, with
        anything unclustered in a strip of its own at the top. Positions inside
        a band are the ones the placement worked out; only the band moves.
        """
        if not self.figure.clusters:
            return
        order = [""] + [cluster.id for cluster in self.figure.clusters]
        cluster_of = {node.id: node.cluster for node in self.figure.nodes}
        title = CLUSTER_TITLE if self.figure.direction == "LR" else 0.0
        pad = 2 * CLUSTER_PAD + title

        running = 0.0
        for band in order:
            members = [
                node_id
                for node_id in self.order
                if cluster_of.get(node_id, "") == band and node_id not in self.dummy
            ]
            if not members:
                continue
            lowest = min(self.cross[node_id] - cross_size[node_id] / 2 for node_id in members)
            highest = max(self.cross[node_id] + cross_size[node_id] / 2 for node_id in members)
            shift = running - lowest
            for node_id in members:
                self.cross[node_id] += shift
            running = highest + shift + pad

        # A line's waypoints belong to no band, so put each one back between
        # the two ends it joins rather than leaving it where the bands were.
        for _, chain, _ in self.segments:
            middle = [node_id for node_id in chain if node_id in self.dummy]
            if not middle:
                continue
            ends = [self.cross[chain[0]], self.cross[chain[-1]]]
            for index, node_id in enumerate(middle, 1):
                fraction = index / (len(middle) + 1)
                self.cross[node_id] = ends[0] + (ends[1] - ends[0]) * fraction

        self._settle(cross_size)

    def _settle(self, cross_size: dict[str, float]) -> None:
        """Re-open any overlap the bands closed, without moving a band.

        Moving a cluster moves its boxes but not the lines threading past them,
        so a waypoint can end up inside a box it used to pass. Each layer is
        put back in its own order and pushed apart just far enough; the first
        node of each layer does not move, so the bands stay where they were.
        """
        cluster_of = {node.id: node.cluster for node in self.figure.nodes}
        for index, layer in enumerate(self.layers):
            ordered = sorted(layer, key=lambda node_id: (self.cross[node_id], node_id))
            self.layers[index] = ordered
            for position in range(1, len(ordered)):
                previous, node_id = ordered[position - 1], ordered[position]
                gap = GAP_DUMMY if node_id in self.dummy or previous in self.dummy else GAP_CROSS
                if cluster_of.get(previous, "") != cluster_of.get(node_id, ""):
                    gap = max(gap, GAP_CROSS)
                floor = (
                    self.cross[previous]
                    + cross_size[previous] / 2
                    + gap
                    + cross_size[node_id] / 2
                )
                self.cross[node_id] = max(self.cross[node_id], floor)

    def _cross_size(self, node_id: str) -> float:
        if node_id in self.dummy:
            return 0.0
        box = self.boxes[node_id]
        return box.width if self.figure.direction == "TB" else box.height

    def _along_size(self, node_id: str) -> float:
        if node_id in self.dummy:
            return 0.0
        box = self.boxes[node_id]
        return box.height if self.figure.direction == "TB" else box.width


def compute(figure: Figure) -> Layout:
    """Lay a figure out. Same figure in, same numbers out, always."""
    graph = _Graph(figure)
    edges = graph.break_cycles()
    graph.assign_layers(edges)
    graph.insert_dummies(edges)
    graph.build_layers()
    graph.order_layers()
    graph.place()

    layout = Layout(figure=figure)
    _position(graph, layout)
    _clusters(graph, layout)
    _routes(graph, layout)
    _spread_labels(layout)
    if figure.flip:
        _normalise(layout)
        _flip(layout, figure.direction == "LR")
    _normalise(layout)
    return layout


def _layer_offsets(graph: _Graph) -> list[float]:
    """Where each layer starts along the reading direction.

    An edge that carries a label needs the gap after its layer opened up, or
    the text lands on a box. Read left to right the label lies *along* the gap,
    so the gap has to be as wide as the text; read top to bottom it lies across
    it, and one line of leading is enough.
    """
    horizontal = graph.figure.direction == "LR"
    labelled: dict[int, float] = {}
    for edge, chain, _ in graph.segments:
        if not edge.label:
            continue
        want = GAP_LAYER_LABELLED
        if horizontal:
            want = max(want, text_width(elide(edge.label, EDGE_LABEL_CHARS), EDGE_LABEL_SIZE) + 22)
        for node_id in chain:
            index = graph.layer[node_id]
            labelled[index] = max(labelled.get(index, 0.0), want)

    # Room above the first layer a cluster reaches, for the cluster's own title.
    titles: dict[int, float] = {}
    for cluster in graph.figure.clusters:
        members = [
            node.id for node in graph.figure.nodes if node.cluster == cluster.id
        ]
        if not members:
            continue
        top = min(graph.layer[member] for member in members)
        titles[top] = CLUSTER_TITLE + CLUSTER_PAD

    offsets: list[float] = []
    running = MARGIN
    for index, layer in enumerate(graph.layers):
        running += titles.get(index, 0.0)
        offsets.append(running)
        extent = max((graph._along_size(node_id) for node_id in layer), default=0.0)
        running += extent + max(GAP_LAYER, labelled.get(index, 0.0))
    return offsets


def _position(graph: _Graph, layout: Layout) -> None:
    offsets = _layer_offsets(graph)
    for index, layer in enumerate(graph.layers):
        extent = max((graph._along_size(node_id) for node_id in layer), default=0.0)
        for node_id in layer:
            if node_id in graph.dummy:
                continue
            box = graph.boxes[node_id]
            along = offsets[index] + (extent - graph._along_size(node_id)) / 2
            cross = graph.cross[node_id]
            if graph.figure.direction == "TB":
                box.x = cross - box.width / 2 + MARGIN
                box.y = along
            else:
                box.x = along
                box.y = cross - box.height / 2 + MARGIN
            layout.boxes.append(box)
    # Dummy points, in the same frame, for the router.
    for index, layer in enumerate(graph.layers):
        extent = max((graph._along_size(node_id) for node_id in layer), default=0.0)
        for node_id in layer:
            if node_id not in graph.dummy:
                continue
            along = offsets[index] + extent / 2
            cross = graph.cross[node_id] + MARGIN
            graph.points[node_id] = (
                (cross, along) if graph.figure.direction == "TB" else (along, cross)
            )


def _clusters(graph: _Graph, layout: Layout) -> None:
    for cluster in graph.figure.clusters:
        boxes = [box for box in layout.boxes if box.node.cluster == cluster.id]
        if not boxes:
            continue
        left = min(box.x for box in boxes) - CLUSTER_PAD
        top = min(box.y for box in boxes) - CLUSTER_PAD - CLUSTER_TITLE
        right = max(box.x + box.width for box in boxes) + CLUSTER_PAD
        bottom = max(box.y + box.height for box in boxes) + CLUSTER_PAD
        width = max(right - left, text_width(cluster.label, SUB_SIZE) + 2 * CLUSTER_PAD)
        layout.clusters.append(
            ClusterBox(
                id=cluster.id,
                label=cluster.label,
                sublabel=cluster.sublabel,
                x=left,
                y=top,
                width=width,
                height=bottom - top,
            )
        )


def _anchor(box: Box, towards: tuple[float, float], direction: str) -> tuple[float, float]:
    """Where a line leaves or meets a box: the middle of the facing side."""
    if direction == "TB":
        if towards[1] >= box.y + box.height:
            return (box.centre_x, box.y + box.height)
        if towards[1] <= box.y:
            return (box.centre_x, box.y)
        return (box.x + box.width if towards[0] > box.centre_x else box.x, box.centre_y)
    if towards[0] >= box.x + box.width:
        return (box.x + box.width, box.centre_y)
    if towards[0] <= box.x:
        return (box.x, box.centre_y)
    return (box.centre_x, box.y + box.height if towards[1] > box.centre_y else box.y)


def _routes(graph: _Graph, layout: Layout) -> None:
    horizontal = graph.figure.direction == "LR"
    for edge, chain, is_reversed in graph.segments:
        if len(chain) == 1:
            layout.routes.append(_self_loop(graph, layout, edge, chain[0], horizontal))
            continue

        points: list[tuple[float, float]] = []
        for node_id in chain[1:-1]:
            points.append(graph.points[node_id])

        first = layout.box(chain[0])
        last = layout.box(chain[-1])
        if first is None or last is None:
            continue

        if graph.layer[chain[0]] == graph.layer[chain[-1]]:
            points = _sideways(first, last, horizontal)
            route = Route(edge=edge, points=points, reversed=is_reversed)
        else:
            head = points[0] if points else (last.centre_x, last.centre_y)
            tail = points[-1] if points else (first.centre_x, first.centre_y)
            start = _anchor(first, head, graph.figure.direction)
            end = _anchor(last, tail, graph.figure.direction)
            route = Route(edge=edge, points=[start, *points, end], reversed=is_reversed)

        if edge.label:
            route.label = elide(edge.label, EDGE_LABEL_CHARS)
            middle = len(route.points) // 2
            if len(route.points) % 2:
                route.label_x, route.label_y = route.points[middle]
            else:
                before, after = route.points[middle - 1], route.points[middle]
                route.label_x = (before[0] + after[0]) / 2
                route.label_y = (before[1] + after[1]) / 2
            _lean(route, horizontal)
        layout.routes.append(route)


def _lean(route: Route, horizontal: bool) -> None:
    """Hang a label off the side its line is going, if the line is going sideways.

    Centred on the line is right for a step that goes straight on. It is wrong
    for the two ways out of a decision, where both labels want the same place
    and the wider one wins; leaning each one outwards puts them either side of
    the fork, which is also where a reader looks for them.
    """
    start, end = route.points[0], route.points[-1]
    drift = (end[0] - start[0]) if not horizontal else (end[1] - start[1])
    if abs(drift) < 24:
        return
    if not horizontal:
        route.label_anchor = "start" if drift > 0 else "end"
        route.label_x += 7 if drift > 0 else -7
    else:
        # Read left to right the label already sits in the gap between two
        # layers; leaning it up or down keeps it off its neighbour's line.
        route.label_y += 9 if drift > 0 else -9


def _sideways(first: Box, last: Box, horizontal: bool) -> list[tuple[float, float]]:
    """Two boxes in one layer: go out the side, along, and back in."""
    if horizontal:
        side = max(first.x + first.width, last.x + last.width) + GAP_CROSS * 0.7
        return [
            (first.x + first.width, first.centre_y),
            (side, first.centre_y),
            (side, last.centre_y),
            (last.x + last.width, last.centre_y),
        ]
    side = max(first.y + first.height, last.y + last.height) + GAP_CROSS * 0.7
    return [
        (first.centre_x, first.y + first.height),
        (first.centre_x, side),
        (last.centre_x, side),
        (last.centre_x, last.y + last.height),
    ]


def _self_loop(
    graph: _Graph, layout: Layout, edge: Edge, node_id: str, horizontal: bool
) -> Route:
    box = layout.box(node_id)
    if box is None:
        return Route(edge=edge, points=[])
    reach = 26.0
    if horizontal:
        points = [
            (box.centre_x, box.y),
            (box.centre_x - reach * 0.6, box.y - reach),
            (box.centre_x + reach * 0.6, box.y - reach),
            (box.centre_x + 6, box.y),
        ]
    else:
        points = [
            (box.x + box.width, box.centre_y - 6),
            (box.x + box.width + reach, box.centre_y - reach * 0.6),
            (box.x + box.width + reach, box.centre_y + reach * 0.6),
            (box.x + box.width, box.centre_y + 6),
        ]
    route = Route(edge=edge, points=points)
    if edge.label:
        route.label = elide(edge.label, EDGE_LABEL_CHARS)
        route.label_x = points[1][0] if horizontal else points[1][0] + 4
        route.label_y = points[1][1] - 4 if horizontal else points[1][1]
    return route


def _spread_labels(layout: Layout) -> None:
    """Move a label off the one it landed on.

    Two guarded branches out of the same `decide` put their labels at the same
    height either side of the diamond, and a long guard then overlaps a short
    one -- which reads as a third condition that is not in the model. The boxes
    count as obstacles too: a label lying across a box is read as belonging to
    it. Each label is nudged along its own edge until it is clear, in placement
    order, so the result is the same on every run.
    """
    horizontal = layout.figure.direction == "LR"
    placed: list[tuple[float, float, float, float]] = [
        (box.x, box.y, box.x + box.width, box.y + box.height) for box in layout.boxes
    ]
    for route in layout.routes:
        if not route.label:
            continue
        for attempt in range(8):
            # Away from where the line put it, alternating sides, a little
            # further each time.
            step = ((attempt + 1) // 2) * 18.0 * (1 if attempt % 2 else -1)
            x = route.label_x + (step if horizontal else 0.0)
            y = route.label_y + (0.0 if horizontal else step)
            if not any(_overlaps(label_box(route, x, y), other) for other in placed):
                route.label_x, route.label_y = x, y
                break
        placed.append(label_box(route, route.label_x, route.label_y))


def label_box(route: Route, x: float, y: float) -> tuple[float, float, float, float]:
    """The rectangle a label occupies, which depends on where it is anchored."""
    width = text_width(route.label, EDGE_LABEL_SIZE) + 8
    if route.label_anchor == "start":
        left = x - 4
    elif route.label_anchor == "end":
        left = x - width + 4
    else:
        left = x - width / 2
    return (left, y - 8, left + width, y + 8)


def _overlaps(left: tuple[float, float, float, float], right: tuple[float, float, float, float]) -> bool:
    return not (
        left[2] <= right[0] or right[2] <= left[0] or left[3] <= right[1] or right[3] <= left[1]
    )


def _flip(layout: Layout, horizontal: bool) -> None:
    """Mirror the layout along the reading direction.

    Used where the edges point one way and the eye expects the other: a
    subtype points at its supertype, and the supertype belongs at the top.
    """
    extent = layout.height if not horizontal else layout.width
    if not extent:
        for box in layout.boxes:
            extent = max(extent, box.y + box.height if not horizontal else box.x + box.width)
    for box in layout.boxes:
        if horizontal:
            box.x = extent - box.x - box.width
        else:
            box.y = extent - box.y - box.height
    for cluster in layout.clusters:
        if horizontal:
            cluster.x = extent - cluster.x - cluster.width
        else:
            cluster.y = extent - cluster.y - cluster.height
    for route in layout.routes:
        if horizontal:
            route.points = [(extent - x, y) for x, y in route.points]
            route.label_x = extent - route.label_x
        else:
            route.points = [(x, extent - y) for x, y in route.points]
            route.label_y = extent - route.label_y


def _normalise(layout: Layout) -> None:
    """Shift everything positive and size the canvas around it."""
    xs: list[float] = []
    ys: list[float] = []
    for box in layout.boxes:
        xs.extend((box.x, box.x + box.width))
        ys.extend((box.y, box.y + box.height))
    for cluster in layout.clusters:
        xs.extend((cluster.x, cluster.x + cluster.width))
        ys.extend((cluster.y, cluster.y + cluster.height))
    for route in layout.routes:
        for x, y in route.points:
            xs.append(x)
            ys.append(y)
        if route.label:
            left, top, right, bottom = label_box(route, route.label_x, route.label_y)
            xs.extend((left, right))
            ys.extend((top, bottom))
    if not xs:
        layout.width = layout.height = 0.0
        return

    shift_x = MARGIN - min(xs)
    shift_y = MARGIN - min(ys)
    for box in layout.boxes:
        box.x += shift_x
        box.y += shift_y
    for cluster in layout.clusters:
        cluster.x += shift_x
        cluster.y += shift_y
    for route in layout.routes:
        route.points = [(x + shift_x, y + shift_y) for x, y in route.points]
        route.label_x += shift_x
        route.label_y += shift_y

    layout.width = round(max(xs) + shift_x + MARGIN, 1)
    layout.height = round(max(ys) + shift_y + MARGIN, 1)
