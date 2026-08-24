# MDR-BUG-FLU-00101 — Presentation latency includes the entire window occlusion interval

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** metrics/presentation-latency
- **Raised:** 2026-08-24T11:33:45Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T114233Z-66ad510a
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00101-run-fix-20260824T114233Z-66ad510a
- **Owner base:** 5f5ee6b567be56e4fa46fe3f1191e35929cfcec7
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T11:42:33Z
- **Owner until:** 2026-08-24T13:42:33Z
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
