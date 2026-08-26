# MDR-BUG-FLU-00123 — AVC444 animation flickers between 4:2:0 and chroma-refined output

- **State:** Open
- **Priority:** Must
- **Severity:** Medium
- **Area:** graphics/avc444
- **Raised:** 2026-08-26T07:01:57Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260826T070341Z-002f10ac
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00123-run-fix-20260826T070341Z-002f10ac
- **Owner base:** b4abba6095cf5fc8991d0e7f9893350b4cbb198c
- **Owner fingerprint:** -
- **Owner since:** 2026-08-26T07:03:41Z
- **Owner until:** 2026-08-26T09:03:41Z
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
