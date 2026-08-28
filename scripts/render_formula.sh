#!/usr/bin/env bash
# Fill homebrew/kora.rb.tmpl from a release's SHA256SUMS file.
#
#   scripts/render_formula.sh 0.0.1 artifacts/SHA256SUMS > kora.rb
set -euo pipefail

version="${1:?usage: render_formula.sh <version> [sums_file]}"
sums="${2:-artifacts/SHA256SUMS}"

sha_for() {
    local target="$1"
    grep "kora-${version}-${target}.tar.gz\$" "$sums" | cut -d' ' -f1
}

sed \
    -e "s/__VERSION__/${version}/g" \
    -e "s/__SHA_MACOS_ARM64__/$(sha_for aarch64-apple-darwin)/" \
    -e "s/__SHA_MACOS_X86_64__/$(sha_for x86_64-apple-darwin)/" \
    -e "s/__SHA_LINUX_ARM64__/$(sha_for aarch64-unknown-linux-gnu)/" \
    -e "s/__SHA_LINUX_X86_64__/$(sha_for x86_64-unknown-linux-gnu)/" \
    homebrew/kora.rb.tmpl
