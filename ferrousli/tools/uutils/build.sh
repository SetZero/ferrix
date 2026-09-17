#!/usr/bin/env bash
# Builds the uutils family as static x86-64 programs against ferrousli: Rust's
# own `std` and unwinder for the musl target, and libferrousli.a in the C
# library's place.
#
#     tools/uutils/build.sh                 # from ferrousli/, every project
#     tools/uutils/build.sh coreutils       # or the ones named
#
# sources.sh pins each release and names the target, the binaries and the
# feature set, each with its reason. On Windows, build-windows.sh builds the
# same programs from the same sources without a C toolchain.
#
# Everything is downloaded to and built under $FERRIX_UUTILS, by default
# ~/.local/share/ferrix/uutils/ferrousli, never inside the repository. Each
# binary is installed as $FERRIX_UUTILS/x86_64/<name>, only if it links.
#
# When a link fails, the symbols the project needs and ferrousli does not have
# are written, sorted, to $FERRIX_UUTILS/undefined-symbols.txt and the script
# exits 1, as tools/busybox/build.sh does for busybox.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ferrousli=$(cd "$here/../.." && pwd)
out=${FERRIX_UUTILS:-$HOME/.local/share/ferrix/uutils/ferrousli}
src=$out/src

step() { printf '\n== %s\n' "$*"; }
fail() {
    echo "${0##*/}: $*" >&2
    exit 1
}

# shellcheck source=sources.sh
. "$here/sources.sh"

# The toolchain Ferrix pins, named outright. The projects are unpacked and
# built outside the repository, where rustup would otherwise fall back to the
# machine's default toolchain -- which is usually older than ferrousli's
# rust-version and has no musl target installed.
toolchain=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$ferrousli/../rust-toolchain.toml")
[ -n "$toolchain" ] || fail "no channel in rust-toolchain.toml"
export RUSTUP_TOOLCHAIN=$toolchain
echo "toolchain $toolchain"

wanted=${*:-$PROJECTS}

if [ -f "$out/Cargo.toml" ]; then
    fail "$out holds a Cargo.toml, so cargo will treat every project unpacked
  below it as a member of that workspace, and say so in terms that name
  neither this script nor the directory. An earlier layout of this script
  unpacked a project there. Remove the directory and run this again:

      rm -rf $out"
fi

step "ferrousli, release"
# Where cargo puts the library: the caller's CARGO_TARGET_DIR when set.
target=${CARGO_TARGET_DIR:-$ferrousli/target}
cargo build --release --lib --manifest-path "$ferrousli/Cargo.toml"
lib=$target/release/libferrousli.a
crt1=$(ls -t "$target"/release/build/ferrousli-*/out/crt1.o | head -1)
[ -f "$lib" ] && [ -f "$crt1" ] || fail "cargo built no libferrousli.a and crt1.o"
echo "$lib"
echo "$crt1"

step "the panic handler, weakened"
# ferrousli is itself Rust, and a no_std library must define a #[panic_handler];
# the symbol that becomes is `rust_begin_unwind`. A Rust program brings std's,
# and two strong definitions is a link error -- the same clash `rust_eh_
# personality` has, which ferrousli settles in source by defining it weak in
# assembly. stable Rust has no way to say that about a #[panic_handler], so it
# is said here instead, on a copy of the library made for this link.
#
# Weakened rather than localized: ferrousli's own panics reach the handler
# from whatever codegen unit they happen in, and a file-local symbol would not
# satisfy those. Weak still resolves across objects, and loses to std's.
#
# The copy is this build's alone. busybox, curl and git link the library
# itself, unweakened, and are not affected by any of this.
lib_weak=$out/libferrousli-weak.a
cp "$lib" "$lib_weak"
# `awk ... exit` would close the pipe early, and under `pipefail` nm's
# SIGPIPE fails the whole line; `sed -n 1p` reads to the end instead.
panic_symbol=$(nm --defined-only "$lib" 2>/dev/null |
    awk '$2 == "T" && $3 ~ /rust_begin_unwind/ { print $3 }' | sed -n 1p)
if [ -n "$panic_symbol" ]; then
    objcopy --weaken-symbol="$panic_symbol" "$lib_weak"
    echo "weakened $panic_symbol"
else
    fail "no rust_begin_unwind in $lib; has the panic handler moved?"
fi
lib=$lib_weak

step "link libraries"
# Empty archives for the libraries rustc names on its linker line. ferrousli
# is one library, as musl is; these only stop the linker refusing a library it
# cannot find before it has said what is undefined.
libs=$out/link-libs
rm -rf "$libs"
mkdir -p "$libs"
for l in c m rt pthread dl util gcc_s; do
    ar rc "$libs/lib$l.a"
done
# -lunwind is not one of those. The musl target names it where the gnu target
# names -lgcc_eh, `std`'s panic machinery really calls into it, and rustc
# ships the answer: self-contained/libunwind.a, beside the musl libc.a this
# build does not want. It is copied rather than reached with -L, so that the
# linker cannot find musl's libc.a next to it and satisfy -lc from there.
selfcontained=$(rustc --print sysroot)/lib/rustlib/$TARGET/lib/self-contained
[ -f "$selfcontained/libunwind.a" ] ||
    fail "no libunwind.a in $selfcontained; run: rustup target add $TARGET"
cp "$selfcontained/libunwind.a" "$libs/libunwind.a"

step "linker wrapper"
# The linker every project is linked with: the host's cc, told to use
# ferrousli's entry object and library and nothing of its own.
wrapper=$out/ferrousli-link
cat > "$wrapper" <<EOF
#!/usr/bin/env bash
# Generated by ferrousli/tools/uutils/build.sh.
# rustc asks cc for lld, which a host gcc has no driver for, and which linker
# does the work is not this build's concern.
args=()
for a in "\$@"; do
    case "\$a" in -fuse-ld=*) ;; *) args+=("\$a") ;; esac
done
exec cc -static -no-pie -nostdlib -L"$libs" "$crt1" "\${args[@]}" "$lib"
EOF
chmod +x "$wrapper"

: > "$out/undefined-symbols.txt"
mkdir -p "$out/x86_64/bin"
# One unpacked tree per project, under a directory of their own, and the lot
# removed first. An earlier layout unpacked a single project directly into
# $out/build, and a Cargo.toml left behind there makes every project below it
# believe it is a member of that one's workspace.
rm -rf "$out/build"

for name in $wanted; do
    project "$name"
    build=$out/build/$PROJECT

    step "$PROJECT $TAG: sources"
    fetch_sources "$src"

    step "$PROJECT $TAG: unpack"
    rm -rf "$build"
    mkdir -p "$build"
    tar -xzf "$src/$TARBALL" -C "$build" --strip-components=1

    step "$PROJECT $TAG: build"
    # The build cache is outside the unpacked tree, so unpacking the
    # same tarball again does not throw it away: tar restores the
    # archive's timestamps, so cargo still sees the sources it built.
    # The one C dependency in the family is oniguruma, under coreutils' `expr`.
    # It is compiled with the host's cc against the host's headers; its object
    # only has to link, and what it calls -- including glibc's fortified
    # __memcpy_chk -- ferrousli has.
    log=$out/$PROJECT.log
    set +e
    (
        cd "$build" || exit 1
        # shellcheck disable=SC2086
        CC_x86_64_unknown_linux_musl=cc \
        AR_x86_64_unknown_linux_musl=ar \
        CARGO_TARGET_DIR=$out/target/$PROJECT \
        RUSTFLAGS="-C target-feature=+crt-static -C link-self-contained=no -C linker=$wrapper -C relocation-model=static" \
            cargo build --release --locked --target "$TARGET" $FEATURES \
            $(for bin in $BINS; do printf -- '--bin %s ' "$bin"; done)
    ) > "$log" 2>&1
    status=$?
    set -e
    echo "cargo exited $status; log in $log"

    grep -oE "undefined reference to \`[^']+'" "$log" |
        sed -E "s/undefined reference to .([^']+)'/\1/" >> "$out/undefined-symbols.txt" || true

    if [ "$status" -ne 0 ]; then
        if ! grep -q "undefined reference to" "$log"; then
            echo "build.sh: $PROJECT failed before linking; the errors:" >&2
            grep -E '^error' "$log" | head -30 >&2
        fi
        sort -u -o "$out/undefined-symbols.txt" "$out/undefined-symbols.txt"
        exit 1
    fi

    step "$PROJECT $TAG: install"
    for bin in $BINS; do
        install -m 755 "$out/target/$PROJECT/$TARGET/release/$bin" "$out/x86_64/bin/$bin"
        ls -l "$out/x86_64/bin/$bin"
    done
done

sort -u -o "$out/undefined-symbols.txt" "$out/undefined-symbols.txt"
step "installed"
ls -l "$out/x86_64/bin"
