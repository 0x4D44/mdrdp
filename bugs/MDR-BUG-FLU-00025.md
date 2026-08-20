# MDR-BUG-FLU-00025 — 5K IDD publishes no frames because every full-surface GPU copy exceeds the fixed 2 ms proof deadline

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** idd/shared-pool
- **Raised:** 2026-08-20T18:41:47Z
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
- **State history:** Open (2026-08-20T18:41:47Z, raised via `deltic bugs new`)

## Observation

Arthur connected to quench at 5120x2880 with mdrdp v0.1.109 and saw only tartan, even after waiting and forcing desktop activity. The live agent reported the requested 5120x2880 mode and a healthy generation-3 shared pool, while the session remained at 0 frames and 0 bytes. A 100-copy D3D11 measurement on quench showed every 5120x2880 BGRA CopyResource taking more than the driver's fixed 2 ms proof deadline (min 2.001 ms, median 2.095 ms, p95 6.815 ms, max 6.848 ms), so SharedFramePool invalidates every destination slot instead of publishing it. Expected: a healthy 5K pool publishes frames to the connected native viewer.

## Fix

<unfixed — raised only>

## Notes
