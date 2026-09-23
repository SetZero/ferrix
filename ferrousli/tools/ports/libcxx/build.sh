#!/usr/bin/env bash
# Builds LLVM 23.1.1's C++ runtime against ferrousli: libc++, libc++abi and
# libunwind, static, for x86-64.
#
#     tools/ports/libcxx/build.sh            # from ferrousli/
#
# Installs, under $FERRIX_PORTS (see ../common.sh):
#   x86_64/include/c++/v1/      libc++'s headers
#   x86_64/lib/libc++.a, libc++abi.a, libunwind.a
#
# which common.sh's ferrousli-c++ compiles and links C++ programs against.
#
# Pinned, and refused if its checksum differs: llvm-project-23.1.1.src.tar.xz
# from LLVM's GitHub release, by its sha256 as first downloaded on 2026-09-16.
# LLVM publishes a sigstore bundle beside it rather than a checksum. Only the
# runtimes, the CMake modules they include, and LLVM libc, whose shared
# floating-point parser libc++'s from_chars uses, are unpacked.
#
# Why LLVM's runtime and not GCC's libstdc++: libc++ supports musl as a
# configuration of its own (LIBCXX_HAS_MUSL_LIBC), which is what ferrousli's
# headers are, and builds from its own directory with CMake. libstdc++ builds
# only as part of a GCC configured for the target. The compiler is still the
# host's gcc, which libc++ supports; the host has no clang.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=../common.sh
. "$here/../common.sh"
# x86-64 only so far: another architecture needs the target's g++, and its
# CMake cross settings are untried.
[ "$arch" = x86_64 ] || fail "libcxx is built for x86_64 only so far, not for $arch"

LLVM_VERSION=23.1.1
LLVM_TARBALL=llvm-project-$LLVM_VERSION.src.tar.xz
LLVM_URL=https://github.com/llvm/llvm-project/releases/download/llvmorg-$LLVM_VERSION/$LLVM_TARBALL
LLVM_SHA256=ebe9be46fe8756d58c5b198ffad0fa2a766257add81a4dc52179bfacc7888ee6

work=$builds/libcxx
src=$ports/src

step "sources"
mkdir -p "$src" "$work"
fetch "$LLVM_URL" "$src/$LLVM_TARBALL" sha256sum "$LLVM_SHA256"
echo "llvm-project $LLVM_VERSION, verified"

build_ferrousli
make_compilers

step "unpack"
tree=$work/llvm-project
rm -rf "$tree"
mkdir -p "$tree"
top=llvm-project-$LLVM_VERSION.src
tar -xJf "$src/$LLVM_TARBALL" -C "$tree" --strip-components=1 \
    "$top/runtimes" "$top/libcxx" "$top/libcxxabi" "$top/libunwind" \
    "$top/cmake" "$top/llvm/cmake" "$top/llvm/utils/llvm-lit" "$top/third-party" \
    "$top/libc"

step "configure"
build=$work/build
rm -rf "$build"
# The C++ compiler here is the bare one: ferrousli's C headers and no C++
# library, because this is the C++ library being built. CMake compiles its
# checks into static libraries rather than programs, since a program would
# need the runtime that does not exist yet.
if ! cmake -G Ninja -S "$tree/runtimes" -B "$build" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$prefix" \
    -DCMAKE_C_COMPILER="$CC" \
    -DCMAKE_CXX_COMPILER="$CXX_BARE" \
    -DCMAKE_AR="$(command -v ar)" \
    -DCMAKE_RANLIB="$(command -v ranlib)" \
    -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY \
    -DCMAKE_POSITION_INDEPENDENT_CODE=OFF \
    -DLLVM_ENABLE_RUNTIMES="libcxx;libcxxabi;libunwind" \
    -DLLVM_INCLUDE_TESTS=OFF \
    -DLIBCXX_HAS_MUSL_LIBC=ON \
    -DLIBCXX_ENABLE_SHARED=OFF \
    -DLIBCXX_ENABLE_STATIC=ON \
    -DLIBCXX_CXX_ABI=libcxxabi \
    -DLIBCXX_USE_COMPILER_RT=OFF \
    -DLIBCXX_INCLUDE_TESTS=OFF \
    -DLIBCXX_INCLUDE_BENCHMARKS=OFF \
    -DLIBCXX_ENABLE_TIME_ZONE_DATABASE=OFF \
    -DLIBCXX_HARDENING_MODE=none \
    -DLIBCXXABI_ENABLE_SHARED=OFF \
    -DLIBCXXABI_USE_LLVM_UNWINDER=ON \
    -DLIBCXXABI_ENABLE_STATIC_UNWINDER=ON \
    -DLIBCXXABI_USE_COMPILER_RT=OFF \
    -DLIBCXXABI_INCLUDE_TESTS=OFF \
    -DLIBUNWIND_ENABLE_SHARED=OFF \
    -DLIBUNWIND_USE_COMPILER_RT=OFF \
    -DLIBUNWIND_INCLUDE_TESTS=OFF \
    -DLIBUNWIND_INCLUDE_DOCS=OFF \
    > "$work/configure.log" 2>&1; then
    tail -40 "$work/configure.log" >&2
    fail "cmake failed; the log is $work/configure.log"
fi

step "build"
if ! ninja -C "$build" -k 0 -j"$jobs" cxx cxxabi unwind > "$work/build.log" 2>&1; then
    grep -E "error:|FAILED" "$work/build.log" | sort -u | head -60 >&2 || true
    fail "the runtime did not build; the log is $work/build.log"
fi

step "install"
rm -rf "$prefix/include/c++"
ninja -C "$build" install-cxx install-cxxabi install-unwind > "$work/install.log" 2>&1 \
    || fail "install failed; the log is $work/install.log"
ls -l "$prefix"/lib/lib{c++,c++abi,unwind}.a

step "a C++ program"
# Exceptions across a throw and a catch, a static constructor, a thread, a
# locale-aware stream and <filesystem>: the parts of the runtime that lean on
# the C library rather than on the compiler.
check=$work/check
mkdir -p "$check"
cat > "$check/hello.cpp" <<'EOF'
#include <filesystem>
#include <iostream>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>

static std::string made_before_main = "constructed";

int main() {
    std::string caught;
    try {
        throw std::runtime_error("thrown");
    } catch (const std::exception& e) {
        caught = e.what();
    }
    int from_thread = 0;
    std::thread t([&] { from_thread = 42; });
    t.join();
    std::ostringstream out;
    out << made_before_main << ' ' << caught << ' ' << from_thread << ' '
        << std::filesystem::path("/proc/self").filename().string();
    std::cout << out.str() << std::endl;
    return out.str() == "constructed thrown 42 self" ? 0 : 1;
}
EOF
set +e
"$CXX" -std=c++23 -O2 -o "$check/hello" "$check/hello.cpp" > "$check/link.log" 2>&1
status=$?
set -e
undefined_symbols "$work/undefined-symbols.txt" "$check/link.log"
if [ "$status" -ne 0 ]; then
    grep -E 'error' "$check/link.log" | head -20 >&2
    fail "a C++ program does not build against the runtime yet"
fi
"$check/hello"
