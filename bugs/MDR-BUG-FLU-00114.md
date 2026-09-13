# MDR-BUG-FLU-00114 — Partial AVC444 validity lets chroma repaint from zero or stale luma

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/avc444
- **Raised:** 2026-08-24T17:22:02Z
- **Discovery source:** Human
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
- **State history:** Open (2026-08-24T17:22:02Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-24T17:55:36Z, deltic:auto role=fix run=fix-20260824T172235Z-bb368359 branch=task/bug-MDR-BUG-FLU-00114-run-fix-20260824T172235Z-bb368359 code=ccff9a0 gate=manual) -> Closed (2026-09-13T05:52:55Z, independent verifier: the regional AVC444 suite and root invalidation mutant proved the fix, application, vendored, and Windows gates passed, model=codex@max)

## Observation

After a partial non-AVC mutation, GraphicsPipelineClient drops the whole surface AVC444 buffer. A following LC1 update for one region recreates the surface-wide buffer, so a disjoint LC2 update is accepted against zero luma and unaffected regions lose their prior odd chroma. Empty or wholly clipped LC1 rectangles likewise establish a false baseline. Track luma validity by region, preserve unaffected AVC444 history when possible, and never emit LC2 for pixels without a valid luma baseline.

## Fix

Commit `ccff9a0baea01d5383454dd1e4f196499ec84be2` keeps AVC444 validity regional
through non-AVC mutations, records the regions actually painted, and refuses LC2
promotion without a complete luma baseline. The `ironrdp-egfx` client filter passed
all 37 tests, including the seven regional regressions added by the fix; the `mdrdp`
graphics filter passed 65 tests. Replacing the regional `SolidFill` invalidation with
whole-surface buffer removal made `partial_non_avc_mutation_preserves_a_disjoint_avc444_baseline`
fail its own luma assertion (`left: 0, right: 114`). The source mutation was restored
and the source diff is empty.

The original partial-mutation observation was reviewed against the client, graphics,
and surface-validity paths. The vendored suite passed 372 tests, and
`scripts/check-windows.sh --locked` passed for both app and Rhydra host targets. This
macOS pass did not start a new Windows RDP session.

## Notes
