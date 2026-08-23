# MDR-BUG-FLU-00069 — Progressive codec state survives encoding-context deletion and same-ID surface recreation

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** rdp/progressive-rendering
- **Raised:** 2026-08-23T20:34:24Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T205316Z-c0acb207
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00069-run-fix-20260823T205316Z-c0acb207
- **Owner base:** 0f75ab213f0d9877cbf9feaab9eb168dcedbf24e
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T20:53:16Z
- **Owner until:** 2026-08-23T22:53:16Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T20:34:24Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

GfxHandler keys ProgressiveDecoder state by surface ID to tolerate Windows context rotation, but it ignores DeleteEncodingContext and on_surface_created does not retire state when an ID is reused. A deleted context or new surface incarnation can therefore refine coefficients from the old surface, painting stale colour blocks or ghosts. Track the active wire context per surface, retire only the matching active context on DeleteEncodingContext, and always clear progressive state on same-ID CreateSurface.

## Fix

<unfixed — raised only>

## Notes
