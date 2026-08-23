# MDR-BUG-FLU-00069 — Progressive codec state survives encoding-context deletion and same-ID surface recreation

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** rdp/progressive-rendering
- **Raised:** 2026-08-23T20:34:24Z
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
- **State history:** Open (2026-08-23T20:34:24Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T21:14:16Z, deltic:auto role=fix run=fix-20260823T205316Z-c0acb207 branch=task/bug-MDR-BUG-FLU-00069-run-fix-20260823T205316Z-c0acb207 code=0fe7a684414981355545ad167dd4ace10f8f8e4b gate=manual)

## Observation

GfxHandler keys ProgressiveDecoder state by surface ID to tolerate Windows context rotation, but it ignores DeleteEncodingContext and on_surface_created does not retire state when an ID is reused. A deleted context or new surface incarnation can therefore refine coefficients from the old surface, painting stale colour blocks or ghosts. Track the active wire context per surface, retire only the matching active context on DeleteEncodingContext, and always clear progressive state on same-ID CreateSurface.

## Fix

<unfixed — raised only>

## Notes
