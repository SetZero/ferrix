#!/usr/bin/env bash
# Builds vkgears, the Vulkan gears of mesa-demos, as a static program against
# ferrousli with Mesa's Venus driver linked into it: Vulkan in the guest,
# executed by the host's GPU through virtio-gpu (docs/GPU.md §6.1).
#
#     tools/ports/vkgears/build.sh [--arch x86_64]    # from ferrousli/
#
# Installs, under $FERRIX_PORTS (see ../common.sh):
#   <arch>/bin/vkgears
#
# A Vulkan program links against a loader, which opens a driver with dlopen.
# Ferrix's ports are static, and ferrousli's dlopen refuses a library with
# thread-local storage, which Mesa has. So the driver is linked into the
# program, and vkshim.py writes the loader's part: one function per Vulkan
# call vkgears makes, resolved through the driver's vk_icdGetInstanceProcAddr.
#
# Pinned, and refused if their checksums differ (sha256, taken on
# 2026-09-24 from the release downloads named below, except where foot's
# recipe already pins the same file):
#   libffi 3.5.2, wayland 1.24.0, wayland-protocols 1.45 and libxkbcommon
#   1.11.0, as foot has them: the Wayland client side;
#   libdrm 2.4.134, which Mesa finds and opens the render node with;
#   Mesa 26.2.3, for its Venus driver and nothing else;
#   Vulkan-Headers 1.4.363, the headers vkgears is compiled against and the
#   registry vkshim.py reads prototypes from;
#   libdecor 0.2.3, which vkgears asks for a window's decorations; with no
#   plugins it leaves them to the compositor;
#   mesa-demos 9.0.0, for vkgears.
#
# The build host needs meson, ninja, bison, pkg-config, python3 with mako,
# glslangValidator, and wayland-scanner 1.24.0, the library's own version.
#
# Patched, in patches/:
#   * vkgears-seat.patch: vkgears asked the seat for a keyboard whether or not
#     the seat had one, which is a protocol error that ends the connection
#     when it has none. It now waits for the capability.
#   * vkgears-xkb.patch: vkgears made its keymap context with XKB's default
#     include path, and libxkbcommon refuses to make one when that directory
#     is not there -- Ferrix carries no XKB data -- so the context was null
#     and the first keymap crashed on it. The compositor sends a whole keymap,
#     which needs no include path, so none is asked for.
#   * venus-open-by-name.patch: Venus found its render node through libdrm's
#     drmGetDevices2, which reads sysfs, and Ferrix has none. When that finds
#     nothing, the render nodes are opened by name and the first whose driver
#     is virtio_gpu is taken.
#
# What is built without, and why:
#   * every driver but Venus, and OpenGL, EGL, GBM and GLX: vkgears is Vulkan;
#   * zlib, libdisplay-info, udev and the shader cache in Mesa: Venus sends
#     SPIR-V to the host and compiles nothing, and has no display of its own;
#   * libdecor's plugins, which are loaded with dlopen and draw with cairo or
#     GTK: the compositor decorates the window;
#   * vkgears' X11 support.
#
# Run it with MESA_VK_WSI_DEBUG=sw until the compositor has dmabuf: the frame
# is drawn by the host's GPU and copied into shared memory for the
# compositor, rather than handed over as a GPU buffer (docs/GPU.md §6.1).
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=../common.sh
. "$here/../common.sh"

[ "$arch" = x86_64 ] || fail "vkgears is x86-64 only: Venus needs a KVM host (docs/GPU.md §6.1)"

# name version file url sha256
SOURCES=(
    "libffi 3.5.2 libffi-3.5.2.tar.gz https://github.com/libffi/libffi/releases/download/v3.5.2/libffi-3.5.2.tar.gz f3a3082a23b37c293a4fcd1053147b371f2ff91fa7ea1b2a52e335676bac82dc"
    "wayland 1.24.0 wayland-1.24.0.tar.xz https://gitlab.freedesktop.org/wayland/wayland/-/releases/1.24.0/downloads/wayland-1.24.0.tar.xz 82892487a01ad67b334eca83b54317a7c86a03a89cfadacfef5211f11a5d0536"
    "wayland-protocols 1.45 wayland-protocols-1.45.tar.xz https://gitlab.freedesktop.org/wayland/wayland-protocols/-/releases/1.45/downloads/wayland-protocols-1.45.tar.xz 4d2b2a9e3e099d017dc8107bf1c334d27bb87d9e4aff19a0c8d856d17cd41ef0"
    "libxkbcommon 1.11.0 xkbcommon-1.11.0.tar.gz https://github.com/xkbcommon/libxkbcommon/archive/refs/tags/xkbcommon-1.11.0.tar.gz 78a6b14f16e9a55025978c252e53ce9e16a02bfdb929550b9a0db5af87db7e02"
    "libdrm 2.4.134 libdrm-2.4.134.tar.xz https://dri.freedesktop.org/libdrm/libdrm-2.4.134.tar.xz ac5e74d157830eb8bee44c6a6bf3ad49774ef0dd2a72bdad74a8f20308b52a95"
    "mesa 26.2.3 mesa-26.2.3.tar.xz https://archive.mesa3d.org/mesa-26.2.3.tar.xz 1628058a8d2c0615975de5a15ab7bbb9638c50000b5bed9456ff423ea034a81f"
    "vulkan-headers 1.4.363 Vulkan-Headers-1.4.363.tar.gz https://github.com/KhronosGroup/Vulkan-Headers/archive/refs/tags/v1.4.363.tar.gz cbaf687d3c59b9666fe080e7f8396b6b4f4d344a768ee6b337c59852f4526b68"
    "libdecor 0.2.3 libdecor-0.2.3.tar.gz https://gitlab.freedesktop.org/libdecor/libdecor/-/archive/0.2.3/libdecor-0.2.3.tar.gz 21a471e3f48088d3fd8ecc5999c45258a32198782c0157482f7ebe82de42f79c"
    "mesa-demos 9.0.0 mesa-demos-9.0.0.tar.xz https://archive.mesa3d.org/demos/mesa-demos-9.0.0.tar.xz 3046a3d26a7b051af7ebdd257a5f23bfeb160cad6ed952329cdff1e9f1ed496b"
)

work=$builds/vkgears
src=$ports/src
stage=$work/stage

step "sources"
mkdir -p "$src" "$work"
for line in "${SOURCES[@]}"; do
    read -r name version file url sum <<< "$line"
    fetch "$url" "$src/$file" sha256sum "$sum"
    echo "$name $version"
done

for tool in meson ninja bison pkg-config wayland-scanner glslangValidator python3; do
    command -v "$tool" > /dev/null || fail "no $tool on this host, which the build needs"
done
scanner=$(wayland-scanner --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')
[ "$scanner" = 1.24.0 ] || fail "wayland-scanner is $scanner, not the library's 1.24.0"
python3 -c 'import mako' 2> /dev/null || fail "python3 has no mako, which Mesa's build generates code with"

build_ferrousli
make_compilers
need_cxx

# The source tree of `name`, unpacked afresh into $work/<name>.
unpack() { # name
    local line name version file url sum dir=$work/$1
    for line in "${SOURCES[@]}"; do
        read -r name version file url sum <<< "$line"
        [ "$name" = "$1" ] && break
    done
    [ "$name" = "$1" ] || fail "no source named $1"
    rm -rf "$dir"
    mkdir -p "$dir"
    tar -xf "$src/$file" -C "$dir" --strip-components=1
    echo "$dir"
}

# Run a step's commands with their output in <name>.log, and the log's tail
# on failure.
logged() { # name command...
    local name=$1
    shift
    if ! "$@" > "$work/$name.log" 2>&1; then
        tail -40 "$work/$name.log" >&2
        fail "$name failed; the log is $work/$name.log"
    fi
}

step "meson cross file"
# As foot's: meson is told it cross-compiles, which keeps it to the staging
# tree's libraries, and on x86-64 the programs it builds still run here.
cross_file=$work/cross.ini
cat > "$cross_file" << EOF
[binaries]
c = '$CC'
cpp = '$CXX'
ar = '$AR'
strip = '$STRIP'
pkg-config = 'pkg-config'

[built-in options]
default_library = 'static'
prefer_static = true
c_args = ['-O2']
cpp_args = ['-O2']

[properties]
sys_root = '$stage'
pkg_config_libdir = ['$stage/usr/lib/pkgconfig', '$stage/usr/share/pkgconfig']
needs_exe_wrapper = false

[host_machine]
system = 'linux'
cpu_family = 'x86_64'
cpu = 'x86_64'
endian = 'little'
EOF
rm -rf "$stage"
mkdir -p "$stage"
echo "$cross_file"

# Configure, build and stage one meson project.
meson_port() { # name option...
    local name=$1 dir
    shift
    dir=$(unpack "$name")
    logged "$name" meson setup "$dir/build" "$dir" --cross-file "$cross_file" \
        --prefix=/usr --libdir=lib --sysconfdir=/etc --localstatedir=/var \
        --buildtype=release --wrap-mode=nodownload "$@"
    logged "$name-build" ninja -C "$dir/build" -j"$jobs"
    DESTDIR=$stage logged "$name-install" meson install -C "$dir/build" --no-rebuild
    echo "$name staged"
}

step "libffi"
dir=$(unpack libffi)
logged libffi sh -c "cd '$dir' && ./configure CC='$CC' AR='$AR' CFLAGS=-O2 \
    --prefix=/usr --libdir=/usr/lib --disable-shared --enable-static \
    --disable-docs --disable-multi-os-directory --disable-exec-static-tramp"
logged libffi-build make -C "$dir" -j"$jobs"
logged libffi-install make -C "$dir" install DESTDIR="$stage"
find "$stage" -name '*.la' -delete
echo "libffi staged"

step "wayland"
meson_port wayland -Dlibraries=true -Dscanner=false -Dtests=false \
    -Ddocumentation=false -Ddtd_validation=false

step "wayland-protocols"
meson_port wayland-protocols -Dtests=false

step "libxkbcommon"
meson_port libxkbcommon -Denable-x11=false -Denable-wayland=false -Denable-docs=false \
    -Denable-tools=false -Denable-xkbregistry=false -Denable-bash-completion=false

step "libdrm"
meson_port libdrm -Dintel=disabled -Dradeon=disabled -Damdgpu=disabled \
    -Dnouveau=disabled -Dvmwgfx=disabled -Domap=disabled -Dexynos=disabled \
    -Dfreedreno=disabled -Dtegra=disabled -Dvc4=disabled -Detnaviv=disabled \
    -Dcairo-tests=disabled -Dman-pages=disabled -Dvalgrind=disabled \
    -Dudev=false -Dtests=false -Dinstall-test-programs=false

step "vulkan-headers"
headers=$(unpack vulkan-headers)
mkdir -p "$stage/usr/include"
cp -r "$headers/include/vulkan" "$headers/include/vk_video" "$stage/usr/include/"
echo "vulkan-headers staged"

step "mesa, the Venus driver"
# Meson's last step would link the driver as a shared object, which a
# static-only C library cannot make. What is built instead is everything
# that link reads -- the driver's own objects and the archives beside them --
# found by asking ninja for the link's inputs.
mesa=$(unpack mesa)
logged mesa-patch patch -d "$mesa" -p1 -i "$here/patches/venus-open-by-name.patch"
logged mesa meson setup "$mesa/build" "$mesa" --cross-file "$cross_file" \
    --prefix=/usr --libdir=lib --buildtype=release --wrap-mode=nodownload \
    -Dplatforms=wayland -Dvulkan-drivers=virtio -Dgallium-drivers= -Dopengl=false \
    -Degl=disabled -Dgbm=disabled -Dglx=disabled -Dgles1=disabled -Dgles2=disabled \
    -Dllvm=disabled -Dvalgrind=disabled -Dlibunwind=disabled -Dzstd=disabled \
    -Dzlib=disabled -Ddisplay-info=disabled -Dxmlconfig=disabled -Dexpat=disabled \
    -Dshader-cache=disabled -Dbuild-tests=false -Dtools= -Dvulkan-layers=
driver=src/virtio/vulkan/libvulkan_virtio.so
# Mesa's own archives are implicit inputs, after `|`, beside the staging
# tree's libraries and the host's, which are absolute paths: only paths in
# the build tree are taken, and the link below names the libraries itself.
mapfile -t inputs < <(ninja -C "$mesa/build" -t query "$driver" \
    | sed -n '/^  input:/,/^  outputs:/p' | sed -E 's/^ +(\|\|? )?//' \
    | grep -E '^[^/].*\.(o|a)$')
[ "${#inputs[@]}" -gt 0 ] || fail "ninja named no inputs for $driver"
logged mesa-build ninja -C "$mesa/build" -j"$jobs" "${inputs[@]}"
venus=()
archives=()
for input in "${inputs[@]}"; do
    case $input in
        *.o) venus+=("$mesa/build/$input") ;;
        *.a) archives+=("$mesa/build/$input") ;;
    esac
done
rm -f "$work/libvulkan_virtio.a"
"$AR" rcs "$work/libvulkan_virtio.a" "${venus[@]}"
echo "Venus: ${#venus[@]} objects and ${#archives[@]} archives"

step "libdecor"
# Its core only: four files and the glue for two protocols, compiled here
# rather than through its meson, whose library is a shared one.
decor=$(unpack libdecor)
protocols=$stage/usr/share/wayland-protocols
generated=$work/generated
rm -rf "$generated"
mkdir -p "$generated"
for protocol in stable/xdg-shell/xdg-shell unstable/xdg-decoration/xdg-decoration-unstable-v1; do
    base=${protocol##*/}
    base=${base%-unstable-v1}
    wayland-scanner client-header "$protocols/$protocol.xml" "$generated/$base-client-protocol.h"
    wayland-scanner private-code "$protocols/$protocol.xml" "$generated/$base-protocol.c"
done
cat > "$generated/config.h" << 'EOF'
#define LIBDECOR_PLUGIN_DIR "/usr/lib/libdecor/plugins-1"
#define LIBDECOR_PLUGIN_API_VERSION 1
#define HAVE_MEMFD_CREATE 1
#define HAVE_POSIX_FALLOCATE 1
EOF
flags=(-O2 -D_GNU_SOURCE -I"$generated" -I"$stage/usr/include" -I"$decor/src" -I"$decor")
objects=()
for file in "$decor"/src/{libdecor,libdecor-fallback,os-compatibility,desktop-settings}.c \
    "$generated"/{xdg-shell,xdg-decoration}-protocol.c; do
    object=$generated/$(basename "$file" .c).o
    logged "libdecor-$(basename "$file" .c)" "$CC" "${flags[@]}" -c "$file" -o "$object"
    objects+=("$object")
done
echo "libdecor compiled"

step "vkgears"
demos=$(unpack mesa-demos)
logged vkgears-patch patch -d "$demos" -p1 -i "$here/patches/vkgears-seat.patch"
logged vkgears-xkb-patch patch -d "$demos" -p1 -i "$here/patches/vkgears-xkb.patch"
for shader in gear.vert gear.frag; do
    logged "glslang-$shader" glslangValidator "$demos/src/vulkan/$shader" -V -x \
        -o "$generated/$shader.spv.h"
done
# vkgears' Wayland code says `uint`, which glibc's <stdlib.h> brings in
# through <sys/types.h> and musl's headers, which ferrousli has, do not.
flags+=(-DWAYLAND_SUPPORT -I"$demos/src/util" -I"$demos/src/vulkan" -include sys/types.h)
for file in "$demos"/src/vulkan/{vkgears.c,wsi/wsi.c,wsi/wayland.c} "$demos/src/util/matrix.c"; do
    object=$generated/$(basename "$file" .c).o
    logged "vkgears-$(basename "$file" .c)" "$CC" "${flags[@]}" -c "$file" -o "$object"
    objects+=("$object")
done
nm -u "$generated"/{vkgears,wsi,wayland}.o | awk '{print $2}' | grep '^vk' | sort -u > "$generated/calls"
python3 "$here/vkshim.py" "$headers/registry/vk.xml" "$generated/calls" "$generated/vkshim.c"
logged vkshim "$CC" "${flags[@]}" -c "$generated/vkshim.c" -o "$generated/vkshim.o"
objects+=("$generated/vkshim.o")

# The driver is made one relocatable object, as the shared object would have
# been one library, from what that library's link reads and the way it reads
# it: the driver's own objects and three archives whole, since Mesa finds its
# entry points through tables rather than through references a linker would
# follow, and from the other archives only what those need. Then everything
# Mesa marked hidden is made local to that object, which is what hidden means
# in a shared object: Mesa's own C11 threads, which it builds on every
# platform but Android, would otherwise collide with ferrousli's, and nothing
# else of Mesa's is anyone else's to call. What stays global is what Mesa
# exports, the vk_icd* entry points vkshim.c calls.
#
# --gc-sections then drops what nothing reaches, as the shared object's link
# does -- without it, code for compiling SPIR-V, which Venus leaves to the
# host, would need a compiler that is not built.
whole=("$work/libvulkan_virtio.a")
rest=()
for archive in "${archives[@]}"; do
    case $archive in
        */libvulkan_lite_runtime.a | */libvulkan_lite_instance.a | */libvulkan_wsi.a) whole+=("$archive") ;;
        *) rest+=("$archive") ;;
    esac
done
logged venus-relocatable "${cross}ld" -r -o "$work/venus.o" \
    --whole-archive "${whole[@]}" --no-whole-archive --start-group "${rest[@]}" --end-group
"${cross}objcopy" --localize-hidden "$work/venus.o"
set +e
"$CXX" -Wl,--gc-sections -o "$work/vkgears" "${objects[@]}" "$work/venus.o" \
    -L"$stage/usr/lib" -ldrm -lwayland-client -lffi -lxkbcommon > "$work/vkgears-link.log" 2>&1
status=$?
set -e
undefined_symbols "$work/undefined-symbols.txt" "$work/vkgears-link.log"
if [ "$status" -ne 0 ]; then
    grep -E 'error|undefined' "$work/vkgears-link.log" | head -30 >&2
    fail "vkgears did not link; the log is $work/vkgears-link.log"
fi

step "install"
mkdir -p "$prefix/bin"
install -m 755 -s --strip-program="$STRIP" "$work/vkgears" "$prefix/bin/vkgears"
file "$prefix/bin/vkgears"
