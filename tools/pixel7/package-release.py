#!/usr/bin/env python3
"""Package a phone desktop for a GitHub release, where the app finds it.

    tools/pixel7/package-release.py <edition> <build-dir> <out-dir> <tag> <commit>
                                    [--volume chromium.img]
    tools/pixel7/package-release.py --serve <out-dir> [--port 47708]

`build-dir` is what `build-desktop.sh` wrote (its `desktop.Image`); the
edition is `full` (built with `--chrome`, and given `--volume`) or `minimal`.
Into `out-dir` go, for the release's assets:

    ferrix-pixel7-<edition>.Image.gz   the desktop, gzipped
    ferrix-pixel7-<edition>.json       what the app reads first (below)
    ferrix-pixel7-chromium.img.gz      full only: Chromium's volume, gzipped

The manifest names each asset with the SHA-256 and size of what it
unpacks to, which the app checks before it puts a file in place:

    {"format": 1, "edition": "full", "tag": "...", "commit": "...",
     "image":  {"asset": "...", "sha256": "...", "size": N},
     "volume": {"asset": "...", "sha256": "...", "size": N, "pins": "..."}}

`pins` names the volume by what it holds rather than by its bytes:
`scripts/fetch/fetch-chromium-arm64.sh` pins every package by its hash, but
mkfs.btrfs writes a different image each time from the same files. So it is
the SHA-256 of that script, and the app fetches a volume again only when the
pins move, never merely because a new release was built.

gzip rather than anything denser: Android unpacks it with nothing but
java.util.zip. The archives carry no name or time, so the same image gives
the same asset.

`--serve` puts an `out-dir` on 127.0.0.1 as GitHub would list it: one
release, named by its manifests' tag, whose assets are the directory's
files. A debug build of the app pointed at `/releases` there
(README.md, "Updates") installs from it as from a published release.
"""

from __future__ import annotations

import argparse
import functools
import gzip
import hashlib
import http.server
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
PINS = HERE.parent.parent / "scripts" / "fetch" / "fetch-chromium-arm64.sh"
VOLUME_ASSET = "ferrix-pixel7-chromium.img.gz"


def pack(source: pathlib.Path, out: pathlib.Path) -> dict[str, object]:
    """Gzip `source` to `out`; the asset's entry for the manifest."""
    digest = hashlib.sha256()
    size = 0
    with source.open("rb") as raw, out.open("wb") as file:
        with gzip.GzipFile(filename="", mode="wb", fileobj=file, mtime=0, compresslevel=6) as packed:
            while chunk := raw.read(1 << 20):
                digest.update(chunk)
                size += len(chunk)
                packed.write(chunk)
    print(f"{out.name}: {size} bytes as {out.stat().st_size}", flush=True)
    return {"asset": out.name, "sha256": digest.hexdigest(), "size": size}


def serve(directory: pathlib.Path, port: int) -> None:
    """Serve `directory` as a one-release list in GitHub's shape, and its files."""
    base = f"http://127.0.0.1:{port}/assets/"
    files = sorted(path for path in directory.iterdir() if path.is_file())
    tags = {json.loads(path.read_text())["tag"] for path in files if path.suffix == ".json"}
    if len(tags) != 1:
        sys.exit(f"{directory}: the manifests name {len(tags)} tags, not one")
    listing = json.dumps([{
        "tag_name": tags.pop(),
        "draft": False,
        "assets": [{"name": path.name, "size": path.stat().st_size,
                    "browser_download_url": base + path.name} for path in files],
    }]).encode()

    class Handler(http.server.SimpleHTTPRequestHandler):
        def do_GET(self) -> None:
            if self.path.split("?")[0] == "/releases":
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(listing)))
                self.end_headers()
                self.wfile.write(listing)
            elif self.path.startswith("/assets/"):
                self.path = self.path.removeprefix("/assets")
                super().do_GET()
            else:
                self.send_error(404)

    server = http.server.ThreadingHTTPServer(
        ("127.0.0.1", port), functools.partial(Handler, directory=str(directory)))
    print(f"serving {directory} at http://127.0.0.1:{port}/releases", flush=True)
    server.serve_forever()


def main() -> None:
    if sys.argv[1:2] == ["--serve"]:
        parser = argparse.ArgumentParser(description="serve packed assets as GitHub's release list")
        parser.add_argument("--serve", type=pathlib.Path, required=True, metavar="out-dir")
        parser.add_argument("--port", type=int, default=47708)
        args = parser.parse_args()
        serve(args.serve, args.port)
        return
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("edition", choices=("full", "minimal"))
    parser.add_argument("build", type=pathlib.Path, help="build-desktop.sh's directory")
    parser.add_argument("out", type=pathlib.Path)
    parser.add_argument("tag")
    parser.add_argument("commit")
    parser.add_argument("--volume", type=pathlib.Path, help="chromium.img, for full")
    args = parser.parse_args()
    if (args.edition == "full") != (args.volume is not None):
        parser.error("full takes --volume, and minimal does not")
    image = args.build / "desktop.Image"
    if not image.is_file():
        parser.error(f"no {image}")
    args.out.mkdir(parents=True, exist_ok=True)

    manifest: dict[str, object] = {
        "format": 1,
        "edition": args.edition,
        "tag": args.tag,
        "commit": args.commit,
        "image": pack(image, args.out / f"ferrix-pixel7-{args.edition}.Image.gz"),
    }
    if args.volume is not None:
        volume = pack(args.volume, args.out / VOLUME_ASSET)
        volume["pins"] = hashlib.sha256(PINS.read_bytes()).hexdigest()
        manifest["volume"] = volume
    (args.out / f"ferrix-pixel7-{args.edition}.json").write_text(json.dumps(manifest, indent=1) + "\n")
    print(json.dumps(manifest), flush=True)


if __name__ == "__main__":
    main()
