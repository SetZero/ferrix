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

# The description is the first thing search engines and GitHub search index.
# 350 characters at most; the first ~120 show in search results.
gh repo edit "$repo" \
  --description "A Rust operating system that runs rustc and builds itself. Own kernel, ring-3 drivers, btrfs, libc, shell and Wayland desktop; runs Linux programs and Chrome. Built by AI agents." \
  --homepage "https://setzero.github.io/ferrix/" \
  --enable-discussions \
  --enable-issues \
  --enable-wiki=false

# Topics: GitHub allows 20. These are the ones people browse
# (github.com/topics/<name>) for this kind of project.
gh repo edit "$repo" \
  --add-topic rust --add-topic operating-system --add-topic kernel --add-topic osdev \
  --add-topic hobby-os --add-topic uefi --add-topic x86-64 --add-topic aarch64 \
  --add-topic armv7 --add-topic qemu --add-topic wayland --add-topic wayland-compositor \
  --add-topic btrfs --add-topic linux-compatibility --add-topic self-hosting --add-topic iommu \
  --add-topic libc --add-topic zsh --add-topic ai-agents --add-topic claude

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
