# What every port shares, sourced by each build.sh: the libraries' under
# tools/ports/ and the ported programs', which are apps (docs/APPS.md):
# pinned downloads, ferrousli's release library, and the compilers that
# build a program against it and nothing else.
#
#     tools/ports/<port>/build.sh                  # from src/user/system/linux/ferrousli/: x86-64
#     tools/ports/<port>/build.sh --arch armv7a    # or aarch64
#
# An app's build.sh, run as `build.sh <arch> <out>`, sets `--arch <arch>`
# before sourcing this.
#
# A port builds a static program against musl's headers in include/, crt1.o
# and libferrousli.a, the way tools/busybox/build.sh builds busybox. x86-64 is
# built with the host's gcc against the host's kernel UAPI headers. AArch64
# and ARMv7-A are cross-compiled with gcc for the target, found as
# $CC_aarch64_unknown_linux_gnu or $CC_armv7_unknown_linux_gnueabihf when set
# (cc-rs's names) and as aarch64-linux-gnu-gcc or arm-linux-gnueabihf-gcc on
# PATH otherwise, with its binutils beside it, against Alpine's linux-headers
# package for the architecture, pinned below as busybox's build pins it. A
# port whose programs are C++ needs the target's g++ as well.
#
# It downloads and builds under $FERRIX_PORTS, by default
# ~/.local/share/ferrix/ports/ferrousli, never inside the repository: x86-64's
# builds in <port>/ there and the others' in build-<arch>/<port>/, and each
# installs into $FERRIX_PORTS/<arch> only once it links. When a link fails,
# the symbols ferrousli does not have yet are written to
# undefined-symbols.txt in the port's build directory and the build exits 1.

here_ports=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ferrousli=$(cd "$here_ports/../.." && pwd)
ports=${FERRIX_PORTS:-$HOME/.local/share/ferrix/ports/ferrousli}
jobs=${JOBS:-$(nproc)}

step() { printf '\n== %s\n' "$*"; }
fail() {
    echo "${0##*/}: $*" >&2
    exit 1
}

# Hold port $1's lock until the script exits. Every checkout's build of a
# port shares its work directory under $builds, which the build empties
# first, so a second build at once would delete the first's objects under
# it; the second waits instead.
lock_port() { # name
    mkdir -p "$builds"
    exec 9> "$builds/$1.lock"
    flock 9 || fail "could not lock $builds/$1.lock"
}

# The architecture: `--arch <name>` first on the port's command line, which
# sourcing this file shifts away.
arch=x86_64
if [ "${1:-}" = --arch ]; then
    arch=${2:-}
    shift 2 || fail "--arch needs aarch64, armv7a or x86_64"
fi
# Alpine v3.22's linux-headers, as tools/busybox/build.sh pins it.
HEADERS=linux-headers-6.14.2-r0.apk
case $arch in
    x86_64)
        target= triple= qemu=
        builds=$ports
        ;;
    aarch64)
        target=aarch64-unknown-linux-gnu triple=aarch64-linux-gnu qemu=qemu-aarch64
        alpine=aarch64
        headers_sha256=08bc7264055d4ceca249e21f47875ccd7ae2dc7eaf49e235a83b1059e06d9089
        builds=$ports/build-$arch
        ;;
    armv7a)
        target=armv7-unknown-linux-gnueabihf triple=arm-linux-gnueabihf qemu=qemu-arm
        alpine=armv7
        headers_sha256=4bc6864f71361fb15a1ca2d96d217343c0e84f2e5dce6518ed84b3f694c0e9df
        builds=$ports/build-$arch
        ;;
    *) fail "no architecture $arch: aarch64, armv7a or x86_64" ;;
esac
prefix=$ports/$arch

# The compilers and binutils: the host's for x86-64, the target's otherwise.
# $cxx is empty where the target has no g++.
if [ -z "$target" ]; then
    cc=gcc cxx=g++ cross=
else
    cc_var=CC_${target//-/_}
    cc=${!cc_var:-$triple-gcc}
    cxx=$triple-g++
    command -v "$cxx" > /dev/null || cxx=
    cross=$triple-
    command -v "$cc" > /dev/null \
        || fail "no $cc: install gcc for $triple (Debian and Ubuntu: gcc-$triple), or name one in \$$cc_var"
fi
AR=${cross}ar
STRIP=${cross}strip

# Run a program built for $arch: directly on x86-64, under qemu-user for the
# others, and not at all, saying so, where qemu-user is not installed.
run_built() {
    if [ -z "$qemu" ]; then
        "$@"
    elif command -v "$qemu" > /dev/null; then
        "$qemu" "$@"
    else
        echo "not run: no $qemu to run $1 with"
    fi
}

fetch() { # url file checksum-command checksum
    local url=$1 file=$2 tool=$3 sum=$4
    if [ ! -f "$file" ]; then
        curl -fsSL --max-time 900 -o "$file.part" "$url"
        mv "$file.part" "$file"
    fi
    local got
    got=$("$tool" "$file" | cut -d' ' -f1)
    if [ "$got" != "$sum" ]; then
        echo "${0##*/}: $file does not match its pinned $tool" >&2
        echo "  expected $sum" >&2
        echo "  got      $got" >&2
        exit 1
    fi
}

# LLVM's prebuilt release for x86-64 Linux, pinned by its sha256 as first
# downloaded on 2026-09-17; LLVM publishes a sigstore bundle beside it rather
# than a checksum. Only clang and its built-in headers are unpacked, under
# $ports/llvm, once. Sets $clang_bin.
LLVM_RELEASE=LLVM-23.1.1-Linux-X64
LLVM_RELEASE_URL=https://github.com/llvm/llvm-project/releases/download/llvmorg-23.1.1/$LLVM_RELEASE.tar.xz
LLVM_RELEASE_SHA256=832aeb58d105de1cabc7b982dd2c65de0610f7377df48ae8fc2dd8e97420a15c
fetch_clang() {
    clang_bin=$ports/llvm/$LLVM_RELEASE/bin
    if [ -x "$clang_bin/clang++" ]; then
        return
    fi
    step "clang, for a C++ runtime this gcc cannot build"
    mkdir -p "$ports/src" "$ports/llvm"
    fetch "$LLVM_RELEASE_URL" "$ports/src/$LLVM_RELEASE.tar.xz" sha256sum "$LLVM_RELEASE_SHA256"
    rm -rf "$ports/llvm/$LLVM_RELEASE"
    tar -xJf "$ports/src/$LLVM_RELEASE.tar.xz" -C "$ports/llvm" --wildcards \
        "$LLVM_RELEASE/bin/clang*" "$LLVM_RELEASE/lib/clang/*"
    [ -x "$clang_bin/clang++" ] || fail "$LLVM_RELEASE has no bin/clang++"
}

# Meson from its own release where the host has none, or one older than the
# version given: Ubuntu 24.04's is 1.3.2, and fontconfig 2.17 wants 1.6.1.
# Meson is Python and runs from its unpacked tree, so this needs no root.
# Pinned by its sha256 as first downloaded on 2026-10-01. Puts a `meson` on
# PATH that runs it.
MESON_VERSION=1.10.1
MESON_URL=https://github.com/mesonbuild/meson/releases/download/$MESON_VERSION/meson-$MESON_VERSION.tar.gz
MESON_SHA256=c42296f12db316a4515b9375a5df330f2e751ccdd4f608430d41d7d6210e4317
need_meson() { # minimum-version
    local have
    have=$(meson --version 2> /dev/null || true)
    if [ -n "$have" ] && printf '%s\n%s\n' "$1" "$have" | sort -V -C; then
        return
    fi
    local dir=$ports/meson tree=$ports/meson/meson-$MESON_VERSION
    if [ ! -f "$tree/meson.py" ]; then
        step "meson $MESON_VERSION, since the host's is ${have:-missing} and this wants $1"
        mkdir -p "$ports/src" "$dir"
        fetch "$MESON_URL" "$ports/src/meson-$MESON_VERSION.tar.gz" sha256sum "$MESON_SHA256"
        rm -rf "$tree"
        tar -xzf "$ports/src/meson-$MESON_VERSION.tar.gz" -C "$dir"
    fi
    mkdir -p "$dir/bin"
    printf '#!/bin/sh\nexec python3 %s/meson.py "$@"\n' "$tree" > "$dir/bin/meson"
    chmod +x "$dir/bin/meson"
    export PATH=$dir/bin:$PATH
    echo "meson $(meson --version), from $tree"
}

# wayland-scanner of the version given, where the host's is another: the code
# it generates has to match the library it is built with, and Ubuntu 24.04's
# is 1.22.0. Built for the host from that wayland's own tarball, which the
# caller has fetched, scanner only, against the host's expat; needs meson
# (need_meson) first. Puts it on PATH, and its wayland-scanner.pc where a
# cross build's meson looks for the build machine's: wayland's own meson
# finds the scanner through pkg-config, not PATH.
need_wayland_scanner() { # version tarball
    local want=$1 tarball=$2 have
    have=$(wayland-scanner --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || true)
    [ "$have" = "$want" ] && return
    local dir=$ports/wayland-scanner-$want
    if [ ! -x "$dir/bin/wayland-scanner" ]; then
        step "wayland-scanner $want for this host, whose is ${have:-missing}"
        rm -rf "$dir"
        mkdir -p "$dir/src"
        tar -xJf "$tarball" -C "$dir/src" --strip-components=1
        { CC=gcc meson setup "$dir/build" "$dir/src" --prefix="$dir" --libdir=lib \
            -Dlibraries=false -Ddocumentation=false -Dtests=false -Ddtd_validation=false \
            && ninja -C "$dir/build" install; } > "$dir/build.log" 2>&1 \
            || fail "wayland-scanner $want did not build; the log is $dir/build.log"
    fi
    export PATH=$dir/bin:$PATH
    export PKG_CONFIG_PATH_FOR_BUILD=$dir/lib/pkgconfig${PKG_CONFIG_PATH_FOR_BUILD:+:$PKG_CONFIG_PATH_FOR_BUILD}
    echo "$(wayland-scanner --version 2>&1), from $dir"
}

# glslangValidator where the host has none: Ubuntu packages it apart
# (glslang-tools), and installing that needs root. Built for the host with
# its gcc, cmake and ninja from Khronos' 16.2.0 source, pinned by its sha256
# as first downloaded on 2026-10-01, without the optimizer, which needs
# SPIRV-Tools and which a compile to SPIR-V does not use. Puts it on PATH.
GLSLANG_VERSION=16.2.0
GLSLANG_URL=https://github.com/KhronosGroup/glslang/archive/refs/tags/$GLSLANG_VERSION.tar.gz
GLSLANG_SHA256=01985335785c97906a91afe3cb5ee015997696181ec6c125bab5555602ba08e2
need_glslang() {
    command -v glslangValidator > /dev/null && return
    local dir=$ports/glslang-$GLSLANG_VERSION
    if [ ! -x "$dir/bin/glslangValidator" ]; then
        step "glslang $GLSLANG_VERSION for this host, which has no glslangValidator"
        command -v cmake > /dev/null || fail "no cmake on this host, which building glslang needs"
        mkdir -p "$ports/src"
        fetch "$GLSLANG_URL" "$ports/src/glslang-$GLSLANG_VERSION.tar.gz" sha256sum "$GLSLANG_SHA256"
        rm -rf "$dir"
        mkdir -p "$dir/src"
        tar -xzf "$ports/src/glslang-$GLSLANG_VERSION.tar.gz" -C "$dir/src" --strip-components=1
        { CC=gcc CXX=g++ cmake -G Ninja -S "$dir/src" -B "$dir/build" \
            -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$dir" \
            -DENABLE_OPT=OFF -DGLSLANG_TESTS=OFF -DGLSLANG_ENABLE_INSTALL=ON \
            -DENABLE_GLSLANG_BINARIES=ON -DBUILD_SHARED_LIBS=OFF \
            && ninja -C "$dir/build" -j"$jobs" install; } > "$dir/build.log" 2>&1 \
            || fail "glslang $GLSLANG_VERSION did not build; the log is $dir/build.log"
    fi
    export PATH=$dir/bin:$PATH
    echo "glslangValidator $GLSLANG_VERSION, from $dir"
}

# Build ferrousli in the release profile and set $lib and $crt1.
build_ferrousli() {
    step "ferrousli, release"
    local target_dir=${CARGO_TARGET_DIR:-$ferrousli/target} release
    if [ -n "$target" ]; then
        cargo build --release --lib --target "$target" --manifest-path "$ferrousli/Cargo.toml"
        release=$target_dir/$target/release
    else
        cargo build --release --lib --manifest-path "$ferrousli/Cargo.toml"
        release=$target_dir/release
    fi
    lib=$release/libferrousli.a
    crt1=$(ls -t "$release"/build/ferrousli-*/out/crt1.o | head -1)
    [ -f "$lib" ] && [ -f "$crt1" ] || fail "cargo built no libferrousli.a and crt1.o"
    echo "$lib"
    echo "$crt1"
}

# Write $ports/bin/ferrousli-cc, ferrousli-c++ and ferrousli-c++-bare, which
# compile against ferrousli's headers and link against crt1.o and
# libferrousli.a, set $CC, $CXX and $CXX_BARE to them, and put them on PATH.
# $CC_CXX is the C compiler of the C++ ones' family, for a build that mixes
# the two languages and gives both one set of warnings: $CC itself unless
# the C++ compilers are clang.
# Needs build_ferrousli first.
#
# The kernel's UAPI headers (<linux/*>, <asm/*>) belong to no C library; the
# host's are used from a directory holding only them, as busybox's build does,
# so that -nostdinc keeps the host's C headers out. musl's headers come first,
# then the UAPI headers, then the compiler's own directory, for intrinsics
# only.
#
# The C++ compiler adds $prefix/include/c++/v1, where tools/ports/libcxx
# installs libc++'s headers, ahead of all of them, and links libc++,
# libc++abi and libunwind from $prefix/lib. It is usable once that port is
# built. The bare C++ compiler has no C++ library at all, for building that
# port. Both link with --eh-frame-hdr, which gcc leaves out of a static link:
# libunwind finds a program's unwind tables through the header it makes. And
# both link gcc's crtbeginT.o and crtend.o, as gcc's own static link does:
# they define __dso_handle, which every C++ static destructor is registered
# against, and which belongs to the compiler rather than to a C library.
#
# What the wrappers add after the caller's arguments comes after `-x none`, so
# a caller that compiles from standard input with `-x c++ -` does not make gcc
# read crtend.o and the libraries as C++ source.
#
# The C++ compilers drive the host's g++ when it is gcc 15 or newer, and
# otherwise clang from LLVM's own release (fetch_clang): LLVM 23's libc++ uses
# compiler built-ins older compilers lack, and WSL's Ubuntu 24.04, where these
# scripts run on Windows, has gcc 13. Either way $CC is gcc, and libgcc,
# crtbeginT.o and crtend.o are gcc's.
#
# Empty libm, libpthread and the rest stand in for the libraries a configure
# script asks for by name: ferrousli is one library, as musl is.
#
# On AArch64 and ARMv7-A the UAPI headers are Alpine's, unpacked from the
# pinned package, and the C++ compilers are written only where the target
# has a g++; $CXX and $CXX_BARE are empty otherwise.
make_compilers() {
    step "compilers"
    local bin=$builds/bin uapi=$builds/uapi stubs=$builds/stub-libs
    mkdir -p "$bin" "$stubs"
    rm -rf "$uapi"
    mkdir -p "$uapi"
    if [ -z "$target" ]; then
        local d
        for d in linux asm-generic mtd scsi sound video drm rdma misc; do
            [ -d "/usr/include/$d" ] && ln -s "/usr/include/$d" "$uapi/$d"
        done
        ln -s /usr/include/x86_64-linux-gnu/asm "$uapi/asm"
    else
        local apk=$ports/src/${HEADERS%.apk}-$alpine.apk headers=$builds/linux-headers
        mkdir -p "$ports/src"
        fetch "https://dl-cdn.alpinelinux.org/alpine/v3.22/main/$alpine/$HEADERS" \
            "$apk" sha256sum "$headers_sha256"
        rm -rf "$headers"
        mkdir -p "$headers"
        tar --warning=no-unknown-keyword -xzf "$apk" -C "$headers" usr/include
        rmdir "$uapi"
        ln -s "$headers/usr/include" "$uapi"
        [ -f "$uapi/asm/unistd.h" ] || fail "Alpine's $HEADERS for $alpine has no asm/unistd.h"
    fi
    local l
    for l in m crypt resolv rt pthread dl util; do
        [ -f "$stubs/lib$l.a" ] || "$AR" rc "$stubs/lib$l.a"
    done
    local compiler_include crtbegin crtend libgcc_dir
    compiler_include=$("$cc" -print-file-name=include)
    crtbegin=$("$cc" -print-file-name=crtbeginT.o)
    crtend=$("$cc" -print-file-name=crtend.o)
    libgcc_dir=$(dirname "$("$cc" -print-libgcc-file-name)")
    local cxx_driver=$cxx cxx_include=$compiler_include cxx_c_driver=
    if [ -z "$target" ] && [ "$(g++ -dumpversion | cut -d. -f1)" -lt 15 ]; then
        fetch_clang
        # musl's triple, which ferrousli's headers are: clang names its
        # target, and btop's Makefile refuses a static link for glibc's.
        cxx_driver="$clang_bin/clang++ --target=x86_64-linux-musl"
        cxx_c_driver="$clang_bin/clang --target=x86_64-linux-musl"
        cxx_include=$("$clang_bin/clang" -print-resource-dir)/include
        echo "g++ $(g++ -dumpversion) is older than 15: C++ with $cxx_driver"
    fi

    CXX= CXX_BARE=
    local name driver kind include
    for name in ferrousli-cc ferrousli-cxx-cc ferrousli-c++ ferrousli-c++-bare; do
        case $name in
            ferrousli-cc) driver=$cc kind=c include=$compiler_include ;;
            ferrousli-cxx-cc) driver=$cxx_c_driver kind=c include=$cxx_include ;;
            ferrousli-c++) driver=$cxx_driver kind=c++ include=$cxx_include ;;
            ferrousli-c++-bare) driver=$cxx_driver kind=bare include=$cxx_include ;;
        esac
        if [ -z "$driver" ]; then
            rm -f "$bin/$name"
            continue
        fi
        cat > "$bin/$name" <<EOF
#!/usr/bin/env bash
# Generated by src/user/system/linux/ferrousli/tools/ports/common.sh.
link=1
for a in "\$@"; do
    case "\$a" in -c|-S|-E|-M|-MM|-r|-print-*|--version|-v|-dumpversion|-dumpmachine) link=0 ;; esac
done
common=(-nostdinc -isystem "$ferrousli/include" -isystem "$uapi" -isystem "$include")
cxx=()
cxxlibs=()
case $kind in
    c++)
        cxx=(-nostdinc++ -isystem "$prefix/include/c++/v1")
        cxxlibs=(-Wl,--eh-frame-hdr -L"$prefix/lib" -lc++ -lc++abi -lunwind)
        ;;
    bare)
        cxx=(-nostdinc++)
        cxxlibs=(-Wl,--eh-frame-hdr)
        ;;
esac
begin=()
end=()
if [ $kind != c ]; then
    begin=("$crtbegin")
    end=("$crtend")
fi
if [ \$link = 1 ]; then
    exec $driver "\${cxx[@]}" "\${common[@]}" -static -no-pie -nostdlib -L"$stubs" -L"$prefix/lib" -L"$libgcc_dir" \\
        "$crt1" "\${begin[@]}" "\$@" -x none "\${cxxlibs[@]}" -Wl,--start-group "$lib" -lgcc -Wl,--end-group "\${end[@]}"
else
    exec $driver "\${cxx[@]}" "\${common[@]}" "\$@"
fi
EOF
        chmod +x "$bin/$name"
    done
    export PATH=$bin:$PATH
    CC=$bin/ferrousli-cc CC_CXX=$bin/ferrousli-cc
    [ -z "$cxx_c_driver" ] || CC_CXX=$bin/ferrousli-cxx-cc
    if [ -n "$cxx" ]; then
        CXX=$bin/ferrousli-c++
        CXX_BARE=$bin/ferrousli-c++-bare
    fi
    echo "$CC"
    echo "${CXX:-no C++ compiler for $arch}"
    echo "${CXX_BARE:-}"
}

# Fail unless make_compilers wrote the C++ compilers, for a port that needs
# them.
need_cxx() {
    [ -n "${CXX:-}" ] || fail "no g++ for $triple, which this port needs (Debian and Ubuntu: g++-$triple)"
}

# Write the symbols a failed link left undefined, from the logs given, to
# $1 and say how many there are.
undefined_symbols() { # list log...
    local list=$1
    shift
    cat "$@" 2>/dev/null \
        | grep -oE "undefined reference to \`[^']+'" \
        | sed -E "s/undefined reference to \`([^']+)'/\1/" \
        | sort -u > "$list" || true
    echo "$(wc -l < "$list") undefined: $list"
}
