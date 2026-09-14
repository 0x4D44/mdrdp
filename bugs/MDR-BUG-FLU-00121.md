# MDR-BUG-FLU-00121 — Driver deploy stores its signing key in the SSH user profile

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/idd-deploy
- **Raised:** 2026-08-25T06:54:28Z
- **Discovery source:** Automation
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T103304Z-9811bada
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00121-run-verify-20260914T103304Z-9811bada
- **Owner base:** c172779da2c3a2e13ee9a331e88a0ab9c5eb7dfb
- **Owner fingerprint:** sha256:fee3ed437a0a860e2051e4a678d663ec81c3c438191df52bc6bda9374aa69797
- **Owner since:** 2026-09-14T10:33:04Z
- **Owner until:** 2026-09-14T12:33:04Z
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
