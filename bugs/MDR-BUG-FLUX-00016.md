# MDR-BUG-FLUX-00016 — ironrdp-egfx test avc444_lc1_updates_luma_only_inside_its_rects fails on origin/main

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** gfx
- **Raised:** 2026-08-19T16:10:47Z
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
- **State history:** Open (2026-08-19T16:10:47Z, raised via `deltic bugs new` model=claude-opus-5@high)

## Observation

`cargo test -p ironrdp-egfx` fails on a clean `origin/main` checkout:

```
---- client::tests::avc444_lc1_updates_luma_only_inside_its_rects ----
assertion `left == right` failed: inside the rect chroma degrades to the replicated average
  left: 86
 right: 31
```

Confirmed pre-existing, not introduced by the branch that found it: run in the main
clone at `origin/main` with no local modifications, it fails with the identical
assertion and the identical values. The other 22 tests in the crate pass.

The expectation is that an LC=1 (luma-only) update degrades chroma inside its rect to
the replicated 4:2:0 block average, value 31. The code now produces 86. That is the
behaviour MDR-BUG-FLUX-00010 deliberately changed ("AVC444 paints changed blocks flat
until chroma catches up", commit 0345af8) — so most likely the test encodes the old
contract and needed updating with the fix, and the assertion is simply stale. That is a
judgement for whoever owns 00010, not for a passing branch to guess at: the alternative
reading is that 00010 regressed LC=1 handling, and the two have very different fixes.

**The second defect here is that trunk went red without anyone noticing.** The vendored
crates are separate packages, so `cargo test --lib` at the workspace root — the check an
author naturally runs — never compiles or runs them. It reports 677 passing while
`ironrdp-egfx` is broken. Whatever gate is meant to protect the trunk did not cover the
vendored crates either.

Expected: `origin/main` is green, and a red vendored crate cannot reach it.

Fix directions:
1. Decide whether the assertion or the behaviour is wrong (owner of MDR-BUG-FLUX-00010).
2. Make the workspace check cover the vendored crates, so `-p ironrdp-egfx` is not a
   thing an author has to think to run.

## Fix

**The assertion was stale, not the behaviour.** Determined by mutation rather than by
argument: removing the preservation branch in `avc444::apply_luma` (`if odd_position &&
seen { continue; }`) makes the test fail with `left: 31, right: 86`. So **31 was the
overwrite value** — the luma frame's own replicated 4:2:0 average — and 86 is the chroma
the aux pass delivered. The old assertion was pinning exactly the behaviour
MDR-BUG-FLUX-00007 deliberately removed when it established that a luma pass must preserve
delivered odd chroma, because overwriting it is what made colours pump on the LC=1/LC=2
alternation Windows sends.

Rewritten to assert the invariant instead of a constant: capture the delivered chroma
before the LC=1 pass, then assert the luma pass left it alone. That cannot go stale when
the aux packing changes, and it fails if preservation is ever removed (verified by the
mutation above). A companion `assert_ne!` pins the captured value away from 31, so the
test cannot pass by the two happening to coincide.

**Second half — the coverage gap — partially closed.** `scripts/test-vendored.sh` now runs
the vendored crates' own suites, and `CLAUDE.md` documents that `cargo test` does not.

Making them workspace members was tried and **rejected**: it works, but resolving their
dev-dependencies adds 37 packages to `Cargo.lock` including `winscard`, `libz-sys`,
`openh264`, `zstd-sys` and `nasm-rs`. `CLAUDE.md` is explicit that the
`winscard -> flate2/zlib -> libz-sys` subtree is what breaks `scripts/check-windows.sh`.
The reasoning is recorded in `Cargo.toml` next to `[patch.crates-io]` so the next person
does not repeat the experiment.

**Residual, deliberately left open in this record:** `ironrdp-graphics` and `ironrdp-pdu`
have dev-dependencies, so cargo refuses to test them outside a workspace at all. Their
tests — including the avc444 codec-math tests guarding MDR-BUG-FLUX-00007 and -00010 —
therefore run nowhere. The script reports them as SKIPPED rather than implying they
passed. Closing that properly needs either upstreaming the vendored patches or a
dependency-isolation approach that does not drag `libz-sys` into the Windows check.

## Notes
