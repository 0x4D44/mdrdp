# MDR-BUG-FLU-00120 — IDD layout change reuses the installed driver version

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/idd-deploy
- **Raised:** 2026-08-25T06:47:35Z
- **Discovery source:** Automation
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260825T064809Z-36a94461
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00120-run-fix-20260825T064809Z-36a94461
- **Owner base:** cd1ecc21fa8693cf1981b69491cb620f7ab4cf92
- **Owner fingerprint:** -
- **Owner since:** 2026-08-25T06:48:09Z
- **Owner until:** 2026-08-25T08:48:09Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-25T06:47:35Z, raised via `deltic bugs new`)

## Observation

Deploying the integrated layout-v2 Rhydra stack to Quench leaves the active IDD publishing layout_version 1. The server reports that it speaks layout 2 and exits repeatedly because deploy sees active DriverVer 0.3.0.2 matching the unchanged INF and skips driver replacement. A shared-pool ABI change must advance DriverVer so Windows stages and activates the matching driver package.

## Fix

<unfixed — raised only>

## Notes
