# MDR-BUG-FLU-00115 — Native sparse updates can overtake recovery and be discarded

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** native/recovery-ordering
- **Raised:** 2026-08-24T17:56:16Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T093731Z-2ca5e05e
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00115-run-verify-20260914T093731Z-2ca5e05e
- **Owner base:** ae056ad7c4bf7d2c6a8968f9779f2cd0186f0bf0
- **Owner fingerprint:** sha256:6e02d254c6144c574b0760a4ed6a4ae8bb404c3bf450f5f5406cd8596073ffc3
- **Owner since:** 2026-09-14T09:37:31Z
- **Owner until:** 2026-09-14T11:37:31Z
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
