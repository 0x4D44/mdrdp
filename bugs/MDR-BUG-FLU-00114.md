# MDR-BUG-FLU-00114 — Partial AVC444 validity lets chroma repaint from zero or stale luma

- **State:** Open
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
- **State history:** Open (2026-08-24T17:22:02Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

After a partial non-AVC mutation, GraphicsPipelineClient drops the whole surface AVC444 buffer. A following LC1 update for one region recreates the surface-wide buffer, so a disjoint LC2 update is accepted against zero luma and unaffected regions lose their prior odd chroma. Empty or wholly clipped LC1 rectangles likewise establish a false baseline. Track luma validity by region, preserve unaffected AVC444 history when possible, and never emit LC2 for pixels without a valid luma baseline.

## Fix

<unfixed — raised only>

## Notes
