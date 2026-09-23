#!/usr/bin/env bash
# Fetch the upstream Rust compiler, Cargo and the Debian userland they need,
# and put them on a btrfs volume: the disk stage 16's exit compiles on
# (docs/ROADMAP.md, `cargo xtask test-rustc`), and the toolchain stage 20's
# `cargo xtask test-selfhost` builds Ferrix with.
#
# Nothing here is built. rustc and Cargo are the rust-lang.org releases, both
# dynamically linked against glibc; rustc's LLVM is a shared library of its
# own. rustc links through `cc`, which is Debian's gcc 14 driver, and gcc runs
# `collect2`, which runs the `ld.lld` rustc points it at, which runs
# `rust-lld`. Every one of those is somebody else's glibc binary, and the
# glibc is Debian 13's, the same one scripts/fetch-debian-busybox.sh pins.
# Beside the host's standard library are the ones Ferrix's own images are
# built against: the kernels' and native programs' freestanding targets, the
# loaders' UEFI targets (and the ARMv7-A loader's musl one, used for its
# position-independent `core`), and the two musl targets zinc is built for.
#
# The volume holds a Debian-shaped tree at its root -- `usr/bin`,
# `usr/lib/x86_64-linux-gnu`, `usr/lib64`, `usr/lib/gcc`, `usr/libexec` --
# and the toolchain under `rust/`. Ferrix mounts it at `/data`, and its images
# link the directories glibc's and gcc's own paths name into it.
#
# Every download is pinned by the SHA-256 its own index gave: Debian's
# Packages file on 2026-09-22, and the channel manifest
# channel-rust-1.97.1.toml (the `xz_hash` of each component).
# Beside the compiler is what stage 20's builds of Ferrix's C programs run:
# busybox and uutils against ferrousli are compiled by gcc 14 (`cc1`, from
# cpp-14) and assembled and archived by binutils, driven by make, bash and
# dash, over GNU coreutils, sed, grep, gawk, findutils, diffutils, tar, the
# compressors and file, with the kernel's UAPI headers from linux-libc-dev.
# Those packages, and the libraries they need, were resolved from the same
# Packages file by their Depends; nothing that runs only at installation
# (debconf, dpkg, PAM) is taken.
#
# Writes $FERRIX_RUSTC_SYSROOT/rustc.img (default
# ~/.local/share/ferrix/rustc/rustc.img). Needs curl, sha256sum, dpkg-deb, tar
# and mkfs.btrfs; no root, since mkfs.btrfs --rootdir writes an image file.
#
# Usage: scripts/fetch-rustc-sysroot.sh

set -euo pipefail

debian=${DEBIAN_MIRROR:-https://deb.debian.org/debian}
rust=${RUST_DIST_SERVER:-https://static.rust-lang.org}
out=${FERRIX_RUSTC_SYSROOT:-$HOME/.local/share/ferrix/rustc}

# Pool path and SHA-256 of each Debian 13 package.
debs=(
    "main/g/glibc/libc6_2.41-12+deb13u4_amd64.deb 967aa62605721081c3eb2a17650611a792aa802d76a6511d1840242623d204c9"
    "main/g/glibc/libc6-dev_2.41-12+deb13u4_amd64.deb 1fda734dabcd80b77266a09ab62b0f1e3e16d8091db62899890b2200745632a2"
    "main/g/gcc-14/libgcc-s1_14.2.0-19_amd64.deb 3c71917b490d1a17aed43196a2787a256ecf060526cdb20216a74bedc061b150"
    "main/g/gcc-14/libgcc-14-dev_14.2.0-19_amd64.deb ca6f2d36d96b19b3eb71405b0b80134d8c89380b02204a2512e5c58ceb090628"
    "main/g/gcc-14/gcc-14-x86-64-linux-gnu_14.2.0-19_amd64.deb a17ef039f1ba482051c3efb5c2c24070002e60dd0bd09fd456ec31481a11b725"
    "main/z/zlib/zlib1g_1.3.dfsg+really1.3.1-1+b1_amd64.deb 015be740d6236ad114582dea500c1d907f29e16d6db00566ca32fb68d71ac90d"
    # Stage 20's C builds.
    "main/b/bash/bash_5.2.37-2+b10_amd64.deb 2fd7b04f1b7caa29c4e683f5216e6af354a41e3cdcf6e75394d7cda680f9ab82"
    "main/b/binutils/binutils_2.44-3_amd64.deb 6bc08c02539ba53b5e748142397144c499f9b20b5fa9bb56431545db124addeb"
    "main/b/binutils/binutils-common_2.44-3_amd64.deb 002da5d23f8757dee97a2c0a40e0e1d4d85a43da094488ee2ee7068d4d3691f9"
    "main/b/binutils/binutils-x86-64-linux-gnu_2.44-3_amd64.deb e6741ce95ff0f7a131c8d9faa3528ccbbc453078bbc62a97da81340ed7462c53"
    "main/b/bzip2/bzip2_1.0.8-6_amd64.deb 6b5fbe71f0fc41e85919bc4b3c514e6c47577c5005dbf1f5af6e4a49cc221aab"
    "main/c/coreutils/coreutils_9.7-3_amd64.deb 1299ab6f9389a288eb2f5f3dd222c26cc777b9a2d5ecb6ee4cbd340cebcdada2"
    "main/g/gcc-14/cpp-14-x86-64-linux-gnu_14.2.0-19_amd64.deb ef274b5379f5f97fc71619d39ecc84d039d3e184570d207b909c90afaa5d79e0"
    "main/d/dash/dash_0.5.12-12_amd64.deb a8902cb6d8650134764a25fb80aec8589d858ef71ece1680a62e84816c37bb04"
    "main/d/diffutils/diffutils_3.10-4_amd64.deb d3412669f7ae53ba895c3a509620ea7494ec1a36faa204c5215442ced381a479"
    "main/f/file/file_5.46-5_amd64.deb 5b980b02a7d91aab9cb50c65ac7e059678a36d36ed626ec52506442f4501a1a7"
    "main/f/findutils/findutils_4.10.0-3_amd64.deb 3edf02b016456b6a98cefdbab99c5ce6f829b786f63affe970badea221aecfc7"
    "main/g/gawk/gawk_5.2.1-2+b1_amd64.deb 0e3b376e74cadfa5049bc8248c563efe89fd45e31466f8891c094c65c0879f5c"
    "main/g/grep/grep_3.11-4_amd64.deb 2d78ed07d86a530ebf538f05cb2415f37c8daf908596bc32fc2563cc7e88c55a"
    "main/g/gzip/gzip_1.13-1+deb13u1_amd64.deb 74cf12212beee4ab8d473bdc0107abf9e8cef737492198e2f4944a19f142b0ac"
    "main/a/acl/libacl1_2.3.2-2+b1_amd64.deb 08074f01e384bc07c0c2d79a58cf4a6523f71cf75d1808101c79617656c9a39d"
    "main/a/attr/libattr1_2.5.2-3_amd64.deb 606b5ee12ea2786be607a17f40c1fb5e65c76ceaff66665bdf8f8c6c1b71d1fb"
    "main/b/binutils/libbinutils_2.44-3_amd64.deb 4f4664c8a8f0ad0c8631c39fab02e3d8d86ccc6f4436a1d59f059dbcb0492679"
    "main/b/bzip2/libbz2-1.0_1.0.8-6_amd64.deb cba4cda04244b5e481bb15524bc3c983a7d1b6f330013b9b381706a2fcb65310"
    "main/libc/libcap2/libcap2_2.75-10+deb13u1+b3_amd64.deb 89fc4d34fc7a28ad6f0fcd0c561ab253b9dedf6f77f5a000b47c276c8295bf67"
    "main/b/binutils/libctf-nobfd0_2.44-3_amd64.deb e280b2be3db6e584500e865c251605b95c346767d87ccb2524e44992048fc657"
    "main/b/binutils/libctf0_2.44-3_amd64.deb 120cafcd93132a276fa92a8fb4cf39b23d14e5a3e348f4f5580638d71ca95ac5"
    "main/g/gmp/libgmp10_6.3.0+dfsg-3_amd64.deb d0d0265eb01770f17afd0f7c8c0622f80479dcfbbe13653a0debeec61464e622"
    "main/b/binutils/libgprofng0_2.44-3_amd64.deb 0d36d0bde85c467ac6735dded08e19dd30ae1369e3908b9eb48ccc0d6ff66648"
    "main/i/isl/libisl23_0.27-1_amd64.deb ac8518042e81c00de1effb72bba7e88ac4ecd488f7ea8b9e3ebc63159cb53b35"
    "main/j/jansson/libjansson4_2.14-2+b3_amd64.deb 60707a62fe6c1228c3389b12a13ca4efd76defc5532473e547a29e99cf7d2a6e"
    "main/x/xz-utils/liblzma5_5.8.1-1+deb13u1_amd64.deb 1cfcc6e0dc36f438a79b6e2189facdb9d150b08f57d190a60e01c98075c7f896"
    "main/f/file/libmagic-mgc_5.46-5_amd64.deb 1368da7c4c7dd10fb2f9ee3ed8a70801650c0c1d249a4076af15e2ddfbb14b46"
    "main/f/file/libmagic1t64_5.46-5_amd64.deb 53f653873216b135570b42c423ea93825c008ac464780b4578e80ee424237a4e"
    "main/m/mpclib3/libmpc3_1.3.1-1+b3_amd64.deb 2af0a5c128e03694a41c0b011bd8a958b7297436cdb3a15ddad7866dae8c300b"
    "main/m/mpfr4/libmpfr6_4.2.2-1_amd64.deb 75dddce11dabc7fc543712c33dc27b7f2ee66a111763eb5eac654d010b42cd92"
    "main/p/pcre2/libpcre2-8-0_10.46-1~deb13u2_amd64.deb 1252b96a5bc44bb5db982bef8eb18e54f5047cede2aff641bce4f8e1edb91c3e"
    "main/r/readline/libreadline8t64_8.2-6_amd64.deb eeadf2b5e755c9f183883feea2d9b5b28560284275d5f54d3e55d0923b1d0967"
    "main/libs/libselinux/libselinux1_3.8.1-1_amd64.deb 68bb8d32bd8d6d7d2f5952a169db03d1484b46ae1e52abccdec42a19dccea5d5"
    "main/b/binutils/libsframe1_2.44-3_amd64.deb 38f625dfdc582717029ac3a3e97c51d994ec2e7a0e9b230c6b44e40d1276311f"
    "main/libs/libsigsegv/libsigsegv2_2.14-1+b2_amd64.deb bac603b965f303ea88dc39cd723b3d88cd86e3be9c948783f0d5e0d00e971302"
    "main/o/openssl/libssl3t64_3.5.7-1~deb13u2_amd64.deb 916f7f40b34a06e6ebfaefcdab331bff458328411da672598f126a760472467d"
    "main/g/gcc-14/libstdc++6_14.2.0-19_amd64.deb ab1fa05837aa7a92aae748fd07a18a35f7d18bb4a71c4724fe2bbf0e32089de0"
    "main/n/ncurses/libtinfo6_6.5+20250216-2_amd64.deb 8b9f6a7983e9418564e48a627518de4c03917b56efe68d7f3e93bd8fffa1cc10"
    "main/libz/libzstd/libzstd1_1.5.7+dfsg-1_amd64.deb 2f6a2aeacfc925eba8b00ac9139bc4bfccf8cacb09eb93de067074b26948eef9"
    "main/l/linux/linux-libc-dev_6.12.94-1_all.deb 6183985d8fa4b97d277e8b55b10ad247bd98bc23aa8b531c1f79f05bfbf50997"
    "main/m/make-dfsg/make_4.4.1-2_amd64.deb 70a9709e665383b06068ccd423d62ea191b65355f318a25565dbf324b254cec6"
    "main/o/openssl/openssl-provider-legacy_3.5.7-1~deb13u2_amd64.deb f155c8191ae6d41da73d792f4182680aeafeb85c3dd223934ee9fdd115c4f1fa"
    "main/s/sed/sed_4.9-2+deb13u1_amd64.deb 7071270ed4f6adda55bc4f926347fb847bacaffbdee3dff917bc6006ed7e3775"
    "main/t/tar/tar_1.35+dfsg-3.1_amd64.deb 214c02e1aa291076a3147b8a4c4cae02fadf4c67df629186e3e313726faef1de"
    "main/x/xz-utils/xz-utils_5.8.1-1+deb13u1_amd64.deb 9e0a39d95373afdaad3c9dfaab3c8b0beb9a2c4654e6be10b4bcf458509a3217"
)

# Path under the dist server and SHA-256 of each Rust component.
components=(
    "dist/2026-07-16/rustc-1.97.1-x86_64-unknown-linux-gnu.tar.xz 9819d0a32d56bd339585319c80260e332779f5541fd66838ab7e016d6c814819"
    "dist/2026-07-16/rust-std-1.97.1-x86_64-unknown-linux-gnu.tar.xz 1c1e704ae80126b7de34f72ea2825f7fd01736dec20732faed47374b95282fba"
    "dist/2026-07-16/rust-std-1.97.1-x86_64-unknown-none.tar.xz 24e213f586ecb1811a11bd40dbb53690fbae469cce89dc60f7cf20eaeaaeab29"
    "dist/2026-07-16/rust-std-1.97.1-x86_64-unknown-uefi.tar.xz 35f18a13185697e26540ef139de4408689fa77fb6427b355e0a9954d632f20fc"
    "dist/2026-07-16/rust-std-1.97.1-aarch64-unknown-none-softfloat.tar.xz 02eb0d235d8f3af63ce896178a96fcf0e78e05b5241e5ddc92d6a07e28bf5e0e"
    "dist/2026-07-16/rust-std-1.97.1-aarch64-unknown-uefi.tar.xz 90fd767018a4800c764bf06212eb96dfd17f0f7da1c2a070ee30d66c0057ac79"
    "dist/2026-07-16/rust-std-1.97.1-armv7a-none-eabi.tar.xz d9afee2a85c5a38a07ba5277ab3b1fe8560a6bfd06301256bdc57b79442f270d"
    "dist/2026-07-16/rust-std-1.97.1-armv7-unknown-linux-musleabi.tar.xz ad50b2c455548ca7a7660251a6b8ea09156182583204053da9ea000b50723ab6"
    "dist/2026-07-16/rust-std-1.97.1-x86_64-unknown-linux-musl.tar.xz 51d83178680556f73a5fa8ad865b76a1ff541867445c00fc65dc67246bc2de66"
    "dist/2026-07-16/rust-std-1.97.1-aarch64-unknown-linux-musl.tar.xz 49ff0879d94e2e8e86d5e85eb15a9215943e8c78b51363d6553443598cab5d31"
    "dist/2026-07-16/cargo-1.97.1-x86_64-unknown-linux-gnu.tar.xz e1be5f5ff7f7f80ca506fb65770b759edbdc6d303781ed71c5de8ec8a8394779"
)

for tool in curl sha256sum dpkg-deb tar mkfs.btrfs; do
    command -v "$tool" > /dev/null || { echo "fetch-rustc-sysroot: $tool is not installed" >&2; exit 1; }
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
        || { echo "fetch-rustc-sysroot: $file does not match its pinned checksum" >&2; rm -f "$file"; exit 1; }
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

# Each component carries an install.sh and a `components` list; its files
# are the directories beside them, laid out from the prefix down.
unpacked=$(mktemp -d)
trap 'rm -rf "$unpacked"' EXIT
for entry in "${components[@]}"; do
    read -r path sum <<< "$entry"
    tar -xf "$(fetch "$path" "$rust" "$sum")" -C "$unpacked"
done
mkdir -p "$tree/rust"
for component in "$unpacked"/*/*/; do
    case "$component" in
        */rustc/ | */cargo/ | */rust-std-*/) cp -a "$component". "$tree/rust/" ;;
    esac
done

# What Debian's postinst scripts and alternatives would have made. `cc` is
# the name rustc runs.
ln -sf x86_64-linux-gnu-gcc-14 "$tree/usr/bin/cc"
ln -sf x86_64-linux-gnu-gcc-14 "$tree/usr/bin/gcc"

# Documentation and manuals, which nothing runs.
rm -rf "$tree/usr/share/doc" "$tree/usr/share/man" "$tree/usr/share/lintian" "$tree/rust/share"

test -x "$tree/rust/bin/rustc" || { echo "fetch-rustc-sysroot: no rustc in the tree" >&2; exit 1; }
test -x "$tree/rust/bin/cargo" || { echo "fetch-rustc-sysroot: no cargo in the tree" >&2; exit 1; }
test -x "$tree/rust/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld" \
    || { echo "fetch-rustc-sysroot: no rust-lld in the tree" >&2; exit 1; }
for target in x86_64-unknown-none x86_64-unknown-uefi aarch64-unknown-none-softfloat     aarch64-unknown-uefi armv7a-none-eabi armv7-unknown-linux-musleabi     x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
    test -d "$tree/rust/lib/rustlib/$target/lib" \
        || { echo "fetch-rustc-sysroot: no standard library for $target in the tree" >&2; exit 1; }
done
for tool in usr/libexec/gcc/x86_64-linux-gnu/14/cc1 usr/bin/as usr/bin/ld usr/bin/ar usr/bin/make \
    usr/bin/bash usr/bin/dash usr/bin/sed usr/bin/gawk usr/bin/tar; do
    test -e "$tree/$tool" || { echo "fetch-rustc-sysroot: no $tool in the tree" >&2; exit 1; }
done
test -e "$tree/usr/include/linux/types.h" \
    || { echo "fetch-rustc-sysroot: no kernel UAPI headers in the tree" >&2; exit 1; }
test -e "$tree/usr/lib64/ld-linux-x86-64.so.2" \
    || { echo "fetch-rustc-sysroot: no linker at usr/lib64" >&2; exit 1; }

# Room to spare for what the compile writes when it runs on the volume
# itself, and a size that does not depend on how mkfs rounds.
size=$(( $(du -sm "$tree" | cut -f1) + 1024 ))
image="$out/rustc.img"
rm -f "$image"
truncate -s "${size}M" "$image"
mkfs.btrfs -q --rootdir "$tree" "$image"
# The tree stays beside the image, so the same files can be tried on the host.
echo "rustc sysroot: $image (${size} MiB), unpacked in $tree"
