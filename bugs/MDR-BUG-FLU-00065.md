# MDR-BUG-FLU-00065 — Default 20 Mbit/s budget makes the 5K native desktop visibly pixelated

- **State:** Closed
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
- **State history:** Open (2026-08-23T20:27:23Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T20:32:26Z, deltic:auto role=fix run=fix-20260823T202741Z-f2e5c4d1 branch=task/bug-MDR-BUG-FLU-00065-run-fix-20260823T202741Z-f2e5c4d1 code=5534fea gate=manual) -> Closed (2026-09-13T05:45:26Z, independent verifier: 409 Rhydra tests, the selected 5K bitrate test, and the Windows cross-target gate passed; reverting the 80,000 kbit/s 5K policy failed its 80,000 assertion, model=codex@max)

## Observation

On the integrated v0.1.147 client and Rhydra v0.5.0 host, Quench negotiated the two-tile H.264 path at 5120x2880/200%, but the desktop was noticeably pixelated compared with ordinary Remote Desktop. The supervised server passes no bitrate override, so the CLI default of 20,000 kbit/s is divided across two 2560x2880 encoders: 10 Mbit/s per tile at a declared 60 fps. Expected: the supervised 5K path uses a resolution-appropriate bitrate that meets or exceeds Microsoft rendering quality without adding stale buffering.

## Fix

Commit `5534fea09b487a5871408c8d6a33b2b297e87e13` makes the supervised 5120x2880
path pass 80,000 kbit/s before the two encoder tiles divide that budget. The selected
`supervised_five_k_gets_four_times_the_1440p_bitrate` test passed with the current tree;
replacing only the 5K policy value with 20,000 made that same test fail its own
`left: 20000, right: 80000` assertion. The full server suite passed 409/409 and
`scripts/check-windows.sh --locked` passed for both the app and Rhydra host targets.

The original Quench pixelation observation was reviewed against the supervised launch
path, but this macOS pass did not start a new Windows IDD or live Quench session.

## Notes
