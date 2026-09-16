#!/usr/bin/env bash
# Builds build.sh's busybox natively on Windows, without WSL: the same pinned
# sources and config, and the same static x86-64 program against ferrousli.
#
#     bash tools/busybox/build-windows.sh    # from ferrousli/, in Git Bash
#
# `cargo xtask busybox` runs it on Windows. It needs Git for Windows (bash and
# the POSIX tools), LLVM (clang, lld and llvm-ar), and Strawberry Perl, whose
# mingw gcc compiles busybox's host programs and whose gmake runs its build.
# LLVM is looked for on PATH and then in its installer's directory; $LLVM_BIN
# overrides both.
#
# Windows has no kernel UAPI headers, so they come from Alpine's linux-headers
# package, pinned below by the sha256 of the v3.22 package as first downloaded.
# Eight netfilter headers in it differ only in case and overwrite each other on
# NTFS; busybox includes none of them.
#
# busybox's build assumes a POSIX host. What changes here, each explained where
# it is done:
#   * ferrousli is built for x86_64-unknown-linux-gnu, not the host's target;
#   * clang cross-compiles and lld links, in place of the host's cc;
#   * mingw gcc compiles kbuild's host programs with hostcompat/, the few POSIX
#     headers they include that mingw lacks;
#   * five lines of busybox's makefiles that native gmake cannot run are edited.
#
# The output is build.sh's: under $FERRIX_BUSYBOX, by default
# ~/.local/share/ferrix/busybox/ferrousli, x86_64/busybox once busybox links,
# and otherwise undefined-symbols.txt, the functions ferrousli does not have.
set -euo pipefail

HEADERS=linux-headers-6.14.2-r0.apk
HEADERS_URL=https://dl-cdn.alpinelinux.org/alpine/v3.22/main/x86_64/$HEADERS
HEADERS_SHA256=b0d7184f0e8d926961b82dff3d8a6a1f85db100dfc97e3fa1e6a25bbe9fd0f71
TARGET=x86_64-unknown-linux-gnu

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ferrousli=$(cd "$here/../.." && pwd)
out=$(cygpath -u "${FERRIX_BUSYBOX:-$HOME/.local/share/ferrix/busybox/ferrousli}")
src=$out/src
build=$out/build
jobs=${JOBS:-4}

step() { printf '\n== %s\n' "$*"; }
fail() {
    echo "build-windows.sh: $*" >&2
    exit 1
}
# A path for a native program's command line.
win() { cygpath -m "$1"; }
# A path for gmake's command line, which splits at a space: the 8.3 name.
spaceless() {
    local path
    path=$(cygpath -ms "$1")
    case "$path" in
        *' '*) fail "$path has a space and no short name for make to use" ;;
    esac
    printf '%s\n' "$path"
}

# shellcheck source=sources.sh
. "$here/sources.sh"

step "tools"
if [ -z "${LLVM_BIN:-}" ]; then
    if command -v clang >/dev/null 2>&1; then
        LLVM_BIN=$(dirname "$(command -v clang)")
    else
        LLVM_BIN="/c/Program Files/LLVM/bin"
    fi
fi
llvm=$(cygpath -u "$LLVM_BIN")
[ -x "$llvm/clang.exe" ] && [ -x "$llvm/ld.lld.exe" ] && [ -x "$llvm/llvm-ar.exe" ] ||
    fail "no clang, ld.lld and llvm-ar in $llvm; install LLVM: winget install LLVM.LLVM"
if ! command -v gmake >/dev/null 2>&1 || ! command -v gcc >/dev/null 2>&1; then
    PATH="$PATH:/c/Strawberry/c/bin"
fi
command -v gmake >/dev/null 2>&1 && command -v gcc >/dev/null 2>&1 ||
    fail "no gcc and gmake; install Strawberry Perl: winget install StrawberryPerl.StrawberryPerl"
# Git's own tools first: the mingw bzip2.exe that Git and Strawberry also ship
# takes /dev/null for a terminal and refuses to write to it, and busybox's
# embedded_scripts then reports bzip2 missing. LLVM next, so that kbuild's LD,
# AR and the rest can be bare names: make splits a path at its space.
export PATH="/usr/bin:$llvm:$PATH"
echo "clang: $llvm/clang.exe"
echo "gcc:   $(command -v gcc)"
echo "gmake: $(command -v gmake)"

step "sources"
fetch_sources "$src"
fetch "$HEADERS_URL" "$src/$HEADERS" sha256sum "$HEADERS_SHA256"
echo "Alpine's $HEADERS, verified"

step "ferrousli, release, $TARGET"
# Where cargo puts the library: the caller's CARGO_TARGET_DIR when set.
target=${CARGO_TARGET_DIR:-$ferrousli/target}
# The host's target is Windows; crt1.o and the library are Linux code.
cargo build --release --lib --target $TARGET --manifest-path "$(win "$ferrousli/Cargo.toml")"
lib=$target/$TARGET/release/libferrousli.a
crt1=$(ls -t "$target"/$TARGET/release/build/ferrousli-*/out/crt1.o | head -1)
[ -f "$lib" ] && [ -f "$crt1" ]
echo "$lib"
echo "$crt1"

step "include directories"
# As in build.sh: musl's headers first, the UAPI headers next, and the
# compiler's own directory last, for intrinsics only.
uapi=$out/linux-headers
rm -rf "$uapi"
mkdir -p "$uapi"
tar --warning=no-unknown-keyword -xzf "$src/$HEADERS" -C "$uapi" usr/include
[ -f "$uapi/usr/include/linux/types.h" ]
compiler_include=$("$llvm/clang.exe" -print-resource-dir | tr '\\' /)/include

# Empty archives for the libraries busybox's link script asks for, as in
# build.sh.
stubs=$out/stub-libs
mkdir -p "$stubs"
for l in m crypt resolv rt pthread dl util; do
    [ -f "$stubs/lib$l.a" ] || "$llvm/llvm-ar.exe" rc "$stubs/lib$l.a"
done

# The compiler busybox is built with: clang for Linux, told where ferrousli
# is, linking with lld. Never the default linker, even for -r: for a Linux
# target on Windows that is whatever ld.exe comes first, a PE linker.
wrapper=$out/ferrousli-cc
cat > "$wrapper" <<EOF
#!/usr/bin/env bash
# Generated by ferrousli/tools/busybox/build-windows.sh.
link=1
args=()
prev=
for a in "\$@"; do
    # As in build.sh: -r links one directory's objects, not a program.
    case "\$a" in -c|-S|-E|-M|-MM|-r) link=0 ;; esac
    # Native gmake lowercases the .S prerequisite of kbuild's "%.o: %.S", and
    # clang would assemble a .s name without the preprocessor the .S needs.
    case "\$prev:\$a" in
        -o:*) args+=("\$a") ;;
        *:*.s|*:*.S) args+=(-x assembler-with-cpp "\$a" -x none) ;;
        *) args+=("\$a") ;;
    esac
    prev=\$a
done
common=(--target=x86_64-linux-gnu -fuse-ld=lld -nostdinc -isystem "$(win "$ferrousli/include")" -isystem "$(win "$uapi/usr/include")" -isystem "$compiler_include")
if [ \$link = 1 ]; then
    exec "$llvm/clang.exe" "\${common[@]}" -static -nostdlib -L"$(win "$stubs")" "$(win "$crt1")" "\${args[@]}" "$(win "$lib")"
else
    exec "$llvm/clang.exe" "\${common[@]}" "\${args[@]}"
fi
EOF
chmod +x "$wrapper"
echo "$wrapper"

step "unpack and configure"
rm -rf "$build"
mkdir -p "$build"
tar -xjf "$src/$TARBALL" -C "$build" --strip-components=1
cp "$src/busyboxconfig" "$build/.config"
# shellcheck source=config.sh
. "$here/config.sh"
apply_config_changes "$build/.config"

# Native gmake's $(PATH) is the Windows list (C:\...;...), which bash cannot
# use when it is assigned inline; the PATH the shell inherits is already right.
sed -i 's/\$(shell PATH="\$(PATH)" /$(shell /' "$build/scripts/Kbuild.include"
grep -q '^cc-version = $(shell $(CONFIG_SHELL)' "$build/scripts/Kbuild.include"
# Two targets are named after $(CURDIR), whose drive colon make reads as a
# second target pattern. Neither is a rule a busybox build runs. (srctree=.
# on the command line is no substitute: kbuild then sees ./scripts and
# scripts as different files, and its dependencies go circular.)
sed -i 's/\$(addprefix _clean_,\$(srctree) /$(addprefix _clean_,. /; s|^\$(objtree)/Module.symvers:|Module.symvers:|' "$build/Makefile"
grep -q '_clean_,\. ' "$build/Makefile"
! grep -q '^\$(objtree)/Module.symvers:' "$build/Makefile"
# Native gmake compares file names case-insensitively, whatever the directory
# says, so in "%.s: %.S" the target is its own source and $< becomes FORCE.
# Objects come from "%.o: %.S"; a preprocessed .s is only for make foo.s.
sed -i '/^%\.s: %\.S prepare scripts FORCE$/,+1d' "$build/Makefile"
sed -i '/^%\.s: %\.S FORCE$/,+1d' "$build/scripts/Makefile.build"
! grep -q '^%\.s: %\.S' "$build/Makefile" "$build/scripts/Makefile.build"
grep -q '^%\.o: %\.S FORCE$' "$build/scripts/Makefile.build"

compat=$(spaceless "$here/hostcompat")
tools=(
    CC="$(spaceless "$wrapper")"
    HOSTCC=gcc
    HOSTCFLAGS="-O2 -I$compat -include $compat/hostcompat.h"
    LD=ld.lld AR=llvm-ar NM=llvm-nm STRIP=llvm-strip
    OBJCOPY=llvm-objcopy OBJDUMP=llvm-objdump
    SHELL=/usr/bin/bash
)
if ! gmake -C "$(win "$build")" "${tools[@]}" oldconfig </dev/null > "$out/oldconfig.log" 2>&1; then
    tail -30 "$out/oldconfig.log" >&2
    fail "oldconfig failed; log in $out/oldconfig.log"
fi
grep -E '^CONFIG_(STATIC|PIE)=|^# CONFIG_(STATIC|PIE) is not set' "$build/.config"

step "build"
set +e
gmake -C "$(win "$build")" -j"$jobs" "${tools[@]}" V=1 busybox > "$out/build.log" 2>&1
status=$?
set -e
echo "make exited $status; log in $out/build.log"

step "undefined symbols"
# lld reports "undefined symbol: NAME"; busybox's scripts/trylink keeps the
# failed link's output in busybox_unstripped.out, so read both, as build.sh.
cat "$out/build.log" "$build/busybox_unstripped.out" 2>/dev/null \
    | grep -oE "undefined symbol: [^[:space:]]+" \
    | sed -E "s/undefined symbol: //" \
    | sort -u > "$out/undefined-symbols.txt" || true
[ -f "$build/busybox_unstripped.out" ] && cp "$build/busybox_unstripped.out" "$out/link.log"
count=$(wc -l < "$out/undefined-symbols.txt")
echo "$count undefined: $out/undefined-symbols.txt"

if [ "$status" -ne 0 ]; then
    if [ "$count" -eq 0 ]; then
        echo "build-windows.sh: the build failed before linking; the end of the log:" >&2
        grep -E 'error|Error' "$out/build.log" | head -30 >&2
    fi
    exit 1
fi

step "install"
mkdir -p "$out/x86_64"
install -m 755 "$build/busybox" "$out/x86_64/busybox"
file "$out/x86_64/busybox"
echo "$out/x86_64/busybox"
