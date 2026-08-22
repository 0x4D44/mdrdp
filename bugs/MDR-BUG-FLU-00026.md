# MDR-BUG-FLU-00026 — mdrdp deploy fast path treats same-size stale driver artifacts as an exact stack

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** deploy/driver
- **Raised:** 2026-08-20T19:02:37Z
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
- **State history:** Open (2026-08-20T19:02:37Z, raised via `deltic bugs new`) -> Fixed (2026-08-22T00:05:46Z, deltic:auto role=fix run=fix-20260821T235315Z-40c465f7 branch=task/bug-MDR-BUG-FLU-00026-run-fix-20260821T235315Z-40c465f7 code=25e540e gate=manual)

## Observation

Deploying integrated driver 0.3.0.1 over quench's rhydra v0.5.0 returned 'already deployed and healthy — verify only' while the active display driver remained 0.3.0.0 and staged packages did not include 0.3.0.1. src/deploy.rs decides exact_stack from version-directory presence, agent version, and artifact basename sizes before it compares active_driver_ver. The changed DLL and INF kept their prior byte lengths, so the fast path skipped copy and driver installation. Expected: a changed driver package identity can never take the exact-stack fast path, even when file sizes are unchanged.

## Fix

<unfixed — raised only>

## Notes
