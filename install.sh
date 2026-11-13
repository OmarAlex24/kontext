#!/bin/sh
# Installs the kontext binary from a GitHub Release.
#
#   curl -fsSL https://raw.githubusercontent.com/OmarAlex24/kontext/main/install.sh | sh
#
# It downloads the archive for this machine (macOS arm64, Linux x86_64 or
# arm64), checks it against the release's SHA256SUMS, and puts `kontext` in
# ~/.local/bin, or in $KONTEXT_INSTALL_DIR.
#
#   KONTEXT_VERSION       a tag (v0.2.0); the latest release if not set
#   KONTEXT_INSTALL_DIR   where to put the binary (~/.local/bin)
#   KONTEXT_DOWNLOAD_BASE where the archives are (the release's download URL);
#                         for tests, any URL curl reads, file:// included
set -eu

REPO="OmarAlex24/kontext"

say() { printf 'kontext-install: %s\n' "$*" >&2; }
fail() { say "$*"; exit 1; }

target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os/$arch" in
        Darwin/arm64 | Darwin/aarch64) echo aarch64-apple-darwin ;;
        Linux/x86_64 | Linux/amd64) echo x86_64-unknown-linux-musl ;;
        Linux/aarch64 | Linux/arm64) echo aarch64-unknown-linux-musl ;;
        *) fail "there is no kontext build for $os $arch; build it with \`cargo install --git https://github.com/$REPO --locked\`" ;;
    esac
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        fail "neither sha256sum nor shasum is installed: cannot check the download"
    fi
}

command -v curl >/dev/null 2>&1 || fail "curl is needed"
command -v tar >/dev/null 2>&1 || fail "tar is needed"

t=$(target)
asset="kontext-$t.tar.gz"
if [ -n "${KONTEXT_DOWNLOAD_BASE:-}" ]; then
    base="${KONTEXT_DOWNLOAD_BASE%/}"
elif [ -n "${KONTEXT_VERSION:-}" ]; then
    base="https://github.com/$REPO/releases/download/$KONTEXT_VERSION"
else
    base="https://github.com/$REPO/releases/latest/download"
fi
dir="${KONTEXT_INSTALL_DIR:-$HOME/.local/bin}"

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t kontext)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $asset from $base"
curl -fsSL "$base/$asset" -o "$tmp/$asset" || fail "could not download $base/$asset"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" || fail "could not download $base/SHA256SUMS"

want=$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
[ -n "$want" ] || fail "SHA256SUMS has no line for $asset"
got=$(sha256_of "$tmp/$asset")
[ "$want" = "$got" ] || fail "the checksum of $asset does not match (expected $want, got $got): nothing was installed"

tar -xzf "$tmp/$asset" -C "$tmp"
[ -f "$tmp/kontext" ] || fail "$asset has no kontext binary inside"
mkdir -p "$dir"
# Replace it in one go, so a running kontext is never left half-written.
cp "$tmp/kontext" "$dir/.kontext.new.$$"
chmod 755 "$dir/.kontext.new.$$"
mv -f "$dir/.kontext.new.$$" "$dir/kontext"

say "installed $("$dir/kontext" --version 2>/dev/null || echo kontext) in $dir/kontext"
case ":$PATH:" in
    *":$dir:"*) ;;
    *) say "$dir is not in your PATH: add it, for example with
    echo 'export PATH=\"$dir:\$PATH\"' >> ~/.profile" ;;
esac
say "next: \`kontext install\` in a code repo (it records this binary's path in the hooks)"
