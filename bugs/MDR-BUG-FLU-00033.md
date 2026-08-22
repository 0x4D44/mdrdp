# MDR-BUG-FLU-00033 — deploy final driver check races PnP version propagation after install

- **State:** Open
- **Priority:** Must
- **Severity:** Medium
- **Area:** deploy/driver
- **Raised:** 2026-08-22T00:09:13Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260822T001032Z-d0709899
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00033-run-fix-20260822T001032Z-d0709899
- **Owner base:** 215a86832abfe350d11adc27f10d76ff811b4182
- **Owner fingerprint:** -
- **Owner since:** 2026-08-22T00:10:32Z
- **Owner until:** 2026-08-22T02:10:32Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T00:09:13Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

The first v0.1.119 deployment installed and bound IDD 0.3.0.2, but the immediate final PROBE_PS1 sample still returned active_driver_ver 0.3.0.1, so deploy reported failure. A direct present-instance query moments later reported oem104.inf / 0.3.0.2, and an unchanged verify-only deploy then passed. Expected: final activation verification remains strict but polls boundedly through normal PnP/CIM propagation rather than producing a false failure.

## Fix

<unfixed — raised only>

## Notes
