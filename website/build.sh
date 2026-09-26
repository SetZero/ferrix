#!/bin/sh
# Assemble the website into _site/ (or $1): the pages in website/, plus the
# images, which live once in docs/brand/ because the README uses them too.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
brand="$here/../docs/brand"
out=${1:-"$here/../_site"}
rm -rf "$out"
mkdir -p "$out/assets"
cp "$here"/*.html "$here"/*.css "$here"/robots.txt "$here"/sitemap.xml "$here"/site.webmanifest "$out/"
cp "$here"/assets/* "$out/assets/"
cp "$brand"/logo-mark.svg "$brand"/favicon.svg "$brand"/favicon-32.png "$brand"/apple-touch-icon.png \
   "$brand"/icon-512.png "$brand"/social-preview.png "$out/assets/"
cp "$brand"/screenshots/*.png "$out/assets/"
touch "$out/.nojekyll"
echo "website: built $out"
