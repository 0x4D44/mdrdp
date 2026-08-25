# MDR-BUG-FLU-00121 — Driver deploy stores its signing key in the SSH user profile

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/idd-deploy
- **Raised:** 2026-08-25T06:54:28Z
- **Discovery source:** Automation
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260825T065502Z-6683973a
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00121-run-fix-20260825T065502Z-6683973a
- **Owner base:** 265340c946a0f87ada8a03700e641d6c2041fc50
- **Owner fingerprint:** -
- **Owner since:** 2026-08-25T06:55:02Z
- **Owner until:** 2026-08-25T08:55:02Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-25T06:54:28Z, raised via `deltic bugs new`)

## Observation

On Quench, deploying a fresh IDD package fails in driver-install.ps1. The saved CurrentUser certificate has a missing keyset, and after removing it New-SelfSignedCertificate fails with NTE_PERM in the SSH account profile. Expected: the elevated machine-wide driver deployment owns a usable machine-wide signing key and SignTool selects that store; actual: it depends on the transient SSH user's CurrentUser key store and cannot install the driver.

## Fix

<unfixed — raised only>

## Notes
