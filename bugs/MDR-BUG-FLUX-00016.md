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

<unfixed — raised only>

## Notes
