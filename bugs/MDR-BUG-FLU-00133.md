# MDR-BUG-FLU-00133 — Dock icons do not return after Dock restarts

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** ui
- **Raised:** 2026-10-27T05:36:27Z
- **Discovery source:** Human
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
- **State history:** Open (2026-10-27T05:36:27Z, raised via `deltic bugs new --land`) -> Fixed (2026-10-27T05:48:35Z, deltic:auto role=fix run=fix-20261027T053636Z-2a515e06 branch=task/bug-MDR-BUG-FLU-00133-run-fix-20261027T053636Z-2a515e06 code=18dc8833c69c944f50d648dcce9f8814ef641fc8 gate=manual)

## Observation

Repro: Open an mdrdp session with its Dock tile visible, then restart the macOS Dock while the session remains running. Expected: the session's mdrdp icon returns to the Dock. Actual: the icon does not reappear.

Evidence fingerprint: `manual:v1:dock-icons-do-not-return-after-dock-restarts`


## Fix

<unfixed — raised only>

## Notes
