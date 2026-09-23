export PATH=/data/rust/bin:/data/usr/bin:/bin
export HOME=/data/home
cd /data/src/probe
rustc --edition 2021 -O check.rs -o /tmp/check || exit 3
/tmp/check mimic /data/src/probe/mimic.bin
/tmp/check mimic /tmp/mimic.bin
rustc --edition 2021 --crate-type=rlib -C save-temps --out-dir /data/src/probe lib.rs
echo "probe rlib on btrfs: $?"
for f in /data/src/probe/*.o; do /tmp/check $f; done
/tmp/check /data/src/probe/liblib.rlib
rustc --edition 2021 --crate-type=rlib -C save-temps --out-dir /tmp lib.rs
echo "probe rlib on tmpfs: $?"
for f in /tmp/*.o; do /tmp/check $f; done
exit 20
