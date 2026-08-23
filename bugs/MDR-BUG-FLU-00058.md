# MDR-BUG-FLU-00058 — 5K cadence gate copies frames that it immediately discards

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/presentation-latency
- **Raised:** 2026-08-23T12:21:07Z
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
- **State history:** Open (2026-08-23T12:21:07Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-23T12:27:52Z, deltic:auto role=fix run=fix-20260823T122126Z-3e6dc0d9 branch=task/bug-MDR-BUG-FLU-00058-run-fix-20260823T122126Z-3e6dc0d9 code=66d74b5 gate=manual)

## Observation

SessionApp::redraw acquires the surface-store lock and copies the full presentation snapshot before it checks the 33 ms large-canvas cadence. A platform redraw inside the cadence window therefore copies roughly 56 MiB at 5120x2880, blocks decoding behind the store lock, and then returns without presenting those bytes. Check the generation and cadence before allocating a platform frame or copying the presentation snapshot, while retaining the post-copy guard needed for races and geometry changes.

## Fix

<unfixed — raised only>

## Notes
