# MDR-BUG-FLU-00129 — Windowed RDP resolution stays reduced after a monitor returns

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** window/resolution
- **Raised:** 2026-09-14T06:44:19Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T105020Z-1c8b83f8
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00129-run-verify-20260914T105020Z-1c8b83f8
- **Owner base:** 1ea5182316fdddfcaa1abc6c13ada04ce32afc1a
- **Owner fingerprint:** sha256:94a5bc6f3327d7469fdf65546f8bbe50b141ac899e9b331b4b96acb8210640f0
- **Owner since:** 2026-09-14T10:50:20Z
- **Owner until:** 2026-09-14T12:50:20Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-09-14T06:44:19Z, raised via `deltic bugs new`) -> Fixed (2026-09-14T07:10:25Z, deltic:auto role=fix run=fix-20260914T064508Z-4727eab6 branch=task/bug-MDR-BUG-FLU-00129-run-fix-20260914T064508Z-4727eab6 code=b9603adc5684d47ed3a2a1bce1b751bcd753b623 gate=manual)

## Observation

With Dynamic resolution enabled, switching an external monitor off can cause an RDP
session that was using a large resolution to renegotiate to the laptop's smaller
resolution. When the monitor is turned back on, the RDP session may be on another
macOS desktop/Space and does not renegotiate back to the larger resolution. Expected:
a transient monitor power change must not permanently replace the chosen session or
window resolution; when the original display/window geometry returns, mdrdp should
renegotiate accordingly. Actual: the session can remain at the smaller resolution
after the monitor returns.

## Fix

<unfixed — raised only>

## Notes
