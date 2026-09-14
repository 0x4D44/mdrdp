# MDR-BUG-FLU-00121 — Driver deploy stores its signing key in the SSH user profile

- **State:** Closed
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
- **State history:** Open (2026-08-25T06:54:28Z, raised via `deltic bugs new`) -> Fixed (2026-08-25T06:58:01Z, deltic:auto role=fix run=fix-20260825T065502Z-6683973a branch=task/bug-MDR-BUG-FLU-00121-run-fix-20260825T065502Z-6683973a code=1412a73 gate=manual) -> Closed (2026-09-14T10:43:29Z, 0x4D44/Codex verify run=verify-20260914T103304Z-9811bada)

## Observation

On Quench, deploying a fresh IDD package fails in driver-install.ps1. The saved CurrentUser certificate has a missing keyset, and after removing it New-SelfSignedCertificate fails with NTE_PERM in the SSH account profile. Expected: the elevated machine-wide driver deployment owns a usable machine-wide signing key and SignTool selects that store; actual: it depends on the transient SSH user's CurrentUser key store and cannot install the driver.

## Fix

`1412a73` creates and reuses the development signing certificate in
`Cert:\LocalMachine\My` and selects that store with SignTool's `/sm` option.
The change is integrated in `1412a73`.

## Verification

The independent verifier passed
`deploy::tests::driver_signing_key_is_machine_scoped` and its 36-test deploy
family.

The independent verifier removed `/sm` from `DRIVER_INSTALL_PS1` at
`src/deploy.rs:1331`. The focused test failed at `src/deploy.rs:2725` because
the machine-scope assertion no longer held. The lead made the same mutation
and observed the same failure. Restoring `/sm` made the focused test and the
lead's 40-test deploy family pass again. No live Windows or RDP runtime was
used.

## Notes
