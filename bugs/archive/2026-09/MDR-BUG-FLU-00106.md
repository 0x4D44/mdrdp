# MDR-BUG-FLU-00106 — RDP input deadline expires while waiting for hinted display traffic

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/input-latency
- **Raised:** 2026-08-24T12:24:24Z
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
- **State history:** Open (2026-08-24T12:24:24Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T12:35:03Z, deltic:auto role=fix run=fix-20260824T122653Z-f395f582 branch=task/bug-MDR-BUG-FLU-00106-run-fix-20260824T122653Z-f395f582 code=99ec3be gate=manual) -> Closed (2026-09-14T09:01:13Z, 0x4D44/Codex verify run=verify-20260914T085003Z-b940bd66)

## Observation

After one input arms session.rs input_turn_deadline, the session may spend most or all of that five-second budget waiting for a hinted server PDU. A later input then inherits the stale absolute deadline and can fail immediately even though the socket is writable. Reset the outbound input-batch deadline after inbound waits, and prove two inputs separated by a delayed hinted PDU both write successfully.

## Fix

`5da40d4` resets the reactivation input deadline after a real hinted-display
wait, so the next input batch receives a fresh absolute write budget. The
change is integrated in `99ec3be`.

## Verification

The lead and independent verifiers ran
`session::tests::delayed_hinted_wait_starts_a_fresh_input_write_budget`; each
selected and passed one test. The session test family passed 96 tests for both
baseline runs.

The independent verifier disabled the reset at `src/session.rs:1420`. The
focused test then failed because the second input inherited the expired
deadline. The lead made the same behavioral mutation; its focused test failed
at `src/session.rs:2042` with “input after a delayed hinted wait must get a fresh
write budget.” Restoring the helper made the independent focused test pass
again. A concurrent native-session mutation contaminated that verifier's
restored family run; the final six repository gates rerun the clean restored
tree. No live RDP session was run.

## Notes
