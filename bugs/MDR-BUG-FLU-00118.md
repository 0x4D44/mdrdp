# MDR-BUG-FLU-00118 — Blocked RDP writes busy-poll at 1 kHz until the outbound deadline

- **State:** Fixed
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
- **State history:** Open (2026-08-24T19:04:27Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T19:14:33Z, deltic:auto role=fix run=fix-20260824T190450Z-40f617ac branch=task/bug-MDR-BUG-FLU-00118-run-fix-20260824T190450Z-40f617ac code=4bff7e5 gate=manual)

## Observation

When the nonblocking TLS transport returns WouldBlock, write_framed_with_clock sleeps for 1 ms and retries until the five-second absolute deadline. A stalled peer can therefore wake the session thread roughly 5,000 times, wasting CPU and contending with graphics and input precisely under backpressure. Expected: wait for socket writability until the same absolute deadline, then retry only when the OS reports progress or termination.

## Fix

<unfixed — raised only>

## Notes
