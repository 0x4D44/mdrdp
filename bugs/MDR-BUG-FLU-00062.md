# MDR-BUG-FLU-00062 — Clipped SurfaceToSurface updates can paint nothing or leave unannounced partial pixels

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/display-surface
- **Raised:** 2026-08-23T12:44:12Z
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
- **State history:** Open (2026-08-23T12:44:12Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T19:46:27Z, deltic:auto role=fix run=fix-20260823T193108Z-286b9a76 branch=task/bug-MDR-BUG-FLU-00062-run-fix-20260823T193108Z-286b9a76 code=182f3cf gate=manual)

## Observation

SurfaceStore clips an overhanging source rectangle when extracting pixels but continues to use the requested width and height for source stride and destination checks. The blit can reject a valid clipped copy or partially mutate an early destination before returning without a generation bump. Use one clipped source rectangle consistently for extraction, filtering, stride, and all destinations.

## Fix

<unfixed — raised only>

## Notes
