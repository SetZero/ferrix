# Ferrix brand

The shareable logo, colours and reusable copy are collected in the
[press and brand kit](../marketing/README.md).

## The mark

`logo-mark.svg` is a cube of iron's crystal lattice (body-centred cubic,
*ferrum*) drawn in isometric view, with a molten core. It stands for a hard
structure with something running hot inside, and it stays legible down to a
16 px favicon.

| File | Use |
|---|---|
| `logo-mark.svg`, `logo-mark-512.png` | the mark on a transparent background |
| `favicon.svg`, `favicon-32.png`, `apple-touch-icon.png`, `icon-512.png` | the mark on a dark rounded tile, for icons |
| `banner-dark.png`, `banner-light.png` | the README header, chosen by the reader's theme |
| `social-preview.png` | 1280×640: GitHub's social preview, and the website's `og:image` |
| `screenshots/` | real captures of Ferrix, each described in `screenshots/CAPTIONS.md` |

The PNGs are rendered from HTML by `python3 tools/common/gen/gen-brand-images.py`
with the tree's own fonts. Change the copy there, not in an image editor.

## Colour

| Token | Dark | Light | Role |
|---|---|---|---|
| ink | `#0d0f12` | `#f7f5f2` | background |
| surface | `#161a20` | `#ffffff` | cards |
| line | `#262c35` | `#e1dbd2` | borders |
| text | `#eef1f5` | `#16181c` | body |
| muted | `#9aa4b2` | `#5b6470` | secondary text |
| rust | `#ff7a2b` | `#c64a06` | the accent: links, the core, code |
| ember | `#ffb547` | `#a85f00` | success lines in terminal captures |

Use one accent. Rust orange marks what matters on the screen, so everything
else stays grey.

## Type

Inter for text (bundled in `assets/fonts/inter`). Liberation Mono in images and
JetBrains Mono on the web for code and terminal output.

## Voice

- **Claims come with their proof.** Put a screenshot, command or test close to
  each technical claim. Write "runs `rustc`", not "supports Rust development".
- **Describe how it is built plainly.** Explain the Claude sessions in the
  project story, after explaining what Ferrix does.
- **Show the evidence.** Name a working program, a test or a screenshot instead
  of using adjectives such as "massive" or "incredible".
- **Honest about limits.** It is not a daily driver. Say so before someone asks.

Website hero:

* Eyebrow: *Ferrix · an experimental Rust OS*
* Headline: *Linux apps without Linux.*

The README banner uses the same headline. Explain it nearby: tested Linux
programs run unchanged on Ferrix's own Rust kernel. Name working programs as
evidence, and say that drivers run in separate, restartable processes. Keep
the project's working process in its own section. Never imply that every
Linux program works or that Ferrix is ready for daily use.

Avoid "self-hosting" on its own: stage 20 (full self-hosting) is still in
progress. "Builds its own image" is what the gate proves.
