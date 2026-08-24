# MDR-BUG-FLU-00115 — Native sparse updates can overtake recovery and be discarded

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** native/recovery-ordering
- **Raised:** 2026-08-24T17:56:16Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T175633Z-30b96e7b
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00115-run-fix-20260824T175633Z-30b96e7b
- **Owner base:** 0ad13e5494b99e200518b13f55e5f8c328db80ed
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T17:56:33Z
- **Owner until:** 2026-08-24T19:56:33Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T17:56:16Z, raised via `deltic bugs new` model=gpt-5.6-sol@max)

## Observation

When native sparse mode loses its base frame, SparseSink discards subsequent sparse updates while recovery is in flight. If the recovery frame then paints an older sequence, the newer sparse damage is permanently lost until another server-side change happens, leaving the client blank or stale. The sparse reader must wait behind the recovery/base-ready condition and resume the held update after a successful recovery paint.

## Fix

<unfixed — raised only>

## Notes
