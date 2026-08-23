# MDR-BUG-FLU-00068 — AVC444 luma-only updates repaint stale pixels beyond a cropped decoded frame

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/avc444-rendering
- **Raised:** 2026-08-23T20:34:24Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T204552Z-335ef56a
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00068-run-fix-20260823T204552Z-335ef56a
- **Owner base:** ace519bb838cf3af4f287c67354bf4d2fe0fd2ea
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T20:45:52Z
- **Owner until:** 2026-08-23T22:45:52Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T20:34:24Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

The LC=1 path accepts any internally well-formed decoded YUV frame, even when its dimensions do not cover the advertised update rectangles. apply_luma silently updates only the intersection, but decode_avc444 emits every full wire rectangle from the persistent YUV444 buffer. A cropped or malformed main frame therefore repaints old or black pixels as though they were current, producing partial redraws and outlines. Reject the update or emit only the proven covered region.

## Fix

<unfixed — raised only>

## Notes
