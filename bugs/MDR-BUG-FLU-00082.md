# MDR-BUG-FLU-00082 — Rhydra cold-starts black when the Windows console is locked

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/secure-desktop
- **Raised:** 2026-08-23T22:04:30Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T131945Z-3c508bc0
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00082-run-verify-20260913T131945Z-3c508bc0
- **Owner base:** 064efd42a620b4877bab8c7fcb9e56f5edbc1517
- **Owner fingerprint:** sha256:ad90ecc8b797cce17d426ee0fe85575633055cfb8ef5de3dd35b335248394da8
- **Owner since:** 2026-09-13T13:19:45Z
- **Owner until:** 2026-09-13T15:19:45Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T22:04:30Z, raised via `deltic bugs new`) -> Fixed (2026-08-23T22:11:32Z, deltic:auto role=fix run=fix-20260823T220531Z-aad19780 branch=task/bug-MDR-BUG-FLU-00082-run-fix-20260823T220531Z-aad19780 code=39a1844 gate=manual)

## Observation

Restarting RhydraAgent while Quench is on the Winlogon PIN desktop starts a healthy 5120x2880/200% worker and accepts secure-desktop input, but new native viewers receive a black frame and zero encoded frames. Starting unlocked and then locking captures the PIN screen correctly. Expected: a service start or restart while locked immediately captures the Winlogon desktop, so unattended reboot and recovery remain usable.

## Fix

<unfixed — raised only>

## Notes
