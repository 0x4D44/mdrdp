# MDR-BUG-FLU-00066 — Native sessions cannot unlock the Windows PIN desktop and all input is rejected

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/auth-input
- **Raised:** 2026-08-23T20:27:23Z
- **Discovery source:** Human
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
- **State history:** Open (2026-08-23T20:27:23Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

On Quench at 5120x2880/200%, a native connection showed the Windows PIN screen, unlike an ordinary Remote Desktop connection, and keyboard and pointer input did nothing. The contemporaneous rhydra-server log recorded SendInput injected 0 of 1 for every mouse, button, and scancode event although the control health ladder reported input-desktop ok. Expected: native connection performs an authenticated Windows logon/unlock and accepts input, or refuses before presenting an unusable locked session.

## Fix

<unfixed — raised only>

## Notes
