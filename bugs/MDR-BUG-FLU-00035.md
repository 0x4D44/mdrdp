# MDR-BUG-FLU-00035 — Rhydra leaves the physical display primary, so applications open outside the captured virtual desktop

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/display-provisioning
- **Raised:** 2026-08-22T18:08:59Z
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
- **State history:** Open (2026-08-22T18:08:59Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-22T18:52:06Z, deltic:auto role=fix run=fix-20260822T183242Z-2240d049 branch=task/bug-MDR-BUG-FLU-00035-run-fix-20260822T183242Z-2240d049 code=558c2a3 gate=manual) -> Open (2026-08-22T18:58:04Z, independent live verifier found deployed Quench still reported the IDD secondary at (1920, 0), mode_ok=false, and server stopped) -> Fixed (2026-08-22T19:31:32Z, deltic:auto role=fix run=fix-20260822T190258Z-363b54b0 branch=task/bug-MDR-BUG-FLU-00035-run-fix-20260822T190258Z-363b54b0 code=5f12d37322da2410885a02eedcb5165bcc885c18 gate=manual) -> Closed (2026-08-22T19:33:44Z, independent verifier: read-only Quench status was stably green with the CCD placement predicate satisfied, 5K/240/200% active, and server listening)

## Observation

On Quench, the Rhydra server captured the virtual display at physical origin (1920,0), while interactive inspection showed DISPLAY1 remained primary at logical 0..1920. Command Prompt, File Explorer, Media Player, and other application windows all opened on DISPLAY1 and were therefore absent from the captured Rhydra desktop. Provisioning must make the Rhydra display primary at (0,0) before the server captures and injects input.

## Fix

The first integrated attempt (`558c2a3`) made placement observable and tried a staged
Win32 primary-display transaction. Live Quench verification showed that Windows rejected
or failed that transaction; the agent did not retain the exact platform error, so the
next fix must expose it before correcting the transaction.

## Notes

Independent verification (2026-08-22): over read-only SSH as `marti`, the active scheduled
`C:\mdrdp\v0.5.0\rhydra-agent.exe` reported `mode_ok: true`, no stuck rung, all health
rungs green, `server.running: true`, and `viewer_connected: false`. The active process
was listening on `127.0.0.1:9500`; its stats header reported a 5120x2880 IDD source with
two tiles. The CCD agent's `mode_ok` predicate includes primary origin `(0,0)`; this is
the GUI/Windows oracle because the verifier runs on macOS. No viewer started and no host
state changed. The unversioned `C:\mdrdp\rhydra-agent.exe` status client was stale and
could not parse schema 7, so verification used the scheduled active binary.
