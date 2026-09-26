# Continuously, from stage 1

* Every stage's exit criterion joins the CI boot test and stays there. As
  it stands on 2026-09-23, CI runs `test-boot` on all three architectures,
  `test-rustc` and `test-selfhost`; the exits that need a binary the repository does not carry,
  a disk judged on the host or a screendump — `test-shell`, `test-vfs`,
  `test-btrfs`, `test-powerfail`, `test-display`, `test-input`, `test-seat`
  and `test-compositor` — run in the landing gates of `docs/BACKLOG.md`, not
  in CI.
* The assembly allow-list is not added to without an argument in the diff.
* Anything expressible as a pure function of bytes goes to `libs/` and gets a
  fuzz target and a Miri run — before it is called from the kernel, not after.
