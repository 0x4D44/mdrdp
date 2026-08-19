#!/usr/bin/env bash
# Run the vendored crates' own test suites. `cargo test` at the repo root does NOT.
#
# The vendored IronRDP crates arrive through `[patch.crates-io]` as path dependencies, not
# as workspace members, so cargo builds them as libraries and never compiles their
# `#[cfg(test)]` code. That is how origin/main sat red with a failing `ironrdp-egfx` test
# while `cargo test` reported ~690 passing (MDR-BUG-FLUX-00016).
#
# Run this alongside `cargo test`. Package names match their vendor/ directory names.
#
# KNOWN GAP, deliberately not closed: a crate with dev-dependencies cannot be tested this
# way at all — cargo refuses with "requires dev-dependencies and is not a member of the
# workspace". Today that is ironrdp-graphics and ironrdp-pdu, so the avc444 codec-math
# tests do not run anywhere. Making them workspace members does fix it, and was tried:
# it resolves their dev-dependencies into Cargo.lock, adding 37 packages including
# `winscard`, `libz-sys`, `openh264`, `zstd-sys` and `nasm-rs`. CLAUDE.md is explicit that
# the `winscard -> flate2/zlib -> libz-sys` subtree is what breaks
# `scripts/check-windows.sh`, so that cure is worse than the disease. This script reports
# those crates as SKIPPED rather than pretending they passed.
set -uo pipefail

cd "$(dirname "$0")/.."

failed=()
skipped=()

for dir in vendor/*/; do
    [ -f "${dir}Cargo.toml" ] || continue
    pkg="$(basename "$dir")"
    echo "=== ${pkg} ==="
    output="$(cargo test -p "$pkg" 2>&1 </dev/null)"
    status=$?
    echo "$output"
    if [ "$status" -eq 0 ]; then
        continue
    fi
    if grep -q 'requires dev-dependencies and is not a member of the workspace' <<<"$output"; then
        skipped+=("$pkg")
    else
        failed+=("$pkg")
    fi
done

echo
if [ "${#skipped[@]}" -gt 0 ]; then
    echo "SKIPPED (cannot be tested outside a workspace): ${skipped[*]}"
fi
if [ "${#failed[@]}" -gt 0 ]; then
    echo "FAILED: ${failed[*]}"
    echo "These crates are NOT covered by 'cargo test' — fix them before integrating."
    exit 1
fi
echo "All runnable vendored crates passed."
