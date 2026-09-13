# MDR-BUG-FLU-00032 — mdrdp deploy resets Rhydra display scale to 100% during agent reinstall

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** deploy/display
- **Raised:** 2026-08-21T23:26:41Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T081303Z-6105fc88
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00032-run-verify-20260913T081303Z-6105fc88
- **Owner base:** 12ed6cc293371c302fc652afa412b0cea964d239
- **Owner fingerprint:** sha256:33cb04a4b7096a53905844acdcea486aa849afb3fda1790a580a77a00b9befe8
- **Owner since:** 2026-09-13T08:13:03Z
- **Owner until:** 2026-09-13T10:13:03Z
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
