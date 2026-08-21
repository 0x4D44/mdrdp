# MDR-BUG-FLU-00032 — mdrdp deploy resets Rhydra display scale to 100% during agent reinstall

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** deploy/display
- **Raised:** 2026-08-21T23:26:41Z
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
- **State history:** Open (2026-08-21T23:26:41Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-21T23:45:17Z, deltic:auto role=fix run=fix-20260821T232706Z-320466c2 branch=task/bug-MDR-BUG-FLU-00032-run-fix-20260821T232706Z-320466c2 code=83deec1 gate=manual)

## Observation

Observed on Quench while deploying integrated v0.1.117: the host was healthy at 200% scale, but the full deploy ran rhydra-agent install without display arguments. The new agent reported desired_desktop_scale_percent=100 and refused to start the server because the actual console remained at 200%. A full deploy must preserve the existing desired display mode and scale instead of silently reverting agent defaults.

## Fix

<unfixed — raised only>

## Notes
