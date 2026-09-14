# MDR-BUG-FLU-00120 — IDD layout change reuses the installed driver version

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/idd-deploy
- **Raised:** 2026-08-25T06:47:35Z
- **Discovery source:** Automation
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T101448Z-8d55f9b8
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00120-run-verify-20260914T101448Z-8d55f9b8
- **Owner base:** 69786f96c4b5cd042b30b1fd203997bd6f902eec
- **Owner fingerprint:** sha256:09f75ffb59e63f39afb9b6de19149af66d71883bfc68137601b72631d6a674f5
- **Owner since:** 2026-09-14T10:14:48Z
- **Owner until:** 2026-09-14T12:14:48Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-25T06:47:35Z, raised via `deltic bugs new`) -> Fixed (2026-08-25T06:51:06Z, deltic:auto role=fix run=fix-20260825T064809Z-36a94461 branch=task/bug-MDR-BUG-FLU-00120-run-fix-20260825T064809Z-36a94461 code=809d72f gate=manual)

## Observation

Deploying the integrated layout-v2 Rhydra stack to Quench leaves the active IDD publishing layout_version 1. The server reports that it speaks layout 2 and exits repeatedly because deploy sees active DriverVer 0.3.0.2 matching the unchanged INF and skips driver replacement. A shared-pool ABI change must advance DriverVer so Windows stages and activates the matching driver package.

## Fix

<unfixed — raised only>

## Notes
