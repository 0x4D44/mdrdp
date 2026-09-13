# MDR-BUG-FLU-00062 — Clipped SurfaceToSurface updates can paint nothing or leave unannounced partial pixels

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/display-surface
- **Raised:** 2026-08-23T12:44:12Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T111922Z-862bf4fc
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00062-run-verify-20260913T111922Z-862bf4fc
- **Owner base:** cf694459f99486b1648177d96240cf633fba2106
- **Owner fingerprint:** sha256:95b6471d62b235ad8e27c7242ea7bfe77ed77aed1e5832d4b07501e3fd84c790
- **Owner since:** 2026-09-13T11:19:22Z
- **Owner until:** 2026-09-13T13:19:22Z
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
