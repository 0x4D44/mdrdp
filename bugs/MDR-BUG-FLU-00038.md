# MDR-BUG-FLU-00038 — A partial Rhydra input record blocks every later input client indefinitely

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/input
- **Raised:** 2026-08-22T19:40:40Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T084817Z-f8be8942
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00038-run-verify-20260913T084817Z-f8be8942
- **Owner base:** c1959be401e7b88d3fe45c721948027c6014937e
- **Owner fingerprint:** sha256:a2fc91ae8349cc04026b98fcea1d47f263003d934e145ffa80560a1cfd63a6cc
- **Owner since:** 2026-09-13T08:48:17Z
- **Owner until:** 2026-09-13T10:48:17Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T13:31:35Z, deltic:auto role=fix run=fix-20260823T131759Z-56a2bc45 branch=task/bug-MDR-BUG-FLU-00038-run-fix-20260823T131759Z-56a2bc45 code=34011e7 gate=manual)

## Observation

tools/latency-spike/server/src/win/input.rs:453-505 reads one input record with unbounded read_exact calls, and input.rs:581-587 serves one connection inline. A peer that sends only a kind byte or partial body and remains connected blocks the sole listener from accepting a replacement input connection. Bound partial-record reads and recover the listener.

## Fix

Input record parsing now leaves the socket unlimited while it waits for the next
kind byte, then applies one absolute one-second deadline to the rest of that record.
A silent or byte-dribbling partial record therefore ends only that connection;
`serve_one` still flushes movement telemetry and releases held keys/buttons before
the listener accepts its replacement.

The socket-boundary logic is portable and tested on loopback. Regression tests
cover a silent partial body, a byte dribble which cannot extend the deadline,
healthy idle time between complete records, and the existing unknown-kind close.

## Notes
