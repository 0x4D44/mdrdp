# MDR-BUG-FLU-00103 — Odd-sized AVC444 edge chroma is discarded by later luma

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/avc444
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
- **State history:** Open (2026-08-24T12:03:48Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

Yuv444Buffer::record_partial_chroma requires all three auxiliary samples for every 2x2 block. On odd-width or odd-height surface edges some sample positions do not exist, so valid edge detail never becomes chroma_seen and the next LC1 luma overwrites it with 4:2:0 averages. Promote against the mask of auxiliary positions that actually exist inside the surface.

## Fix

<unfixed — raised only>

## Notes
