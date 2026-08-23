# MDR-BUG-FLU-00051 — Rhydra 5K queue can drop one tile from an otherwise complete frame

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/server-latency
- **Raised:** 2026-08-23T12:09:37Z
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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

The server outbound queue holds two messages, while one 5K capture can enqueue a rect update plus two H.264 tile access units. Under sender lag, rect plus tile 0 fill the queue and tile 1 is dropped; the native client cannot complete that logical frame and waits for recovery. Make backpressure and dropping operate on complete logical frames, or otherwise prove both tiles remain coherent without adding stale backlog.

## Fix

<unfixed — raised only>

## Notes
