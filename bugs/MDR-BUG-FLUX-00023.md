# MDR-BUG-FLUX-00023 — rhydra HEVC keyframes leave newly exposed desktop regions stale when raw rects are disabled

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** native-video
- **Raised:** 2026-08-20T11:26:57Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260820T113333Z-p5164-n303507000-c1
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLUX-00023-run-fix-20260820T113333Z-p5164-n303507000-c1
- **Owner base:** ac7d2d67c9474cfe6b226ec0908cbbd10374da01
- **Owner fingerprint:** -
- **Owner since:** 2026-08-20T11:33:33Z
- **Owner until:** 2026-08-20T13:33:33Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-20T11:26:57Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

On quench at 1920x1080 and 20 Mbit/s, the AC3 control arm ran the integrated HEVC host with --no-rects. The client decoded 37 frames with zero errors, including two host-confirmed keyframes, but a large white Notepad window region still showed the previous wallpaper; a near-simultaneous host capture was correct and the RGB PSNR was only 36.0 dB against the predeclared 40 dB floor. The pre-HEVC H.264 control painted the same region correctly. Expected: an HEVC keyframe replaces every pixel of the 1920x1080 canvas without help from raw rectangle overlays.

## Fix

<unfixed — raised only>

## Notes
