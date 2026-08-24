# MDR-BUG-FLU-00119 — Fixed-coordinate AVC chroma ghosts lack a final-pixel scroll regression

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** testing/graphics
- **Raised:** 2026-08-24T20:16:01Z
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
- **State history:** Open (2026-08-24T20:16:01Z, raised via `deltic bugs new`)

## Observation

On Kiln with mdrdp 0.1.211, purple chroma remnants remain fixed at the bottom-right of the desktop. Moving the window beneath them and scrolling its text do not move or clear the remnants. Current 0.1.214+ tests cover decoder validity state and emitted rectangles but do not reproduce a same-surface scroll/copy and assert the final RGBA pixels at the old and new coordinates.

## Fix

<unfixed — raised only>

## Notes
