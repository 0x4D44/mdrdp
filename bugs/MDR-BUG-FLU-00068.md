# MDR-BUG-FLU-00068 — AVC444 luma-only updates repaint stale pixels beyond a cropped decoded frame

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/avc444-rendering
- **Raised:** 2026-08-23T20:34:24Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T113856Z-788a1171
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00068-run-verify-20260913T113856Z-788a1171
- **Owner base:** 8aaecf6e24ecb2e2427e259984ec353c31997cad
- **Owner fingerprint:** sha256:f4eb4bc87b129777549841318652609c1ef0a1a598a7559b261cb6e828f0c485
- **Owner since:** 2026-09-13T11:38:56Z
- **Owner until:** 2026-09-13T13:38:56Z
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
