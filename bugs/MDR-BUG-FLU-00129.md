# MDR-BUG-FLU-00129 — Windowed RDP resolution stays reduced after a monitor returns

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** window/resolution
- **Raised:** 2026-09-14T06:44:19Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260914T064508Z-4727eab6
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00129-run-fix-20260914T064508Z-4727eab6
- **Owner base:** 1db27a7597ae4787483a6a3435c9c880102cc718
- **Owner fingerprint:** -
- **Owner since:** 2026-09-14T06:45:08Z
- **Owner until:** 2026-09-14T08:45:08Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-09-14T06:44:19Z, raised via `deltic bugs new`)

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
