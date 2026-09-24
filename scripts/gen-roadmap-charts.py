#!/usr/bin/env python3
"""Draw the roadmap's burndown and Gantt charts as SVG.

    python3 scripts/gen-roadmap-charts.py

writes docs/img/burndown.svg and docs/img/gantt.svg. It uses the standard
library only, so it runs wherever the other gen-* scripts do. The numbers
are the ones docs/ROADMAP.md's status table and docs/BACKLOG.md's
*Velocity* give; change them here when those change, and rerun.
"""

import math
from datetime import date, timedelta
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "docs" / "img"

TODAY = date(2026, 9, 24)

# Points landed per day (docs/BACKLOG.md, *Velocity*). 09-18 to 09-23 were
# sized afterwards from `git log`; 09-23 is what is left of that backfill.
LANDED = [
    (date(2026, 9, 14), 131),
    (date(2026, 9, 15), 34),
    (date(2026, 9, 16), 66),
    (date(2026, 9, 17), 214),
    (date(2026, 9, 18), 50),
    (date(2026, 9, 19), 50),
    (date(2026, 9, 20), 55),
    (date(2026, 9, 21), 96),
    (date(2026, 9, 22), 20),
    (date(2026, 9, 23), 53),
    (date(2026, 9, 24), 99),
]
BACKFILLED = {date(2026, 9, d) for d in range(18, 24)}

# The status table's sized, unfinished rows after 2026-09-24's landings,
# in the table's order.
REMAINING = [
    ("Client pages as texture backing", 8),
    ("XWayland and the second pass", 48),
    ("GC400, the rest", 21),
    ("Stage 13, the controllers", 58),
    ("Stage 15, the init's rest", 51),
    ("Chrome on the DK1", 50),
    ("Stage 14, real-time", 40),
    ("dmabuf and virgl", 48),
    ("Stage 22, Steam's sized part", 51),
    ("Stage 22, the rest (guess)", 100),
]
SCOPE = sum(p for _, p in REMAINING)
RATES = [(79, "79 a day, the running average"), (54, "54 a day, as 09-18 to 09-23")]
FORECAST_RATE = 54

D = date
DONE = [
    ("Stages 7 to 11, ring-3 disk", D(2026, 9, 13), D(2026, 9, 14)),
    ("Networking (50)", D(2026, 9, 15), D(2026, 9, 16)),
    ("Stages 17 and 18 (170)", D(2026, 9, 16), D(2026, 9, 17)),
    ("GPU path A (52)", D(2026, 9, 18), D(2026, 9, 19)),
    ("Dynamic linking, Arm port (73)", D(2026, 9, 20), D(2026, 9, 23)),
    ("Stage 12, btrfs write (60)", D(2026, 9, 21), D(2026, 9, 21)),
    ("Stage 16, rustc", D(2026, 9, 22), D(2026, 9, 22)),
    ("Cursor plane (13)", D(2026, 9, 23), D(2026, 9, 23)),
    ("sysfs, device queue, Chrome", D(2026, 9, 24), D(2026, 9, 24)),
]
ACTIVE = [
    ("Stage 19, the rest (56 left)", D(2026, 9, 17)),
    ("Stage 20, self-hosting", D(2026, 9, 22)),
    ("Stage 13 cgroups (27 of 85)", D(2026, 9, 23)),
    ("Gears (50 of 71)", D(2026, 9, 24)),
    ("Stage 15 init (16 of 67)", D(2026, 9, 24)),
]

FONT = "system-ui, -apple-system, 'Segoe UI', Helvetica, Arial, sans-serif"
INK = "#1f2328"
MUTED = "#656d76"
GRID = "#d8dee4"
DONE_C = "#8c959f"
ACTIVE_C = "#0969da"
FORECAST_C = "#54aeff"
LINE_C = ["#0969da", "#bf3989"]
TODAY_C = "#cf222e"


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


class Svg:
    def __init__(self, w, h, title):
        self.w, self.h = w, h
        self.parts = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" '
            f'viewBox="0 0 {w} {h}" font-family="{FONT}" font-size="12" fill="{INK}">',
            f"<title>{esc(title)}</title>",
            # An opaque background, so the chart reads on a dark page too.
            f'<rect width="{w}" height="{h}" fill="#ffffff"/>',
        ]

    def add(self, s):
        self.parts.append(s)

    def text(self, x, y, s, anchor="start", size=12, fill=INK, weight="normal"):
        self.add(
            f'<text x="{x:.1f}" y="{y:.1f}" text-anchor="{anchor}" font-size="{size}" '
            f'fill="{fill}" font-weight="{weight}">{esc(s)}</text>'
        )

    def line(self, x1, y1, x2, y2, stroke, width=1, dash=None):
        d = f' stroke-dasharray="{dash}"' if dash else ""
        self.add(
            f'<line x1="{x1:.1f}" y1="{y1:.1f}" x2="{x2:.1f}" y2="{y2:.1f}" '
            f'stroke="{stroke}" stroke-width="{width}"{d}/>'
        )

    def rect(self, x, y, w, h, fill, stroke=None, dash=None, rx=2):
        s = f' stroke="{stroke}" stroke-width="1"' if stroke else ""
        d = f' stroke-dasharray="{dash}"' if dash else ""
        self.add(
            f'<rect x="{x:.1f}" y="{y:.1f}" width="{w:.1f}" height="{h:.1f}" '
            f'rx="{rx}" fill="{fill}"{s}{d}/>'
        )

    def polyline(self, pts, stroke, width=2, dash=None):
        p = " ".join(f"{x:.1f},{y:.1f}" for x, y in pts)
        d = f' stroke-dasharray="{dash}"' if dash else ""
        self.add(
            f'<polyline points="{p}" fill="none" stroke="{stroke}" '
            f'stroke-width="{width}" stroke-linejoin="round"{d}/>'
        )

    def dot(self, x, y, fill, r=3):
        self.add(f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{r}" fill="{fill}"/>')

    def write(self, path):
        path.parent.mkdir(parents=True, exist_ok=True)
        with open(path, "w", encoding="utf-8", newline="\n") as f:
            f.write("\n".join(self.parts + ["</svg>"]) + "\n")


def label(d):
    return d.strftime("%m-%d")


def burndown():
    w, h = 880, 640
    svg = Svg(w, h, "Ferrix burndown")
    left, right = 70, w - 70

    # Panel 1: sized points remaining, forecast from today.
    top, bottom = 60, 290
    svg.text(left, 28, f"Sized points remaining at the end of each day, from {label(TODAY)}",
             size=16, weight="bold")
    svg.text(left, 46, f"{SCOPE} points in the status table's sized, unfinished rows; "
             "unsized stages (20, 21, Chrome's zygote) are outside it",
             size=12, fill=MUTED)
    days = 10
    ymax = 500
    x = lambda i: left + (right - left) * i / (days - 1)
    y = lambda v: bottom - (bottom - top) * v / ymax
    for v in range(0, ymax + 1, 100):
        svg.line(left, y(v), right, y(v), GRID)
        svg.text(left - 8, y(v) + 4, str(v), anchor="end", fill=MUTED)
    for i in range(days):
        svg.text(x(i), bottom + 18, label(TODAY + timedelta(days=i)),
                 anchor="middle", fill=MUTED)
    svg.text(18, (top + bottom) / 2, "points", fill=MUTED,
             anchor="middle")
    for (rate, name), colour, ly in zip(RATES, LINE_C, (top + 8, top + 26)):
        # Whole days while work is left, then the day it runs out, exactly.
        pts = [(x(i), y(SCOPE - rate * i)) for i in range(days) if SCOPE - rate * i > 0]
        pts.append((x(SCOPE / rate), y(0)))
        svg.polyline(pts, colour, width=2.5)
        for px, py in pts:
            svg.dot(px, py, colour)
        # A tick is the end of its day, so the work runs out during the day
        # after the last tick it has passed.
        end = TODAY + timedelta(days=math.ceil(SCOPE / rate))
        svg.line(right - 290, ly, right - 266, ly, colour, width=2.5)
        svg.text(right - 258, ly + 4, f"{name}: done {label(end)}")

    # Panel 2: what has landed, per day and in total.
    top, bottom = 390, 590
    svg.text(left, 350, "Points landed per day, and the running total", size=16,
             weight="bold")
    svg.text(left, 368, "hatched days were sized afterwards from git log, "
             "not reported by a session", size=12, fill=MUTED)
    svg.add('<defs><pattern id="hatch" width="6" height="6" '
            'patternUnits="userSpaceOnUse" patternTransform="rotate(45)">'
            f'<rect width="6" height="6" fill="{FORECAST_C}"/>'
            '<line x1="0" y1="0" x2="0" y2="6" stroke="#ffffff" stroke-width="2"/>'
            '</pattern></defs>')
    n = len(LANDED)
    slot = (right - left) / n
    bmax, cmax = 250, 1000
    yb = lambda v: bottom - (bottom - top) * v / bmax
    yc = lambda v: bottom - (bottom - top) * v / cmax
    for v in range(0, bmax + 1, 50):
        svg.line(left, yb(v), right, yb(v), GRID)
        svg.text(left - 8, yb(v) + 4, str(v), anchor="end", fill=MUTED)
    for v in range(0, cmax + 1, 200):
        svg.text(right + 8, yc(v) + 4, str(v), fill=LINE_C[1])
    svg.text(18, (top + bottom) / 2, "a day", fill=MUTED, anchor="middle")
    svg.text(w - 18, (top + bottom) / 2, "total", fill=LINE_C[1], anchor="middle")
    total, pts = 0, []
    for i, (d, p) in enumerate(LANDED):
        cx = left + slot * (i + 0.5)
        fill = "url(#hatch)" if d in BACKFILLED else ACTIVE_C
        svg.rect(cx - slot * 0.32, yb(p), slot * 0.64, yb(0) - yb(p), fill)
        svg.text(cx, yb(p) - 5, str(p), anchor="middle", size=11)
        svg.text(cx, bottom + 18, label(d), anchor="middle", fill=MUTED)
        total += p
        pts.append((cx, yc(total)))
    svg.polyline(pts, LINE_C[1], width=2.5)
    for px, py in pts:
        svg.dot(px, py, LINE_C[1])
    svg.text(pts[-1][0] - 8, pts[-1][1] - 8, f"≈ {total}", anchor="end",
             fill=LINE_C[1], weight="bold")
    svg.write(OUT / "burndown.svg")


def gantt():
    # One queue at FORECAST_RATE, in the status table's order.
    forecast, t = [], 0.0
    for name, pts in REMAINING:
        d = pts / FORECAST_RATE
        forecast.append((f"{name} ({pts})", t, t + d))
        t += d
    start_day = date(2026, 9, 13)
    queue_start = TODAY + timedelta(days=1)
    end_day = queue_start + timedelta(days=int(t) + 1)
    span = (end_day - start_day).days

    rows = len(DONE) + len(ACTIVE) + len(forecast)
    row_h, sec_h = 22, 30
    w = 980
    left, right = 250, w - 60
    top = 70
    h = top + rows * row_h + 3 * sec_h + 50
    svg = Svg(w, h, "Ferrix Gantt")
    svg.text(24, 28, "Ferrix: done, in progress, and a forecast", size=16,
             weight="bold")
    svg.text(24, 46, f"The forecast is one queue at {FORECAST_RATE} points a day in "
             "the status table's order: the size of the work, not a plan",
             fill=MUTED)
    x = lambda days: left + (right - left) * days / span
    bottom = h - 40
    for i in range(span + 1):
        d = start_day + timedelta(days=i)
        svg.line(x(i), top, x(i), bottom, GRID if i % 7 else "#afb8c1")
        if i % 2 == 0 and i < span:
            svg.text(x(i) + 2, bottom + 16, label(d), fill=MUTED, size=11)
    tx = x((queue_start - start_day).days)
    svg.line(tx, top - 6, tx, bottom, TODAY_C, width=1.5, dash="4 3")
    svg.text(tx, top - 10, f"today, {label(TODAY)}", anchor="middle", fill=TODAY_C,
             weight="bold")

    yy = top

    def section(title):
        nonlocal yy
        yy += sec_h
        svg.text(24, yy - 9, title, weight="bold", size=13)

    def bar(name, a, b, fill, stroke=None, dash=None):
        nonlocal yy
        svg.text(left - 10, yy + row_h / 2 + 4, name, anchor="end")
        svg.rect(x(a), yy + 4, max(x(b) - x(a), 3), row_h - 8, fill, stroke, dash)
        yy += row_h

    day = lambda d: (d - start_day).days
    section("Done")
    for name, a, b in DONE:
        bar(name, day(a), day(b) + 1, DONE_C)
    section("In progress")
    for name, a in ACTIVE:
        bar(name, day(a), day(queue_start), ACTIVE_C)
    section(f"Forecast, {FORECAST_RATE} a day, one queue")
    q = day(queue_start)
    for name, a, b in forecast:
        bar(name, q + a, q + b, FORECAST_C, ACTIVE_C, "3 2")
    svg.text(x(q + t) + 6, yy - row_h / 2 + 4,
             label(queue_start + timedelta(days=t)), fill=ACTIVE_C, weight="bold")
    svg.write(OUT / "gantt.svg")


if __name__ == "__main__":
    burndown()
    gantt()
    print(f"wrote {OUT / 'burndown.svg'} and {OUT / 'gantt.svg'}")
