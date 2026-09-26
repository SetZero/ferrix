# Fonts

The faces a Ferrix desktop draws text in, carried in the tree so that no
image depends on a font being installed anywhere else. `cargo xtask` puts
them in `/usr/share/ferrix/fonts` on every image that runs Chrome in a window
(`run-compositor --chrome`, `test-chrome-window`, `bench-chrome`), with
`fonts.conf` beside them and `FONTCONFIG_FILE` naming it.

| directory | face | version | licence |
|---|---|---|---|
| `inter/` | Inter, variable, upright and italic | 4.1 | SIL Open Font License 1.1, `inter/LICENSE` |
| `liberation/` | Liberation Sans, Serif and Mono, four styles each | 2.1.5 | SIL Open Font License 1.1, `liberation/LICENSE` |

**Inter** is the sans-serif: Chrome's own interface and a page's
`sans-serif`, `sans` and `system-ui`. It was drawn for screens, and its
variable file carries every weight from Thin to Black in 0.9 MB.

**Liberation** is metric-compatible with Arial, Times New Roman and Courier
New: every glyph is as wide as the face it stands in for, so a page laid out
for those does not reflow. Chrome's default fonts on Linux are those three,
and a great many pages name them; fontconfig's `30-metric-aliases.conf`
answers them with Liberation.

The OFL lets both be bundled and redistributed with software, provided the
licence travels with them and they are not sold on their own. It does, in
each directory.

## Where they come from

Each file is unchanged from its project's release archive:

| archive | SHA-256 |
|---|---|
| [`Inter-4.1.zip`](https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip) | `9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e` |
| [`liberation-fonts-ttf-2.1.5.tar.gz`](https://github.com/liberationfonts/liberation-fonts/files/7261482/liberation-fonts-ttf-2.1.5.tar.gz) | `7191c669bf38899f73a2094ed00f7b800553364f90e2637010a69c0e268f25d0` |

`inter/LICENSE` is the archive's `LICENSE.txt`.

## `fonts.conf`

It adds this directory to fontconfig's, makes Inter the first choice for the
three sans-serif names, and asks for greyscale antialiasing with slight
hinting. Then it includes `/etc/fonts/fonts.conf`, so everything the system
configures still applies. The file's own comments say why its edits come
before that include.
