# MDR-BUG-FLU-00082 — Rhydra cold-starts black when the Windows console is locked

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/secure-desktop
- **Raised:** 2026-08-23T22:04:30Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T220531Z-aad19780
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00082-run-fix-20260823T220531Z-aad19780
- **Owner base:** b73b208b8c782092d0a49f8598974b106a7521da
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T22:05:31Z
- **Owner until:** 2026-08-24T00:05:31Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T22:04:30Z, raised via `deltic bugs new`)

## Observation

Restarting RhydraAgent while Quench is on the Winlogon PIN desktop starts a healthy 5120x2880/200% worker and accepts secure-desktop input, but new native viewers receive a black frame and zero encoded frames. Starting unlocked and then locking captures the PIN screen correctly. Expected: a service start or restart while locked immediately captures the Winlogon desktop, so unattended reboot and recovery remain usable.

## Fix

<unfixed — raised only>

## Notes
