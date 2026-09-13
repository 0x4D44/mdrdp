# MDR-BUG-FLU-00069 — Progressive codec state survives encoding-context deletion and same-ID surface recreation

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** rdp/progressive-rendering
- **Raised:** 2026-08-23T20:34:24Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T113905Z-ec68ea04
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00069-run-verify-20260913T113905Z-ec68ea04
- **Owner base:** ea47fde4c4a453d24161bbd63085d73ba927b96a
- **Owner fingerprint:** sha256:90c14f4d1cec73e686f6637a9d0bd16f85bb2317cb5c0d6d39f38b6142bd4d92
- **Owner since:** 2026-09-13T11:39:05Z
- **Owner until:** 2026-09-13T13:39:05Z
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
