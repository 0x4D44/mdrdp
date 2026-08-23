# MDR-BUG-FLU-00059 — A split input burst can strand its tail for 250 ms

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/input-latency
- **Raised:** 2026-08-23T12:28:55Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T122913Z-6b4401de
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00059-run-fix-20260823T122913Z-6b4401de
- **Owner base:** 86199391392ed3b3c21439eb466983ee06971f7f
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T12:29:13Z
- **Owner until:** 2026-08-23T14:29:13Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T12:28:55Z, raised via `deltic bugs new` model=gpt-5.6-sol@max)

## Observation

The pump drains every queued doorbell before sending one bounded 255-event fast-path batch. If a 256th event was already queued, its doorbell has been consumed and drain_input leaves the event behind; when no server PDU is buffered, the pump can then enter the full 250 ms idle wait. A full batch must force a zero-time readiness check and another pump turn when the socket is not ready, preserving inbound fairness without losing the tail wake.

## Fix

<unfixed — raised only>

## Notes
