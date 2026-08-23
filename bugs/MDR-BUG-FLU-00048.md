# MDR-BUG-FLU-00048 — Rhydra disconnect can leave injected keys or mouse buttons held

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/input
- **Raised:** 2026-08-23T12:09:37Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T122412Z-d7777dc8
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00048-run-fix-20260823T122412Z-d7777dc8
- **Owner base:** 072639a61dd41df369facfbea57e90f332b9095b
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T12:24:12Z
- **Owner until:** 2026-08-23T14:24:12Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

The Windows input server injects key and mouse-button transitions immediately but keeps no per-connection held-input ledger. EOF and connection errors flush only mouse-move telemetry, then accept the next client without synthesizing releases. A tunnel loss after a down record can therefore leave Windows with a stuck modifier or button; track successfully injected downs and release them on every connection exit.

## Fix

<unfixed — raised only>

## Notes
