# MDR-BUG-FLU-00035 — Rhydra leaves the physical display primary, so applications open outside the captured virtual desktop

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/display-provisioning
- **Raised:** 2026-08-22T18:08:59Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260822T183242Z-2240d049
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00035-run-fix-20260822T183242Z-2240d049
- **Owner base:** ea43bde77dc644b14fa5ad11f0c3c9df1f880468
- **Owner fingerprint:** -
- **Owner since:** 2026-08-22T18:32:42Z
- **Owner until:** 2026-08-22T20:32:42Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T18:08:59Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

On Quench, the Rhydra server captured the virtual display at physical origin (1920,0), while interactive inspection showed DISPLAY1 remained primary at logical 0..1920. Command Prompt, File Explorer, Media Player, and other application windows all opened on DISPLAY1 and were therefore absent from the captured Rhydra desktop. Provisioning must make the Rhydra display primary at (0,0) before the server captures and injects input.

## Fix

<unfixed — raised only>

## Notes
