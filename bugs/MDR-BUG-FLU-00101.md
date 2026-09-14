# MDR-BUG-FLU-00101 — Presentation latency includes the entire window occlusion interval

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** metrics/presentation-latency
- **Raised:** 2026-08-24T11:33:45Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T074512Z-e679caa5
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00101-run-verify-20260914T074512Z-e679caa5
- **Owner base:** 147c3ef99ed405243e7fd58aec37aa559dbbaacc
- **Owner fingerprint:** sha256:ec8834eb84740aa432fa7d31ea103ad582a07d1984b3c6004553ca9ac3777153
- **Owner since:** 2026-09-14T07:45:12Z
- **Owner until:** 2026-09-14T09:45:12Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T11:33:45Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T11:54:28Z, deltic:auto role=fix run=fix-20260824T114233Z-66ad510a branch=task/bug-MDR-BUG-FLU-00101-run-fix-20260824T114233Z-66ad510a code=a56efa7 gate=manual)

## Observation

SessionStats retains a paint-to-present sample while the window is occluded. A late hidden paint can remain pending until reveal, so the first redraw records seconds or minutes of hidden time as renderer presentation latency. Occlusion transitions must cancel pending presentation samples while allowing later visible paint-to-present measurements to resume.

## Fix

<unfixed — raised only>

## Notes
