# MDR-BUG-FLU-00064 — Rhydra raw rects can paint against a dropped video baseline during recovery

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/rendering
- **Raised:** 2026-08-23T19:42:47Z
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
- **State history:** Open (2026-08-23T19:42:47Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T19:46:30Z, deltic:auto role=fix run=fix-20260823T194320Z-e1a72827 branch=task/bug-MDR-BUG-FLU-00064-run-fix-20260823T194320Z-e1a72827 code=76ebf9f gate=manual)

## Observation

When a complete encoded frame is dropped by the bounded outbound queue, logical-frame recovery suppresses later inter-frame video until an all-tile keyframe arrives, but pipeline.rs still emits raw metadata/diff rects before consulting that recovery state. PixelDiff also advances its retained baseline before outbound admission, so the next raw rect can describe a delta from pixels the viewer never received and visibly corrupt the desktop. Gate rect emission and baseline trust on the same recovery invariant, while preserving the raw fast path after a confirmed recovery frame.

## Fix

`logical_frame::Recovery` now exposes whether the viewer has an admitted all-tile
keyframe baseline. The capture loop snapshots that state once per captured frame and
allows neither metadata rects nor measured pixel-diff rects while recovery is waiting.
After a complete recovery keyframe enters the bounded outbound queue, both overlay
paths resume against the retained texture from that same admitted desktop frame.

The regression test proves overlays start disabled, become legal only after an
all-keyframe admission, and are disabled again by a recovery reset. The existing
recovery test also covers incomplete frames, mixed-tile keyframes, queue rejection,
and pre-encode shedding.

## Notes
