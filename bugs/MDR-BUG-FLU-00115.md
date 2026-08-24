# MDR-BUG-FLU-00115 — Native sparse updates can overtake recovery and be discarded

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** native/recovery-ordering
- **Raised:** 2026-08-24T17:56:16Z
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
- **State history:** Open (2026-08-24T17:56:16Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-24T18:07:36Z, deltic:auto role=fix run=fix-20260824T175633Z-30b96e7b branch=task/bug-MDR-BUG-FLU-00115-run-fix-20260824T175633Z-30b96e7b code=c5409c4 gate=manual)

## Observation

When native sparse mode loses its base frame, SparseSink discards subsequent sparse updates while recovery is in flight. If the recovery frame then paints an older sequence, the newer sparse damage is permanently lost until another server-side change happens, leaving the client blank or stale. The sparse reader must wait behind the recovery/base-ready condition and resume the held update after a successful recovery paint.

## Fix

<unfixed — raised only>

## Notes
