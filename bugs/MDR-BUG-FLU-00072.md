# MDR-BUG-FLU-00072 — A replacement surface discards the last-good fallback after its first partial paint

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/surface-handoff
- **Raised:** 2026-08-23T20:34:25Z
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
- **State history:** Open (2026-08-23T20:34:25Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T21:43:53Z, deltic:auto role=fix run=fix-20260823T212026Z-10440d77 branch=task/bug-MDR-BUG-FLU-00072-run-fix-20260823T212026Z-10440d77 code=b8e10007a29ceedbbf75136b882d913ce6e05c4d gate=manual)

## Observation

SurfaceStore retains the old painted output while a mapped replacement is unpainted, but finish_surface_mutation clears that fallback after the replacement's first pixel mutation. If the first update covers only part of the new surface, the rest is still zero-filled and immediately replaces the complete old desktop. Keep the fallback until a complete replacement frame is committed rather than treating one changed region as a complete surface.

## Fix

<unfixed — raised only>

## Notes
