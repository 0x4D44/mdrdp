# MDR-BUG-FLU-00065 — Default 20 Mbit/s budget makes the 5K native desktop visibly pixelated

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/rendering-quality
- **Raised:** 2026-08-23T20:27:23Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T053204Z-da05f7d1
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00065-run-verify-20260913T053204Z-da05f7d1
- **Owner base:** 96a2a629f480bd6ccb1580177563f214bd4271c8
- **Owner fingerprint:** sha256:02f6bcba9188076fb485a4fa4c47c8ff95bb3edc302f41e6c4b906e8e65d5fdc
- **Owner since:** 2026-09-13T05:32:04Z
- **Owner until:** 2026-09-13T07:32:04Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T20:27:23Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T20:32:26Z, deltic:auto role=fix run=fix-20260823T202741Z-f2e5c4d1 branch=task/bug-MDR-BUG-FLU-00065-run-fix-20260823T202741Z-f2e5c4d1 code=5534fea gate=manual)

## Observation

On the integrated v0.1.147 client and Rhydra v0.5.0 host, Quench negotiated the two-tile H.264 path at 5120x2880/200%, but the desktop was noticeably pixelated compared with ordinary Remote Desktop. The supervised server passes no bitrate override, so the CLI default of 20,000 kbit/s is divided across two 2560x2880 encoders: 10 Mbit/s per tile at a declared 60 fps. Expected: the supervised 5K path uses a resolution-appropriate bitrate that meets or exceeds Microsoft rendering quality without adding stale buffering.

## Fix

<unfixed — raised only>

## Notes
