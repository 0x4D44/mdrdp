# MDR-BUG-FLU-00079 — Offscreen and cache-only EGFX work is counted as a presented frame

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/presentation-latency
- **Raised:** 2026-08-23T21:51:27Z
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
- **State history:** Open (2026-08-23T21:51:27Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T22:10:53Z, deltic:auto role=fix run=fix-20260823T215208Z-d4d3caae branch=task/bug-MDR-BUG-FLU-00079-run-fix-20260823T215208Z-d4d3caae code=f405b3a gate=manual)

## Observation

SurfaceStore uses generation as its visible presentation-damage token, but create/delete and mutation of offscreen surfaces plus cache-only operations also advance it. Session::notify_if_painted then wakes the window, increments frame and paint statistics, copies unchanged output, and can close input-to-paint latency against unrelated work. Restrict presentation generation to visible output changes and update cache diagnostics through a separate non-presentation path.

## Fix

<unfixed — raised only>

## Notes
