# MDR-BUG-FLU-00123 — AVC444 animation flickers between 4:2:0 and chroma-refined output

- **State:** Open
- **Priority:** Must
- **Severity:** Medium
- **Area:** graphics/avc444
- **Raised:** 2026-08-26T07:01:57Z
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
- **State history:** Open (2026-08-26T07:01:57Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

Arthur reports that the earlier persistent chroma degradation is fixed, but animated regions still show a repeatable chroma flicker. The visible shape matches an LC=1 main-view update being presented as required 4:2:0 output, followed by a separately presented LC=2 chroma refinement; a subsequent luma frame then returns the region to 4:2:0. Chroma-only refinement needs presentation debouncing so it settles after motion without delaying fresh luma or retaining stale auxiliary samples.

## Fix

<unfixed — raised only>

## Notes
