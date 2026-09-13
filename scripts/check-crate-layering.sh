#!/usr/bin/env bash
# Assert the layering this OS rests on. See docs/ARCHITECTURE.md.
#
#   1. The loader and the kernel are two programs that meet at one struct.
#      `boot` must never link `ferrix-kernel` and the kernel must never link
#      the loader; everything they agree on lives in `ferrix-bootinfo`.
#   2. `libs/*` is host-testable, architecture-neutral logic. That is the whole
#      reason it exists -- it is the code `cargo test`, Miri and the fuzzers can
#      reach -- so it must depend on neither the kernel nor the loader, and must
#      contain no `#[cfg(target_arch)]`.
#   2a. `user/*` is ring-3 programs: the native runtime, devmgr, drivers. A
#      program may use `libs/*` and never the kernel or the loader, and neither
#      of those may link a program. Its `target_arch` conditionals live under
#      its own `src/arch/`.
#   3. Architecture code is reached through the `crate::arch` facade and never
#      by name. Generic kernel code that says `arch::x86_64::` compiles on one
#      machine and breaks the other, and the break is discovered by whoever
#      next builds for AArch64 rather than by whoever wrote it.
#   4. `#[cfg(target_arch)]` appears only under `kernel/src/arch/` and
#      `boot/src/arch/`. This is rule 3's teeth: without it, "the facade" is a
#      naming convention that erodes one conditional at a time.
#   5. Every workspace member inherits the workspace lint table.
#
# A principle in a design document does not survive contact with a bring-up
# session at 2am. A build failure does.
#
# Passes trivially while a layer is still empty, so it can be wired into CI
# before the code it guards exists.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

status=0

have() { cargo metadata --no-deps --format-version 1 2>/dev/null | grep -q "\"name\":\"$1\""; }

# Resolve a crate's shipping dependency graph, or fail loudly.
#
# A `cargo tree` that errors must NOT be treated as "no violations found": that
# turns a broken manifest into a green layering check, which is worse than no
# check at all.
#
# Only edges that ship (`--edges normal`). A `[dev-dependencies]` entry is not
# part of the artifact: `libs/elf` may legitimately pull in a test-only crate,
# and that says nothing about what the kernel links.
resolve() {
    local crate="$1"
    if ! cargo tree -p "$crate" --edges normal --prefix none --no-dedupe 2>/tmp/cargo-tree-err.$$; then
        echo "ERROR: could not resolve the dependency graph for $crate." >&2
        echo "  The layering rule was NOT checked. Fix the manifest first:" >&2
        sed 's/^/    /' /tmp/cargo-tree-err.$$ >&2
        rm -f /tmp/cargo-tree-err.$$
        return 1
    fi
    rm -f /tmp/cargo-tree-err.$$
}

# Assert that $crate (described as $role) depends on none of $forbidden, an
# extended-regex alternation of package names. A crate that does not exist yet
# is skipped, not failed, so a layer can be guarded before it is filled.
forbid() {
    local crate="$1" role="$2" forbidden="$3"
    have "$crate" || return 0

    local deps
    if ! deps=$(resolve "$crate"); then
        status=1
        return 0
    fi

    # Word-anchored, and it matters: `ferrix-boot` is a prefix of
    # `ferrix-bootinfo`, so an unanchored match reports every crate that depends
    # on the hand-off ABI as depending on the loader. `-` is not a word
    # character to grep, so `\b` lands exactly between `boot` and `info`.
    local offenders
    offenders=$(echo "$deps" | grep -oE "\\b(${forbidden})\\b" | sort -u || true)
    if [[ -n "$offenders" ]]; then
        echo "LAYERING VIOLATION: $crate ($role) links a crate it must not:" >&2
        echo "$offenders" | sed 's/^/    /' >&2
        status=1
    else
        echo "ok:   $crate ($role) links nothing it must not"
    fi
}

# ---------------------------------------------------------------------------
# 1. Loader and kernel are two programs.
# ---------------------------------------------------------------------------
forbid ferrix-boot   "UEFI loader" "ferrix-kernel"
forbid ferrix-kernel "kernel"      "ferrix-boot"

# ---------------------------------------------------------------------------
# 2. The host-testable libraries sit below both.
# ---------------------------------------------------------------------------
forbid ferrix-bootinfo "handoff ABI"  "ferrix-kernel|ferrix-boot|ferrix-elf|ferrix-frame"
forbid ferrix-elf      "ELF64 reader" "ferrix-kernel|ferrix-boot"
forbid ferrix-frame    "frame allocator" "ferrix-kernel|ferrix-boot"
forbid ferrix-paging   "page tables"  "ferrix-kernel|ferrix-boot"
forbid ferrix-sched    "scheduler logic" "ferrix-kernel|ferrix-boot"
forbid ferrix-pci      "PCI configuration space" "ferrix-kernel|ferrix-boot"
forbid ferrix-block    "block core" "ferrix-kernel|ferrix-boot"
forbid ferrix-btrfs    "btrfs read path" "ferrix-kernel|ferrix-boot"

# ---------------------------------------------------------------------------
# 2a. Ring-3 programs sit beside the kernel, never in it.
#
# A crate under `user/` -- the native runtime, devmgr, a driver -- runs in user
# mode. It may use any `libs/` crate, `libs/native-abi` above all, and nothing
# of the kernel or the loader, whose code cannot run there. Neither of those may
# link a `user/` crate either: that would be ring-3 code in ring 0. The names
# come from each manifest's `[package]`, so a new program is covered without an
# edit here.
# ---------------------------------------------------------------------------
user_crates=$(find user -name Cargo.toml -not -path '*/target/*' 2>/dev/null | sort \
    | xargs -r sed -n '/^\[package\]/,/^\[/s/^name = "\(.*\)"$/\1/p' \
    | paste -sd '|' - || true)
if [[ -n "$user_crates" ]]; then
    for crate in ${user_crates//|/ }; do
        forbid "$crate" "ring-3 program" "ferrix-kernel|ferrix-boot"
    done
    forbid ferrix-kernel "kernel"      "$user_crates"
    forbid ferrix-boot   "UEFI loader" "$user_crates"
fi
forbid ferrix-btrfs-vfs "btrfs mount" "ferrix-kernel|ferrix-boot"
forbid ferrix-blkring  "block ring protocol" "ferrix-kernel|ferrix-boot"

# ---------------------------------------------------------------------------
# 3. Generic kernel code reaches architecture code through the facade.
#
# `kernel/src/arch/mod.rs` is the facade and is where the architecture names
# are supposed to appear -- the three architectures', and the drivers the Arm
# pair share; everything else under kernel/src must go through it.
# ---------------------------------------------------------------------------
if [[ -d kernel/src ]]; then
    offenders=$(grep -rnE '(crate::)?arch::(x86_64|aarch64|armv7a|gicv2|pl011)::' kernel/src \
        --include='*.rs' \
        | grep -v '^kernel/src/arch/' || true)
    if [[ -n "$offenders" ]]; then
        echo "LAYERING VIOLATION: generic kernel code names an architecture module directly:" >&2
        echo "$offenders" | sed 's/^/    /' >&2
        echo "    Add the operation to the crate::arch facade instead." >&2
        status=1
    else
        echo "ok:   generic kernel code reaches the CPU only through crate::arch"
    fi
fi

# ---------------------------------------------------------------------------
# 4. Conditional compilation on the architecture lives in the arch directories.
#
# `target_pointer_width` and `target_endian` are not architecture selection --
# they are properties both of our targets share, and asserting one is fine
# anywhere. `target_os` likewise: xtask is a host program.
#
# A native program under `user/` follows the same rule: its crate's
# `src/arch/` is the facade, as `kernel/src/arch/` is the kernel's.
# ---------------------------------------------------------------------------
offenders=$(grep -rnE 'cfg[^)]*target_arch' kernel/src boot/src libs user 2>/dev/null \
    --include='*.rs' \
    | grep -vE '^((kernel|boot)/src/arch/|user/[^/]+/src/arch/)' || true)
if [[ -n "$offenders" ]]; then
    echo "LAYERING VIOLATION: target_arch conditional outside an arch directory:" >&2
    echo "$offenders" | sed 's/^/    /' >&2
    echo "    Architecture differences belong in kernel/src/arch/<arch>/ behind" >&2
    echo "    the facade, not as a conditional in generic code." >&2
    status=1
else
    echo "ok:   every target_arch conditional is inside an arch directory"
fi

# ---------------------------------------------------------------------------
# 5. Every workspace member inherits the workspace lint table.
#
# `[lints] workspace = true` and a per-crate override cannot coexist: cargo
# refuses the manifest. That is what makes this check a straight yes or no. A
# crate that genuinely needs an exception states it in its own source with a
# reason, where the code that needs it is, rather than opting out wholesale.
# ---------------------------------------------------------------------------
while IFS= read -r manifest; do
    grep -q '^\[package\]' "$manifest" || continue
    # A manifest declaring its own `[workspace]` (the fuzz crate does, so
    # `cargo fuzz` can pick its own profile) inherits from itself.
    grep -q '^\[workspace\]' "$manifest" && continue
    if ! sed -n '/^\[lints\]/,/^\[/p' "$manifest" | grep -qE '^\s*workspace\s*=\s*true'; then
        echo >&2 "$manifest does not inherit the workspace lints"
        echo >&2 "  add:  [lints]"
        echo >&2 "        workspace = true"
        status=1
    fi
done < <(find boot kernel libs user xtask -name Cargo.toml -not -path '*/target/*' 2>/dev/null | sort)

if [[ $status -ne 0 ]]; then
    echo >&2
    echo "See docs/ARCHITECTURE.md. The loader and kernel meet at one struct, the" >&2
    echo "host-testable libraries sit below both, and the CPU is reached through" >&2
    echo "one facade. The build is what makes those structural." >&2
fi

exit $status
