# MDR-BUG-FLU-00066 — Native sessions cannot unlock the Windows PIN desktop and all input is rejected

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/auth-input
- **Raised:** 2026-08-23T20:27:23Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T205857Z-c8a7b1ba
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00066-run-fix-20260823T205857Z-c8a7b1ba
- **Owner base:** 7cf4fd06a1b8b11e3d107887b34b903084d4dddf
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T20:58:57Z
- **Owner until:** 2026-08-23T22:58:57Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T20:27:23Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

On Quench at 5120x2880/200%, a native connection showed the Windows PIN screen, unlike an ordinary Remote Desktop connection, and keyboard and pointer input did nothing. The contemporaneous rhydra-server log recorded SendInput injected 0 of 1 for every mouse, button, and scancode event although the control health ladder reported input-desktop ok. Expected: native connection performs an authenticated Windows logon/unlock and accepts input, or refuses before presenting an unusable locked session.

## Fix

<unfixed — raised only>

## Notes
