# MDR-BUG-FLU-00051 — Rhydra 5K queue can drop one tile from an otherwise complete frame

- **State:** Fixed
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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T13:17:06Z, deltic:auto role=fix run=fix-20260823T125338Z-38f85501 branch=task/bug-MDR-BUG-FLU-00051-run-fix-20260823T125338Z-38f85501 code=1dec4bb gate=manual)

## Observation

The server outbound queue holds two messages, while one 5K capture can enqueue a rect update plus two H.264 tile access units. Under sender lag, rect plus tile 0 fill the queue and tile 1 is dropped; the native client cannot complete that logical frame and waits for recovery. Make backpressure and dropping operate on complete logical frames, or otherwise prove both tiles remain coherent without adding stale backlog.

## Fix

The server now assembles every advertised tile for a capture sequence before
admitting one `FrameSet` to the bounded sender queue. Pending assemblies are capped
at three logical frames; expiry and late-tile retirement are explicit. A dropped
set enters a recovery fence which suppresses complete delta sets until every tile
in one sequence is a keyframe, and mixed keyframe responses request alignment again.

Dirty rectangles retain their independent immediate path. This is safe because the
client advances pixel exactness from a complete rect update while continuing to
decode suppressed tile AUs for reference state; bundling rects behind both encoders
would add avoidable interaction latency. Telemetry schema 10 now counts each logical
frame withheld by assembly, recovery, or queue admission once.

Portable tests cover two-tile and one-tile completion, out-of-order callbacks,
duplicates, bounded expiry, reconnect retirement, and recovery after queue pressure.
The full Rhydra test suite and the Windows cross-target check pass.

## Notes
