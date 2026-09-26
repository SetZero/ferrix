#!/bin/sh
# Build the desktop the launcher's VM shows: `desktop.Image`, the Pixel 7
# loader wrapping a kernel and initramfs whose init is the compositor.
#
# Usage: tools/pixel7/build-desktop.sh <out-dir> [scale] [--chrome] [--push]
#
#   out-dir   a new directory; the stage and desktop.Image are written there
#   scale     the monitor's scale, 2 when not given: 1080x2400 at 1 is text
#             a few millimetres high on a 6.3-inch screen
#   --chrome  Chromium on the desktop, from the volume
#             scripts/fetch/fetch-chromium-arm64.sh makes, which the app
#             gives the VM as its disk when chromium.img is on the phone;
#             with --push, the volume is copied there too if it is not
#   --push    copy desktop.Image to /data/local/tmp/ferrix-vm/ on the phone,
#             where the app boots it in place of ferrix.Image
#
# The keymap is US, since the app turns Android's text into US key codes.
# CARGO_TARGET_DIR defaults to one of this worktree's own: two worktrees
# sharing one build each other's xtask.
set -eu
out=${1:?usage: build-desktop.sh <out-dir> [scale] [--chrome] [--push]}
shift
scale=2 push= chrome=
for arg in "$@"; do
    case $arg in
        --push) push=1 ;;
        --chrome) chrome=--chrome ;;
        *) scale=$arg ;;
    esac
done
[ -e "$out" ] && { echo "$out exists" >&2; exit 1; }

root=$(git rev-parse --show-toplevel)
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.local/share/ferrix/target-$(basename "$root")}"
objcopy="$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy"

mkdir -p "$out"
out=$(cd "$out" && pwd)
cd "$root"
cargo xtask flash --arch aarch64 --release --compositor --size 1080x2400 \
    --scale "$scale" --layout us --wallpaper none $chrome --stage "$out/stage"

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

if [ -n "$push" ]; then
    serial=${FERRIX_PIXEL7_SERIAL:-28171FDH2001RC}
    adb -s "$serial" push "$out/desktop.Image" /data/local/tmp/ferrix-vm/desktop.Image
    if [ -n "$chrome" ]; then
        volume=${FERRIX_CHROMIUM_VOLUME:-$HOME/.local/share/ferrix/chromium-arm64}/chromium.img
        # The phone's copy is written to: Chromium's profile and caches.
        # Pushed once, and again only when it was made anew here.
        there=$(adb -s "$serial" shell "stat -c %s /data/local/tmp/ferrix-vm/chromium.img 2>/dev/null" | tr -d '\r')
        if [ "$there" != "$(stat -c %s "$volume")" ]; then
            adb -s "$serial" push "$volume" /data/local/tmp/ferrix-vm/chromium.img
        fi
    fi
fi
