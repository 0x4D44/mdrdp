# MDR-BUG-FLU-00114 — Partial AVC444 validity lets chroma repaint from zero or stale luma

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/avc444
- **Raised:** 2026-08-24T17:22:02Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T054745Z-ec08590e
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00114-run-verify-20260913T054745Z-ec08590e
- **Owner base:** cfabca213f3a5254d4e9f17005d5654418bfdb9b
- **Owner fingerprint:** sha256:af686fa96275224a79a6be3efdd48439d0fa5df3b49551b3b279a36396de03d2
- **Owner since:** 2026-09-13T05:47:45Z
- **Owner until:** 2026-09-13T07:47:45Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T17:22:02Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-24T17:55:36Z, deltic:auto role=fix run=fix-20260824T172235Z-bb368359 branch=task/bug-MDR-BUG-FLU-00114-run-fix-20260824T172235Z-bb368359 code=ccff9a0 gate=manual)

## Observation

After a partial non-AVC mutation, GraphicsPipelineClient drops the whole surface AVC444 buffer. A following LC1 update for one region recreates the surface-wide buffer, so a disjoint LC2 update is accepted against zero luma and unaffected regions lose their prior odd chroma. Empty or wholly clipped LC1 rectangles likewise establish a false baseline. Track luma validity by region, preserve unaffected AVC444 history when possible, and never emit LC2 for pixels without a valid luma baseline.

## Fix

<unfixed — raised only>

## Notes
