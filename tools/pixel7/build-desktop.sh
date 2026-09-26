#!/bin/sh
# Build the desktop the launcher's VM shows: `desktop.Image`, the Pixel 7
# loader wrapping a kernel and initramfs whose init is the compositor.
#
# Usage: tools/pixel7/build-desktop.sh <out-dir> [scale] [--push]
#
#   out-dir  a new directory; the stage and desktop.Image are written there
#   scale    the monitor's scale, 2 when not given: 1080x2400 at 1 is text
#            a few millimetres high on a 6.3-inch screen
#   --push   copy desktop.Image to /data/local/tmp/ferrix-vm/ on the phone,
#            where the app boots it in place of ferrix.Image
#
# The keymap is US, since the app turns Android's text into US key codes.
# CARGO_TARGET_DIR defaults to one of this worktree's own: two worktrees
# sharing one build each other's xtask.
set -eu
out=${1:?usage: build-desktop.sh <out-dir> [scale] [--push]}
scale=${2:-2}
push=${3:-}
[ "$scale" = --push ] && { push=--push; scale=2; }
[ -e "$out" ] && { echo "$out exists" >&2; exit 1; }

root=$(git rev-parse --show-toplevel)
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.local/share/ferrix/target-$(basename "$root")}"
objcopy="$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy"

mkdir -p "$out"
out=$(cd "$out" && pwd)
cd "$root"
cargo xtask flash --arch aarch64 --release --compositor --size 1080x2400 \
    --scale "$scale" --layout us --wallpaper none --stage "$out/stage"

kernel=$out/stage/FERRIX/KERNEL.ELF
initrd=$out/stage/FERRIX/INITRD.IMG
FERRIX_PIXEL7_KERNEL=$kernel \
FERRIX_PIXEL7_KERNEL_DIGEST=$(sha256sum "$kernel" | cut -d' ' -f1) \
FERRIX_PIXEL7_INITRD=$initrd \
FERRIX_PIXEL7_INITRD_DIGEST=$(sha256sum "$initrd" | cut -d' ' -f1) \
    cargo build -q -p ferrix-boot-pixel7 --target aarch64-unknown-none-softfloat --release
"$objcopy" -O binary "$CARGO_TARGET_DIR/aarch64-unknown-none-softfloat/release/ferrix-boot-pixel7" \
    "$out/desktop.Image"
echo "built $out/desktop.Image (scale $scale)"

if [ "$push" = --push ]; then
    adb -s "${FERRIX_PIXEL7_SERIAL:-28171FDH2001RC}" push "$out/desktop.Image" \
        /data/local/tmp/ferrix-vm/desktop.Image
fi
