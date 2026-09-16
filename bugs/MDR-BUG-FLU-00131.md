# MDR-BUG-FLU-00131 — Fullscreen resolution restoration is discarded after an EGFX-only resize

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** session/resolution
- **Raised:** 2026-09-16T06:45:34Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260916T064547Z-3e6476f9
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00131-run-fix-20260916T064547Z-3e6476f9
- **Owner base:** 7063b819300417f2ca5f23fb74c5826dab345f3d
- **Owner fingerprint:** -
- **Owner since:** 2026-09-16T06:45:47Z
- **Owner until:** 2026-09-16T08:45:47Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-09-16T06:45:34Z, raised via `deltic bugs new --land`)

## Observation

Arthur reports Kiln remains at 1920x1080 after a fullscreen monitor power-off/return although the desktop is 2560x1440. The v0.1.249 Kiln session log records connecting at 2560x1440, requesting 1920x1080, and subsequent visibility events without a restoration request or full session reactivation. Investigate whether the resize duplicate check retains the initial dimensions while EGFX changes the graphics output size; expected return to the current fullscreen monitor resolution.

Evidence fingerprint: `manual:v1:fullscreen-resolution-restoration-is-discarded--1c7aa6a9da28daee`


## Fix

<unfixed — raised only>

## Notes
