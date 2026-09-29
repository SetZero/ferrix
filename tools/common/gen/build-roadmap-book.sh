#!/usr/bin/env bash
# Build the roadmap, docs/roadmap/, into a website with mdBook.
#
# The Markdown under docs/roadmap/ is the source and is what gets edited; the
# site is for reading it. index.html opens on the overview with every stage in
# the sidebar, and print.html is the whole roadmap on one page, which is the
# one to read from the top or print. Links that leave docs/roadmap/ go to the
# repository's web view, and the charts are inlined, so print.html stands on
# its own (tools/common/gen/split-roadmap.py's `mdbook` preprocessor does both).
#
# The output, docs/roadmap/book/, is not committed.
#
# Usage: tools/common/gen/build-roadmap-book.sh

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

if ! command -v mdbook > /dev/null; then
    echo "mdbook is not on PATH; install it with: cargo install mdbook --locked" >&2
    exit 1
fi

mdbook build "$root/docs/roadmap"

echo "$root/docs/roadmap/book/index.html"
echo "$root/docs/roadmap/book/print.html"
