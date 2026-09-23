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
# A name the table marks `-` is one glibc answers only for programs built
# with a 32-bit `time_t` or `off_t` (ARMv7-A's `stat`, `time`, `lseek`, ...):
# ferrousli's function of that name takes the 64-bit form, so it is left out
# of the dynamic symbol table rather than answering such a program wrongly.
#
# Usage: tools/build-shared.sh [--arch x86_64|aarch64|armv7a] [output directory]
#   default: x86_64, into $CARGO_TARGET_DIR (or target)/shared/<arch>
# Leaves libc.so.6 and ld.so there.
#
# x86-64's `libc.so.6` is linked with the host's `cc`. The Arm ones are linked
# with the Rust toolchain's own rust-lld, and both loaders are built by cargo
# alone, so no C toolchain for either Arm target is needed.

set -euo pipefail
export LC_ALL=C

here=$(cd "$(dirname "$0")/.." && pwd)
arch=x86_64
if [ "${1:-}" = --arch ]; then
    arch=$2
    shift 2
fi
case $arch in
x86_64)
    lib_target=
    loader_target=x86_64-unknown-linux-musl
    base_version=GLIBC_2.2.5
    ;;
aarch64)
    lib_target=aarch64-unknown-linux-gnu
    loader_target=aarch64-unknown-linux-musl
    base_version=GLIBC_2.17
    ;;
armv7a)
    lib_target=armv7-unknown-linux-gnueabihf
    loader_target=armv7-unknown-linux-musleabihf
    base_version=GLIBC_2.4
    ;;
*)
    echo "build-shared: no architecture $arch" >&2
    exit 2
    ;;
esac
target_dir=${CARGO_TARGET_DIR:-$here/target}
out=${1:-$target_dir/shared/$arch}
versions=$here/tools/glibc-versions/$arch.txt

cd "$here"
if [ -n "$lib_target" ]; then
    cargo build --quiet --release --lib --target "$lib_target"
    archive=$target_dir/$lib_target/release/libferrousli.a
else
    cargo build --quiet --release --lib
    archive=$target_dir/release/libferrousli.a
fi
cargo build --quiet --release -p ferrousli-ld --bin ld-ferrousli --features loader --target "$loader_target"

mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The C names the archive defines. Rust's own symbols, the compiler's
# anonymous ones, the library's own `__ferrousli_` internals, and the literal
# `name@VERSION` aliases a static link needs (the version script below gives
# those names their versions) are left out.
nm -g --defined-only "$archive" 2> /dev/null \
    | awk 'NF == 3 && $2 ~ /^[TDBRVWi]$/ { print $3 }' \
    | grep -Ev '^(_ZN|_R|__rust|rust_|anon\.|__ferrousli_)|[@.]' \
    | sort -u > "$work/names"

# Each name with its version, then one node per version. The base node takes
# `local: *`, which hides everything not named, including the names the
# table marks `-`.
awk -v base="$base_version" '
    NR == FNR { if ($0 !~ /^#/) version[$1] = $2; next }
    { v = ($1 in version) ? version[$1] : base; if (v != "-") print v, $1 }
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

if [ "$arch" = x86_64 ]; then
    cc -shared -nostdlib -o "$out/libc.so.6" \
        -Wl,--whole-archive "$archive" -Wl,--no-whole-archive \
        "${bounds[@]}" \
        -Wl,-soname,libc.so.6 \
        -Wl,--version-script="$work/map" \
        -Wl,-z,now -Wl,-z,relro -Wl,--hash-style=gnu -Wl,--no-undefined
else
    # The same link, with rust-lld as the linker itself.
    host=$(rustc -vV | sed -n 's/^host: //p')
    lld=$(rustc --print sysroot)/lib/rustlib/$host/bin/rust-lld
    "$lld" -flavor gnu -shared -o "$out/libc.so.6" \
        --whole-archive "$archive" --no-whole-archive \
        "${bounds[@]#-Wl,}" \
        -soname libc.so.6 \
        --version-script="$work/map" \
        -z now -z relro -z max-page-size=4096 --hash-style=gnu --no-undefined
fi
cp "$target_dir/$loader_target/release/ld-ferrousli" "$out/ld.so"

echo "build-shared: $(wc -l < "$work/names") names in $(grep -c ' {' "$work/map") versions: $out"
