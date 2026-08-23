# MDR-BUG-FLU-00048 — Rhydra disconnect can leave injected keys or mouse buttons held

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/input
- **Raised:** 2026-08-23T12:09:37Z
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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

The Windows input server injects key and mouse-button transitions immediately but keeps no per-connection held-input ledger. EOF and connection errors flush only mouse-move telemetry, then accept the next client without synthesizing releases. A tunnel loss after a down record can therefore leave Windows with a stuck modifier or button; track successfully injected downs and release them on every connection exit.

## Fix

<unfixed — raised only>

## Notes
