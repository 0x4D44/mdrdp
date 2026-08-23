# MDR-BUG-FLU-00038 — A partial Rhydra input record blocks every later input client indefinitely

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/input
- **Raised:** 2026-08-22T19:40:40Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T131759Z-56a2bc45
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00038-run-fix-20260823T131759Z-56a2bc45
- **Owner base:** f738eeda03d05f36695e7b5aa404aacf1c62078d
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T13:17:59Z
- **Owner until:** 2026-08-23T15:17:59Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

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
