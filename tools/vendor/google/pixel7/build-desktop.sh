#!/bin/sh
# Build the desktop the launcher's VM shows: `desktop.Image`, the Pixel 7
# loader wrapping a kernel and initramfs whose init is the compositor.
#
# Usage: tools/vendor/google/pixel7/build-desktop.sh <out-dir> [scale] [--chrome] [--dotfiles]
#            [--bar-zoom=N] [--push] [--reset-home]
#
#   out-dir   a new directory; the stage and desktop.Image are written there
#   scale     the monitor's scale, 2 when not given: 1080x2400 at 1 is text
#             a few millimetres high on a 6.3-inch screen
#   --chrome  Chromium on the desktop, from the volume
#             tools/common/fetch/fetch-chromium-arm64.sh makes, which the app
#             gives the VM as its disk when chromium.img is on the phone;
#             with --push, the volume is copied there too if it is not
#   --dotfiles  this machine's ~/.config/hypr/hyprland.conf and the dotfiles
#             beside it, as `run-compositor --everything` boots them: the
#             desktop runs as the user ferrix (`flash --compositor --session`)
#             and the dotfiles seed its home once, so what is changed there
#             on the phone is kept across new desktops
#   --bar-zoom=N  with --dotfiles, waybar N times the size the PC's files
#             give it, 3 when not given: the bar is drawn at scale 1, so
#             a monitor's bar on the phone's screen is a few millimetres
#             high. It is what the home is seeded with, so a change to it
#             reaches a phone that has a home.img only with --reset-home
#   --push    copy desktop.Image to /data/local/tmp/ferrix-vm/ on the phone,
#             where the app boots it in place of ferrix.Image, and stops
#             the app updating it from GitHub's releases until asked to;
#             with --dotfiles, and no home.img on the phone yet, make one
#             there: the empty `ferrix-home` volume the app gives the VM as
#             a disk, which Ferrix mounts at /home and nothing here replaces
#   --reset-home  make the phone's home.img again, empty, so that the next
#             boot seeds it from the dotfiles anew: what was changed on the
#             phone is lost
#
# The release job (.github/workflows/release.yml) runs this too, once per
# edition: `full` with --chrome, `minimal` without.
#
# The keymap is US, since the app turns Android's text into US key codes.
# CARGO_TARGET_DIR defaults to one of this worktree's own: two worktrees
# sharing one build each other's xtask.
set -eu
out=${1:?usage: build-desktop.sh <out-dir> [scale] [--chrome] [--dotfiles] [--bar-zoom=N] [--push] [--reset-home]}
shift
scale=2 push= chrome= dotfiles= reset_home= bar_zoom=3
for arg in "$@"; do
    case $arg in
        --push) push=1 ;;
        --chrome) chrome=--chrome ;;
        --dotfiles) dotfiles=1 ;;
        --reset-home) reset_home=1 ;;
        --bar-zoom=*) bar_zoom=${arg#--bar-zoom=} ;;
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
if [ -n "$dotfiles" ]; then
    set -- --session --config "$HOME/.config/hypr/hyprland.conf" --bar-zoom "$bar_zoom"
else
    set --
fi
cargo xtask flash --arch aarch64 --release --compositor --size 1080x2400 \
    --scale "$scale" --layout us --wallpaper none $chrome "$@" --stage "$out/stage"

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
    # The phone: FERRIX_PIXEL7_SERIAL, else ~/.local/share/ferrix/pixel7/serial.
    serial=${FERRIX_PIXEL7_SERIAL:-$(cat "$HOME/.local/share/ferrix/pixel7/serial" 2>/dev/null)}
    if [ -z "$serial" ]; then
        echo "no phone named: set FERRIX_PIXEL7_SERIAL or write ~/.local/share/ferrix/pixel7/serial" >&2
        exit 1
    fi
    # A phone behind a hub can drop off USB for a few seconds: wait for it
    # rather than fail a build that took minutes.
    timeout 120 adb -s "$serial" wait-for-device
    adb -s "$serial" push "$out/desktop.Image" /data/local/tmp/ferrix-vm/desktop.Image
    # The app's record of the release it installed: gone, the app takes
    # what is there for this build and does not update it over the top
    # (README.md, "Updates").
    adb -s "$serial" shell rm -f /data/local/tmp/ferrix-vm/release.json
    if [ -n "$dotfiles" ]; then
        # The home: made on the phone from xtask's packed `ferrix-home`
        # fixture, 8 GiB sparse, by writing its few non-zero blocks there
        # rather than pushing 8 GiB. Never made over one that is there
        # unless --reset-home says so: it holds what the user changed.
        vm=/data/local/tmp/ferrix-vm
        there=$(adb -s "$serial" shell "[ -f $vm/home.img ] && echo yes" | tr -d '\r')
        if [ "$there" != yes ] || [ -n "$reset_home" ]; then
            blocks=$out/home-blocks
            python3 - "$root/src/lib/fs/btrfs/testdata/home.img.packed" "$blocks" <<'PY'
import sys
packed = open(sys.argv[1], "rb").read()
record = 8 + 4096
with open(sys.argv[2] + ".bin", "wb") as data, open(sys.argv[2] + ".sh", "w") as script:
    script.write("set -e\nrm -f home.img.new\ntruncate -s 8G home.img.new\n")
    for index in range(len(packed) // record):
        at = index * record
        offset = int.from_bytes(packed[at:at + 8], "little")
        data.write(packed[at + 8:at + record])
        script.write(f"dd if=home-blocks.bin of=home.img.new bs=4096 skip={index} "
                     f"seek={offset // 4096} count=1 conv=notrunc 2>/dev/null\n")
    script.write("rm home-blocks.bin home-blocks.sh\nmv home.img.new home.img\n")
PY
            adb -s "$serial" push "$blocks.bin" "$blocks.sh" "$vm/" >/dev/null
            adb -s "$serial" shell "cd $vm && sh home-blocks.sh"
            echo "made the phone's $vm/home.img (empty: the next boot seeds it)"
        fi
    fi
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
