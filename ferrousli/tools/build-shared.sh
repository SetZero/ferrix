#!/usr/bin/env bash
# Build ferrousli as glibc's stand-in: `libc.so.6` and the loader `ld.so`.
#
# `libferrousli.a` is linked whole into a shared object named `libc.so.6`,
# which is the name every glibc program's `DT_NEEDED` asks for. The loader
# answers glibc's other library names (`libm.so.6`, `libresolv.so.2`, ...)
# with the same object, since ferrousli is one library.
#
# Each exported symbol gets the version glibc gives it by default, from
# `tools/glibc-versions/<arch>.txt` (written by `tools/gen-glibc-versions.py`):
# a program linked against glibc asks for `printf@GLIBC_2.2.5`, and only a
# definition carrying that version answers it. A name glibc does not have is
# put in the oldest version, where nothing linked against glibc will look.
#
# Usage: tools/build-shared.sh [output directory]
#   default: $CARGO_TARGET_DIR (or target)/shared/x86_64
# Leaves libc.so.6 and ld.so there.

set -euo pipefail
export LC_ALL=C

here=$(cd "$(dirname "$0")/.." && pwd)
arch=x86_64
loader_target=x86_64-unknown-linux-musl
base_version=GLIBC_2.2.5
target_dir=${CARGO_TARGET_DIR:-$here/target}
out=${1:-$target_dir/shared/$arch}
versions=$here/tools/glibc-versions/$arch.txt

cd "$here"
cargo build --quiet --release --lib
cargo build --quiet --release -p ferrousli-ld --bin ld-ferrousli --features loader --target "$loader_target"

mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
archive=$target_dir/release/libferrousli.a

# The C names the archive defines. Rust's own symbols, the compiler's
# anonymous ones, the library's own `__ferrousli_` internals, and the literal
# `name@VERSION` aliases a static link needs (the version script below gives
# those names their versions) are left out.
nm -g --defined-only "$archive" 2> /dev/null \
    | awk 'NF == 3 && $2 ~ /^[TDBRVWi]$/ { print $3 }' \
    | grep -Ev '^(_ZN|_R|__rust|rust_|anon\.|__ferrousli_)|[@.]' \
    | sort -u > "$work/names"

# Each name with its version, then one node per version. The base node takes
# `local: *`, which hides everything not named.
awk -v base="$base_version" '
    NR == FNR { if ($0 !~ /^#/) version[$1] = $2; next }
    { print (($1 in version) ? version[$1] : base), $1 }
' "$versions" "$work/names" | sort > "$work/pairs"
awk -v base="$base_version" '
    { names[$1] = names[$1] "\t" $2 ";\n" }
    END {
        printf "%s {\n  global:\n%s  local: *;\n};\n", base, names[base]
        for (v in names) if (v != base) printf "%s {\n  global:\n%s};\n", v, names[v]
    }
' "$work/pairs" > "$work/map"

# The linker defines the bounds of a program's constructor arrays only for a
# program. The library names them for a static program's start; linked as
# `libc.so.6` it asks the loader instead (`src/loader.rs`), so here each pair
# is the same address, an empty range, and nothing is left undefined.
bounds=()
for array in preinit_array init_array fini_array; do
    bounds+=("-Wl,--defsym=__${array}_start=0" "-Wl,--defsym=__${array}_end=0")
done

cc -shared -nostdlib -o "$out/libc.so.6" \
    -Wl,--whole-archive "$archive" -Wl,--no-whole-archive \
    "${bounds[@]}" \
    -Wl,-soname,libc.so.6 \
    -Wl,--version-script="$work/map" \
    -Wl,-z,now -Wl,-z,relro -Wl,--hash-style=gnu -Wl,--no-undefined
cp "$target_dir/$loader_target/release/ld-ferrousli" "$out/ld.so"

echo "build-shared: $(wc -l < "$work/names") names in $(grep -c ' {' "$work/map") versions: $out"
