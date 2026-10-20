# MDR-BUG-FLU-00115 — Native sparse updates can overtake recovery and be discarded

- **State:** Closed
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
- **State history:** Open (2026-08-24T17:56:16Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-24T18:07:36Z, deltic:auto role=fix run=fix-20260824T175633Z-30b96e7b branch=task/bug-MDR-BUG-FLU-00115-run-fix-20260824T175633Z-30b96e7b code=c5409c4 gate=manual) -> Closed (2026-09-14T09:45:24Z, 0x4D44/Codex verify run=verify-20260914T093731Z-2ca5e05e)

## Observation

When native sparse mode loses its base frame, SparseSink discards subsequent sparse updates while recovery is in flight. If the recovery frame then paints an older sequence, the newer sparse damage is permanently lost until another server-side change happens, leaving the client blank or stale. The sparse reader must wait behind the recovery/base-ready condition and resume the held update after a successful recovery paint.

## Fix

`c5409c4` adds the `BaseReady` condition and makes the sparse reader wait
behind recovery, with cancellation and a bounded timeout. Later recovery edge
case handling remains in the current implementation.

## Verification

The independent verifier and lead each ran
`native::session::tests::sparse_update_waits_for_recovery_and_base_wait_is_bounded_and_cancellable`;
each focused run passed one test. The native-session family passed 64 tests for
both verifiers, including the restored lead run.

The independent verifier bypassed recovery waiting at
`src/native/session.rs:2445`; the focused test failed because the sparse update
did not enter the recovery wait. The lead made the same mutation and observed
the assertion fail at `src/native/session.rs:5168`. Restoring the wait made the
focused test and 64-test native-session family pass again. No live RDP runtime
was used.

## Notes
