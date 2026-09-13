# MDR-BUG-FLU-00066 — Native sessions cannot unlock the Windows PIN desktop and all input is rejected

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/auth-input
- **Raised:** 2026-08-23T20:27:23Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T053213Z-c68ce077
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00066-run-verify-20260913T053213Z-c68ce077
- **Owner base:** 9040b57ff3543601fc9c33f4fbd7bb42b0354a5f
- **Owner fingerprint:** sha256:50f80aa64e8f908a12940a202c570246c223b7ccf00b4bf469da36af252edf04
- **Owner since:** 2026-09-13T05:32:13Z
- **Owner until:** 2026-09-13T07:32:13Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T20:27:23Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T21:37:56Z, deltic:auto role=fix run=fix-20260823T205857Z-c8a7b1ba branch=task/bug-MDR-BUG-FLU-00066-run-fix-20260823T205857Z-c8a7b1ba code=bae02ec gate=manual)

## Observation

On Quench at 5120x2880/200%, a native connection showed the Windows PIN screen, unlike an ordinary Remote Desktop connection, and keyboard and pointer input did nothing. The contemporaneous rhydra-server log recorded SendInput injected 0 of 1 for every mouse, button, and scancode event although the control health ladder reported input-desktop ok. Expected: native connection performs an authenticated Windows logon/unlock and accepts input, or refuses before presenting an unusable locked session.

## Fix

<unfixed — raised only>

## Notes
