# MDR-BUG-FLU-00070 — Native tiled frames can be presented after only one tile has updated

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** native/rendering-atomicity
- **Raised:** 2026-08-23T20:34:24Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T115128Z-76a9783b
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00070-run-verify-20260913T115128Z-76a9783b
- **Owner base:** 2d34c42e75823d12c6259323b122972c3e01ffea
- **Owner fingerprint:** sha256:c2769b0316a11d59fc6f37b2ed2deb90ab1f6f4369463cf5ae26e592074ee4cb
- **Owner since:** 2026-09-13T11:51:28Z
- **Owner until:** 2026-09-13T13:51:28Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T20:34:24Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T21:14:47Z, deltic:auto role=fix run=fix-20260823T204716Z-15a4fc0c branch=task/bug-MDR-BUG-FLU-00070-run-fix-20260823T204716Z-15a4fc0c code=42b82b1d63e7cd8b0b36ae6887f61891d23a3171 gate=manual)

## Observation

NativeSink writes each decoded tile directly into the shared SurfaceStore and increments its visible generation before the logical frame is complete. Wakeups wait for all tiles, but an already queued platform redraw can snapshot between tile writes and present half of frame N+1 beside half of frame N. Commit a tiled sequence to the presentation-visible surface atomically only after every advertised tile for that sequence has arrived.

## Fix

<unfixed — raised only>

## Notes
