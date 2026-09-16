#!/usr/bin/env bash
# Builds curl 8.22.0 with mbedTLS 3.6.7 as a static x86-64 program against
# ferrousli, and fetches the CA certificates it verifies servers with.
#
#     tools/ports/curl/build.sh            # from ferrousli/
#
# Installs, under $FERRIX_PORTS (see ../common.sh):
#   x86_64/bin/curl
#   x86_64/etc/ssl/certs/ca-certificates.crt
#   x86_64/usr/libexec/ferrix/ssl_server2, Mbed TLS's test server, with its
#   test certificate, key and CA in x86_64/usr/share/ferrix/tls-test/
#
# Pinned, and refused if their checksums differ:
#   * curl.se's curl-8.22.0.tar.gz, by the sha256 curl.se/info publishes;
#   * Mbed TLS 3.6.7, the long-term-support branch, by the sha256 its release
#     publishes. curl builds against 3.6; 4.x moved the crypto half into
#     TF-PSA-Crypto;
#   * curl.se's extract of Mozilla's CA certificates of 2026-08-13, by the
#     sha256 published beside it.
#
# What curl is built without, and why:
#   * zlib, brotli, zstd, nghttp2, libidn2, libpsl, libssh2, LDAP, GSS-API:
#     each is another library to port first. --compressed and HTTP/2 wait
#     for them; HTTP/1.1, HTTPS, FTP(S), file:, and the rest of curl's
#     protocols that need no library are in;
#   * a CA directory: one bundle, where Alpine and Debian also put it.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=../common.sh
. "$here/../common.sh"

CURL_VERSION=8.22.0
CURL_TARBALL=curl-$CURL_VERSION.tar.gz
CURL_URL=https://curl.se/download/$CURL_TARBALL
CURL_SHA256=d54dd598bf05927a726deb38df31c6a255ba83ff1de57c5d1464dac3ed8f44a1
MBEDTLS_VERSION=3.6.7
MBEDTLS_TARBALL=mbedtls-$MBEDTLS_VERSION.tar.bz2
MBEDTLS_URL=https://github.com/Mbed-TLS/mbedtls/releases/download/mbedtls-$MBEDTLS_VERSION/$MBEDTLS_TARBALL
MBEDTLS_SHA256=a7e8bcbec0e6f761b4af24f25677626b35f762f68eef79c08677a363212d11f6
CACERT=cacert-2026-08-13.pem
CACERT_URL=https://curl.se/ca/$CACERT
CACERT_SHA256=f66dff1bdf8f96060b8177976f8b7d9254bc89bc4db933d769f7384d28480bc9
# Where curl looks for the bundle on the guest, and where xtask puts it.
CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt

work=$ports/curl
src=$ports/src

step "sources"
mkdir -p "$src" "$work"
fetch "$CURL_URL" "$src/$CURL_TARBALL" sha256sum "$CURL_SHA256"
fetch "$MBEDTLS_URL" "$src/$MBEDTLS_TARBALL" sha256sum "$MBEDTLS_SHA256"
fetch "$CACERT_URL" "$src/$CACERT" sha256sum "$CACERT_SHA256"
echo "curl $CURL_VERSION, Mbed TLS $MBEDTLS_VERSION and $CACERT, all verified"

build_ferrousli
make_compilers

step "mbedtls"
mbedtls=$work/mbedtls
tls=$work/mbedtls-install
rm -rf "$mbedtls" "$tls"
mkdir -p "$mbedtls" "$tls/lib"
tar -xjf "$src/$MBEDTLS_TARBALL" -C "$mbedtls" --strip-components=1
if ! make -C "$mbedtls/library" -j"$jobs" CC="$CC" AR=ar CFLAGS=-O2 static \
    > "$work/mbedtls.log" 2>&1; then
    tail -30 "$work/mbedtls.log" >&2
    fail "mbedtls did not build; the log is $work/mbedtls.log"
fi
cp -r "$mbedtls/include" "$tls/include"
cp "$mbedtls"/library/libmbed{tls,x509,crypto}.a "$tls/lib/"

step "mbedtls: the test server"
# Mbed TLS's own test server, for test-net's HTTPS program: it answers
# https://localhost with the test certificate for "localhost" that its test
# CA signed, both valid from 2023 and 2019, so a guest whose clock still
# reads 1970 refuses the connection and one that took firmware's time
# accepts it.
if ! make -C "$mbedtls/programs" -j"$jobs" CC="$CC" AR=ar CFLAGS=-O2 ssl/ssl_server2 > "$work/mbedtls-programs.log" 2>&1; then
    tail -30 "$work/mbedtls-programs.log" >&2
    fail "mbedtls's ssl_server2 did not build; the log is $work/mbedtls-programs.log"
fi

step "curl: configure"
build=$work/build
rm -rf "$build"
mkdir -p "$build"
tar -xzf "$src/$CURL_TARBALL" -C "$build" --strip-components=1
# Configured natively rather than as a cross build: the programs configure
# links are static x86-64 programs against ferrousli, which run on this host,
# so its run-time checks ask the library curl will actually use.
if ! (cd "$build" && ./configure \
    CC="$CC" CFLAGS=-O2 \
    CPPFLAGS="-I$tls/include" LDFLAGS="-L$tls/lib" \
    --disable-shared --enable-static \
    --with-mbedtls="$tls" \
    --with-ca-bundle="$CA_BUNDLE" --without-ca-path \
    --without-zlib --without-brotli --without-zstd \
    --without-nghttp2 --without-nghttp3 --without-ngtcp2 \
    --without-libidn2 --without-libpsl --without-libssh2 --without-libssh \
    --without-librtmp --without-gssapi \
    --disable-ldap --disable-ldaps --disable-docs --disable-manual \
    > "$work/configure.log" 2>&1); then
    tail -30 "$work/configure.log" >&2
    fail "curl's configure failed; the log is $work/configure.log and config.log beside the sources"
fi
grep -E '^  (SSL|Protocols|Features):' "$work/configure.log" || true

step "curl: build"
set +e
make -C "$build" -j"$jobs" V=1 > "$work/build.log" 2>&1
status=$?
set -e
echo "make exited $status; log in $work/build.log"
undefined_symbols "$work/undefined-symbols.txt" "$work/build.log"
if [ "$status" -ne 0 ]; then
    grep -E 'error|Error' "$work/build.log" | head -30 >&2
    exit 1
fi

step "install"
mkdir -p "$prefix/bin" "$prefix/etc/ssl/certs"
# Stripped: the image carries it, and nothing on the guest reads its symbols.
install -m 755 -s "$build/src/curl" "$prefix/bin/curl"
install -m 644 "$src/$CACERT" "$prefix$CA_BUNDLE"
mkdir -p "$prefix/usr/libexec/ferrix" "$prefix/usr/share/ferrix/tls-test"
install -m 755 -s "$mbedtls/programs/ssl/ssl_server2" "$prefix/usr/libexec/ferrix/ssl_server2"
for f in server5.crt server5.key test-ca2.crt; do
    install -m 644 "$mbedtls/framework/data_files/$f" "$prefix/usr/share/ferrix/tls-test/$f"
done
file "$prefix/bin/curl"
"$prefix/bin/curl" --version
