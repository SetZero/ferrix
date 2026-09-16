#!/usr/bin/env bash
# Builds git 2.55.0 as static x86-64 programs against ferrousli, with zlib
# from tools/ports/zlib and libcurl and Mbed TLS from tools/ports/curl, which
# must be built first.
#
#     tools/ports/git/build.sh            # from ferrousli/
#
# Installs, under $FERRIX_PORTS (see ../common.sh):
#   x86_64/usr/bin/git, and x86_64/bin/git linking to it
#   x86_64/usr/libexec/git-core/        the helpers git runs that are not
#                                       built in: git-remote-http and its
#                                       https and ftp links, the shell scripts
#   x86_64/usr/share/git-core/templates/
#
# Pinned, and refused if its checksum differs: kernel.org's git-2.55.0.tar.xz,
# by the sha256 kernel.org publishes in sha256sums.asc.
#
# What git is built without, and why:
#   * Perl, Python and Tcl/Tk: none is on the image, so git send-email, git p4,
#     gitk and git gui are left out;
#   * gettext and iconv: ferrousli has neither yet, so messages are English
#     and commit encodings are not re-encoded;
#   * expat: pushing over WebDAV needs it; fetching and cloning over HTTP(S)
#     do not.
#   * Rust: git's Rust half is built by cargo for the host, against glibc's
#     Rust standard library, which cannot be linked into a program on
#     ferrousli. NO_RUST keeps the C implementations it replaces.
#
# And with git's own regex rather than ferrousli's: git needs REG_STARTEND,
# which musl's regex, and so ferrousli's, does not have. Alpine builds git for
# musl the same way.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=../common.sh
. "$here/../common.sh"

GIT_VERSION=2.55.0
GIT_TARBALL=git-$GIT_VERSION.tar.xz
GIT_URL=https://www.kernel.org/pub/software/scm/git/$GIT_TARBALL
GIT_SHA256=457fdb04dc8728e007d4688695e6912e6f680727920f2a40bf11eacc17505357

work=$ports/git
src=$ports/src
curl_build=$ports/curl/build
tls=$ports/curl/mbedtls-install

step "sources"
mkdir -p "$src" "$work"
fetch "$GIT_URL" "$src/$GIT_TARBALL" sha256sum "$GIT_SHA256"
echo "git $GIT_VERSION, verified"

build_ferrousli
make_compilers
[ -f "$prefix/lib/libz.a" ] || fail "no zlib in $prefix/lib; run tools/ports/zlib/build.sh first"
[ -f "$curl_build/lib/.libs/libcurl.a" ] || fail "no libcurl; run tools/ports/curl/build.sh first"

step "build"
build=$work/build
rm -rf "$build"
mkdir -p "$build"
tar -xJf "$src/$GIT_TARBALL" -C "$build" --strip-components=1

# One set of make variables for the build and the install, so the paths git
# compiles in are the ones it is installed at on the guest.
options=(
    CC="$CC" AR=ar CFLAGS=-O2 LDFLAGS=-static
    prefix=/usr gitexecdir=/usr/libexec/git-core
    template_dir=/usr/share/git-core/templates sysconfdir=/etc
    NO_PERL=YesPlease NO_PYTHON=YesPlease NO_TCLTK=YesPlease
    NO_GETTEXT=YesPlease NO_ICONV=YesPlease NO_EXPAT=YesPlease
    NO_OPENSSL=YesPlease NO_INSTALL_HARDLINKS=YesPlease
    NO_REGEX=NeedsStartEnd
    NO_RUST=YesPlease
    INSTALL_SYMLINKS=YesPlease SKIP_DASHED_BUILT_INS=YesPlease
    ZLIB_PATH="$prefix"
    CURL_CFLAGS="-I$curl_build/include"
    CURL_LDFLAGS="-L$curl_build/lib/.libs -lcurl -L$tls/lib -lmbedtls -lmbedx509 -lmbedcrypto"
    V=1
)
set +e
make -C "$build" -j"$jobs" "${options[@]}" all > "$work/build.log" 2>&1
status=$?
set -e
echo "make exited $status; log in $work/build.log"
undefined_symbols "$work/undefined-symbols.txt" "$work/build.log"
if [ "$status" -ne 0 ]; then
    grep -E 'error' "$work/build.log" | sort -u | head -30 >&2
    exit 1
fi

step "install"
root=$work/root
rm -rf "$root"
make -C "$build" "${options[@]}" DESTDIR="$root" install > "$work/install.log" 2>&1 \
    || fail "install failed; the log is $work/install.log"
rm -rf "$prefix/usr/libexec/git-core" "$prefix/usr/share/git-core" "$prefix/bin/git"
mkdir -p "$prefix/bin" "$prefix/usr/bin" "$prefix/usr/libexec" "$prefix/usr/share"
# At /usr/bin/git, where the links in git-core point (../../bin/git), and
# linked from /bin, which is the shell's PATH.
install -m 755 -s "$root/usr/bin/git" "$prefix/usr/bin/git"
ln -s ../usr/bin/git "$prefix/bin/git"
cp -a "$root/usr/libexec/git-core" "$prefix/usr/libexec/git-core"
cp -a "$root/usr/share/git-core" "$prefix/usr/share/git-core"
# Not carried: the servers and mail helpers, each a static program of its own
# a few megabytes long, and the Perl, Python and Tcl scripts that cannot run
# without their interpreters. What is left is what fetching, cloning and the
# shell-script commands need: git-remote-http and its https and ftp links, and
# git-sh-i18n--envsubst beside the scripts.
for f in git-daemon git-http-backend git-http-fetch git-imap-send git-shell     git-archimport git-cvsexportcommit git-cvsimport git-cvsserver git-p4     git-send-email git-instaweb; do
    rm -f "$prefix/usr/libexec/git-core/$f"
done
# Every program in git-core is stripped; the scripts are left alone.
find "$prefix/usr/libexec/git-core" -type f -perm -u+x -exec sh -c \
    'file "$1" | grep -q ELF && strip "$1"' sh {} \;
file "$prefix/bin/git"
"$prefix/usr/bin/git" --version
du -sh "$prefix/usr/libexec/git-core"
