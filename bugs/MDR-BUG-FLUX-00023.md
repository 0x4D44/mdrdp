# MDR-BUG-FLUX-00023 — rhydra HEVC keyframes leave newly exposed desktop regions stale when raw rects are disabled

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** native-video
- **Raised:** 2026-08-20T11:26:57Z
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
- **State history:** Open (2026-08-20T11:26:57Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-20T11:52:55Z, deltic:auto role=fix run=fix-20260820T113333Z-p5164-n303507000-c1 branch=task/bug-MDR-BUG-FLUX-00023-run-fix-20260820T113333Z-p5164-n303507000-c1 code=ae58a6f gate=manual)

## Observation

On quench at 1920x1080 and 20 Mbit/s, the AC3 control arm ran the integrated HEVC host with --no-rects. The client decoded 37 frames with zero errors, including two host-confirmed keyframes, but a large white Notepad window region still showed the previous wallpaper; a near-simultaneous host capture was correct and the RGB PSNR was only 36.0 dB against the predeclared 40 dB floor. The pre-HEVC H.264 control painted the same region correctly. Expected: an HEVC keyframe replaces every pixel of the 1920x1080 canvas without help from raw rectangle overlays.

## Fix

<unfixed — raised only>

## Notes
