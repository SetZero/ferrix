#!/usr/bin/env bash
# Builds foot 1.24.0, a Wayland terminal nobody here wrote, as a static
# program against ferrousli, with every library it links and a font for it
# to draw with. It is docs/CHROME.md §6's first milestone: a foreign toolkit
# client on Ferrix's compositor, proving the client libraries a browser
# needs for a fraction of a browser's cost.
#
#     tools/ports/foot/build.sh [--arch <arch>]    # from ferrousli/
#
# Installs, under $FERRIX_PORTS (see ../common.sh):
#   <arch>/bin/foot, <arch>/bin/footclient
#   <arch>/etc/fonts/fonts.conf, fontconfig's configuration
#   <arch>/usr/share/fonts/dejavu/DejaVuSansMono*.ttf, the only font
#
# The libraries are built into a staging tree, <arch>/foot/stage/usr under
# the port's build directory, and installed nowhere an image looks: foot is
# linked against them statically, so the guest needs none of them.
#
# Pinned, and refused if their checksums differ (sha256, taken on
# 2026-09-23 and 2026-09-24 from the release downloads named below):
#   libffi 3.5.2, which wayland-client's closures are called through;
#   wayland 1.24.0, for libwayland-client and libwayland-cursor;
#   wayland-protocols 1.45, the protocol XML foot generates its glue from;
#   libxkbcommon 1.11.0, which turns the compositor's keymap into symbols;
#   pixman 0.46.4, which foot and fcft draw with;
#   freetype 2.14.1, expat 2.7.3 and fontconfig 2.17.1, which find and
#   rasterise a font;
#   tllist 1.1.0 and fcft 3.3.2, foot's author's list header and font
#   library;
#   DejaVu fonts 2.37, whose Sans Mono is the font on the image;
#   gperf 3.3, a build-time tool fontconfig generates a hash with, built for
#   the host.
#
# The build host needs meson, ninja, bison, pkg-config and wayland-scanner
# 1.24.0, the same version as the library: wayland's own scanner is a host
# tool, and this script builds only its libraries.
#
# What is built without, and why:
#   * harfbuzz and utf8proc: fcft's text shaping and foot's grapheme
#     clustering, each another port. A terminal draws one glyph per cell
#     without them; combining characters and emoji sequences do not join;
#   * libpng, zlib, bzip2 and brotli in freetype: DejaVu is plain TrueType;
#   * foot's terminfo: TERM is xterm-256color, which every program knows,
#     rather than foot, which none on the image does;
#   * documentation, tests, and every tool the libraries ship.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=../common.sh
. "$here/../common.sh"

# name version file url sha256
SOURCES=(
    "libffi 3.5.2 libffi-3.5.2.tar.gz https://github.com/libffi/libffi/releases/download/v3.5.2/libffi-3.5.2.tar.gz f3a3082a23b37c293a4fcd1053147b371f2ff91fa7ea1b2a52e335676bac82dc"
    "wayland 1.24.0 wayland-1.24.0.tar.xz https://gitlab.freedesktop.org/wayland/wayland/-/releases/1.24.0/downloads/wayland-1.24.0.tar.xz 82892487a01ad67b334eca83b54317a7c86a03a89cfadacfef5211f11a5d0536"
    "wayland-protocols 1.45 wayland-protocols-1.45.tar.xz https://gitlab.freedesktop.org/wayland/wayland-protocols/-/releases/1.45/downloads/wayland-protocols-1.45.tar.xz 4d2b2a9e3e099d017dc8107bf1c334d27bb87d9e4aff19a0c8d856d17cd41ef0"
    "libxkbcommon 1.11.0 xkbcommon-1.11.0.tar.gz https://github.com/xkbcommon/libxkbcommon/archive/refs/tags/xkbcommon-1.11.0.tar.gz 78a6b14f16e9a55025978c252e53ce9e16a02bfdb929550b9a0db5af87db7e02"
    "pixman 0.46.4 pixman-0.46.4.tar.gz https://www.cairographics.org/releases/pixman-0.46.4.tar.gz d09c44ebc3bd5bee7021c79f922fe8fb2fb57f7320f55e97ff9914d2346a591c"
    "freetype 2.14.1 freetype-2.14.1.tar.xz https://download.savannah.gnu.org/releases/freetype/freetype-2.14.1.tar.xz 32427e8c471ac095853212a37aef816c60b42052d4d9e48230bab3bdf2936ccc"
    "expat 2.7.3 expat-2.7.3.tar.xz https://github.com/libexpat/libexpat/releases/download/R_2_7_3/expat-2.7.3.tar.xz 71df8f40706a7bb0a80a5367079ea75d91da4f8c65c58ec59bcdfbf7decdab9f"
    "gperf 3.3 gperf-3.3.tar.gz https://ftp.gnu.org/gnu/gperf/gperf-3.3.tar.gz fd87e0aba7e43ae054837afd6cd4db03a3f2693deb3619085e6ed9d8d9604ad8"
    "fontconfig 2.17.1 fontconfig-2.17.1.tar.xz https://www.freedesktop.org/software/fontconfig/release/fontconfig-2.17.1.tar.xz 9f5cae93f4fffc1fbc05ae99cdfc708cd60dfd6612ffc0512827025c026fa541"
    "tllist 1.1.0 tllist-1.1.0.tar.gz https://codeberg.org/dnkl/tllist/archive/1.1.0.tar.gz 0e7b7094a02550dd80b7243bcffc3671550b0f1d8ba625e4dff52517827d5d23"
    "fcft 3.3.2 fcft-3.3.2.tar.gz https://codeberg.org/dnkl/fcft/archive/3.3.2.tar.gz 22bcf73f51480ad48110a52cb26f93180d125929ac6b29bef67545fc36967479"
    "foot 1.24.0 foot-1.24.0.tar.gz https://codeberg.org/dnkl/foot/archive/1.24.0.tar.gz e86cf92895f16bbd3f02c6bae706717790f5a7a686bc37182d715a3defe73349"
    "dejavu 2.37 dejavu-fonts-ttf-2.37.tar.bz2 https://github.com/dejavu-fonts/dejavu-fonts/releases/download/version_2_37/dejavu-fonts-ttf-2.37.tar.bz2 fa9ca4d13871dd122f61258a80d01751d603b4d3ee14095d65453b4e846e17d7"
)

work=$builds/foot
src=$ports/src
stage=$work/stage
host_tools=$work/host

step "sources"
mkdir -p "$src" "$work"
for line in "${SOURCES[@]}"; do
    read -r name version file url sum <<< "$line"
    fetch "$url" "$src/$file" sha256sum "$sum"
    echo "$name $version"
done

for tool in meson ninja bison pkg-config wayland-scanner; do
    command -v "$tool" > /dev/null || fail "no $tool on this host, which the build needs"
done
scanner=$(wayland-scanner --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')
[ "$scanner" = 1.24.0 ] || fail "wayland-scanner is $scanner, not the library's 1.24.0"

build_ferrousli
make_compilers

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
# Meson is told it cross-compiles even on x86-64, which is what makes it
# take the libraries from the staging tree through pkg-config's sysroot
# rather than from the host's /usr. On x86-64 the programs it builds still
# run here, so its run-time checks ask ferrousli.
case $arch in
    x86_64) cpu_family=x86_64 cpu=x86_64 wrapper=false ;;
    aarch64) cpu_family=aarch64 cpu=aarch64 wrapper=true ;;
    armv7a) cpu_family=arm cpu=armv7a wrapper=true ;;
esac
cross=$work/cross.ini
cat > "$cross" << EOF
[binaries]
c = '$CC'
ar = '$AR'
strip = '$STRIP'
pkg-config = 'pkg-config'

[built-in options]
default_library = 'static'
prefer_static = true
c_args = ['-O2']

[properties]
sys_root = '$stage'
pkg_config_libdir = ['$stage/usr/lib/pkgconfig', '$stage/usr/share/pkgconfig']
needs_exe_wrapper = $wrapper

[host_machine]
system = 'linux'
cpu_family = '$cpu_family'
cpu = '$cpu'
endian = 'little'
EOF
rm -rf "$stage" "$host_tools"
mkdir -p "$stage" "$host_tools"
echo "$cross"

# Configure, build and stage one meson project.
meson_port() { # name option...
    local name=$1 dir
    shift
    dir=$(unpack "$name")
    logged "$name" meson setup "$dir/build" "$dir" --cross-file "$cross" \
        --prefix=/usr --libdir=lib --sysconfdir=/etc --localstatedir=/var \
        --buildtype=release --wrap-mode=nodownload "$@"
    logged "$name-build" ninja -C "$dir/build" -j"$jobs"
    DESTDIR=$stage logged "$name-install" meson install -C "$dir/build" --no-rebuild
    echo "$name staged"
}

# Configure, build and stage one autotools project.
autotools_port() { # name option...
    local name=$1 dir
    shift
    dir=$(unpack "$name")
    local host=()
    [ -n "$triple" ] && host=(--host="$triple")
    logged "$name" sh -c "cd '$dir' && ./configure ${host[*]} CC='$CC' AR='$AR' CFLAGS=-O2 \
        --prefix=/usr --libdir=/usr/lib --disable-shared --enable-static $*"
    logged "$name-build" make -C "$dir" -j"$jobs"
    logged "$name-install" make -C "$dir" install DESTDIR="$stage"
    # libtool's archives name /usr/lib, the guest's path rather than the
    # staging tree's; pkg-config's files are what the later builds read.
    find "$stage" -name '*.la' -delete
    echo "$name staged"
}

step "gperf, for the host"
dir=$(unpack gperf)
logged gperf sh -c "cd '$dir' && CC=gcc CXX=g++ ./configure --prefix='$host_tools' && make -j'$jobs' && make install"
export PATH=$host_tools/bin:$PATH

step "libffi"
autotools_port libffi --disable-docs --disable-multi-os-directory --disable-exec-static-tramp

step "wayland"
meson_port wayland -Dlibraries=true -Dscanner=false -Dtests=false \
    -Ddocumentation=false -Ddtd_validation=false

step "wayland-protocols"
meson_port wayland-protocols -Dtests=false

step "libxkbcommon"
meson_port libxkbcommon -Denable-x11=false -Denable-wayland=false -Denable-docs=false \
    -Denable-tools=false -Denable-xkbregistry=false -Denable-bash-completion=false

step "pixman"
meson_port pixman -Dtests=disabled -Ddemos=disabled -Dgtk=disabled -Dlibpng=disabled \
    -Dopenmp=disabled

step "freetype"
meson_port freetype -Dzlib=disabled -Dpng=disabled -Dbzip2=disabled -Dbrotli=disabled \
    -Dharfbuzz=disabled -Dtests=disabled

step "expat"
autotools_port expat --without-docbook --without-examples --without-tests --without-xmlwf

step "fontconfig"
meson_port fontconfig -Ddoc=disabled -Dtests=disabled -Dtools=disabled \
    -Dcache-build=disabled -Dnls=disabled -Diconv=disabled -Dxml-backend=expat

step "tllist"
meson_port tllist

step "fcft"
meson_port fcft -Dgrapheme-shaping=disabled -Drun-shaping=disabled -Dsvg-backend=none \
    -Ddocs=disabled -Dtest-text-shaping=false

step "foot"
# Configured and staged as the libraries are, but a failed link says which
# symbols ferrousli does not have yet before it stops.
foot=$(unpack foot)
logged foot meson setup "$foot/build" "$foot" --cross-file "$cross" \
    --prefix=/usr --libdir=lib --sysconfdir=/etc --localstatedir=/var \
    --buildtype=release --wrap-mode=nodownload \
    -Ddocs=disabled -Dgrapheme-clustering=disabled -Dterminfo=disabled \
    -Ddefault-terminfo=xterm-256color -Dthemes=false -Dtests=false
set +e
ninja -C "$foot/build" -j"$jobs" > "$work/foot-build.log" 2>&1
status=$?
set -e
echo "ninja exited $status; log in $work/foot-build.log"
undefined_symbols "$work/undefined-symbols.txt" "$work/foot-build.log"
if [ "$status" -ne 0 ]; then
    grep -E 'error|Error' "$work/foot-build.log" | head -30 >&2
    exit 1
fi
DESTDIR=$stage logged foot-install meson install -C "$foot/build" --no-rebuild

step "install"
mkdir -p "$prefix/bin" "$prefix/etc/fonts" "$prefix/usr/share/fonts/dejavu"
# Stripped: the image carries them, and nothing on the guest reads their
# symbols.
for program in foot footclient; do
    install -m 755 -s --strip-program="$STRIP" "$stage/usr/bin/$program" "$prefix/bin/$program"
done
install -m 644 "$stage/etc/fonts/fonts.conf" "$prefix/etc/fonts/fonts.conf"
dejavu=$(unpack dejavu)
for face in DejaVuSansMono DejaVuSansMono-Bold DejaVuSansMono-Oblique DejaVuSansMono-BoldOblique; do
    install -m 644 "$dejavu/ttf/$face.ttf" "$prefix/usr/share/fonts/dejavu/$face.ttf"
done
file "$prefix/bin/foot"
run_built "$prefix/bin/foot" --version
