# MDR-BUG-FLU-00103 — Odd-sized AVC444 edge chroma is discarded by later luma

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/avc444
- **Raised:** 2026-08-24T12:03:48Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T120414Z-824ae691
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00103-run-fix-20260824T120414Z-824ae691
- **Owner base:** eca70f39f6f7a48282ccb8c6c45dda5662e45876
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T12:04:14Z
- **Owner until:** 2026-08-24T14:04:14Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T12:03:48Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

Yuv444Buffer::record_partial_chroma requires all three auxiliary samples for every 2x2 block. On odd-width or odd-height surface edges some sample positions do not exist, so valid edge detail never becomes chroma_seen and the next LC1 luma overwrites it with 4:2:0 averages. Promote against the mask of auxiliary positions that actually exist inside the surface.

## Fix

<unfixed — raised only>

## Notes
