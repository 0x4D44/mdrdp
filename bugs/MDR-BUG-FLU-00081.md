# MDR-BUG-FLU-00081 — Clipped SurfaceToCache rectangles leave EGFX cache metadata at the unclipped size

- **State:** Fixed
- **Priority:** Must
- **Severity:** Medium
- **Area:** rdp/surface-cache
- **Raised:** 2026-08-23T22:02:07Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T130731Z-4fc1a88c
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00081-run-verify-20260913T130731Z-4fc1a88c
- **Owner base:** 52f989e5290b55be2f8b4cde559e6955c7d3b0cd
- **Owner fingerprint:** sha256:54165ca83b84adbf6db41e4fa54bb00f2ff07418c37652d2311844776a39c050
- **Owner since:** 2026-09-13T13:07:31Z
- **Owner until:** 2026-09-13T15:07:31Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T22:02:07Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T22:06:14Z, deltic:auto role=fix run=fix-20260823T220228Z-01dc9cf1 branch=task/bug-MDR-BUG-FLU-00081-run-fix-20260823T220228Z-01dc9cf1 code=1bd98a8 gate=manual)

## Observation

SurfaceStore clips an overhanging SurfaceToCache rectangle and stores the clipped bitmap dimensions, while GfxHandler records the original requested dimensions after any Ok result, including a fully out-of-bounds no-op. Later CacheToSurface placement checks and cache diagnostics use metadata that does not match the stored pixels, so valid placements can be skipped and diagnostics report the wrong size. Return the actual stored dimensions from SurfaceStore and update metadata only when a cache entry was written.

## Fix

<unfixed — raised only>

## Notes
