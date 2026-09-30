#!/bin/sh
# Compares the layouts tools/abi-compare/layouts.c prints when compiled
# against ferrousli's headers and against glibc's, for one architecture:
#
#   tools/abi-compare/compare.sh x86_64|aarch64|armv7a
#
# Both builds link glibc, which only prints: every number is a compile-time
# constant of the headers the program was compiled against. Against glibc on
# ARMv7-A the program is compiled as Debian compiles armhf since 2024, with
# 64-bit time_t and off_t, which is the ABI of the glibc programs ferrousli
# must load there. Prints the differing lines and exits 1 if there are any.
#
# CC_<arch> names the compiler (defaults: cc, aarch64-linux-gnu-gcc,
# arm-linux-gnueabihf-gcc), and RUN_<arch> the command to run a program
# (defaults: none, qemu-aarch64 -L <sysroot>, qemu-arm -L <sysroot>).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
include=$here/../../include
out=${TMPDIR:-/tmp}/ferrousli-abi-compare.$$
mkdir -p "$out"
trap 'rm -rf "$out"' EXIT

arch=${1:?usage: compare.sh x86_64|aarch64|armv7a}
case $arch in
x86_64)
	cc=${CC_x86_64:-cc}
	run=${RUN_x86_64:-}
	glibc_flags=""
	;;
aarch64)
	cc=${CC_aarch64:-aarch64-linux-gnu-gcc}
	run=${RUN_aarch64:-qemu-aarch64 -L /usr/aarch64-linux-gnu}
	glibc_flags=""
	;;
armv7a)
	cc=${CC_armv7a:-arm-linux-gnueabihf-gcc}
	run=${RUN_armv7a:-qemu-arm -L /usr/arm-linux-gnueabihf}
	glibc_flags="-D_FILE_OFFSET_BITS=64 -D_TIME_BITS=64"
	;;
*)
	echo "compare.sh: unknown architecture $arch" >&2
	exit 2
	;;
esac

# shellcheck disable=SC2086 # the flags are words
$cc -std=c11 -O0 $glibc_flags -o "$out/glibc" "$here/layouts.c"
# The compiler's own headers stay: stddef.h, stdarg.h and the like are the
# compiler's on both sides.
$cc -std=c11 -O0 -isystem "$include" -o "$out/ferrousli" "$here/layouts.c"
# shellcheck disable=SC2086 # the runner is words
$run "$out/glibc" > "$out/glibc.txt"
# shellcheck disable=SC2086
$run "$out/ferrousli" > "$out/ferrousli.txt"
if diff -u "$out/glibc.txt" "$out/ferrousli.txt" > "$out/diff.txt"; then
	echo "abi-compare $arch: $(wc -l < "$out/glibc.txt") lines, no difference"
	exit 0
fi
grep '^[-+][^-+]' "$out/diff.txt" | sed 's/^-/glibc     /; s/^+/ferrousli /'
exit 1
