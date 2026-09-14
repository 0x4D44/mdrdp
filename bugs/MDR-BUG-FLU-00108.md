# MDR-BUG-FLU-00108 — Native reliable input can starve resize and visibility commands

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** native/input-latency
- **Raised:** 2026-08-24T12:24:25Z
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
- **State history:** Open (2026-08-24T12:24:25Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T12:45:29Z, deltic:auto role=fix run=fix-20260824T123646Z-570c4a3a branch=task/bug-MDR-BUG-FLU-00108-run-fix-20260824T123646Z-570c4a3a code=303bb5a gate=manual) -> Closed (2026-09-14T09:15:01Z, 0x4D44/Codex verify run=verify-20260914T090513Z-67f921de)

## Observation

The native input loop drains the reliable event queue until empty before servicing resize and visibility commands. A continuously fed reliable or scripted stream can therefore starve commands and focus-loss releases indefinitely. Bound each reliable-input batch and interleave command service, with a regression that keeps enqueueing input while requiring bounded command delivery.

## Fix

`91c98dd` limits each native reliable-input turn to
`NATIVE_INPUT_BATCH_MAX` events and services resize and visibility commands
between batches. The change is integrated in `303bb5a`.

## Verification

The lead and independent verifiers ran
`native::session::tests::native_reliable_input_batch_yields_to_commands`; each
selected and passed one test. The native-session family passed 64 tests for
both verifiers.

The independent verifier changed the batch bound at
`src/native/session.rs:1497` from `NATIVE_INPUT_BATCH_MAX` to
`NATIVE_INPUT_BATCH_MAX + 1`. The focused test failed at
`src/native/session.rs:5856` because the batch became `Sent { full: false }`
instead of `Sent { full: true }`. Restoring the bound made the focused and
64-test family runs pass again. No live RDP runtime was used.

## Notes
