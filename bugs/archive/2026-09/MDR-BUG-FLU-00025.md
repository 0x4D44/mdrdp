# MDR-BUG-FLU-00025 — 5K IDD publishes no frames because every full-surface GPU copy exceeds the fixed 2 ms proof deadline

- **State:** Closed
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
- **State history:** Open (2026-08-20T18:41:47Z, raised via `deltic bugs new`) -> Fixed (2026-08-20T18:55:53Z, deltic:auto role=fix run=fix-20260820T184219Z-56b84acc branch=task/bug-MDR-BUG-FLU-00025-run-fix-20260820T184219Z-56b84acc code=8bd484e gate=manual) -> Closed (2026-09-13T05:26:31Z, independent verifier: dynamic 5K deadline oracle passed and the fixed-floor mutant failed its named assertion; model=codex@max)

## Observation

Arthur connected to quench at 5120x2880 with mdrdp v0.1.109 and saw only tartan, even after waiting and forcing desktop activity. The live agent reported the requested 5120x2880 mode and a healthy generation-3 shared pool, while the session remained at 0 frames and 0 bytes. A 100-copy D3D11 measurement on quench showed every 5120x2880 BGRA CopyResource taking more than the driver's fixed 2 ms proof deadline (min 2.001 ms, median 2.095 ms, p95 6.815 ms, max 6.848 ms), so SharedFramePool invalidates every destination slot instead of publishing it. Expected: a healthy 5K pool publishes frames to the connected native viewer.

## Fix

Commit `8bd484e021004848e391e85bcd9a7fba02aabe0d` derives the IDD copy-wait limit from
surface pixels, clamps it between 2 ms and 16 ms, and starts the wait budget before GPU
submission. A named runtime oracle extracted from the current `CopyWaitLimitUs` helper
passed the 0, 1080p, 1440p, 5K, and cap cases. Reversing the root `MinimumUs` value from
2000 to 1000 made the same oracle fail `CopyWaitLimitUs(0, 0) == 2000` with actual 1000.

The post-fix server suite passed 409/409. The original 5120x2880 zero-frame observation
and its measured D3D11 copy tail were reviewed against the current source; this macOS
pass did not run the Windows IDD hardware or a new live Quench session.

## Notes
