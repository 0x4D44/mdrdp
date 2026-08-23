# MDR-BUG-FLU-00071 — EGFX updates can be presented before EndFrame completes the logical frame

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/rendering-atomicity
- **Raised:** 2026-08-23T20:34:24Z
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
- **State history:** Open (2026-08-23T20:34:24Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T22:55:51Z, deltic:auto role=fix run=fix-20260823T214405Z-857a448c branch=task/bug-MDR-BUG-FLU-00071-run-fix-20260823T214405Z-857a448c code=a25b787 gate=manual)

## Observation

The RDP session treats any SurfaceStore generation change as paint and can wake the window after processing a DVC payload that contains StartFrame plus only part of the updates. Nothing gates presentation on EndFrame; the callback currently updates statistics only. Legal channel segmentation can therefore expose a mixed old/new desktop. Publish or wake presentation only at the EndFrame commit boundary while preserving unframed-control behavior.

## Fix

<unfixed — raised only>

## Notes
