# MDR-BUG-FLU-00106 — RDP input deadline expires while waiting for hinted display traffic

- **State:** Open
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
- **State history:** Open (2026-08-24T12:24:24Z, raised via `deltic bugs new`)

## Observation

After one input arms session.rs input_turn_deadline, the session may spend most or all of that five-second budget waiting for a hinted server PDU. A later input then inherits the stale absolute deadline and can fail immediately even though the socket is writable. Reset the outbound input-batch deadline after inbound waits, and prove two inputs separated by a delayed hinted PDU both write successfully.

## Fix

<unfixed — raised only>

## Notes
