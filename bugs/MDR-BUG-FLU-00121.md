# MDR-BUG-FLU-00121 — Driver deploy stores its signing key in the SSH user profile

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/idd-deploy
- **Raised:** 2026-08-25T06:54:28Z
- **Discovery source:** Automation
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
- **State history:** Open (2026-08-25T06:54:28Z, raised via `deltic bugs new`) -> Fixed (2026-08-25T06:58:01Z, deltic:auto role=fix run=fix-20260825T065502Z-6683973a branch=task/bug-MDR-BUG-FLU-00121-run-fix-20260825T065502Z-6683973a code=1412a73 gate=manual)

## Observation

On Quench, deploying a fresh IDD package fails in driver-install.ps1. The saved CurrentUser certificate has a missing keyset, and after removing it New-SelfSignedCertificate fails with NTE_PERM in the SSH account profile. Expected: the elevated machine-wide driver deployment owns a usable machine-wide signing key and SignTool selects that store; actual: it depends on the transient SSH user's CurrentUser key store and cannot install the driver.

## Fix

<unfixed — raised only>

## Notes
