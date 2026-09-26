"""Draw a laid-out figure as SVG: inline in the page, or as a file of its own.

Two targets, one set of rules. Inline, the diagram inherits the page's palette,
so it answers the reader's OS theme along with everything else. As a file, it
carries the same palette in an embedded `<style>`, because a `.svg` opened on
its own -- from a CI artifact, or from GitHub's blob view -- has no page to
inherit from. Neither fetches anything: no font host, no sprite sheet, no CDN.

Shapes say what kind of element they are, and colour says one thing only: the
lifecycle keyword. That is the rule the rest of the document keeps -- maturity
is the one thing a reader scans for -- and a diagram that also coloured by
subsystem would spend the reader's attention twice.

The marker ids are prefixed with the figure's name because a page inlines a
dozen of these, and two `<marker id="arrow">` in one document is one arrow.
"""

from __future__ import annotations

import html

from . import layout as layout_module
from .figure import Figure
from .layout import Box, Layout, Route

# Rules for a diagram that sits inside the generated page: they use the
# variables the page already defines, so there is one palette, not two.
SVG_RULES = """
figure.diagram{margin:22px 0 26px; padding:0}
figure.diagram svg{display:block; max-width:100%; height:auto;
  background:var(--surface); border:1px solid var(--rule); border-radius:3px}
figure.diagram figcaption{margin-top:9px; font-size:13px; color:var(--muted); max-width:74ch}
figure.diagram figcaption .figname{color:var(--ink-2); font-weight:600}
.dg-text{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace; fill:var(--ink)}
.dg-sub{fill:var(--muted)}
.dg-stereo{fill:var(--muted); letter-spacing:.02em}
.dg-row{fill:var(--ink-2)}
.dg-shape{fill:var(--surface-2); stroke:var(--rule-strong); stroke-width:1.1}
.dg-terminal{fill:var(--sunk); stroke:var(--rule-strong)}
.dg-note{fill:var(--surface-2); stroke:var(--rule); stroke-dasharray:3 2}
.dg-rule{stroke:var(--rule-strong); stroke-width:1}
.dg-cluster{fill:none; stroke:var(--rule); stroke-width:1; stroke-dasharray:4 3}
.dg-cluster-label{fill:var(--muted); font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
.dg-edge{fill:none; stroke:var(--rule-strong); stroke-width:1.4}
.dg-edge-trace{stroke-dasharray:5 4}
.dg-edge-label{fill:var(--ink-2); font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
.dg-edge-label-bg{fill:var(--surface); stroke:none}
.dg-head{fill:var(--rule-strong); stroke:none}
.dg-head-hollow{fill:var(--surface); stroke:var(--rule-strong); stroke-width:1.1}
.dg-legend{fill:var(--muted); font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
.dg-mat-implemented{fill:var(--implemented-bg); stroke:var(--implemented)}
.dg-mat-inProgress{fill:var(--inProgress-bg); stroke:var(--inProgress)}
.dg-mat-writtenAhead{fill:var(--writtenAhead-bg); stroke:var(--writtenAhead)}
.dg-mat-planned{fill:var(--planned-bg); stroke:var(--planned)}
.dg-mat-deferred{fill:var(--deferred-bg); stroke:var(--deferred)}
a.dg-link{text-decoration:none}
"""

# The same palette the page defines, for a file that has no page. Kept in one
# string so the two cannot drift: the page's own values live in render_html.
STANDALONE_VARS = """
svg{
  --surface:#fff; --surface-2:#f6f7f8; --sunk:#e4e7ea;
  --ink:#16191d; --ink-2:#3b434c; --muted:#6a737e;
  --rule:#d2d7dc; --rule-strong:#b3bbc3;
  --implemented:#2c6e4e; --implemented-bg:#dceae2;
  --inProgress:#2a5f8f; --inProgress-bg:#dae5f0;
  --writtenAhead:#6b4fa0; --writtenAhead-bg:#e5dff0;
  --planned:#6a737e; --planned-bg:#e4e7ea;
  --deferred:#87600b; --deferred-bg:#f3e8ce;
}
@media (prefers-color-scheme:dark){svg{
  --surface:#1a1e22; --surface-2:#21262b; --sunk:#0e1113;
  --ink:#e7eaed; --ink-2:#bfc6cd; --muted:#8d97a1;
  --rule:#2c3238; --rule-strong:#5a646e;
  --implemented:#6abf92; --implemented-bg:#16301f;
  --inProgress:#74acdb; --inProgress-bg:#122639;
  --writtenAhead:#a98fd8; --writtenAhead-bg:#241c38;
  --planned:#8d97a1; --planned-bg:#262b31;
  --deferred:#d9a83e; --deferred-bg:#2e2511;
}}
"""

LEGEND_HEIGHT = 30.0
LEGEND_SWATCH = 11.0


def _esc(text: str) -> str:
    return html.escape(text, quote=True)


def _number(value: float) -> str:
    """Coordinates rounded to a tenth, with no trailing `.0`.

    The file is committed and diffed, so a coordinate that carried fifteen
    digits would make every layout change unreadable.
    """
    rounded = round(value, 1)
    if rounded == int(rounded):
        return str(int(rounded))
    return f"{rounded:g}"


def _path(points: list[tuple[float, float]], radius: float = 9.0) -> str:
    """A polyline with its corners rounded, which reads as one line.

    A bend is where an edge passes a layer, and a square bend on a diagram of
    eighty edges looks like a circuit board. The radius shrinks rather than
    overshooting when a segment is short.
    """
    if len(points) < 2:
        return ""
    out = [f"M{_number(points[0][0])},{_number(points[0][1])}"]
    for index in range(1, len(points) - 1):
        previous, current, following = points[index - 1], points[index], points[index + 1]
        before = _shorten(current, previous, radius)
        after = _shorten(current, following, radius)
        out.append(f"L{_number(before[0])},{_number(before[1])}")
        out.append(
            f"Q{_number(current[0])},{_number(current[1])} "
            f"{_number(after[0])},{_number(after[1])}"
        )
    last = points[-1]
    out.append(f"L{_number(last[0])},{_number(last[1])}")
    return " ".join(out)


def _shorten(
    point: tuple[float, float], towards: tuple[float, float], radius: float
) -> tuple[float, float]:
    dx, dy = towards[0] - point[0], towards[1] - point[1]
    distance = (dx * dx + dy * dy) ** 0.5
    if distance < 0.01:
        return point
    step = min(radius, distance / 2)
    return (point[0] + dx / distance * step, point[1] + dy / distance * step)


# The arrow each relation ends in. SysML draws these distinctly because they
# mean different things, and a reader who knows UML reads them without a key.
HEADS = {
    "flow": "arrow",
    "transition": "arrow",
    "connect": "none",
    "composition": "none",
    "specialization": "hollow",
    "dependency": "open",
    "satisfy": "open",
    "verify": "open",
    "allocate": "open",
}
TAILS = {"composition": "diamond"}


def _markers(prefix: str) -> str:
    return "".join(
        (
            f'<marker id="{prefix}-arrow" markerWidth="9" markerHeight="7" refX="8.5" refY="3.5" '
            'orient="auto" markerUnits="userSpaceOnUse">'
            '<path class="dg-head" d="M0,0 L9,3.5 L0,7 L1.6,3.5 z"/></marker>',
            f'<marker id="{prefix}-open" markerWidth="10" markerHeight="8" refX="9" refY="4" '
            'orient="auto" markerUnits="userSpaceOnUse">'
            '<path class="dg-edge" d="M0.5,0.5 L9,4 L0.5,7.5"/></marker>',
            f'<marker id="{prefix}-hollow" markerWidth="12" markerHeight="10" refX="11" refY="5" '
            'orient="auto" markerUnits="userSpaceOnUse">'
            '<path class="dg-head-hollow" d="M0.5,0.5 L11,5 L0.5,9.5 z"/></marker>',
            f'<marker id="{prefix}-diamond" markerWidth="14" markerHeight="9" refX="0.5" refY="4.5" '
            'orient="auto" markerUnits="userSpaceOnUse">'
            '<path class="dg-head" d="M0.5,4.5 L7,0.5 L13.5,4.5 L7,8.5 z"/></marker>',
        )
    )


def render(
    figure: Figure,
    standalone: bool = False,
    computed: Layout | None = None,
) -> str:
    """One figure as SVG. `standalone` adds the palette and the `xmlns`."""
    placed = computed if computed is not None else layout_module.compute(figure)
    legend = figure.maturities()
    height = placed.height + (LEGEND_HEIGHT if legend else 0.0)
    prefix = f"dg-{figure.name}"

    out: list[str] = []
    attributes = [
        f'viewBox="0 0 {_number(placed.width)} {_number(height)}"',
        f'width="{_number(placed.width)}"',
        f'height="{_number(height)}"',
        'role="img"',
        f'aria-label="{_esc(figure.alt_text())}"',
    ]
    if standalone:
        attributes.insert(0, 'xmlns="http://www.w3.org/2000/svg"')
        attributes.insert(1, 'xmlns:xlink="http://www.w3.org/1999/xlink"')
    out.append(f"<svg {' '.join(attributes)}>")
    out.append(f"<title>{_esc(figure.title)}</title>")
    if standalone:
        out.append(f"<style>{STANDALONE_VARS}{SVG_RULES}</style>")
        out.append(
            f'<rect width="{_number(placed.width)}" height="{_number(height)}" '
            'fill="var(--surface)"/>'
        )
    out.append(f"<defs>{_markers(prefix)}</defs>")

    for cluster in placed.clusters:
        out.append(
            f'<rect class="dg-cluster" x="{_number(cluster.x)}" y="{_number(cluster.y)}" '
            f'width="{_number(cluster.width)}" height="{_number(cluster.height)}" rx="4"/>'
        )
        label = cluster.label + (f" — {cluster.sublabel}" if cluster.sublabel else "")
        out.append(
            f'<text class="dg-cluster-label" x="{_number(cluster.x + 9)}" '
            f'y="{_number(cluster.y + 13)}" font-size="10.5">{_esc(label)}</text>'
        )

    for route in placed.routes:
        out.append(_route(route, prefix))
    for box in placed.boxes:
        out.append(_box(box))
    for route in placed.routes:
        out.append(_route_label(route))
    if legend:
        out.append(_legend(legend, placed.height, placed.width))

    out.append("</svg>")
    return "".join(out)


def _route(route: Route, prefix: str) -> str:
    if len(route.points) < 2:
        return ""
    points = list(route.points)
    kind = route.edge.kind
    head = HEADS.get(kind, "arrow")
    tail = TAILS.get(kind, "none")
    classes = "dg-edge" + (" dg-edge-trace" if route.edge.is_trace else "")
    attributes = [f'class="{classes}"', f'd="{_path(points)}"']
    if head != "none":
        attributes.append(f'marker-end="url(#{prefix}-{head})"')
    if tail != "none":
        attributes.append(f'marker-start="url(#{prefix}-{tail})"')
    return f"<path {' '.join(attributes)}/>"


def _route_label(route: Route) -> str:
    """The label, on a patch of background so the line does not cross the text."""
    if not route.label:
        return ""
    left, top, right, bottom = layout_module.label_box(route, route.label_x, route.label_y)
    return (
        f'<rect class="dg-edge-label-bg" x="{_number(left)}" y="{_number(top + 1)}" '
        f'width="{_number(right - left)}" height="{_number(bottom - top - 2)}" rx="2"/>'
        f'<text class="dg-text dg-edge-label" x="{_number(route.label_x)}" '
        f'y="{_number(route.label_y + 3.4)}" text-anchor="{route.label_anchor}" '
        f'font-size="{layout_module.EDGE_LABEL_SIZE:g}">{_esc(route.label)}</text>'
    )


def _shape(box: Box) -> str:
    node = box.node
    classes = "dg-shape"
    if node.maturity:
        classes = f"dg-shape dg-mat-{node.maturity}"
    if node.kind == "terminal":
        radius = box.height / 2
        classes = classes.replace("dg-shape", "dg-shape dg-terminal")
        return (
            f'<rect class="{classes}" x="{_number(box.x)}" y="{_number(box.y)}" '
            f'width="{_number(box.width)}" height="{_number(box.height)}" '
            f'rx="{_number(radius)}"/>'
        )
    if node.kind == "choice":
        points = " ".join(
            f"{_number(px)},{_number(py)}"
            for px, py in (
                (box.centre_x, box.y),
                (box.x + box.width, box.centre_y),
                (box.centre_x, box.y + box.height),
                (box.x, box.centre_y),
            )
        )
        return f'<polygon class="{classes}" points="{points}"/>'
    if node.kind == "note":
        fold = 11.0
        path = (
            f"M{_number(box.x)},{_number(box.y)} "
            f"H{_number(box.x + box.width - fold)} "
            f"L{_number(box.x + box.width)},{_number(box.y + fold)} "
            f"V{_number(box.y + box.height)} H{_number(box.x)} z"
        )
        return f'<path class="{classes} dg-note" d="{path}"/>'
    radius = 10 if node.kind == "state" else 4
    return (
        f'<rect class="{classes}" x="{_number(box.x)}" y="{_number(box.y)}" '
        f'width="{_number(box.width)}" height="{_number(box.height)}" rx="{radius}"/>'
    )


def _box(box: Box) -> str:
    node = box.node
    out = [_shape(box)]

    cursor = box.y + layout_module.PAD_Y
    if box.stereotype:
        cursor += layout_module.SUB_LINE
        out.append(
            f'<text class="dg-text dg-stereo" x="{_number(box.centre_x)}" '
            f'y="{_number(cursor - 4)}" text-anchor="middle" '
            f'font-size="{layout_module.STEREOTYPE_SIZE:g}">{_esc(box.stereotype)}</text>'
        )
    for line in box.lines:
        cursor += layout_module.LABEL_LINE
        out.append(
            f'<text class="dg-text" x="{_number(box.centre_x)}" y="{_number(cursor - 4)}" '
            f'text-anchor="middle" font-size="{layout_module.LABEL_SIZE:g}">{_esc(line)}</text>'
        )
    if node.sublabel:
        cursor += layout_module.SUB_LINE
        out.append(
            f'<text class="dg-text dg-sub" x="{_number(box.centre_x)}" '
            f'y="{_number(cursor - 4)}" text-anchor="middle" '
            f'font-size="{layout_module.SUB_SIZE:g}">'
            f"{_esc(layout_module.elide(node.sublabel, 38))}</text>"
        )
    if box.rows:
        cursor += 5
        out.append(
            f'<line class="dg-rule" x1="{_number(box.x)}" y1="{_number(cursor)}" '
            f'x2="{_number(box.x + box.width)}" y2="{_number(cursor)}"/>'
        )
        for row in box.rows:
            cursor += layout_module.ROW_LINE
            out.append(
                f'<text class="dg-text dg-row" x="{_number(box.x + layout_module.PAD_X)}" '
                f'y="{_number(cursor - 4)}" font-size="{layout_module.ROW_SIZE:g}">'
                f"{_esc(row)}</text>"
            )

    body = "".join(out)
    if node.doc:
        body += f"<title>{_esc(_first_sentence(node.doc))}</title>"
    if node.href:
        return f'<a class="dg-link" href="{_esc(node.href)}">{body}</a>'
    return f"<g>{body}</g>"


def _first_sentence(text: str, limit: int = 220) -> str:
    flat = " ".join(text.split())
    for index, char in enumerate(flat):
        if char == "." and index + 1 < len(flat) and flat[index + 1] == " ":
            return flat[: index + 1]
    return flat if len(flat) <= limit else flat[: limit - 1] + "…"


def _legend(maturities: list[str], top: float, width: float) -> str:
    """A strip naming the colours the figure used, and nothing else.

    Without it a colour is a decoration. With it the diagram is readable on its
    own, which matters because these files are also read outside the document.
    """
    out: list[str] = []
    x = layout_module.MARGIN
    y = top + 8
    labels = {
        "implemented": "implemented",
        "inProgress": "in progress",
        "writtenAhead": "written ahead",
        "planned": "planned",
        "deferred": "deferred",
    }
    for maturity in maturities:
        out.append(
            f'<rect class="dg-shape dg-mat-{maturity}" x="{_number(x)}" y="{_number(y)}" '
            f'width="{_number(LEGEND_SWATCH)}" height="{_number(LEGEND_SWATCH)}" rx="2"/>'
        )
        x += LEGEND_SWATCH + 5
        text = labels.get(maturity, maturity)
        out.append(
            f'<text class="dg-text dg-legend" x="{_number(x)}" '
            f'y="{_number(y + LEGEND_SWATCH - 1.5)}" font-size="10">{_esc(text)}</text>'
        )
        x += layout_module.text_width(text, 10) + 16
        if x > width - 60:
            break
    return "".join(out)
