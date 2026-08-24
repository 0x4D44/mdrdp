# MDR-BUG-FLU-00024 — Cold 5K mode switch outlives native display-status deadline, failing the first connection

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** native/probe
- **Raised:** 2026-08-20T17:42:01Z
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
- **State history:** Open (2026-08-20T17:42:01Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T18:57:46Z, deltic:auto role=fix run=fix-20260824T184631Z-eca420fc branch=task/bug-MDR-BUG-FLU-00024-run-fix-20260824T184631Z-eca420fc code=3180b7f gate=manual)

## Observation

On quench with integrated mdrdp v0.1.109 and rhydra v0.5.0 freshly deployed, the host was green at 2560x1440. Running target/release/mdrdp quench.lan.example --native --ssh-user ano --size 5120x2880 --duration 15 --foreground returned error: native probe of quench.lan.example timed out during display status. Immediately afterward, rhydra-agent status reported actual and desired display mode 5120x2880 at 240 Hz with mode_ok true; rerunning the identical client command connected successfully at 5120x2880. Expected: a supported cold 5K transition either completes within the probe deadline or the client continues polling long enough to connect on the first attempt. Actual: the first attempt fails even though the requested transition completes moments later.

## Fix

<unfixed — raised only>

## Notes
