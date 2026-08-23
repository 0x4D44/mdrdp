# MDR-BUG-FLU-00082 — Rhydra cold-starts black when the Windows console is locked

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/secure-desktop
- **Raised:** 2026-08-23T22:04:30Z
- **Discovery source:** Agent
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
- **State history:** Open (2026-08-23T22:04:30Z, raised via `deltic bugs new`)

## Observation

Restarting RhydraAgent while Quench is on the Winlogon PIN desktop starts a healthy 5120x2880/200% worker and accepts secure-desktop input, but new native viewers receive a black frame and zero encoded frames. Starting unlocked and then locking captures the PIN screen correctly. Expected: a service start or restart while locked immediately captures the Winlogon desktop, so unattended reboot and recovery remain usable.

## Fix

<unfixed — raised only>

## Notes
