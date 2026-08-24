# MDR-BUG-FLU-00090 — AVC stream region masks omit the final row and column

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/AVC
- **Raised:** 2026-08-24T09:17:01Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T091738Z-b26417bf
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00090-run-fix-20260824T091738Z-b26417bf
- **Owner base:** 819fb324b8e984f53004d55529a583d984478aca
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T09:17:38Z
- **Owner until:** 2026-08-24T11:17:38Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T09:17:01Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

Avc420Region stores inclusive right/bottom bounds but copies them unchanged into wire RDPGFX_RECT16 stream rectangles, whose right/bottom bounds are exclusive. A 4x4 full-frame region therefore emits (0,0,3,3), so AVC420, AVC444, and mixed-tile clients leave the last row and column stale. Convert producer stream-mask bounds to exclusive coordinates and cover full-frame plus subregion encoding with focused regressions.

## Fix

<unfixed — raised only>

## Notes
