# MDR-BUG-FLU-00074 — RDP mouse moves use an unbounded stale FIFO ahead of keys and buttons

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** rdp/input-latency
- **Raised:** 2026-08-23T20:34:25Z
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
- **State history:** Open (2026-08-23T20:34:25Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T21:15:15Z, deltic:auto role=fix run=fix-20260823T204628Z-83e09493 branch=task/bug-MDR-BUG-FLU-00074-run-fix-20260823T204628Z-83e09493 code=c1f90b345fe2def611ce48e834d9542621c3c74f gate=manual)

## Observation

Only native sessions use LatestMouseMove. RDP sessions enqueue every pointer move into an unbounded mpsc FIFO, while the session sends at most 255 events per turn. During decode, clipboard work, or slow transport, thousands of obsolete moves can accumulate ahead of a later key or button, adding visible lag and unbounded memory growth. Coalesce lossy motion without reordering reliable input.

## Fix

<unfixed — raised only>

## Notes
