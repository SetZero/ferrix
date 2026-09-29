#!/bin/sh
# One-time GitHub settings for SetZero/ferrix: what search, the repository
# page and link previews show. Everything here is public the moment it runs;
# read it first. Needs `gh auth login` with admin rights on the repository.
#
#   sh scripts/marketing/github-setup.sh            # settings only
#   sh scripts/marketing/github-setup.sh releases   # also publish the old tags
#
# The social preview image cannot be set through the API. Upload
# docs/brand/social-preview.png by hand: Settings -> General -> Social preview.
set -eu
repo=SetZero/ferrix

# Keep the public description short enough to read in repository search.
gh repo edit "$repo" \
  --description "Linux apps without Linux. Ferrix is an experimental Rust OS with a Wayland desktop and drivers outside the kernel." \
  --homepage "https://setzero.github.io/ferrix/" \
  --enable-discussions \
  --enable-issues \
  --enable-wiki=false

# A focused set of topics helps people browsing the relevant topic pages.
# PUT replaces the old topic list, so stale niche tags do not accumulate.
gh api -X PUT "repos/$repo/topics" --input - <<'JSON'
{"names":["rust","operating-system","osdev","kernel","rust-kernel","linux-compatibility","userland","wayland","btrfs","qemu","aarch64","ai-agents"]}
JSON

# Pages from Actions (the Website workflow deploys it).
gh api -X POST "repos/$repo/pages" -f build_type=workflow >/dev/null 2>&1 \
  || gh api -X PUT "repos/$repo/pages" -f build_type=workflow

# Private vulnerability reporting, which SECURITY.md points to.
gh api -X PUT "repos/$repo/private-vulnerability-reporting"

if [ "${1:-}" = releases ]; then
  # Backfill GitHub Releases for the tags that already exist, oldest first,
  # so the newest ends up as "Latest". Uses the same notes as the workflow.
  for tag in stage-9 stage-9.1-console-and-iommu stage-11-ring-3-disk-and-btrfs \
             stage-11.1-network-display-and-threads; do
    gh release view "$tag" -R "$repo" >/dev/null 2>&1 && continue
    python3 scripts/gen/release-notes.py "$tag" > /tmp/ferrix-notes.md
    gh release create "$tag" -R "$repo" --verify-tag \
      --title "$(python3 scripts/gen/release-notes.py --title "$tag")" \
      --notes-file /tmp/ferrix-notes.md
  done
  rm -f /tmp/ferrix-notes.md
fi

echo "done. Now upload docs/brand/social-preview.png under Settings -> General -> Social preview."
