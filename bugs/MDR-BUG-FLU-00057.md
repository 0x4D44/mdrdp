# MDR-BUG-FLU-00057 — Rhydra IOSurface presenter can overwrite a surface still in compositor use

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/presentation
- **Raised:** 2026-08-23T12:09:38Z
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
- **State history:** Open (2026-08-23T12:09:38Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

The macOS IOSurface presenter first seeks a non-last surface whose is_in_use flag is false, then falls back to any non-last surface when none is free. Writing that compositor-owned surface can produce torn rows, mixed frames, or old-frame flashes. Treat no free surface as backpressure and retry or drop safely; never overwrite an in-use surface.

## Fix

<unfixed — raised only>

## Notes
