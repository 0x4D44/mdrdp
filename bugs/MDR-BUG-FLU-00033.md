# MDR-BUG-FLU-00033 — deploy final driver check races PnP version propagation after install

- **State:** Fixed
- **Priority:** Must
- **Severity:** Medium
- **Area:** deploy/driver
- **Raised:** 2026-08-22T00:09:13Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T083218Z-52835e90
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00033-run-verify-20260913T083218Z-52835e90
- **Owner base:** 6cae659992a425fd27b7f4eb4ea870813a2557bb
- **Owner fingerprint:** sha256:85283190afe4081ab8387d70276d48d802d51d54d903d0349ee5d392ca6800d6
- **Owner since:** 2026-09-13T08:32:18Z
- **Owner until:** 2026-09-13T10:32:18Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T00:09:13Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-22T00:24:59Z, deltic:auto role=fix run=fix-20260822T001032Z-d0709899 branch=task/bug-MDR-BUG-FLU-00033-run-fix-20260822T001032Z-d0709899 code=917c366 gate=manual)

## Observation

The first v0.1.119 deployment installed and bound IDD 0.3.0.2, but the immediate final PROBE_PS1 sample still returned active_driver_ver 0.3.0.1, so deploy reported failure. A direct present-instance query moments later reported oem104.inf / 0.3.0.2, and an unchanged verify-only deploy then passed. Expected: final activation verification remains strict but polls boundedly through normal PnP/CIM propagation rather than producing a false failure.

## Fix

<unfixed — raised only>

## Notes
