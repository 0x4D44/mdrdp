# MDR-BUG-FLU-00055 — Native input latency telemetry starts after the whole queued burst

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** rhydra/telemetry
- **Raised:** 2026-08-23T12:09:38Z
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
- **State history:** Open (2026-08-23T12:09:38Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T22:22:10Z, deltic:auto role=fix run=fix-20260823T221118Z-3786a30b branch=task/bug-MDR-BUG-FLU-00055-run-fix-20260823T221118Z-3786a30b code=9d88631 gate=manual)

## Observation

The native input pump drains and writes the available record burst, then stamps InputClock once. The metric claims to measure the first unanswered input but starts after the final write, understating the first event by the rest of the burst. Stamp when the first record is committed to the socket and preserve one outstanding causal sample until paint.

## Fix

<unfixed — raised only>

## Notes
