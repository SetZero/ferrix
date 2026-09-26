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

# The archive in two: this library's own objects, linked whole so that every
# name it defines is in libc.so.6, and the Rust runtime's (compiler_builtins,
# core, and compiler-rt's C), linked as an ordinary archive, from which the
# linker takes only what the library's objects call. compiler_builtins defines
# part of the C math library itself -- ceil, floor, fabs, fma, fmod, sqrt,
# trunc and their kin -- as weak symbols with hidden visibility. Linked whole
# beside this library's own definitions, the two merge with the stricter
# visibility, and those names left the dynamic symbol table: Chrome, asking
# for ceil@GLIBC_2.2.5, found nothing. Not linked at all, they cannot.
own=$work/own.a
runtime=$work/runtime.a
ar t "$archive" > "$work/members"
cp "$archive" "$own"
cp "$archive" "$runtime"
grep -v '^ferrousli-' "$work/members" | xargs -r ar d "$own"
grep '^ferrousli-' "$work/members" | xargs -r ar d "$runtime"

# Where glibc's `libc.so.6` exports GNU's variant under a name whose POSIX
# variant this library's header declares -- `strerror_r`, returning the
# message rather than an `int` -- the library defines GNU's as
# `__ferrousli_gnu_<name>`, and here the two swap names: the static library
# keeps POSIX's, and `libc.so.6` answers GNU's, as glibc's does.
nm -g --defined-only "$own" 2> /dev/null \
    | awk 'NF == 3 { sub(/^__ferrousli_gnu_/, "", $3) && gnu[$3] = 1 } END { for (n in gnu) print n }' \
    | sort > "$work/gnu"
# The toolchain's llvm-objcopy, which `rust-toolchain.toml`'s `llvm-tools`
# installs: the host's binutils cannot read the Arm objects, and said so
# only as a warning.
if [ -s "$work/gnu" ]; then
    host=$(rustc -vV | sed -n 's/^host: //p')
    objcopy=$(rustc --print sysroot)/lib/rustlib/$host/bin/llvm-objcopy
    swaps=()
    while read -r name; do
        swaps+=(--redefine-sym "$name=__ferrousli_posix_$name")
        swaps+=(--redefine-sym "__ferrousli_gnu_$name=$name")
    done < "$work/gnu"
    "$objcopy" "${swaps[@]}" "$own"
    if nm -g --defined-only "$own" 2> /dev/null | grep -q ' __ferrousli_gnu_'; then
        echo "build-shared: a GNU variant kept its own name:" \
            "$(nm -g --defined-only "$own" 2> /dev/null | grep -o '__ferrousli_gnu_[A-Za-z0-9_]*' | sort -u)" >&2
        exit 1
    fi
fi

# The C names the library defines. Rust's own symbols, the compiler's
# anonymous ones, the library's own `__ferrousli_` internals, and the literal
# `name@VERSION` aliases a static link needs (the version script below gives
# those names their versions) are left out.
nm -g --defined-only "$own" 2> /dev/null \
    | awk 'NF == 3 && $2 ~ /^[TDBRVWi]$/ { print $3 }' \
    | grep -Ev '^(_ZN|_R|__rust|rust_|anon\.|__ferrousli_)|[@.]' \
    | sort -u > "$work/names"

# Names an object marks hidden are the library's own, for its own objects,
# however `nm` shows them: `__restore_rt`, `ferrousli_clone`.
readelf -sW "$own" 2> /dev/null \
    | awk '$6 == "HIDDEN" && $7 != "UND" { print $8 }' \
    | sort -u > "$work/hidden"
comm -23 "$work/names" "$work/hidden" > "$work/visible"
mv "$work/visible" "$work/names"

# Each name with its version.
awk -v base="$base_version" '
    NR == FNR { if ($0 !~ /^#/) version[$1] = $2; next }
    { v = ($1 in version) ? version[$1] : base; if (v != "-") print v, $1 }
' "$versions" "$work/names" | sort > "$work/pairs"

# The older versions each exported function answers too, from
# `tools/glibc-versions/<arch>-compat.txt`: `pthread_create@GLIBC_2.2.5`
# beside `pthread_create@@GLIBC_2.34`, which is what a program linked against
# glibc before 2.34 asks for. Each is an alias named `name@VERSION`, which the
# linker reads as that name at that version, not the default: a tail jump to
# the function, as `src/termios.rs` makes its two. The jumps are assembled
# by rustc, which every target here has, so no C toolchain is needed for Arm.
compat=$here/tools/glibc-versions/$arch-compat.txt
: > "$work/compat"
if [ -f "$compat" ]; then
    awk '
        NR == FNR { if ($2 != "-") exported[$2] = $1; next }
        !/^#/ && ($1 in exported) && exported[$1] != $2 { print $2, $1 }
    ' "$work/pairs" "$compat" | sort -u > "$work/compat"
fi
case $arch in
x86_64) jump='jmp {}@PLT' ;;
aarch64) jump='b {}' ;;
armv7a) jump='b {}' ;;
esac
{
    echo '#![no_std]'
    echo '//! Older glibc versions of exported functions, generated by build-shared.sh.'
    echo 'core::arch::global_asm!('
    echo '    ".pushsection .text.ferrousli_glibc_compat,\"ax\",%progbits",'
    [ "$arch" = armv7a ] && echo '    ".arm",'
    while read -r version name; do
        alias="\\\"$name@$version\\\""
        echo "    \".p2align 4\","
        echo "    \".globl $alias\","
        echo "    \".type $alias, %function\","
        echo "    \"$alias:\","
        echo "    \"${jump//\{\}/$name}\","
    done < "$work/compat"
    echo '    ".popsection",'
    echo ');'
} > "$work/compat.rs"
compat_target=${lib_target:-$(rustc -vV | sed -n 's/^host: //p')}
rustc --edition 2021 --crate-type lib --crate-name ferrousli_glibc_compat \
    --emit obj -C opt-level=0 --target "$compat_target" \
    -o "$work/compat.o" "$work/compat.rs"

# One node per version, including those only an older alias carries.
# `local: *` hides everything not named, including the names the table marks
# `-`. It goes in GLIBC_PRIVATE's node, where no alias is: the linker hides a
# `name@VERSION` alias that VERSION's own node does not name when that node
# holds the `local: *`, so in the base node it would hide every alias at the
# base version, `pthread_create@GLIBC_2.2.5` among them.
awk '
    NR == FNR { names[$1] = names[$1] "\t" $2 ";\n"; next }
    { if (!($1 in names)) names[$1] = "" }
    END {
        private = names["GLIBC_PRIVATE"]
        printf "GLIBC_PRIVATE {\n%s  local: *;\n};\n", private == "" ? "" : "  global:\n" private
        for (v in names) {
            if (v == "GLIBC_PRIVATE") continue
            if (names[v] == "") printf "%s { };\n", v
            else printf "%s {\n  global:\n%s};\n", v, names[v]
        }
    }
' "$work/pairs" "$work/compat" > "$work/map"


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
        -Wl,--whole-archive "$own" -Wl,--no-whole-archive \
        "$work/compat.o" "$runtime" \
        "${bounds[@]}" \
        -Wl,-soname,libc.so.6 \
        -Wl,--version-script="$work/map" \
        -Wl,-z,now -Wl,-z,relro -Wl,--hash-style=gnu -Wl,--no-undefined
else
    # The same link, with rust-lld as the linker itself.
    host=$(rustc -vV | sed -n 's/^host: //p')
    lld=$(rustc --print sysroot)/lib/rustlib/$host/bin/rust-lld
    "$lld" -flavor gnu -shared -o "$out/libc.so.6" \
        --whole-archive "$own" --no-whole-archive \
        "$work/compat.o" "$runtime" \
        "${bounds[@]#-Wl,}" \
        -soname libc.so.6 \
        --version-script="$work/map" \
        -z now -z relro -z max-page-size=4096 --hash-style=gnu --no-undefined
fi
cp "$target_dir/$loader_target/release/ld-ferrousli" "$out/ld.so"

# Every name the version script exports, and every older alias, must be in
# the dynamic symbol table. One that is not was hidden by a definition the
# link took beside the library's, which is how ceil went missing.
nm -D --defined-only "$out/libc.so.6" | awk '{ print $NF }' | sed 's/@@/@/' | sort -u > "$work/exported"
{
    awk '{ print $2 "@" $1 }' "$work/pairs"
    awk '{ print $2 "@" $1 }' "$work/compat"
} | sort -u > "$work/expected"
missing=$(comm -23 "$work/expected" "$work/exported")
if [ -n "$missing" ]; then
    echo "build-shared: libc.so.6 does not export:" $missing >&2
    exit 1
fi

echo "build-shared: $(wc -l < "$work/names") names in $(grep -c ' {' "$work/map") versions," \
    "$(wc -l < "$work/compat") older versions: $out"
