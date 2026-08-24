# MDR-BUG-FLUX-00020 — ironrdp-graphics and ironrdp-pdu tests never run: cargo refuses to test a path dep with dev-dependencies

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** testing
- **Raised:** 2026-08-19T23:06:09Z
- **Discovery source:** Agent
- **Owner:** -
- **Owner role:** -
- **Owner run:** -
- **Owner host:** -
- **Owner branch:** -
- **Owner base:** -
- **Owner fingerprint:** -
- **Owner since:** -
- **Owner until:** -
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-19T23:06:09Z, raised via `deltic bugs new` model=claude-opus-5@high) -> Fixed (2026-08-24T19:32:28Z, deltic:auto role=fix run=fix-20260824T191841Z-ef9a618c branch=task/bug-MDR-BUG-FLUX-00020-run-fix-20260824T191841Z-ef9a618c code=0f6c16a0d933e1e2d126c2d8d79f97ea2a2a2ed5 gate=manual)

## Observation

`cargo test -p ironrdp-graphics` and `cargo test -p ironrdp-pdu` both refuse outright:

```
error: package `ironrdp-graphics` cannot be tested because it requires dev-dependencies
and is not a member of the workspace
```

So those two crates' tests run **nowhere**. `cargo test` at the repo root does not compile
their `#[cfg(test)]` code (they are `[patch.crates-io]` path dependencies, not workspace
members — MDR-BUG-FLUX-00016), and `-p` is refused for the reason above.

**What that leaves uncovered is not incidental.** `vendor/ironrdp-graphics/src/avc444.rs`
holds the AVC444 combination math and its tests, including the ones written as the
regression guards for MDR-BUG-FLUX-00007 (a luma pass must preserve delivered odd chroma)
and MDR-BUG-FLUX-00010 (paint the flat average while chroma is stale) — e.g.
`a_changed_average_under_a_luma_only_frame_paints_flat_until_chroma_catches_up`. Those
guards have never executed. The only reason the FLUX-00007 contract is currently tested at
all is `ironrdp-egfx`'s client-level test, which was itself stale and red until
MDR-BUG-FLUX-00016.

`scripts/test-vendored.sh` reports both crates as SKIPPED with the reason rather than
implying they passed, so the gap is visible — but visible is not covered.

Expected: every vendored crate's tests run somewhere in the routine sweep.

**One fix has already been tried and rejected — do not simply retry it.** Making the
vendored crates workspace members does work, and `cargo test --workspace` then runs all of
them. But resolving their dev-dependencies and optional features adds 37 packages to
`Cargo.lock`, including `winscard`, `libz-sys`, `openh264`, `zstd-sys` and `nasm-rs`.
`CLAUDE.md` is explicit that the `winscard -> flate2/zlib -> libz-sys` subtree is what
breaks `scripts/check-windows.sh`, which is the repo's only guard against silent
macOS-only drift. Measured 2026-08-19; the reasoning is recorded in `Cargo.toml` beside
`[patch.crates-io]`.

Fix directions worth exploring instead:
1. A workspace scoped so the vendored crates' dev-dependencies do not reach the mdrdp
   binary's resolution — e.g. a second workspace rooted at `vendor/`, tested separately.
2. Trim the vendored crates' own dev-dependencies (they are our patched copies; several
   are for upstream tests we do not run).
3. Upstream the vendored patches so the fork disappears and the question with it.

## Fix

<unfixed — raised only>

## Notes
