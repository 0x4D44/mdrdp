# MDR-BUG-FLU-00035 — Rhydra leaves the physical display primary, so applications open outside the captured virtual desktop

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/display-provisioning
- **Raised:** 2026-08-22T18:08:59Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260822T193215Z-4575f714
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00035-run-verify-20260822T193215Z-4575f714
- **Owner base:** 75e1dc67277f2f067de932c93b3042b6e6f4dec9
- **Owner fingerprint:** sha256:596e22608e8829f069a66cb7f440cca2aa0720f63adce6a5522edf18e4291c5b
- **Owner since:** 2026-08-22T19:32:15Z
- **Owner until:** 2026-08-22T21:32:15Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T18:08:59Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-22T18:52:06Z, deltic:auto role=fix run=fix-20260822T183242Z-2240d049 branch=task/bug-MDR-BUG-FLU-00035-run-fix-20260822T183242Z-2240d049 code=558c2a3 gate=manual) -> Open (2026-08-22T18:58:04Z, independent live verifier found deployed Quench still reported the IDD secondary at (1920, 0), mode_ok=false, and server stopped) -> Fixed (2026-08-22T19:31:32Z, deltic:auto role=fix run=fix-20260822T190258Z-363b54b0 branch=task/bug-MDR-BUG-FLU-00035-run-fix-20260822T190258Z-363b54b0 code=5f12d37322da2410885a02eedcb5165bcc885c18 gate=manual)

## Observation

On Quench, the Rhydra server captured the virtual display at physical origin (1920,0), while interactive inspection showed DISPLAY1 remained primary at logical 0..1920. Command Prompt, File Explorer, Media Player, and other application windows all opened on DISPLAY1 and were therefore absent from the captured Rhydra desktop. Provisioning must make the Rhydra display primary at (0,0) before the server captures and injects input.

## Fix

The first integrated attempt (`558c2a3`) made placement observable and tried a staged
Win32 primary-display transaction. Live Quench verification showed that Windows rejected
or failed that transaction; the agent did not retain the exact platform error, so the
next fix must expose it before correcting the transaction.

## Notes
