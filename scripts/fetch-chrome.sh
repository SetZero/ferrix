#!/usr/bin/env bash
# Fetch Google's prebuilt headless Chrome and the Debian libraries it loads,
# and put them on a btrfs volume: the disk `cargo xtask test-chrome` runs
# Chrome from (docs/CHROME.md).
#
# Nothing here is built. `chrome-headless-shell` is Chrome for Testing's
# linux64 build, a 198 MB position-independent glibc program with its own
# ANGLE and SwiftShader beside it. It loads forty libraries of the system's
# -- glib, NSS, D-Bus, the X11 client libraries, gbm, udev, ALSA and what
# those load -- which are Debian 13's here, as are the glibc and the loader
# that run it: the same glibc scripts/fetch-rustc-sysroot.sh pins. That is
# the fast way to a browser on Ferrix; ferrousli standing in for that glibc
# is the other, and the same volume serves both.
#
# The volume holds a Debian-shaped tree at its root -- `usr/lib64`,
# `usr/lib/x86_64-linux-gnu` -- and Chrome under `chrome/`. Ferrix mounts it
# at `/data`, and the test's image links the directories glibc names into
# it, as test-rustc's does.
#
# Every download is pinned by its SHA-256: Debian's Packages files for
# trixie and trixie-security on 2026-09-24, and Chrome for Testing's zip as
# downloaded on 2026-09-24 (its own index publishes no digest). After
# unpacking, every library each ELF file on the volume needs is looked for
# on it, so a dependency Debian has and this list lacks fails here rather
# than on Ferrix.
#
# Writes $FERRIX_CHROME_VOLUME/chrome.img (default
# ~/.local/share/ferrix/chrome/chrome.img). Needs curl, sha256sum, dpkg-deb,
# unzip, readelf and mkfs.btrfs; no root.
#
# Usage: scripts/fetch-chrome.sh

set -euo pipefail

debian=${DEBIAN_MIRROR:-https://deb.debian.org/debian}
security=${DEBIAN_SECURITY_MIRROR:-https://security.debian.org/debian-security}
chrome_dist=${CHROME_FOR_TESTING:-https://storage.googleapis.com/chrome-for-testing-public}
out=${FERRIX_CHROME_VOLUME:-$HOME/.local/share/ferrix/chrome}

CHROME_VERSION=154.0.8037.57
CHROME_SHA256=5a6979d0ab7cf952ea575d35164e7bdce4872b2ced8f8a215c8f8e8eda00ee09

# Pool path and SHA-256 of each Debian 13 package from the main archive.
debs=(
    "main/g/glibc/libc6_2.41-12+deb13u4_amd64.deb 967aa62605721081c3eb2a17650611a792aa802d76a6511d1840242623d204c9"
    "main/g/gcc-14/libgcc-s1_14.2.0-19_amd64.deb 3c71917b490d1a17aed43196a2787a256ecf060526cdb20216a74bedc061b150"
    "main/g/gcc-14/libatomic1_14.2.0-19_amd64.deb 212b399aae2f7299203d261a57e49372e09565a9a5ea971905f94a3960366c05"
    "main/z/zlib/zlib1g_1.3.dfsg+really1.3.1-1+b1_amd64.deb 015be740d6236ad114582dea500c1d907f29e16d6db00566ca32fb68d71ac90d"
    "main/a/alsa-lib/libasound2t64_1.2.14-1+deb13u1_amd64.deb 5495496142d57e5ad6d581ac5456ec3c87f872c109a2bfb815d154b673eebc67"
    "main/a/at-spi2-core/libatk-bridge2.0-0t64_2.56.2-1+deb13u2_amd64.deb c0fe87ea1bdca2f938eaa8286af5ad38ecd15cf18c6cfb5961d7d46ac3192325"
    "main/a/at-spi2-core/libatk1.0-0t64_2.56.2-1+deb13u2_amd64.deb 24786bc90e3ff80c4d1d8bdb5ec18bb5f06df9081a5233eb080f3202fcf6a1c3"
    "main/a/at-spi2-core/libatspi2.0-0t64_2.56.2-1+deb13u2_amd64.deb 65e9c9ecbd820fba2d04104cddd0bad33d256cbdfe0d1cb5365f10e363df2315"
    "main/d/dbus/libdbus-1-3_1.16.2-2_amd64.deb bbcb711daff7e104b5f80f9b05475b6142674bfe65518a36a56e96a91068a3f5"
    "main/libd/libdrm/libdrm2_2.4.124-2_amd64.deb fe2276901c7cd7b8079de63072d37fe1cbeb4eb001a3bc1f1d662ad89aa0890e"
    "main/libf/libffi/libffi8_3.4.8-2_amd64.deb 0ebdc340de33333639c3c63874cd4b15ac2e83dfa1ef3053b7eefaf4919f4f68"
    "main/m/mesa/libgbm1_25.0.7-2+deb13u1_amd64.deb 31fb6d76b9ceaf13848fa617df53f85f62626b4fe7464a93811c720af6d5f2dd"
    "main/g/glib2.0/libglib2.0-0t64_2.84.4-3~deb13u5_amd64.deb e2baf92c57d1db1753e5781a61450d9a01e7833c7bd2dfe87776a6811c11ecc2"
    "main/n/nspr/libnspr4_4.36-1_amd64.deb 87fc2039ee89cff2b8010fa9535706b1def40031acfc52bf12197dcb3cb7b064"
    "main/p/pcre2/libpcre2-8-0_10.46-1~deb13u2_amd64.deb 1252b96a5bc44bb5db982bef8eb18e54f5047cede2aff641bce4f8e1edb91c3e"
    "main/libs/libselinux/libselinux1_3.8.1-1_amd64.deb 68bb8d32bd8d6d7d2f5952a169db03d1484b46ae1e52abccdec42a19dccea5d5"
    "main/s/systemd/libsystemd0_257.13-1~deb13u1_amd64.deb ab0d4127b5e46e6f8c015a1db15a62ba9ae274cdefa150083adf90ada0600ea1"
    "main/s/systemd/libudev1_257.13-1~deb13u1_amd64.deb 5d41c284f5a93b05bc7d648b61a02dd2bb9ff05b2261ad1a8b7d96044a0cfa88"
    "main/w/wayland/libwayland-server0_1.23.1-3_amd64.deb 2967212bd582e0dffca443fdc44f4c660e7368d41f7ee3a7f6314e0c3abfe9ea"
    "main/libx/libx11/libx11-6_1.8.12-1_amd64.deb b5a3fd3bf8c8fd0364bfb9bea00dcba7fc301229bd02dded084632d31f5b0fb3"
    "main/libx/libxau/libxau6_1.0.11-1_amd64.deb 689a9f0e0ba3e2c65431f864871e303ee904de69dd28abfc462663fae030227f"
    "main/libx/libxcb/libxcb1_1.17.0-2+b1_amd64.deb 5c222a72d11b866447da31693254f738430726e3e065a384e82687b2fd2f978b"
    "main/libx/libxcomposite/libxcomposite1_0.4.6-1_amd64.deb 20e3c1d9b2135f0c8c4246a9fd26a51a57d5850c3b36ecc173c45d6be7328af3"
    "main/libx/libxdamage/libxdamage1_1.1.6-1+b2_amd64.deb e51e43f23f3befbc1f9408271f4df6773d37caece9ce7e1f38abade382f7fbf7"
    "main/libx/libxdmcp/libxdmcp6_1.1.5-1_amd64.deb 0740dc760916b2008b45417a42a8fd7dd5de370fb57d31373f15034cda8acf0b"
    "main/libx/libxext/libxext6_1.3.4-1+b3_amd64.deb fc618ec40465e5ce48622606299cb47833efc3fb235ba15543b81f850722f443"
    "main/libx/libxfixes/libxfixes3_6.0.0-2+b4_amd64.deb 3fdd95d86d8e9d63e11f52070935c8f9f912c36aa3b17338d969f7685f3ed4fb"
    "main/libx/libxi/libxi6_1.8.2-1_amd64.deb 093d0903f35bb7a9f6815180ee040e6951fecf9b66c128cd72f064710210606e"
    "main/libx/libxkbcommon/libxkbcommon0_1.7.0-2_amd64.deb f75ee544f55acc6a271debfab3ea4ae0458afc89d81cfe1a71137e07d4895b86"
    "main/libx/libxrandr/libxrandr2_1.5.4-1+b3_amd64.deb 11e3490de93a8bbee3daba719cb8e1325a26fb3c125525c34bdcb7deb05eb9b2"
    "main/libx/libxrender/libxrender1_0.9.12-1_amd64.deb 9d042dfd5e613be1e02e6ddd0c5c4adef19c5eb08f6db838c2eba672c496dca4"
    "main/libx/libxres/libxres1_1.2.1-1+b2_amd64.deb 995b467fa9d0d47b4bff46cafd11ac7067f36a0e30cfaa121e73df7f8af93041"
    "main/libc/libcap2/libcap2_2.75-10+deb13u1+b3_amd64.deb 89fc4d34fc7a28ad6f0fcd0c561ab253b9dedf6f77f5a000b47c276c8295bf67"
    "main/s/sqlite3/libsqlite3-0_3.46.1-7+deb13u2_amd64.deb 0a459adaffd901109f7811ab65f58e7a957b4907d05539cf3d1184efdcde0468"
    # fontconfig's configuration and a font: Chrome finds fonts through the
    # fontconfig it carries, and draws no text without one.
    "main/f/fontconfig/fontconfig-config_2.15.0-2.3_amd64.deb 0475c00d02660c07a15085051818625331bb502e053242106aaaa1f2ddb41225"
    "main/f/fonts-dejavu/fonts-dejavu-core_2.37-8_all.deb 86635b3d25b3655fc11cb3ecc3af59f0bf19643b02b94f2de48bd10253cdba12"
    "main/f/fonts-dejavu/fonts-dejavu-mono_2.37-8_all.deb 3003e98a5debfdeadc7040a7f715fe9fe6fb67f68deacf6049b54e30f07fc014"
)

# The same, from the security archive.
security_debs=(
    "updates/main/u/util-linux/libblkid1_2.41.5-0+deb13u1_amd64.deb 81535f3c2c0efc732965907c8749103a0a26377c761622c9ce39b4c92dcde52f"
    "updates/main/u/util-linux/libmount1_2.41.5-0+deb13u1_amd64.deb 6d00f45f2e80e078e906e3eedecd3ba6913e39fef49bff361ee583f17f00ec05"
    "updates/main/e/expat/libexpat1_2.8.3-1~deb13u1_amd64.deb 38abe0e710a07688e9c149d74536e67cfee0364bdb64dd6d644c32a1cfad389f"
    "updates/main/n/nss/libnss3_3.110-1+deb13u4_amd64.deb 8d20d0754039e15e9bd2c8484e98dc9c2982907d4f20834da29a9501913d00b5"
)

for tool in curl sha256sum dpkg-deb unzip readelf mkfs.btrfs; do
    command -v "$tool" > /dev/null || { echo "fetch-chrome: $tool is not installed" >&2; exit 1; }
done

# Download $2/$1 into the pool unless it is there, and check it against $3.
fetch() {
    local path=$1 base=$2 sum=$3
    local file="$out/pool/${path##*/}"
    if [ ! -f "$file" ]; then
        curl -fsSL -o "$file.part" "$base/$path"
        mv "$file.part" "$file"
    fi
    echo "$sum  $file" | sha256sum -c --quiet \
        || { echo "fetch-chrome: $file does not match its pinned checksum" >&2; rm -f "$file"; exit 1; }
    printf '%s\n' "$file"
}

mkdir -p "$out/pool"
tree="$out/tree"
rm -rf "$tree"
mkdir -p "$tree"

for entry in "${debs[@]}"; do
    read -r path sum <<< "$entry"
    dpkg-deb -x "$(fetch "$path" "$debian/pool" "$sum")" "$tree"
done
for entry in "${security_debs[@]}"; do
    read -r path sum <<< "$entry"
    dpkg-deb -x "$(fetch "$path" "$security/pool" "$sum")" "$tree"
done

zip=$(fetch "$CHROME_VERSION/linux64/chrome-headless-shell-linux64.zip" "$chrome_dist" "$CHROME_SHA256")
unpacked=$(mktemp -d)
trap 'rm -rf "$unpacked"' EXIT
unzip -q "$zip" -d "$unpacked"
mv "$unpacked/chrome-headless-shell-linux64" "$tree/chrome"

# Documentation and manuals, which nothing runs.
rm -rf "$tree/usr/share/doc" "$tree/usr/share/man" "$tree/usr/share/lintian"

test -x "$tree/chrome/chrome-headless-shell" \
    || { echo "fetch-chrome: no chrome-headless-shell in the tree" >&2; exit 1; }
test -e "$tree/usr/lib64/ld-linux-x86-64.so.2" \
    || { echo "fetch-chrome: no linker at usr/lib64" >&2; exit 1; }

# Every library an ELF file on the volume needs must be on it: in glibc's
# directory, beside Chrome, or glibc itself.
missing=0
while IFS= read -r -d '' file; do
    head -c 4 "$file" | grep -q $'\x7fELF' || continue
    for needed in $(readelf -d "$file" 2> /dev/null | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'); do
        if [ ! -e "$tree/usr/lib/x86_64-linux-gnu/$needed" ] \
            && [ ! -e "$tree/lib/x86_64-linux-gnu/$needed" ] \
            && [ ! -e "$tree/chrome/$needed" ] \
            && [ ! -e "$tree/usr/lib64/$needed" ]; then
            echo "fetch-chrome: ${file#"$tree"/} needs $needed, which is not on the volume" >&2
            missing=1
        fi
    done
done < <(find "$tree/chrome" "$tree/usr/lib/x86_64-linux-gnu" -maxdepth 1 -type f -print0)
[ "$missing" = 0 ] || exit 1

# Room for Chrome's profile, caches and the screenshot, and a size that does
# not depend on how mkfs rounds.
size=$(( $(du -sm "$tree" | cut -f1) + 512 ))
image="$out/chrome.img"
rm -f "$image"
truncate -s "${size}M" "$image"
mkfs.btrfs -q --rootdir "$tree" "$image"
# The tree stays beside the image, so the same files can be tried on the host.
echo "chrome volume: $image (${size} MiB), unpacked in $tree"
