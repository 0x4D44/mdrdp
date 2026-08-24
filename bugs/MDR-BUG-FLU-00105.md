# MDR-BUG-FLU-00105 — Mapping an incomplete surface wakes a suppressed presentation

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** display/frame-atomicity
- **Raised:** 2026-08-24T12:03:48Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T120451Z-2f52ab07
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00105-run-fix-20260824T120451Z-2f52ab07
- **Owner base:** 49c95e9b68e95bb239d56d537d818568ccab01cf
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T12:04:51Z
- **Owner until:** 2026-08-24T14:04:51Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T12:03:48Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

SurfaceStore::map_to_output_geometry advances generation for a painted but incomplete surface even while presentation_suppressed is true. copy_presentation_state still returns Retained, so the window wakes and re-presents an old snapshot before coverage completes, and the new mapping dimensions can affect cadence. Keep generation unchanged until the replacement becomes complete.

## Fix

<unfixed — raised only>

## Notes
