# MDR-BUG-FLU-00106 — RDP input deadline expires while waiting for hinted display traffic

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/input-latency
- **Raised:** 2026-08-24T12:24:24Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T085003Z-b940bd66
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00106-run-verify-20260914T085003Z-b940bd66
- **Owner base:** 28141d7cfa41de4da1bfea39ecbea60f0ded9a9e
- **Owner fingerprint:** sha256:124f571577efcb622e1f3ed26c314da5d1648e558b3e2bd493f55f13cbc01395
- **Owner since:** 2026-09-14T08:50:03Z
- **Owner until:** 2026-09-14T10:50:03Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T12:24:24Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T12:35:03Z, deltic:auto role=fix run=fix-20260824T122653Z-f395f582 branch=task/bug-MDR-BUG-FLU-00106-run-fix-20260824T122653Z-f395f582 code=99ec3be gate=manual)

## Observation

After one input arms session.rs input_turn_deadline, the session may spend most or all of that five-second budget waiting for a hinted server PDU. A later input then inherits the stale absolute deadline and can fail immediately even though the socket is writable. Reset the outbound input-batch deadline after inbound waits, and prove two inputs separated by a delayed hinted PDU both write successfully.

## Fix

<unfixed — raised only>

## Notes
