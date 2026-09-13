# MDR-BUG-FLU-00048 — Rhydra disconnect can leave injected keys or mouse buttons held

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/input
- **Raised:** 2026-08-23T12:09:37Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T095013Z-7e90d5eb
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00048-run-verify-20260913T095013Z-7e90d5eb
- **Owner base:** 9c25d79f9be609d6db7197ff6f0102be696b3da3
- **Owner fingerprint:** sha256:cf2d59b3502fa8ab1ad1119136e14d89c9fea873782f9449a3da6fc32cf1c50a
- **Owner since:** 2026-09-13T09:50:13Z
- **Owner until:** 2026-09-13T11:50:13Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T12:36:42Z, deltic:auto role=fix run=fix-20260823T122412Z-d7777dc8 branch=task/bug-MDR-BUG-FLU-00048-run-fix-20260823T122412Z-d7777dc8 code=f5f5fc9 gate=manual)

## Observation

The Windows input server injects key and mouse-button transitions immediately but keeps no per-connection held-input ledger. EOF and connection errors flush only mouse-move telemetry, then accept the next client without synthesizing releases. A tunnel loss after a down record can therefore leave Windows with a stuck modifier or button; track successfully injected downs and release them on every connection exit.

## Fix

Track successfully injected virtual keys, scancodes, and mouse buttons for each input
connection. Successful ups clear the matching hold; duplicate downs remain one hold. Every
connection exit now flushes pending move telemetry and attempts matching releases before
the serial listener accepts another peer. Failed releases are explicit in the server log.

## Notes

- The release-plan regression was observed red with two failed assertions before the
  implementation, then passed 2/2.
- The full Rhydra suite passed 310 tests and `scripts/check-windows.sh` passed.
