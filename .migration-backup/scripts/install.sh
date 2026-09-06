#!/usr/bin/env sh
# Install the latest kora release binary for this OS/arch.
#
#   curl -fsSL https://raw.githubusercontent.com/ImAbhishekTomar/kora-lang/main/scripts/install.sh | sh
#
# Override the install directory with KORA_INSTALL_DIR (default: ~/.local/bin).
# Pin a version with KORA_VERSION (default: latest release).
set -eu

REPO="ImAbhishekTomar/kora-lang"
INSTALL_DIR="${KORA_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'kora-install: %s\n' "$1"; }
die() { printf 'kora-install: error: %s\n' "$1" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "'$1' is required but not found"; }
need curl
need tar
need mktemp

os=$(uname -s)
arch=$(uname -m)

case "$os" in
    Linux) platform_os="unknown-linux-gnu" ;;
    Darwin) platform_os="apple-darwin" ;;
    *) die "unsupported OS: $os (download a release manually from https://github.com/$REPO/releases)" ;;
esac

case "$arch" in
    x86_64|amd64) platform_arch="x86_64" ;;
    arm64|aarch64) platform_arch="aarch64" ;;
    *) die "unsupported architecture: $arch" ;;
esac

target="${platform_arch}-${platform_os}"

if [ "${KORA_VERSION:-}" ]; then
    version="$KORA_VERSION"
else
    version=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
        | grep -m1 '"tag_name"' | sed -E 's/.*"v?([^"]+)".*/\1/')
    [ "$version" ] || die "could not determine the latest release; set KORA_VERSION to pin one"
fi

archive="kora-${version}-${target}.tar.gz"
url="https://github.com/$REPO/releases/download/v${version}/${archive}"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

say "downloading kora ${version} for ${target}"
curl -fsSL "$url" -o "$tmp/$archive" || die "no release for $target at $url"

say "verifying checksum"
sums_url="https://github.com/$REPO/releases/download/v${version}/SHA256SUMS"
if curl -fsSL "$sums_url" -o "$tmp/SHA256SUMS" 2>/dev/null; then
    expected=$(grep "$archive\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)
    if [ "$expected" ]; then
        if command -v shasum >/dev/null 2>&1; then
            actual=$(shasum -a 256 "$tmp/$archive" | cut -d' ' -f1)
        else
            actual=$(sha256sum "$tmp/$archive" | cut -d' ' -f1)
        fi
        [ "$expected" = "$actual" ] || die "checksum mismatch for $archive"
    fi
fi

mkdir -p "$tmp/extracted"
tar xzf "$tmp/$archive" -C "$tmp/extracted"

bin_path=$(find "$tmp/extracted" -type f -name kora | head -n1)
[ "$bin_path" ] || die "'kora' binary not found in $archive"

mkdir -p "$INSTALL_DIR"
cp "$bin_path" "$INSTALL_DIR/kora"
chmod +x "$INSTALL_DIR/kora"

say "installed kora ${version} to $INSTALL_DIR/kora"

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) say "note: $INSTALL_DIR is not on your PATH. Add this to your shell profile:
    export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

"$INSTALL_DIR/kora" --version
