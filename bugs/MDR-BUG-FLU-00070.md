# MDR-BUG-FLU-00070 — Native tiled frames can be presented after only one tile has updated

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** native/rendering-atomicity
- **Raised:** 2026-08-23T20:34:24Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T204716Z-15a4fc0c
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00070-run-fix-20260823T204716Z-15a4fc0c
- **Owner base:** 18b52a579c9d43d860e90fc44b9d4a5ddc60284e
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T20:47:16Z
- **Owner until:** 2026-08-23T22:47:16Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T20:34:24Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

NativeSink writes each decoded tile directly into the shared SurfaceStore and increments its visible generation before the logical frame is complete. Wakeups wait for all tiles, but an already queued platform redraw can snapshot between tile writes and present half of frame N+1 beside half of frame N. Commit a tiled sequence to the presentation-visible surface atomically only after every advertised tile for that sequence has arrived.

## Fix

<unfixed — raised only>

## Notes
