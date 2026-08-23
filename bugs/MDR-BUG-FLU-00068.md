# MDR-BUG-FLU-00068 — AVC444 luma-only updates repaint stale pixels beyond a cropped decoded frame

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/avc444-rendering
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
- **State history:** Open (2026-08-23T20:34:24Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T20:52:59Z, deltic:auto role=fix run=fix-20260823T204552Z-335ef56a branch=task/bug-MDR-BUG-FLU-00068-run-fix-20260823T204552Z-335ef56a code=0c69a57 gate=manual)

## Observation

The LC=1 path accepts any internally well-formed decoded YUV frame, even when its dimensions do not cover the advertised update rectangles. apply_luma silently updates only the intersection, but decode_avc444 emits every full wire rectangle from the persistent YUV444 buffer. A cropped or malformed main frame therefore repaints old or black pixels as though they were current, producing partial redraws and outlines. Reject the update or emit only the proven covered region.

## Fix

<unfixed — raised only>

## Notes
