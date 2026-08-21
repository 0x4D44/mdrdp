# MDR-BUG-FLU-00032 — mdrdp deploy resets Rhydra display scale to 100% during agent reinstall

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** deploy/display
- **Raised:** 2026-08-21T23:26:41Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260821T232706Z-320466c2
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00032-run-fix-20260821T232706Z-320466c2
- **Owner base:** 1cbb98e697887f2a69154e8955e7e1651f9fa414
- **Owner fingerprint:** -
- **Owner since:** 2026-08-21T23:27:06Z
- **Owner until:** 2026-08-22T01:44:50Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-21T23:26:41Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

Observed on Quench while deploying integrated v0.1.117: the host was healthy at 200% scale, but the full deploy ran rhydra-agent install without display arguments. The new agent reported desired_desktop_scale_percent=100 and refused to start the server because the actual console remained at 200%. A full deploy must preserve the existing desired display mode and scale instead of silently reverting agent defaults.

## Fix

<unfixed — raised only>

## Notes
