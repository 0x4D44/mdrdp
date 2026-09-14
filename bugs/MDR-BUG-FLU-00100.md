# MDR-BUG-FLU-00100 — Damage cadence uses stale snapshot dimensions after output shrinks

- **State:** Fixed
- **Priority:** Must
- **Severity:** Medium
- **Area:** window/presentation-latency
- **Raised:** 2026-08-24T11:32:42Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T074453Z-762d25c9
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00100-run-verify-20260914T074453Z-762d25c9
- **Owner base:** caca7eb21fcedc5b577ada0669ce4fd1c0cc3b18
- **Owner fingerprint:** sha256:07f5e0ae635e7e9ad4051018c227c78ae1a57978eb6f0d87c2648f1a8467c91e
- **Owner since:** 2026-09-14T07:44:53Z
- **Owner until:** 2026-09-14T09:44:53Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T11:32:42Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T11:42:11Z, deltic:auto role=fix run=fix-20260824T113301Z-709a42a8 branch=task/bug-MDR-BUG-FLU-00100-run-fix-20260824T113301Z-709a42a8 code=bb6a329 gate=manual)

## Observation

request_damage_redraw chooses cadence from the previously copied presentation dimensions before copying the current store generation. After a large-to-small output change, the stale large snapshot triggers the 33 ms large-surface delay even though the current visible output is small and eligible immediately. The generation and current presentation dimensions must be read coherently from the store before the cadence decision.

## Fix

<unfixed — raised only>

## Notes
