# MDR-BUG-FLU-00057 — Rhydra IOSurface presenter can overwrite a surface still in compositor use

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/presentation
- **Raised:** 2026-08-23T12:09:38Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T121504Z-d89abe60
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00057-run-fix-20260823T121504Z-d89abe60
- **Owner base:** 509645f716f6430d8983c2a627be77f8ef0b5661
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T12:15:04Z
- **Owner until:** 2026-08-23T14:15:04Z
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
