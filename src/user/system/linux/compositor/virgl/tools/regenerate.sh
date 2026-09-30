#!/bin/sh
# Regenerate src/tgsi/*.tgsi from shaders/*.
#
# virgl carries a shader as TGSI assembly text, and the compositor has no
# GLSL compiler to make it with: Mesa is the compiler, and there is no Mesa
# on Ferrix. So the shaders are written in GLSL, compiled *here*, on a Linux
# host, by Mesa's own virgl driver -- run against virglrenderer's test server
# in place of a guest's virtio-gpu -- and the TGSI text that driver sends is
# what is committed. It is the text a Linux guest's Mesa would have sent for
# the same shader, which is as well tested as TGSI for virglrenderer gets.
#
# Needs: cc, EGL and GLES headers, a Mesa built with virgl's vtest winsys
# (`GALLIUM_DRIVER=virpipe`), and `virgl_test_server` (Debian: virgl-server).
# Run it after changing anything under shaders/, and commit what it wrote;
# tests/host.rs then runs the result on the host's GL.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
shaders=$here/../shaders
out=$here/../src/tgsi
work=$(mktemp -d)
trap 'kill "$server" 2>/dev/null || true; rm -rf "$work"' EXIT

cc -O1 -o "$work/glsl2tgsi" "$here/glsl2tgsi.c" -lEGL -lGLESv2

virgl_test_server --no-fork --use-egl-surfaceless --multi-clients \
    --socket-path "$work/vtest.sock" >"$work/server.log" 2>&1 &
server=$!
while [ ! -S "$work/vtest.sock" ]; do sleep 0.1; done

# GLSL ES has no #include: a line naming a file is replaced by the file.
expand() {
    while IFS= read -r line; do
        case $line in
            '#include "'*'"') included=${line#\#include \"}; cat "$shaders/${included%\"}" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done <"$1"
}

# Mesa, not whatever vendor's EGL the host prefers, and its virgl driver
# over the socket rather than a GPU of the host's.
run() {
    __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json \
    LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=virpipe VTEST_SOCKET_NAME="$work/vtest.sock" \
    VIRGL_DEBUG=tgsi "$work/glsl2tgsi" "$1" "$2" 2>&1
}

# The stage's text out of the driver's log: from its first line to END.
stage() {
    sed -n 's/^MESA: info: //p' | awk -v stage="$1" '
        $0 == stage { on = 1 }
        on { print }
        on && $0 ~ /^ *[0-9]+: END$/ { exit }'
}

expand "$shaders/quad.vert" >"$work/quad.vert"
mkdir -p "$out"
for fragment in "$shaders"/*.frag; do
    name=$(basename "$fragment" .frag)
    expand "$fragment" >"$work/$name.frag"
    run "$work/quad.vert" "$work/$name.frag" >"$work/$name.log" || {
        cat "$work/$name.log" >&2
        exit 1
    }
    stage FRAG <"$work/$name.log" >"$out/$name.tgsi"
    [ -s "$out/$name.tgsi" ] || { echo "$name: the driver printed no TGSI" >&2; exit 1; }
    # The vertex shader from the pair that reads both of its outputs: a
    # linker drops an output nothing reads, and every pair shares this one.
    if [ "$name" = surface ]; then
        stage VERT <"$work/$name.log" >"$out/quad.tgsi"
    fi
    echo "  $name"
done
[ -s "$out/quad.tgsi" ] || { echo "no vertex shader was printed" >&2; exit 1; }
