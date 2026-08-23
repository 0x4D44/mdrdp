# MDR-BUG-FLU-00065 — Default 20 Mbit/s budget makes the 5K native desktop visibly pixelated

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/rendering-quality
- **Raised:** 2026-08-23T20:27:23Z
- **Discovery source:** Human
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
- **State history:** Open (2026-08-23T20:27:23Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

On the integrated v0.1.147 client and Rhydra v0.5.0 host, Quench negotiated the two-tile H.264 path at 5120x2880/200%, but the desktop was noticeably pixelated compared with ordinary Remote Desktop. The supervised server passes no bitrate override, so the CLI default of 20,000 kbit/s is divided across two 2560x2880 encoders: 10 Mbit/s per tile at a declared 60 fps. Expected: the supervised 5K path uses a resolution-appropriate bitrate that meets or exceeds Microsoft rendering quality without adding stale buffering.

## Fix

<unfixed — raised only>

## Notes
