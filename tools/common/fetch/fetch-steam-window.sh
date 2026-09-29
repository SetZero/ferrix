#!/usr/bin/env bash
# Make the volume `cargo xtask run-steam` and `test-steam-window` boot Steam's
# window from (docs/STEAM.md): Valve's Steam client bootstrap, yserver, and
# the Debian 13 userland the client, its 32-bit GL UI and lsof run on.
#
# It is the union of three trees, laid over each other in this order:
#
# 1. yserver's volume tree, which scripts/fetch/fetch-yserver.sh builds (the
#    X server at the commit it pins, its amd64 Debian libraries, lavapipe).
# 2. The Steam bootstrap tree scripts/fetch/fetch-steam.sh makes (Valve's
#    bootstrap under steam/, i386 and amd64 glibc, bash, GNU tar, xz and the
#    other tools, the certificate bundle).
#
# Both scripts run here, into directories of this volume's own (yserver/ and
# bootstrap/ beside the image), so the trees are the pinned ones and not a
# copy another run has changed. The second run of each reuses its downloads
# and, for yserver, its build.
# 3. The packages below: i386 Mesa with llvmpipe and its X libraries, which
#    the 32-bit client's own UI draws through with GLX; i386 libstdc++; and
#    Debian's amd64 lsof and what it links, which the client runs to learn
#    which process opened its UI websocket.
#
# Then the launch-side workarounds in scripts/steam/workarounds/, compiled
# with the host's gcc into steam-workarounds/lib/<multiarch>/ (the client
# preloads them through ld.so's $LIB), and the links unpacking does not make:
# libGLX.so, which the client's updater dlopens by its development name to
# choose its X11 UI, and awk.
#
# The Steam client proper is not here: the bootstrap downloads it (about
# 500 MB) on the guest's first start, as it does on Linux.
#
# Pinned by SHA-256: Debian's trixie Packages files of 2026-09-28.
#
# Writes $FERRIX_STEAM_WINDOW_VOLUME/steam-window.img (default
# ~/.local/share/ferrix/steam-window/steam-window.img). Needs what
# fetch-steam.sh and fetch-yserver.sh need, and gcc able to build -m32
# objects without a C library (-nostdlib). No root.
#
# Usage: scripts/fetch/fetch-steam-window.sh

set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
out=${FERRIX_STEAM_WINDOW_VOLUME:-$HOME/.local/share/ferrix/steam-window}
yserver=$out/yserver
bootstrap=$out/bootstrap
debian=${DEBIAN_MIRROR:-https://deb.debian.org/debian}

# Debian's packages: the pool path, then its SHA-256.
DEBS=(
    # i386 Mesa: GLX through libglvnd, llvmpipe, and the X libraries under it.
    pool/main/libg/libglvnd/libglvnd0_1.7.0-1+b2_i386.deb 756fce6f41fae7c61ce16cdb80815083833f78e382819b93d2b65d63f6dd71a0
    pool/main/libg/libglvnd/libglx0_1.7.0-1+b2_i386.deb 94b2d3e10d5fa4aac9fd98bbdff7d7285f70051276225b67ac8017d0516d5830
    pool/main/m/mesa/libglx-mesa0_25.0.7-2+deb13u1_i386.deb 17899990360a7c885777343f837f0da145d6175e670d929b1555ff4bf77681cb
    pool/main/m/mesa/libgl1-mesa-dri_25.0.7-2+deb13u1_i386.deb c5886813cd61d9cb94bba70930aa106faa38a505f602364864529a2851fd1eec
    pool/main/v/vulkan-loader/libvulkan1_1.4.309.0-1_i386.deb 5561b8b95bfd4123dc6f7c73e332591840d3e116e5d067e9e813843a630e1230
    pool/main/m/mesa/libgbm1_25.0.7-2+deb13u1_i386.deb 2912458ba1037c30e9116efccfa0977507b2a0eb6ef904ce1ae14ffd20e81331
    pool/main/m/mesa/mesa-libgallium_25.0.7-2+deb13u1_i386.deb 981a6f758f1ac3b838c9d09c40e75b6e5788df300972ae4d79aab325a9ea7882
    pool/main/z/zlib/zlib1g_1.3.dfsg+really1.3.1-1+b1_i386.deb 8950c79b1ade51f53b2f37863aec532cfaca3dff315e28f0c5b53902425c41ed
    pool/main/libz/libzstd/libzstd1_1.5.7+dfsg-1_i386.deb 305f0ab5f16552c44906b3f3f6e9ad3eb33f6f857c3347fdf3bd987abd6989a9
    pool/main/libx/libxshmfence/libxshmfence1_1.3.3-1_i386.deb 4ec808885b3d94846ed2fa7039b1388e21828ed083630b50f74a338c2075654b
    pool/main/libx/libxcb/libxcb1_1.17.0-2+b1_i386.deb a4c421cf27ef4f66b32f650e3ac8bbd12bc8ec28b70a3be5dcc56dc3453e8d3e
    pool/main/libx/libxdmcp/libxdmcp6_1.1.5-1_i386.deb 06b3a5d82fd69b1ff66e4e68fd4cf55f616864a29f456609a1f92b275b6126f7
    pool/main/libx/libxau/libxau6_1.0.11-1_i386.deb 1645415b2ea8b64268f87944b733b9f9a2f243f89eb73555120eaf459fa06a57
    pool/main/libx/libxcb/libxcb-xfixes0_1.17.0-2+b1_i386.deb b715b1a5d5f41632b5c5592f1c413401cf1538d9788f8e8ab91448522f26b123
    pool/main/libx/libxcb/libxcb-sync1_1.17.0-2+b1_i386.deb 5d18a7856f73baaa192d51ef6e8b3f03b4268837c9769a508a04067274446aff
    pool/main/libx/libxcb/libxcb-randr0_1.17.0-2+b1_i386.deb e31b2b6b7d0937a4052f143ccd4c639c2afae28a22c32a6c083d07c5bce025a9
    pool/main/libx/libxcb/libxcb-present0_1.17.0-2+b1_i386.deb d3453767790b2ef25a546e3c3ecd4545945a588c9dd95221fd0ecd9fded054f1
    pool/main/libx/libxcb/libxcb-dri3-0_1.17.0-2+b1_i386.deb 8ec62578b10cdf0b85e5298563ac16fe69bfdc1c1a87c5be35140343f3dc8927
    pool/main/libx/libx11/libx11-xcb1_1.8.12-1_i386.deb c17ba14a5a1c8b5733eef8eb54e640df27816d5fa6636c512d75374ca5305029
    pool/main/libx/libx11/libx11-6_1.8.12-1_i386.deb 0a4ee67fffb3299953a9bd298c33e4f66d1b7f2f112eb83eb32c8e4351927741
    pool/main/libx/libx11/libx11-data_1.8.12-1_all.deb c54f87069888f80ba4da586da6147d74c7598ccdd8b90906dbc4271fa414c738
    pool/main/l/lm-sensors/libsensors5_3.6.2-2_i386.deb b999b735f40d17cc274f80fe98194e1a1edfd134de772168d34acd19ce587ca2
    pool/main/l/lm-sensors/libsensors-config_3.6.2-2_all.deb 3056da80c7d963af795dab480ab6f6f4b154ad4ac39f522dc52d17c834fea253
    pool/main/l/llvm-toolchain-19/libllvm19_19.1.7-3+b1_i386.deb a285dd9bf5f9552084a68886ce1ac1cc83913cc9b68dfb58504e17c06d0f9846
    pool/main/z/z3/libz3-4_4.13.3-1_i386.deb 9d2e21a41837b7d3dbb97cebf38a7f14b5d4dc1e12a14c0cc3a10c99b198b66d
    pool/main/libx/libxml2/libxml2_2.12.7+dfsg+really2.9.14-2.1+deb13u3_i386.deb 361ae44454c7bbdb0a01a3564a9838a0fc5d32b563e4a7f03bcae8d4b484588b
    pool/main/x/xz-utils/liblzma5_5.8.1-1+deb13u1_i386.deb d686140b257bdc58e342a22ab5006c9d0327553a3ac273838109ef9ddf55621c
    pool/main/libf/libffi/libffi8_3.4.8-2_i386.deb 8bafefb38c320ca27796070c881595ce04d900c3b534ac142a494daf1fe1db13
    pool/main/libe/libedit/libedit2_3.1-20250104-1_i386.deb 484df5a0249534e8a1378733f33058a616ff4b9d3349c7830d1b14548e9759c3
    pool/main/n/ncurses/libtinfo6_6.5+20250216-2_i386.deb 3488ab080c07056b2ddef81f3d75fac7358ebfd63ac04a224fa87571152f39b7
    pool/main/libb/libbsd/libbsd0_0.12.2-2_i386.deb 72dccfe87335f7c5efde97345b3ddf3157fdc5b0c441a8cb413722928ddcd510
    pool/main/libm/libmd/libmd0_1.1.0-2+b1_i386.deb 42e150e95df064f5e9704cca46fc6d5755416681c96811cfa468e5a638dd425d
    pool/main/g/gcc-14/libatomic1_14.2.0-19_i386.deb 99b97383f7b8c768e04b5bed83b3854c5a4d9eaed2558632508fde8ef2ec7906
    pool/main/e/expat/libexpat1_2.8.3-1~deb13u1_i386.deb d0396d41674a2fb1946af2b131630534a2cf09d160c8ea13bc5bbaa7416f071a
    pool/main/e/elfutils/libelf1t64_0.192-4_i386.deb 857d2cfe946eea39d9c670522d61675a0fc546f65e7fb33ae5088626c7293468
    pool/main/libd/libdrm/libdrm2_2.4.124-2_i386.deb 366cf26a5c5f1003225ef57b442b3dce8a5dd555487633bd2de5278edca9e854
    pool/main/libd/libdrm/libdrm-common_2.4.124-2_all.deb 9a8a6c65c165e9964f106fb4ac710959b5d33e0790227e3ab6b27c4742d1254a
    pool/main/libd/libdrm/libdrm-intel1_2.4.124-2_i386.deb a7e30963f22af48d68d78f6400ea43d25d5452d108f651886ebe962504ce3469
    pool/main/libp/libpciaccess/libpciaccess0_0.17-3+b3_i386.deb d16fddaa98f1f1016b70ea6bb15e24e5fc2a837c8c1107e0c6e02cdbf8d3c999
    pool/main/libd/libdrm/libdrm-amdgpu1_2.4.124-2_i386.deb e4f8efca9d0214ffe890165ed87a8a5675ace48713a4d53f979df68c32ae4ccb
    pool/main/w/wayland/libwayland-server0_1.23.1-3_i386.deb 44d52ef7b5ce89bee25133e3eaafa2cac130e7c2b31d3862f97bfeadc8ad76cb
    pool/main/libx/libxxf86vm/libxxf86vm1_1.1.4-1+b4_i386.deb b3f612262c335fde6536d4fa62df37640a5e376c29e8e484bfce70f7ee6eabd0
    pool/main/libx/libxext/libxext6_1.3.4-1+b3_i386.deb 9e15d0b7f325c70081acb33e21bf019a1bb7c9f0df262ece49bef91d072705a1
    pool/main/libx/libxcb/libxcb-shm0_1.17.0-2+b1_i386.deb c96eb689f88d25e284328eff50f2faf334c13648ef5ea66fe7e1a3ef5849b0b5
    pool/main/libx/libxcb/libxcb-glx0_1.17.0-2+b1_i386.deb eec799cf9a756f8b8ec5357d21ae973e526ce2ccce8be2e141695f7337df0902
    pool/main/libg/libglvnd/libgl1_1.7.0-1+b2_i386.deb 2d61aa7c7b39a07e3bc691239b524fee99a2fc92da2b6706b518db7b0e315554
    # i386 C++ runtime for the client's libraries.
    pool/main/g/gcc-14/libgcc-s1_14.2.0-19_i386.deb a4c71fd856d2a48a7505a087b4186e3cca23f94603c05e3fb7c799b27e72f761
    pool/main/g/gcc-14/libstdc++6_14.2.0-19_i386.deb b6020260b92a97ac33ae58a73b16f3ab31fed7632e9b861f7cd5fc393facd6ed
    # amd64: lsof, and what it links beyond fetch-steam.sh's tree.
    pool/main/e/e2fsprogs/libcom-err2_1.47.2-3+b12_amd64.deb bd43e020b8fed399db3eebf098111f10fba4c436fff99ccad72cc7e27c9764ec
    pool/main/k/krb5/libgssapi-krb5-2_1.21.3-5+deb13u1_amd64.deb 30847c1fde4240567d7ed3aeab4f655dd591203758b857e85e824045aae70299
    pool/main/libi/libidn2/libidn2-0_2.3.8-2_amd64.deb 90b039bcdc4578f8e1c4935adf8dbb525e36a164deefdbbb8c45bac347d48278
    pool/main/k/krb5/libk5crypto3_1.21.3-5+deb13u1_amd64.deb 7da07ee674b47f1f0be7cc89317c25310086a1f1761217d0f72e6ae2c5a69b84
    pool/main/k/keyutils/libkeyutils1_1.6.3-6_amd64.deb 0b11ad17be0300b63ad4eeb4c6450fed24d34b7b740f23e5363dcb29ee6d5eba
    pool/main/k/krb5/libkrb5-3_1.21.3-5+deb13u1_amd64.deb 47d71d6a7f2e59b9bae5f89602397594805113b95889ad18fa703cd53abafc97
    pool/main/k/krb5/libkrb5support0_1.21.3-5+deb13u1_amd64.deb 3a0acd8b37955c0e102c756b52c97df2a31f67b96453c35dab70df218d309117
    pool/main/s/systemd/libsystemd0_257.13-1~deb13u1_amd64.deb ab0d4127b5e46e6f8c015a1db15a62ba9ae274cdefa150083adf90ada0600ea1
    pool/main/libt/libtirpc/libtirpc-common_1.3.6+ds-1_all.deb 300f582e2c9151a8d329568c705cadc76252c417147312fe331c4560d2c7a6c3
    pool/main/libt/libtirpc/libtirpc3t64_1.3.6+ds-1_amd64.deb ef4536c09bf5063554310e22d586ae90990100ba66d2fe7c35660e9e658913b6
    pool/main/libu/libunistring/libunistring5_1.3-2_amd64.deb 6dd3490bef06ea1096f32d10766b7e016cf579bfae8451d1b7df15ce05b8aa46
    pool/main/libx/libxfixes/libxfixes3_6.0.0-2+b4_amd64.deb 3fdd95d86d8e9d63e11f52070935c8f9f912c36aa3b17338d969f7685f3ed4fb
    pool/main/l/lsof/lsof_4.99.4+dfsg-2_amd64.deb 76ca9b82da6d5dabd66609937675768a593c20c5e544171a2d9544060a1780b7
)

for tool in curl sha256sum dpkg-deb gcc mkfs.btrfs; do
    command -v "$tool" > /dev/null || { echo "fetch-steam-window: $tool is not installed" >&2; exit 1; }
done

# Download $1 to the pool unless it is there, check it against $2, and print
# where it is.
fetch() {
    local url=$1 sum=$2
    local file="$out/pool/${url##*/}"
    if [ ! -f "$file" ]; then
        curl -fsSL -o "$file.part" "$url"
        mv "$file.part" "$file"
    fi
    echo "$sum  $file" | sha256sum -c --quiet \
        || { echo "fetch-steam-window: $file does not match its pinned checksum" >&2; rm -f "$file"; exit 1; }
    printf '%s\n' "$file"
}

FERRIX_YSERVER_VOLUME=$yserver "$repo/scripts/fetch/fetch-yserver.sh"
FERRIX_STEAM_VOLUME=$bootstrap "$repo/scripts/fetch/fetch-steam.sh"

mkdir -p "$out/pool"
tree="$out/tree"
rm -rf "$tree"
mkdir -p "$tree"
cp -a "$yserver/tree/." "$tree/"
cp -a "$bootstrap/tree/." "$tree/"

set -- "${DEBS[@]}"
while [ $# -gt 0 ]; do
    dpkg-deb -x "$(fetch "$debian/$1" "$2")" "$tree"
    shift 2
done

# What installing would have made: the development link the updater
# dlopens, and the awk alternative.
ln -sfn libGLX.so.0 "$tree/usr/lib/i386-linux-gnu/libGLX.so"
[ -e "$tree/usr/bin/awk" ] || ln -s mawk "$tree/usr/bin/awk"

# The workarounds, each for the ABIs that need it (see each file's header).
work="$repo/scripts/steam/workarounds"
i386="$tree/steam-workarounds/lib/i386-linux-gnu"
amd64="$tree/steam-workarounds/lib/x86_64-linux-gnu"
mkdir -p "$i386" "$amd64"
for name in pipe2-direct; do
    gcc -m32 -O2 -shared -fPIC -nostdlib -o "$i386/$name.so" "$work/$name.c"
    gcc -O2 -shared -fPIC -nostdlib -o "$amd64/$name.so" "$work/$name.c"
done

rm -rf "$tree/usr/share/doc" "$tree/usr/share/man" "$tree/usr/share/locale" \
    "$tree/usr/share/lintian" "$tree/usr/share/info"

test -x "$tree/yserver/yserver" || { echo "fetch-steam-window: no yserver" >&2; exit 1; }
test -x "$tree/steam/ubuntu12_32/steam" || { echo "fetch-steam-window: no bootstrap" >&2; exit 1; }
test -e "$tree/usr/lib/i386-linux-gnu/dri/swrast_dri.so" \
    || test -e "$tree/usr/lib/i386-linux-gnu/libgallium-25.0.7-2+deb13u1.so" \
    || { echo "fetch-steam-window: no i386 llvmpipe" >&2; exit 1; }
test -x "$tree/usr/bin/lsof" || { echo "fetch-steam-window: no lsof" >&2; exit 1; }

# Room for the client the bootstrap downloads and installs (2.2 GB
# unpacked, 2026-09-28), the packages it downloads, and its logs.
size=$(( $(du -sm "$tree" | cut -f1) + 6144 ))
image="$out/steam-window.img"
rm -f "$image"
truncate -s "${size}M" "$image"
mkfs.btrfs -q --rootdir "$tree" "$image"
echo "steam window volume: $image (${size} MiB), unpacked in $tree"
