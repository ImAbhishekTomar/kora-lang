#!/usr/bin/env bash
# Fill homebrew/kora.rb.tmpl from a release's SHA256SUMS file.
#
#   scripts/render_formula.sh 0.0.1 artifacts/SHA256SUMS > kora.rb
set -euo pipefail

version="${1:?usage: render_formula.sh <version> [sums_file]}"
sums="${2:-artifacts/SHA256SUMS}"

sha_for() {
    target="$1"
    sha=$(grep "kora-${version}-${target}.tar.gz$" "$sums" | cut -d' ' -f1)
    # An absent checksum used to render as `sha256 ""`, which looks like a
    # finished formula and fails only for whoever installs on that platform.
    if [ -z "$sha" ]; then
        echo "render_formula: no checksum for ${target} in ${sums}" >&2
        echo "render_formula: the release is missing that platform's build" >&2
        return 1
    fi
    printf '%s' "$sha"
}

# Resolved before rendering, so a missing build stops the script rather than
# failing inside a substitution whose exit status nothing reads.
macos_arm64=$(sha_for aarch64-apple-darwin)
macos_x86_64=$(sha_for x86_64-apple-darwin)
linux_arm64=$(sha_for aarch64-unknown-linux-gnu)
linux_x86_64=$(sha_for x86_64-unknown-linux-gnu)

sed \
    -e "s/__VERSION__/${version}/g" \
    -e "s/__SHA_MACOS_ARM64__/${macos_arm64}/" \
    -e "s/__SHA_MACOS_X86_64__/${macos_x86_64}/" \
    -e "s/__SHA_LINUX_ARM64__/${linux_arm64}/" \
    -e "s/__SHA_LINUX_X86_64__/${linux_x86_64}/" \
    homebrew/kora.rb.tmpl
