# MDR-BUG-FLU-00079 — Offscreen and cache-only EGFX work is counted as a presented frame

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/presentation-latency
- **Raised:** 2026-08-23T21:51:27Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T125304Z-3a2d3d07
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00079-run-verify-20260913T125304Z-3a2d3d07
- **Owner base:** ae37e80004df83a09d69a639c1cb30b6197e1d80
- **Owner fingerprint:** sha256:d580b7557d99a11d64755b9dbe78bf4ff9a052ebab836aedda8ab623e8637f1e
- **Owner since:** 2026-09-13T12:53:04Z
- **Owner until:** 2026-09-13T14:53:04Z
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
