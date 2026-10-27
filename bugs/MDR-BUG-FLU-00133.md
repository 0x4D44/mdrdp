# MDR-BUG-FLU-00133 — Dock icons do not return after Dock restarts

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** ui
- **Raised:** 2026-10-27T05:36:27Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20261027T053636Z-2a515e06
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00133-run-fix-20261027T053636Z-2a515e06
- **Owner base:** 9db1c1f2d19a8b2f356f99d8b53c35479120d26f
- **Owner fingerprint:** -
- **Owner since:** 2026-10-27T05:36:36Z
- **Owner until:** 2026-10-27T09:36:36Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-10-27T05:36:27Z, raised via `deltic bugs new --land`)

## Observation

Repro: Open an mdrdp session with its Dock tile visible, then restart the macOS Dock while the session remains running. Expected: the session's mdrdp icon returns to the Dock. Actual: the icon does not reappear.

Evidence fingerprint: `manual:v1:dock-icons-do-not-return-after-dock-restarts`


## Fix

<unfixed — raised only>

## Notes
