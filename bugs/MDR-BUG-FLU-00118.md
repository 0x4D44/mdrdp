# MDR-BUG-FLU-00118 — Blocked RDP writes busy-poll at 1 kHz until the outbound deadline

- **State:** Closed
- **Priority:** Should
- **Severity:** Medium
- **Area:** rdp/transport-latency
- **Raised:** 2026-08-24T19:04:27Z
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
- **State history:** Open (2026-08-24T19:04:27Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T19:14:33Z, deltic:auto role=fix run=fix-20260824T190450Z-40f617ac branch=task/bug-MDR-BUG-FLU-00118-run-fix-20260824T190450Z-40f617ac code=4bff7e5 gate=manual) -> Closed (2026-09-14T10:26:51Z, 0x4D44/Codex verify run=verify-20260914T101427Z-55a6b855)

## Observation

When the nonblocking TLS transport returns WouldBlock, write_framed_with_clock sleeps for 1 ms and retries until the five-second absolute deadline. A stalled peer can therefore wake the session thread roughly 5,000 times, wasting CPU and contending with graphics and input precisely under backpressure. Expected: wait for socket writability until the same absolute deadline, then retry only when the OS reports progress or termination.

## Fix

`4bff7e5` makes blocked writes wait for OS writability while preserving one
absolute outbound deadline. The change is integrated in `4bff7e5`.

## Verification

The independent verifier passed
`connect::tests::blocked_writer_waits_for_writability_instead_of_retrying_each_millisecond`
and its 24-test connect-oriented filter.

The independent verifier replaced the readiness wait at `src/connect.rs:448`
with a no-op. The focused test failed at `src/connect.rs:1173` after observing
four writes instead of one. The lead made the same mutation and observed the
same failure. Restoring the wait made the focused test and the lead's
32-test `connect` filter pass again. No live RDP runtime was used.

## Notes
