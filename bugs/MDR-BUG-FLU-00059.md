# MDR-BUG-FLU-00059 — A split input burst can strand its tail for 250 ms

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/input-latency
- **Raised:** 2026-08-23T12:28:55Z
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
- **State history:** Open (2026-08-23T12:28:55Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-23T12:33:43Z, deltic:auto role=fix run=fix-20260823T122913Z-6b4401de branch=task/bug-MDR-BUG-FLU-00059-run-fix-20260823T122913Z-6b4401de code=96d3821 gate=manual)

## Observation

The pump drains every queued doorbell before sending one bounded 255-event fast-path batch. If a 256th event was already queued, its doorbell has been consumed and drain_input leaves the event behind; when no server PDU is buffered, the pump can then enter the full 250 ms idle wait. A full batch must force a zero-time readiness check and another pump turn when the socket is not ready, preserving inbound fairness without losing the tail wake.

## Fix

<unfixed — raised only>

## Notes
