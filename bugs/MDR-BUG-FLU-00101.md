# MDR-BUG-FLU-00101 — Presentation latency includes the entire window occlusion interval

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** metrics/presentation-latency
- **Raised:** 2026-08-24T11:33:45Z
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
- **State history:** Open (2026-08-24T11:33:45Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

SessionStats retains a paint-to-present sample while the window is occluded. A late hidden paint can remain pending until reveal, so the first redraw records seconds or minutes of hidden time as renderer presentation latency. Occlusion transitions must cancel pending presentation samples while allowing later visible paint-to-present measurements to resume.

## Fix

<unfixed — raised only>

## Notes
