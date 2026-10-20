# MDR-BUG-FLU-00122 — RDP keystrokes can stall until the Kiln session reconnects

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/input
- **Raised:** 2026-08-25T10:06:15Z
- **Discovery source:** Human
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
- **State history:** Open (2026-08-25T10:06:15Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-25T23:00:44Z, deltic:auto role=fix run=fix-20260825T221643Z-587da9f0 branch=task/bug-MDR-BUG-FLU-00122-run-fix-20260825T221643Z-587da9f0 code=2dc13af4107778571be01676b027596229b62bba gate=manual) -> Closed (2026-09-13T06:02:59Z, 0x4D44/Codex verify run=verify-20260913T055445Z-3e34e297)

## Observation

On Kiln with mdrdp v0.1.234, an established RDP session stopped accepting keystrokes or delayed them beyond five seconds. Arthur abandoned the wait and reconnected; keystrokes worked immediately in the replacement session. Expected: reliable key transitions reach the server promptly for the lifetime of a connected session. Actual: session-local input delivery can remain unusable until reconnect. The failed session ended gracefully with AVC444v2 active and no decode or surface errors, so the report does not establish whether the stall is before wire delivery, in the server input path, or only in paint feedback; diagnose those stages before changing the protocol.

## Fix

Commit `2dc13af4107778571be01676b027596229b62bba` polls read and write readiness and buffers inbound TLS plaintext before retrying a blocked outbound write. The focused regressions `connect::tests::blocked_write_drains_inbound_tls_to_break_full_duplex_deadlock`, `connect::tests::simultaneous_read_and_write_readiness_retries_the_write`, and `connect::tests::blocked_write_drains_more_than_one_inbound_tls_record` passed; the blocked-write filter passed 2 tests and the simultaneous-readiness test passed 1. As a behavioral red check, forcing `readiness.readable` to false made the deadlock regression fail at its own expectation with `TimedOut: RDP outbound write deadline expired`; the source was restored and its diff is empty. `cargo fmt --all -- --check`, `./scripts/check-windows.sh --locked`, and `./scripts/test-vendored.sh` also passed. The original Kiln input stall was reviewed, but no fresh live RDP session was available for this verification pass.

## Notes
