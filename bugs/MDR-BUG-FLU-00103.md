# MDR-BUG-FLU-00103 — Odd-sized AVC444 edge chroma is discarded by later luma

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/avc444
- **Raised:** 2026-08-24T12:03:48Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T080750Z-2601c231
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00103-run-verify-20260914T080750Z-2601c231
- **Owner base:** 3fcbe90348273a30e5dd7316411d2bb5112d8789
- **Owner fingerprint:** sha256:801932d8d94e1b22c4eef8fb9784cccedeb26ff3edf237ff6b21f99cd414ceec
- **Owner since:** 2026-09-14T08:07:50Z
- **Owner until:** 2026-09-14T10:07:50Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T12:03:48Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-24T12:13:16Z, deltic:auto role=fix run=fix-20260824T120414Z-824ae691 branch=task/bug-MDR-BUG-FLU-00103-run-fix-20260824T120414Z-824ae691 code=404b723 gate=manual)

## Observation

Yuv444Buffer::record_partial_chroma requires all three auxiliary samples for every 2x2 block. On odd-width or odd-height surface edges some sample positions do not exist, so valid edge detail never becomes chroma_seen and the next LC1 luma overwrites it with 4:2:0 averages. Promote against the mask of auxiliary positions that actually exist inside the surface.

## Fix

<unfixed — raised only>

## Notes
