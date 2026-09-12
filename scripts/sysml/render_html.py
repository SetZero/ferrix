"""Render a `document.Doc` as one self-contained HTML page.

Self-contained means what it says: no stylesheet to fetch, no script, no font
host, no CDN. The page has to open from a file:// path on a machine with no
network, because that is how a CI artifact gets looked at.

The styling is deliberately plain -- a reading column, a sticky table of
contents, tabular figures, and a palette that answers the reader's OS theme.
The maturity keywords get the only colour on the page, because they are the
one thing a reader scans for.
"""

from __future__ import annotations

import html

from . import render_svg
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

STYLE = """
:root{
  --ground:#eef0f2; --surface:#fff; --surface-2:#f6f7f8; --sunk:#e4e7ea;
  --ink:#16191d; --ink-2:#3b434c; --muted:#6a737e;
  --rule:#d2d7dc; --rule-strong:#b3bbc3; --accent:#a83c22;
  --implemented:#2c6e4e; --implemented-bg:#dceae2;
  --inProgress:#2a5f8f; --inProgress-bg:#dae5f0;
  --writtenAhead:#6b4fa0; --writtenAhead-bg:#e5dff0;
  --planned:#6a737e; --planned-bg:#e4e7ea;
  --deferred:#87600b; --deferred-bg:#f3e8ce;
}
@media (prefers-color-scheme:dark){:root:not([data-theme=light]){
  --ground:#131619; --surface:#1a1e22; --surface-2:#21262b; --sunk:#0e1113;
  --ink:#e7eaed; --ink-2:#bfc6cd; --muted:#8d97a1;
  --rule:#2c3238; --rule-strong:#3f4750; --accent:#e2734f;
  --implemented:#6abf92; --implemented-bg:#16301f;
  --inProgress:#74acdb; --inProgress-bg:#122639;
  --writtenAhead:#a98fd8; --writtenAhead-bg:#241c38;
  --planned:#8d97a1; --planned-bg:#262b31;
  --deferred:#d9a83e; --deferred-bg:#2e2511;
}}
:root[data-theme=dark]{
  --ground:#131619; --surface:#1a1e22; --surface-2:#21262b; --sunk:#0e1113;
  --ink:#e7eaed; --ink-2:#bfc6cd; --muted:#8d97a1;
  --rule:#2c3238; --rule-strong:#3f4750; --accent:#e2734f;
  --implemented:#6abf92; --implemented-bg:#16301f;
  --inProgress:#74acdb; --inProgress-bg:#122639;
  --writtenAhead:#a98fd8; --writtenAhead-bg:#241c38;
  --planned:#8d97a1; --planned-bg:#262b31;
  --deferred:#d9a83e; --deferred-bg:#2e2511;
}
*{box-sizing:border-box}
html{color-scheme:light dark}
body{
  margin:0; background:var(--ground); color:var(--ink);
  font:16px/1.6 Charter,"Bitstream Charter","Iowan Old Style",Georgia,serif;
  -webkit-text-size-adjust:100%;
}
.page{max-width:1180px; margin:0 auto; padding-inline:20px; padding-block:0 72px}
header.masthead{padding-block:48px 26px; border-bottom:2px solid var(--ink)}
h1{font:700 clamp(30px,5vw,50px)/1.05 system-ui,-apple-system,"Segoe UI",sans-serif;
   letter-spacing:-.022em; margin:0; text-wrap:balance}
.subtitle{color:var(--ink-2); font-size:18px; margin:14px 0 0; max-width:60ch}
.banner{margin:20px 0 0; padding:12px 15px; background:var(--surface);
  border:1px solid var(--rule); border-left:3px solid var(--accent); border-radius:3px;
  font:400 13px/1.55 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace; color:var(--ink-2)}
.shell{display:block}
@media (min-width:1000px){.shell{display:grid; grid-template-columns:224px minmax(0,1fr); gap:48px; align-items:start}}
nav.toc{display:none}
@media (min-width:1000px){nav.toc{display:block; position:sticky; top:20px; padding-top:40px;
  max-height:calc(100vh - 40px); overflow-y:auto}}
nav.toc ol{list-style:none; margin:0; padding:0}
nav.toc li{margin:0 0 1px}
nav.toc li.sub{padding-left:14px}
nav.toc a{display:block; padding:4px 6px 4px 0; text-decoration:none; color:var(--muted);
  font:500 13px/1.34 system-ui,-apple-system,"Segoe UI",sans-serif}
nav.toc a:hover{color:var(--accent)}
main{min-width:0}
h2{font:700 clamp(23px,3vw,31px)/1.12 system-ui,-apple-system,"Segoe UI",sans-serif;
   letter-spacing:-.018em; margin:46px 0 14px; padding-top:12px;
   border-top:1px solid var(--rule); text-wrap:balance}
h2:first-of-type{border-top:0}
h3{font:600 19px/1.25 system-ui,-apple-system,"Segoe UI",sans-serif; letter-spacing:-.01em;
   margin:30px 0 10px; text-wrap:balance}
h4{font:600 15px/1.3 system-ui,-apple-system,"Segoe UI",sans-serif; margin:24px 0 8px; color:var(--ink-2)}
p{margin:0 0 13px; max-width:74ch}
a{color:var(--accent); text-underline-offset:2px}
code{font:400 .86em/1.45 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
  background:var(--sunk); padding:.1em .34em; border-radius:2px; overflow-wrap:anywhere}
pre{background:var(--surface); border:1px solid var(--rule); border-radius:3px;
  padding:14px 16px; overflow-x:auto; margin:16px 0}
pre code{background:none; padding:0; font-size:12.5px; line-height:1.55}
ul,ol{margin:0 0 13px; padding-left:22px; max-width:74ch}
li{margin-bottom:6px}
ol.steps{padding-left:0; list-style:none; counter-reset:step; max-width:74ch}
ol.steps li{counter-increment:step; display:grid; grid-template-columns:30px 1fr; gap:10px;
  padding:7px 0; border-bottom:1px solid var(--rule)}
ol.steps li::before{content:counter(step); font:500 12px/1.7 ui-monospace,monospace; color:var(--muted);
  font-variant-numeric:tabular-nums}
ol.steps li:last-child{border-bottom:0}
dl.defs{margin:0 0 13px; max-width:74ch}
dl.defs div{padding:8px 0; border-bottom:1px solid var(--rule)}
dl.defs div:last-child{border-bottom:0}
dl.defs dt{font-weight:600; margin:0 0 3px}
dl.defs dd{margin:0; color:var(--ink-2); font-size:15px}
.note{background:var(--surface); border:1px solid var(--rule); border-left:3px solid var(--accent);
  border-radius:3px; padding:13px 16px; margin:18px 0; max-width:74ch}
.note b{font-family:system-ui,-apple-system,"Segoe UI",sans-serif}
.tablewrap{overflow-x:auto; margin:16px 0; border:1px solid var(--rule); border-radius:3px;
  background:var(--surface)}
table{border-collapse:collapse; width:100%; font-size:14px; min-width:480px}
th,td{text-align:left; padding:8px 12px; border-bottom:1px solid var(--rule); vertical-align:top}
th{font:500 11px/1.4 ui-monospace,SFMono-Regular,Menlo,monospace; letter-spacing:.07em;
  text-transform:uppercase; color:var(--muted); background:var(--surface-2);
  border-bottom:1px solid var(--rule-strong); white-space:nowrap}
td.right,th.right{text-align:right; font-variant-numeric:tabular-nums; white-space:nowrap}
/* A qualified name or a path must not break mid-token to please a narrow
   column; the wrapper scrolls instead. */
td code{overflow-wrap:normal; word-break:keep-all}
tbody tr:last-child td{border-bottom:0}
caption{caption-side:bottom; text-align:left; font-size:13px; color:var(--muted);
  padding:9px 12px; border-top:1px solid var(--rule)}
.mat{display:inline-block; padding:1px 6px; border-radius:2px; white-space:nowrap;
  font:500 11px/1.6 ui-monospace,SFMono-Regular,Menlo,monospace}
.mat-implemented{background:var(--implemented-bg); color:var(--implemented)}
.mat-inProgress{background:var(--inProgress-bg); color:var(--inProgress)}
.mat-writtenAhead{background:var(--writtenAhead-bg); color:var(--writtenAhead)}
.mat-planned{background:var(--planned-bg); color:var(--planned)}
.mat-deferred{background:var(--deferred-bg); color:var(--deferred)}
:focus-visible{outline:2px solid var(--accent); outline-offset:2px}
@media print{nav.toc{display:none} body{background:#fff}}
"""

# The diagrams draw with the variables declared above, so a page in dark mode
# has dark diagrams without either side knowing about the other.
STYLE += render_svg.SVG_RULES


def _esc(text: str) -> str:
    return html.escape(text, quote=True)


def inline(content) -> str:
    out: list[str] = []
    for run in runs(content):
        kind = run[0]
        if kind == "text":
            out.append(_esc(run[1]))
        elif kind == "code":
            value = _esc(run[1])
            # A maturity keyword is the one thing a reader scans for, so it
            # gets the page's only colour rather than another grey code span.
            bare = run[1].lstrip("#")
            if run[1].startswith("#") and bare in (
                "implemented",
                "inProgress",
                "writtenAhead",
                "planned",
            ):
                out.append(f'<span class="mat mat-{bare}">{value}</span>')
            elif run[1] in ("@deferred", "deferred"):
                out.append(f'<span class="mat mat-deferred">{value}</span>')
            else:
                out.append(f"<code>{value}</code>")
        elif kind == "strong":
            out.append(f"<strong>{_esc(run[1])}</strong>")
        elif kind == "em":
            out.append(f"<em>{_esc(run[1])}</em>")
        elif kind == "link":
            out.append(f'<a href="{_esc(run[2])}">{_esc(run[1])}</a>')
    return "".join(out)


def render(doc: Doc) -> str:
    out: list[str] = []
    out.append("<!doctype html>")
    out.append('<html lang="en">')
    out.append("<head>")
    out.append('<meta charset="utf-8">')
    out.append('<meta name="viewport" content="width=device-width, initial-scale=1">')
    out.append(f"<title>{_esc(doc.title)}</title>")
    if doc.subtitle:
        out.append(f'<meta name="description" content="{_esc(doc.subtitle)}">')
    out.append(f"<style>{STYLE}</style>")
    out.append("</head>")
    out.append("<body>")
    out.append('<div class="page">')

    out.append('<header class="masthead">')
    out.append(f"<h1>{_esc(doc.title)}</h1>")
    if doc.subtitle:
        out.append(f'<p class="subtitle">{_esc(doc.subtitle)}</p>')
    banner = doc.meta.get("banner")
    if banner:
        out.append(f'<p class="banner">{_esc(banner)}</p>')
    out.append("</header>")

    out.append('<div class="shell">')
    headings = doc.headings(2)
    if headings:
        out.append('<nav class="toc" aria-label="Contents"><ol>')
        for heading in headings:
            css = ' class="sub"' if heading.level > 1 else ""
            out.append(f'<li{css}><a href="#{heading.anchor}">{_esc(heading.text)}</a></li>')
        out.append("</ol></nav>")
    else:
        out.append("<div></div>")

    out.append("<main>")
    for block in doc.blocks:
        out.append(_block(block))
    out.append("</main>")
    out.append("</div></div></body></html>")
    return "\n".join(part for part in out if part) + "\n"


def _block(block) -> str:
    if isinstance(block, Heading):
        level = min(block.level + 1, 6)
        return f'<h{level} id="{_esc(block.anchor)}">{_esc(block.text)}</h{level}>'

    if isinstance(block, P):
        return f"<p>{inline(block.parts)}</p>"

    if isinstance(block, Note):
        return f'<p class="note"><b>{_esc(block.label)}</b> — {inline(block.parts)}</p>'

    if isinstance(block, Bullets):
        items = "".join(f"<li>{inline(item)}</li>" for item in block.items)
        return f"<ul>{items}</ul>"

    if isinstance(block, Ordered):
        items = "".join(f"<li>{inline(item)}</li>" for item in block.items)
        return f"<ol>{items}</ol>"

    if isinstance(block, Steps):
        items = "".join(f"<li><span>{inline(item)}</span></li>" for item in block.items)
        return f'<ol class="steps">{items}</ol>'

    if isinstance(block, Defs):
        items = "".join(
            f"<div><dt>{inline(term)}</dt><dd>{inline(description)}</dd></div>"
            for term, description in block.items
        )
        return f'<dl class="defs">{items}</dl>'

    if isinstance(block, Tree):
        lines = "\n".join("  " * depth + _esc(plain(content)) for depth, content in block.lines)
        return f"<pre><code>{lines}</code></pre>"

    if isinstance(block, Code):
        return f"<pre><code>{_esc(block.text.rstrip())}</code></pre>"

    if isinstance(block, Diagram):
        return _diagram(block)

    if isinstance(block, Table):
        return _table(block)

    raise TypeError(f"no HTML rendering for {type(block).__name__}")


def _diagram(block: Diagram) -> str:
    """The figure, laid out and inlined.

    Inlined rather than linked: the page has to open from a `file://` path with
    nothing beside it, and an `<img>` pointing at a sibling file is a broken
    picture as soon as the HTML is copied somewhere on its own.
    """
    figure = block.figure
    caption = f'<span class="figname">Figure {block.number} — {_esc(figure.title)}.</span>'
    if figure.caption:
        caption += " " + _caption(figure.caption)
    if block.file:
        caption += f' <a href="{_esc(block.file)}">SVG</a>'
    if figure.source:
        caption += f" Source: <code>{_esc(figure.source)}</code>."
    return (
        f'<figure class="diagram" id="{_esc(block.anchor)}">'
        f"{render_svg.render(figure)}"
        f"<figcaption>{caption}</figcaption></figure>"
    )


def _caption(caption: str) -> str:
    """`Backticks` in a caption are code spans, as they are everywhere else."""
    out: list[str] = []
    for index, part in enumerate(caption.split("`")):
        out.append(f"<code>{_esc(part)}</code>" if index % 2 else _esc(part))
    return "".join(out)


def _table(block: Table) -> str:
    align = block.align or ["left"] * len(block.head)
    out = ['<div class="tablewrap"><table>']
    if block.caption:
        out.append(f"<caption>{inline(block.caption)}</caption>")
    out.append("<thead><tr>")
    for index, head in enumerate(block.head):
        css = ' class="right"' if align[index] == "right" else ""
        out.append(f"<th{css}>{_esc(head)}</th>")
    out.append("</tr></thead><tbody>")
    for row in block.rows:
        out.append("<tr>")
        for index in range(len(block.head)):
            cell = row[index] if index < len(row) else None
            css = ' class="right"' if align[index] == "right" else ""
            out.append(f"<td{css}>{inline(cell)}</td>")
        out.append("</tr>")
    out.append("</tbody></table></div>")
    return "".join(out)
