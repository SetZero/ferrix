#!/bin/sh
# Ferrix builds Ferrix on the Pixel 7: stage 20's self-hosting test on Arm
# hardware. The phone's own desktop.Image boots in crosvm, unchanged, with
# pid 1 named on its command line (`ferrix.init`): a script on a btrfs
# volume that also carries the AArch64 toolchain
# (FERRIX_SYSROOT_ARCH=arm64 tools/common/fetch/fetch-rustc-sysroot.sh),
# every file git tracks in this checkout and the workspace's vendored
# crates. The script runs `cargo xtask build --arch aarch64` and powers the
# guest off, which commits the volume; `pull` brings the volume back for
# `btrfs check` and the image Ferrix built.
#
# Usage: tools/vendor/google/pixel7/selfhost.sh stage|push|run|status|pull
#
#   stage   make the volume in $FERRIX_SELFHOST (default
#           ~/.local/share/ferrix/selfhost-arm64): toolchain, sources,
#           vendored crates, the guest's Cargo configuration and init
#   push    copy it to the phone in checksummed pieces, gzipped (the volume
#           is mostly free space, and the USB link drops now and then: a
#           piece that fails is sent again, not the whole volume)
#   run     stop any VM the app started, boot desktop.Image with the volume
#           as vdd and the build as pid 1, and return; the serial log is
#           selfhost.serial beside the images on the phone
#   status  the serial log's last lines, and whether crosvm still runs
#   pull    the volume back from the phone, after the guest powered off
#
# Environment: FERRIX_PIXEL7_SERIAL (else ~/.local/share/ferrix/pixel7/serial),
# FERRIX_SELFHOST_MEMORY (MiB, 5120), FERRIX_SELFHOST_CPUS (4),
# FERRIX_SELFHOST_JOBS (2). The phone has 7.6 GiB; Ferrix keeps a btrfs
# file's pages in memory, so memory, not processors, bounds the jobs.
set -eu
what=${1:?usage: selfhost.sh stage|push|run|status|pull}
work=${FERRIX_SELFHOST:-$HOME/.local/share/ferrix/selfhost-arm64}
toolchain=${FERRIX_RUSTC_SYSROOT:-$HOME/.local/share/ferrix/rustc-arm64}/tree
serial=${FERRIX_PIXEL7_SERIAL:-$(cat "$HOME/.local/share/ferrix/pixel7/serial" 2>/dev/null || true)}
vm=/data/local/tmp/ferrix-vm
crosvm=/apex/com.android.virt/bin/crosvm
memory=${FERRIX_SELFHOST_MEMORY:-5120}
cpus=${FERRIX_SELFHOST_CPUS:-4}
jobs=${FERRIX_SELFHOST_JOBS:-2}
piece=256M

# Run $1 as root on the phone and answer with its status, not adb's or a
# pipe's. The USB link drops now and then: wait for the phone first.
phone() {
    [ -n "$serial" ] || { echo "selfhost: no phone named: set FERRIX_PIXEL7_SERIAL" >&2; exit 1; }
    adb -s "$serial" wait-for-device
    said=$(adb -s "$serial" shell "su -c '$1'; echo \"status=\$?\"" | tr -d '\r')
    printf '%s\n' "${said%status=*}" | sed '$d'
    return "${said##*status=}"
}

# Run $1 as root on the phone detached from adb, so a dropped link cannot
# end it, and wait until it has finished.
detached() {
    phone "rm -f $vm/.done; setsid sh -c \"$1; echo \\\$? > $vm/.done\" </dev/null >$vm/.out 2>&1 &"
    until phone "[ -f $vm/.done ]" 2>/dev/null; do sleep 5; done
    phone "cat $vm/.out; exit \$(cat $vm/.done)"
}

# pid 1 in the guest. /bin/busybox is the desktop's own; the links are the
# ones gcc names that the Chromium desktop's links into /data do not cover.
init() {
    cat <<EOF
#!/bin/busybox sh
set -o pipefail
export PATH=/data/rust/bin:/data/usr/bin:/bin:/sbin
for link in usr/lib/gcc usr/libexec/gcc usr/include lib/ld-linux-aarch64.so.1 \\
    lib/aarch64-linux-gnu usr/lib/aarch64-linux-gnu; do
    [ -e /\$link ] && continue
    mkdir -p "/\${link%/*}"
    ln -s "/data/usr/\${link#usr/}" "/\$link"
done
export HOME=/data/home CARGO_HOME=/data/cargo-home CARGO_TARGET_DIR=/data/target
export CARGO_BUILD_JOBS=$jobs
cd /data/src
echo "SELFHOST-BEGIN uptime \$(cut -d' ' -f1 /proc/uptime) jobs $jobs"
{ cargo -V && cargo xtask build --arch aarch64; } 2>&1 | tee /data/selfhost.log
status=\$?
echo "SELFHOST-STATUS \$status uptime \$(cut -d' ' -f1 /proc/uptime)"
sync
EOF
}

case $what in
stage)
    [ -x "$toolchain/rust/bin/rustc" ] \
        || { echo "selfhost: no toolchain in $toolchain: run FERRIX_SYSROOT_ARCH=arm64 fetch-rustc-sysroot.sh" >&2; exit 1; }
    stage=$work/stage
    rm -rf "$stage"
    mkdir -p "$stage/src" "$stage/cargo-home" "$stage/home" "$stage/selfhost"
    cp -a --reflink=auto "$toolchain/." "$stage/"
    root=$(git rev-parse --show-toplevel)
    git -C "$root" ls-files -z | (cd "$root" && tar --null -T - -cf -) | tar -xf - -C "$stage/src"
    (cd "$root" && cargo vendor --locked --offline "$stage/vendor") > "$work/vendor-config.toml"
    { sed "s|$stage/vendor|/data/vendor|g" "$work/vendor-config.toml"; printf '\n[net]\noffline = true\n'; } \
        > "$stage/cargo-home/config.toml"
    init > "$stage/selfhost/init"
    chmod 755 "$stage/selfhost/init"
    # Room for the target directory (about 2 GiB for a debug build) and
    # btrfs keeping its metadata twice.
    size=$(( $(du -sm "$stage" | cut -f1) + 6144 ))
    rm -f "$work/selfhost.img"
    truncate -s "${size}M" "$work/selfhost.img"
    mkfs.btrfs -q --rootdir "$stage" "$work/selfhost.img"
    echo "selfhost: $work/selfhost.img (${size} MiB, sparse), $(git -C "$root" rev-parse --short HEAD)"
    ;;
push)
    rm -rf "$work/pieces"
    mkdir -p "$work/pieces"
    gzip -1 -c "$work/selfhost.img" | split -b "$piece" -d -a 3 - "$work/pieces/p"
    (cd "$work/pieces" && sha256sum p* > SUMS)
    phone "mkdir -p $vm/pieces && chmod 777 $vm/pieces"
    adb -s "$serial" push "$work/pieces/SUMS" "$vm/pieces/SUMS" > /dev/null
    for file in "$work"/pieces/p*; do
        name=${file##*/}
        sum=$(grep " $name\$" "$work/pieces/SUMS" | cut -d' ' -f1)
        for try in 1 2 3 4 5; do
            there=$(adb -s "$serial" shell "sha256sum $vm/pieces/$name 2>/dev/null" | cut -d' ' -f1) || true
            [ "$there" = "$sum" ] && break
            adb -s "$serial" wait-for-device
            adb -s "$serial" push "$file" "$vm/pieces/$name" > /dev/null || sleep 3
        done
        [ "$there" = "$sum" ] || { echo "selfhost: $name did not arrive whole" >&2; exit 1; }
        echo "  $name"
    done
    detached "cd $vm/pieces && sha256sum -c SUMS >/dev/null && cat p* | gunzip > $vm/selfhost.img && rm -rf $vm/pieces"
    detached "sha256sum $vm/selfhost.img" | cut -d' ' -f1 > "$work/phone.sum"
    [ "$(cat "$work/phone.sum")" = "$(sha256sum "$work/selfhost.img" | cut -d' ' -f1)" ] \
        || { echo "selfhost: the phone's volume differs from $work/selfhost.img" >&2; exit 1; }
    echo "selfhost: the volume is on the phone, checksum matched"
    ;;
run)
    # crosvm takes a few seconds to go after `stop`.
    phone "$crosvm stop $vm/crosvm.sock >/dev/null 2>&1; $crosvm stop $vm/selfhost.sock >/dev/null 2>&1; \
i=0; while pidof crosvm >/dev/null && [ \$i -lt 30 ]; do sleep 1; i=\$((i+1)); done; ! pidof crosvm" \
        || { echo "selfhost: a crosvm is still running" >&2; exit 1; }
    phone "cd $vm && for b in a b c; do [ -f blank-\$b.img ] || truncate -s 1M blank-\$b.img; done; rm -f selfhost.sock selfhost.serial; \
setsid $crosvm run --disable-sandbox -m $memory --cpus $cpus -s $vm/selfhost.sock \
--serial type=file,path=$vm/selfhost.serial,num=1 \
--block path=$vm/blank-a.img --block path=$vm/blank-b.img --block path=$vm/blank-c.img --block path=$vm/selfhost.img \
-p \"ferrix.checks=skip ferrix.init=/data/selfhost/init\" desktop.Image > $vm/selfhost.crosvm 2>&1 < /dev/null &"
    sleep 5
    phone "pidof crosvm; tail -3 $vm/selfhost.serial"
    ;;
status)
    phone "pidof crosvm || echo 'crosvm: not running'; tail -n ${2:-15} $vm/selfhost.serial"
    ;;
pull)
    phone "pidof crosvm" && { echo "selfhost: the guest still runs" >&2; exit 1; }
    detached "cd $vm && rm -f selfhost.img.gz && gzip -1 -k selfhost.img && sha256sum selfhost.img.gz" > "$work/pulled.sum"
    adb -s "$serial" pull "$vm/selfhost.img.gz" "$work/" > /dev/null
    (cd "$work" && sed 's|/.*/||' pulled.sum | sha256sum -c - && gunzip -f selfhost.img.gz)
    echo "selfhost: $work/selfhost.img is back"
    ;;
*)
    echo "usage: selfhost.sh stage|push|run|status|pull" >&2
    exit 1
    ;;
esac
