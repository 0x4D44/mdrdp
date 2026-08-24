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
# Crates with dev-dependencies cannot be selected from mdrdp's package context.
# Run those from their own manifest and tracked lockfile instead; their test-only
# resolution then stays separate from the Windows-cross-checked application lock.
set -uo pipefail

cd "$(dirname "$0")/.."

failed=()
vendor_target="${CARGO_TARGET_DIR:-$PWD/target}/vendored-crates"

for dir in vendor/*/; do
    [ -f "${dir}Cargo.toml" ] || continue
    pkg="$(basename "$dir")"
    echo "=== ${pkg} ==="
    case "$pkg" in
        ironrdp-graphics|ironrdp-pdu)
            output="$(CARGO_TARGET_DIR="$vendor_target" cargo test \
                --manifest-path "${dir}Cargo.toml" --locked 2>&1 </dev/null)"
            ;;
        *)
            output="$(cargo test -p "$pkg" 2>&1 </dev/null)"
            ;;
    esac
    status=$?
    echo "$output"
    if [ "$status" -eq 0 ]; then
        continue
    fi
    failed+=("$pkg")
done

echo
if [ "${#failed[@]}" -gt 0 ]; then
    echo "FAILED: ${failed[*]}"
    echo "These crates are NOT covered by 'cargo test' — fix them before integrating."
    exit 1
fi
echo "All vendored crates passed."
