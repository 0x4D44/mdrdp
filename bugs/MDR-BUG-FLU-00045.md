# MDR-BUG-FLU-00045 — Rhydra send_done telemetry records failed video delivery as success

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** rhydra/telemetry
- **Raised:** 2026-08-22T19:40:41Z
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
- **State history:** Open (2026-08-22T19:40:41Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-25T06:09:45Z, deltic:auto role=fix run=fix-20260825T055844Z-0953b581 branch=task/bug-MDR-BUG-FLU-00045-run-fix-20260825T055844Z-0953b581 code=eb8e10c3e0e32c03eea73613ac90f637868ffd51 gate=manual)

## Observation

tools/latency-spike/server/src/win/send.rs:159-177 swallows socket write or flush errors after dropping the client, and send.rs:218-232 stamps send_done_us unconditionally. Stats therefore report wire completion and latency for access units that never reached a client. Propagate delivery outcome into the telemetry record.

## Fix

<unfixed — raised only>

## Notes
