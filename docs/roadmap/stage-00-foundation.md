# Stage 0 — Foundation ✅

Workspace, the quality gates ported from Starling, CI, and the two host-testable
libraries.

**Exit:** `cargo xtask check` runs fmt, clippy on every freestanding target,
the layering check, the assembly allow-list, the unsafe audit and the panic
audit, and all pass on an empty tree.

---

