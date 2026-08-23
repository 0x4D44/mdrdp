# MDR-BUG-FLU-00064 — Rhydra raw rects can paint against a dropped video baseline during recovery

- **State:** Open
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
- **State history:** Open (2026-08-23T19:42:47Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

When a complete encoded frame is dropped by the bounded outbound queue, logical-frame recovery suppresses later inter-frame video until an all-tile keyframe arrives, but pipeline.rs still emits raw metadata/diff rects before consulting that recovery state. PixelDiff also advances its retained baseline before outbound admission, so the next raw rect can describe a delta from pixels the viewer never received and visibly corrupt the desktop. Gate rect emission and baseline trust on the same recovery invariant, while preserving the raw fast path after a confirmed recovery frame.

## Fix

<unfixed — raised only>

## Notes
