# Claude Code — Anthropic's coding agent on Ferrix  ·  *the command line, 2026-09-30; the desktop app being assessed*

Placed after Chrome without a number of its own. The customer asked on
2026-09-30 for Claude Code on Ferrix, the command line first and then the
desktop app; `docs/CLAUDE-CODE.md` is the account. The program is
Anthropic's prebuilt native release for linux-x64, 2.1.280, on Debian 13's
glibc, from a btrfs volume `tools/common/fetch/fetch-claude-code.sh` makes
from pinned downloads, as Chrome's is.

**Exit:** Claude Code runs on Ferrix and uses a tool there, and the desktop
has it. Met for the command line on x86-64 (2026-09-30): `cargo xtask
test-claude-code` runs `claude --version`, then one turn of `claude -p`
against a Messages API xtask serves, in which the model asks for a Bash
command and Claude Code runs it in bash on Ferrix and sends its output
back; `--everything` runs the same on the `run-compositor --everything`
desktop's volume, whose terminals, and `remote-desktop`'s, have `claude`.

**Done (2026-09-30).** In the order running it found them:

* AVX for programs. Claude Code's Bun runtime is built for AVX2 with no
  fallback, and spun forever in its first string conversion on a Ferrix
  that neither saved AVX nor offered it. The kernel now sets `CR4.OSXSAVE`,
  enables x87, SSE and AVX in `XCR0`, saves them with `XSAVE` at every
  switch, and carries them in signal frames as Linux does; xtask's x86-64
  model has x86-64-v3's instructions, and `XSAVEOPT`, without which QEMU's
  TCG loops on the write of `CR4` (`docs/CLAUDE-CODE.md` §3).
* The volume, the gate and its API, and `claude` on the `--everything`
  desktop (§2, §4, §5).

**Still to do:**

* The interactive TUI in a pseudo-terminal, gated; a real account over the
  real network (§5, `docs/BACKLOG.md`).
* An i386 signal frame with the `XSAVE` area (`docs/BACKLOG.md`).
* On ferrousli in glibc's place, and linux-arm64 on AArch64 (§6).
* The Claude desktop app, an Electron application Anthropic builds for
  macOS and Windows only: the customer chose it on 2026-09-30, and an
  assessment comes before any building (§7).
