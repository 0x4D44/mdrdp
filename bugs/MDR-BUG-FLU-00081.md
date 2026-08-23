# MDR-BUG-FLU-00081 — Clipped SurfaceToCache rectangles leave EGFX cache metadata at the unclipped size

- **State:** Open
- **Priority:** Must
- **Severity:** Medium
- **Area:** rdp/surface-cache
- **Raised:** 2026-08-23T22:02:07Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T220228Z-01dc9cf1
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00081-run-fix-20260823T220228Z-01dc9cf1
- **Owner base:** 456d960fbe130a3aa9a33346155d750e3bad0533
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T22:02:28Z
- **Owner until:** 2026-08-24T00:02:28Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T22:02:07Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

SurfaceStore clips an overhanging SurfaceToCache rectangle and stores the clipped bitmap dimensions, while GfxHandler records the original requested dimensions after any Ok result, including a fully out-of-bounds no-op. Later CacheToSurface placement checks and cache diagnostics use metadata that does not match the stored pixels, so valid placements can be skipped and diagnostics report the wrong size. Return the actual stored dimensions from SurfaceStore and update metadata only when a cache entry was written.

## Fix

<unfixed — raised only>

## Notes
