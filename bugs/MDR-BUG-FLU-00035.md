# MDR-BUG-FLU-00035 — Rhydra leaves the physical display primary, so applications open outside the captured virtual desktop

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/display-provisioning
- **Raised:** 2026-08-22T18:08:59Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260822T190258Z-363b54b0
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00035-run-fix-20260822T190258Z-363b54b0
- **Owner base:** 03aa58c2e60dbb806231a233d7dc53731d3f179c
- **Owner fingerprint:** -
- **Owner since:** 2026-08-22T19:02:58Z
- **Owner until:** 2026-08-22T21:02:58Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T18:08:59Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-22T18:52:06Z, deltic:auto role=fix run=fix-20260822T183242Z-2240d049 branch=task/bug-MDR-BUG-FLU-00035-run-fix-20260822T183242Z-2240d049 code=558c2a3 gate=manual) -> Open (2026-08-22T18:58:04Z, independent live verifier found deployed Quench still reported the IDD secondary at (1920, 0), mode_ok=false, and server stopped)

## Observation

On Quench, the Rhydra server captured the virtual display at physical origin (1920,0), while interactive inspection showed DISPLAY1 remained primary at logical 0..1920. Command Prompt, File Explorer, Media Player, and other application windows all opened on DISPLAY1 and were therefore absent from the captured Rhydra desktop. Provisioning must make the Rhydra display primary at (0,0) before the server captures and injects input.

## Fix

The first integrated attempt (`558c2a3`) made placement observable and tried a staged
Win32 primary-display transaction. Live Quench verification showed that Windows rejected
or failed that transaction; the agent did not retain the exact platform error, so the
next fix must expose it before correcting the transaction.

## Notes
