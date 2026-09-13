# MDR-BUG-FLU-00090 — AVC stream region masks omit the final row and column

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/AVC
- **Raised:** 2026-08-24T09:17:01Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T144030Z-5dfa9b95
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00090-run-verify-20260913T144030Z-5dfa9b95
- **Owner base:** 95e92511c68d8c12bae91f4b9e006b1d8d83d052
- **Owner fingerprint:** sha256:b6f640bcfaa0422b7b708b1cc8b8ac52cc0de4c5cb269ee72b7c8eb6a22c7ff1
- **Owner since:** 2026-09-13T14:40:30Z
- **Owner until:** 2026-09-13T16:40:30Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T09:17:01Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T09:24:50Z, deltic:auto role=fix run=fix-20260824T091738Z-b26417bf branch=task/bug-MDR-BUG-FLU-00090-run-fix-20260824T091738Z-b26417bf code=70a5a78 gate=manual)

## Observation

Avc420Region stores inclusive right/bottom bounds but copies them unchanged into wire RDPGFX_RECT16 stream rectangles, whose right/bottom bounds are exclusive. A 4x4 full-frame region therefore emits (0,0,3,3), so AVC420, AVC444, and mixed-tile clients leave the last row and column stale. Convert producer stream-mask bounds to exclusive coordinates and cover full-frame plus subregion encoding with focused regressions.

## Fix

<unfixed — raised only>

## Notes
