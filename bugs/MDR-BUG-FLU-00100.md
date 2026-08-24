# MDR-BUG-FLU-00100 — Damage cadence uses stale snapshot dimensions after output shrinks

- **State:** Open
- **Priority:** Must
- **Severity:** Medium
- **Area:** window/presentation-latency
- **Raised:** 2026-08-24T11:32:42Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T113301Z-709a42a8
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00100-run-fix-20260824T113301Z-709a42a8
- **Owner base:** d3df99f230ab6fe9e12e377b936ed24c3b9afc71
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T11:33:01Z
- **Owner until:** 2026-08-24T13:33:01Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T11:32:42Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

request_damage_redraw chooses cadence from the previously copied presentation dimensions before copying the current store generation. After a large-to-small output change, the stale large snapshot triggers the 33 ms large-surface delay even though the current visible output is small and eligible immediately. The generation and current presentation dimensions must be read coherently from the store before the cadence decision.

## Fix

<unfixed — raised only>

## Notes
