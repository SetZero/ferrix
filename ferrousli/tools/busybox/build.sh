#!/usr/bin/env bash
# Builds busybox 1.37.0 as a static program against ferrousli: musl's
# headers, crt1.o and libferrousli.a, and nothing from the host's C library.
#
#     tools/busybox/build.sh                  # from ferrousli/: x86-64
#     tools/busybox/build.sh --arch aarch64   # or armv7a
#
# x86-64 is built with the host's cc against the host's kernel UAPI headers.
# AArch64 and ARMv7-A are cross-compiled with gcc for the target, found as
# $CC_aarch64_unknown_linux_gnu or $CC_armv7_unknown_linux_gnueabihf when
# set (cc-rs's names) and as aarch64-linux-gnu-gcc or arm-linux-gnueabihf-gcc
# on PATH otherwise, with its binutils beside it. Their UAPI headers are
# Alpine's linux-headers package for the architecture, pinned below by
# sha256 as build-windows.sh pins x86-64's, so no Arm sysroot is needed.
#
# sources.sh pins the busybox tarball and Alpine's config for it, and
# config.sh lists the changes made to that config, and why. On Windows,
# build-windows.sh builds the same program from the same sources.
#
# Everything is downloaded to and built under $FERRIX_BUSYBOX, by default
# ~/.local/share/ferrix/busybox/ferrousli, never inside the repository. The
# binary is installed as $FERRIX_BUSYBOX/<arch>/busybox, only if it links.
#
# When the link fails, the symbols busybox needs and ferrousli does not yet
# have are written, sorted, to $FERRIX_BUSYBOX/undefined-symbols.txt, and the
# script exits 1. That list is ferrousli's plan for busybox.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ferrousli=$(cd "$here/../.." && pwd)
out=${FERRIX_BUSYBOX:-$HOME/.local/share/ferrix/busybox/ferrousli}
src=$out/src
jobs=${JOBS:-4}

arch=x86_64
if [ "${1:-}" = --arch ]; then
    arch=$2
    shift 2
fi
# Alpine v3.22's linux-headers, as first downloaded, for the Arm targets.
HEADERS=linux-headers-6.14.2-r0.apk
case $arch in
x86_64)
    target=
    triple=
    build=$out/build
    ;;
aarch64)
    target=aarch64-unknown-linux-gnu
    triple=aarch64-linux-gnu
    alpine=aarch64
    headers_sha256=08bc7264055d4ceca249e21f47875ccd7ae2dc7eaf49e235a83b1059e06d9089
    build=$out/build-$arch
    ;;
armv7a)
    target=armv7-unknown-linux-gnueabihf
    triple=arm-linux-gnueabihf
    alpine=armv7
    headers_sha256=4bc6864f71361fb15a1ca2d96d217343c0e84f2e5dce6518ed84b3f694c0e9df
    build=$out/build-$arch
    ;;
*)
    echo "build.sh: no architecture $arch" >&2
    exit 2
    ;;
esac

# The compiler: the host's cc for x86-64, the target's gcc for the others.
if [ -z "$target" ]; then
    cc=cc
    cross=
else
    cc_var=CC_${target//-/_}
    cc=${!cc_var:-$triple-gcc}
    cross=$triple-
    if ! command -v "$cc" > /dev/null; then
        echo "build.sh: no $cc: install gcc for $triple (Debian and Ubuntu: gcc-$triple)," >&2
        echo "  or name one in \$$cc_var" >&2
        exit 1
    fi
fi

step() { printf '\n== %s\n' "$*"; }

# shellcheck source=sources.sh
. "$here/sources.sh"

step "sources"
fetch_sources "$src"
if [ -n "$target" ]; then
    fetch "https://dl-cdn.alpinelinux.org/alpine/v3.22/main/$alpine/$HEADERS" \
        "$src/${HEADERS%.apk}-$alpine.apk" sha256sum "$headers_sha256"
    echo "Alpine's $HEADERS for $alpine, verified"
fi

step "ferrousli, release"
# Where cargo puts the library: the caller's CARGO_TARGET_DIR when set.
target_dir=${CARGO_TARGET_DIR:-$ferrousli/target}
if [ -n "$target" ]; then
    cargo build --release --lib --target "$target" --manifest-path "$ferrousli/Cargo.toml"
    release=$target_dir/$target/release
else
    cargo build --release --lib --manifest-path "$ferrousli/Cargo.toml"
    release=$target_dir/release
fi
lib=$release/libferrousli.a
crt1=$(ls -t "$release"/build/ferrousli-*/out/crt1.o | head -1)
[ -f "$lib" ] && [ -f "$crt1" ]
echo "$lib"
echo "$crt1"

step "include directories"
# busybox includes the kernel's UAPI headers (<linux/*>, <asm/*>), which belong
# to no C library. The host's are used, from a directory holding only them, so
# that -nostdinc keeps the host's C headers out. musl's headers come first,
# the UAPI headers next, and the compiler's own directory last, for intrinsics
# only: every name musl's headers provide is found in them first.
if [ -z "$target" ]; then
    uapi=$out/uapi
    rm -rf "$uapi"
    mkdir -p "$uapi"
    for d in linux asm-generic mtd scsi sound video drm rdma misc; do
        [ -d "/usr/include/$d" ] && ln -s "/usr/include/$d" "$uapi/$d"
    done
    ln -s /usr/include/x86_64-linux-gnu/asm "$uapi/asm"
else
    headers=$out/linux-headers-$arch
    rm -rf "$headers"
    mkdir -p "$headers"
    tar --warning=no-unknown-keyword -xzf "$src/${HEADERS%.apk}-$alpine.apk" -C "$headers" usr/include
    uapi=$headers/usr/include
    [ -f "$uapi/asm/unistd.h" ]
fi
compiler_include=$("$cc" -print-file-name=include)

# Empty archives for the libraries busybox's link script asks for. ferrousli
# is one library, as musl is; these only stop the script refusing a library
# it cannot find before it has said what is undefined.
stubs=$out/stub-libs
mkdir -p "$stubs"
for l in m crypt resolv rt pthread dl util; do
    [ -f "$stubs/lib$l.a" ] || ar rc "$stubs/lib$l.a"
done

# The compiler busybox is built with: the host's cc or the target's gcc, told
# where ferrousli is.
wrapper=$out/ferrousli-cc${target:+-$arch}
cat > "$wrapper" <<EOF
#!/usr/bin/env bash
# Generated by ferrousli/tools/busybox/build.sh.
link=1
for a in "\$@"; do
    # -r links one directory's objects into a built-in.o. That is not a
    # program: giving it crt1.o puts a second _start in every binary the
    # object reaches, and the final link then fails on the duplicate.
    case "\$a" in -c|-S|-E|-M|-MM|-r) link=0 ;; esac
done
common=(-nostdinc -isystem "$ferrousli/include" -isystem "$uapi" -isystem "$compiler_include")
if [ \$link = 1 ]; then
    exec "$cc" "\${common[@]}" -static -no-pie -nostdlib -L"$stubs" "$crt1" "\$@" "$lib"
else
    exec "$cc" "\${common[@]}" "\$@"
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
# CROSS_COMPILE names the target's binutils, which kbuild strips and
# archives with; CC is the wrapper either way.
make -C "$build" CROSS_COMPILE="$cross" CC="$wrapper" HOSTCC=cc oldconfig </dev/null > "$out/oldconfig.log" 2>&1
grep -E '^CONFIG_(STATIC|PIE)=|^# CONFIG_(STATIC|PIE) is not set' "$build/.config"

step "build"
set +e
make -C "$build" -j"$jobs" CROSS_COMPILE="$cross" CC="$wrapper" HOSTCC=cc V=1 busybox > "$out/build.log" 2>&1
status=$?
set -e
echo "make exited $status; log in $out/build.log"

step "undefined symbols"
# busybox's scripts/trylink keeps the failed link's output in
# busybox_unstripped.out rather than in make's log, so read both.
cat "$out/build.log" "$build/busybox_unstripped.out" 2>/dev/null \
    | grep -oE "undefined reference to \`[^']+'" \
    | sed -E "s/undefined reference to \`([^']+)'/\1/" \
    | sort -u > "$out/undefined-symbols.txt" || true
[ -f "$build/busybox_unstripped.out" ] && cp "$build/busybox_unstripped.out" "$out/link.log"
count=$(wc -l < "$out/undefined-symbols.txt")
echo "$count undefined: $out/undefined-symbols.txt"

if [ "$status" -ne 0 ]; then
    if [ "$count" -eq 0 ]; then
        echo "build.sh: the build failed before linking; the end of the log:" >&2
        grep -E 'error|Error' "$out/build.log" | head -30 >&2
    fi
    exit 1
fi

step "install"
mkdir -p "$out/$arch"
install -m 755 "$build/busybox" "$out/$arch/busybox"
file "$out/$arch/busybox"
echo "$out/$arch/busybox"
