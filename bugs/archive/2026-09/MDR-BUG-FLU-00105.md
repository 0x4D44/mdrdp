# MDR-BUG-FLU-00105 — Mapping an incomplete surface wakes a suppressed presentation

- **State:** Closed
- **Priority:** Should
- **Severity:** Medium
- **Area:** display/frame-atomicity
- **Raised:** 2026-08-24T12:03:48Z
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
- **State history:** Open (2026-08-24T12:03:48Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-24T12:11:23Z, deltic:auto role=fix run=fix-20260824T120451Z-2f52ab07 branch=task/bug-MDR-BUG-FLU-00105-run-fix-20260824T120451Z-2f52ab07 code=405368f gate=manual) -> Closed (2026-09-14T08:44:38Z, 0x4D44/Codex verify run=verify-20260914T083024Z-70133afe)

## Observation

SurfaceStore::map_to_output_geometry advances generation for a painted but incomplete surface even while presentation_suppressed is true. copy_presentation_state still returns Retained, so the window wakes and re-presents an old snapshot before coverage completes, and the new mapping dimensions can affect cadence. Keep generation unchanged until the replacement becomes complete.

## Fix

`src/surface.rs` keeps `presentation_suppressed` from creating a generation change
when `map_to_output_geometry` selects a painted but incomplete replacement. The
suppressed snapshot remains retained until the replacement is complete
(`4a3629c`, integrated by `405368f`).

## Verification

The lead and independent verifiers ran
`surface::tests::suppressed_presentation_ignores_a_partial_surface_mapped_after_pre_map_paint`; each selected and passed one test. The SurfaceStore test family passed 62 tests for both verifiers.

Removing the `!self.presentation_suppressed` guard at `src/surface.rs:1084` made
the independent focused test fail with generation `left 2, right 1`. The lead
inverted that guard and the focused test failed at `src/surface.rs:2435` because
the partial pre-map pixels advanced generation (`left 1, right 0`). Restoring the
guard made the lead focused test pass again. No live RDP session was run; the
state transition and generation oracle are deterministic unit coverage.

## Notes
