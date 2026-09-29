#!/usr/bin/env python3
"""Render Ferrix's brand images (README banners, social preview) to PNG.

The images are HTML pages drawn with the tree's own fonts and screenshotted by
headless Chrome, so they can be regenerated after the copy changes:

    python3 tools/common/gen/gen-brand-images.py            # all of them
    python3 tools/common/gen/gen-brand-images.py social     # one of them

Needs `google-chrome` (or `chromium`) on PATH. Output lands in docs/brand/.
The terminal lines on the social preview are copied from real gate logs
(test-rustc and test-selfhost); keep them that way.
"""

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
BRAND = ROOT / "docs" / "brand"
FONTS = ROOT / "assets" / "fonts"

MARK = (BRAND / "src" / "mark.svgfrag").read_text()

TAGLINE = "Linux apps without Linux."
FEATURES = ["Linux ABI", "ring-3 drivers", "btrfs", "Wayland desktop", "Chrome"]
ARCHES = "x86-64 &middot; AArch64 &middot; ARMv7-A"

# Real lines from test-rustc and test-selfhost serial logs, timestamps included.
LOG = [
    ("t", "11.41", "FERRIX-BOOT-OK stages 1-12"),
    ("t", "12.38", "rustc 1.97.1 (8bab26f4f 2026-07-14)"),
    ("t", "13.28", "cargo 1.97.1 (c980f4866 2026-06-30)"),
    ("h", "18.17", "rustc-gate: hello from rustc on Ferrix"),
    ("g", "", ""),
    ("t", "51.81", "   Compiling ferrix-kernel v0.1.0"),
    ("t", "106.99", "  image build/x86_64/ferrix.img"),
    ("h", "", "Ferrix built its own image, and it booted"),
]

THEMES = {
    "dark": dict(bg="#0d0f12", bg2="#151920", text="#eef1f5", muted="#9aa4b2",
                 line="#262c35", rust="#ff7a2b", lattice="#ffffff", lattice_a=".035"),
    "light": dict(bg="#f7f5f2", bg2="#ffffff", text="#16181c", muted="#5b6470",
                  line="#e3ded7", rust="#d4520b", lattice="#000000", lattice_a=".04"),
}


def fonts_css():
    inter = (FONTS / "inter" / "InterVariable.ttf").as_uri()
    mono = (FONTS / "liberation" / "LiberationMono-Regular.ttf").as_uri()
    return f"""
@font-face {{ font-family: Inter; src: url("{inter}"); font-weight: 100 900; }}
@font-face {{ font-family: Mono; src: url("{mono}"); }}
"""


def lattice(t):
    # A faint isometric lattice, the logo's cell repeated.
    return (
        "background-image: url(\"data:image/svg+xml;utf8,"
        "<svg xmlns='http://www.w3.org/2000/svg' width='56' height='97'>"
        f"<g fill='none' stroke='{t['lattice'].replace('#', '%23')}' stroke-opacity='{t['lattice_a']}' stroke-width='1.5'>"
        "<path d='M28 0 L56 16 L56 48 L28 64 L0 48 L0 16 Z M28 64 L28 97'/></g></svg>\");"
    )


def base(t, w, h, body):
    return f"""<!doctype html><html><head><meta charset="utf-8"><style>
{fonts_css()}
* {{ box-sizing: border-box; margin: 0; }}
html, body {{ width: {w}px; height: {h}px; overflow: hidden; }}
body {{ background: {t['bg']}; color: {t['text']}; font-family: Inter, sans-serif;
        -webkit-font-smoothing: antialiased; position: relative; }}
.bgl {{ position: absolute; inset: 0; {lattice(t)} }}
.glow {{ position: absolute; border-radius: 50%; filter: blur(80px); opacity: .22;
         background: {t['rust']}; }}
.mark svg {{ display: block; width: 100%; height: 100%; }}
.word {{ font-weight: 750; letter-spacing: -0.035em; line-height: 1; }}
code {{ font-family: Mono, monospace; font-size: .92em; color: {t['rust']}; }}
.chips {{ display: flex; flex-wrap: wrap; gap: 10px; }}
.chip {{ border: 1.5px solid {t['line']}; background: {t['bg2']}; color: {t['text']};
         border-radius: 999px; padding: 6px 14px; font-weight: 550; font-size: 17px; }}
.muted {{ color: {t['muted']}; }}
</style></head><body><div class="bgl"></div>{body}</body></html>"""


def mark_svg():
    return f'<svg viewBox="0 0 128 128" xmlns="http://www.w3.org/2000/svg">{MARK}</svg>'


def terminal(t):
    rows = []
    for kind, ts, text in LOG:
        if kind == "g":
            rows.append('<div class="gap"></div>')
            continue
        stamp = f'<span class="ts">{ts:>7} |</span> ' if ts else '<span class="ts">        </span> '
        cls = "hi" if kind == "h" else ""
        rows.append(f'<div class="{cls}">{stamp.replace(" ", "&nbsp;")}{text.replace(" ", "&nbsp;")}</div>')
    return f"""
<div class="term">
  <div class="bar"><i></i><i></i><i></i><span>serial &mdash; cargo xtask test-rustc / test-selfhost</span></div>
  <div class="scr">{''.join(rows)}</div>
</div>
<style>
.term {{ background: #0a0c0f; border: 1.5px solid #2a313b; border-radius: 14px; overflow: hidden;
        box-shadow: 0 30px 80px rgba(0,0,0,.45); }}
.bar {{ display: flex; align-items: center; gap: 8px; padding: 12px 16px; background: #14181e;
        border-bottom: 1px solid #232932; }}
.bar i {{ width: 12px; height: 12px; border-radius: 50%; background: #3a424d; }}
.bar span {{ margin-left: 10px; color: #7d8793; font-size: 14px; }}
.scr {{ padding: 18px 20px 20px; font-family: Mono, monospace; font-size: 16px; line-height: 1.65;
        color: #c9d1db; white-space: nowrap; }}
.scr .ts {{ color: #56606c; }}
.scr .hi {{ color: #ffb547; }}
.scr .gap {{ height: 10px; }}
</style>"""


def social():
    t = THEMES["dark"]
    body = f"""
<div class="glow" style="width:520px;height:520px;left:-120px;top:-160px"></div>
<div class="glow" style="width:420px;height:420px;right:-80px;bottom:-220px;opacity:.14"></div>
<div style="position:absolute;inset:0;padding:72px 80px;display:flex;flex-direction:column">
  <div style="display:flex;align-items:center;gap:26px">
    <div class="mark" style="width:112px;height:112px">{mark_svg()}</div>
    <div class="word" style="font-size:104px">Ferrix</div>
  </div>
  <div style="font-size:37px;font-weight:600;line-height:1.25;letter-spacing:-.015em;margin-top:34px;max-width:560px">
    {TAGLINE}
  </div>
  <div class="muted" style="font-size:22px;margin-top:22px;max-width:590px;line-height:1.45">
    Chrome, <code>rustc</code>, <code>git</code> and <code>curl</code> run unchanged on Ferrix’s Rust kernel.
    Drivers run as separate processes and can restart after a crash.
  </div>
  <div style="margin-top:auto" class="muted"><span style="font-size:21px;font-weight:600">{ARCHES}</span>
    <span style="font-size:21px">&nbsp;&nbsp;&middot;&nbsp;&nbsp;github.com/SetZero/ferrix</span></div>
</div>
<div style="position:absolute;right:60px;top:150px;width:540px">{terminal(t)}</div>
"""
    return base(t, 1280, 640, body), (1280, 640), "social-preview.png"


def banner(theme):
    t = THEMES[theme]
    chips = "".join(f'<span class="chip">{f}</span>' for f in FEATURES)
    body = f"""
<div class="glow" style="width:420px;height:420px;left:-60px;top:-180px;opacity:{'.20' if theme == 'dark' else '.12'}"></div>
<div style="position:absolute;inset:0;padding:0 64px;display:flex;align-items:center;gap:40px">
  <div class="mark" style="width:150px;height:150px;flex:none">{mark_svg()}</div>
  <div>
    <div class="word" style="font-size:92px">Ferrix</div>
    <div style="font-size:30px;font-weight:600;margin-top:12px;letter-spacing:-.01em">{TAGLINE}</div>
    <div class="chips" style="margin-top:22px">{chips}<span class="chip muted" style="border-style:dashed">{ARCHES}</span></div>
  </div>
</div>
"""
    return base(t, 1280, 330, body), (1280, 330), f"banner-{theme}.png"


JOBS = {
    "social": social,
    "banner-dark": lambda: banner("dark"),
    "banner-light": lambda: banner("light"),
}


def chrome():
    for name in ("google-chrome", "chromium", "chromium-browser"):
        if shutil.which(name):
            return name
    sys.exit("gen-brand-images: needs google-chrome or chromium on PATH")


def render(html, size, out, scale):
    with tempfile.TemporaryDirectory() as tmp:
        page = Path(tmp) / "page.html"
        page.write_text(html)
        subprocess.run(
            [chrome(), "--headless=new", "--no-sandbox", "--disable-gpu", "--hide-scrollbars",
             f"--force-device-scale-factor={scale}", "--allow-file-access-from-files",
             f"--window-size={size[0]},{size[1]}", f"--screenshot={out}", page.as_uri()],
            check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def main():
    wanted = sys.argv[1:] or list(JOBS)
    for key in wanted:
        html, size, name = JOBS[key]()
        # The social preview is uploaded at its native size; banners are drawn
        # at 2x so they stay sharp on high-density screens.
        scale = 1 if key == "social" else 2
        out = BRAND / name
        render(html, size, out, scale)
        print(f"{out.relative_to(ROOT)}  {size[0] * scale}x{size[1] * scale}")


if __name__ == "__main__":
    main()
