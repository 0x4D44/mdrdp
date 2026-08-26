# MDR-BUG-FLU-00125 — Scripted duration close can hang past its deadline

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** session/lifecycle
- **Raised:** 2026-08-26T14:03:10Z
- **Discovery source:** Automation
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
- **State history:** Open (2026-08-26T14:03:10Z, raised via `deltic bugs new`)

## Observation

A release mdrdp 0.1.237 run against Quench connected successfully, but --duration 8 did not exit before a 60-second outer guard killed the process. No metrics file or graceful-shutdown evidence was produced; inspection found no surviving Quench viewer, and the run was not retried. Investigate the closer-to-window-exit-to-session-join path beginning at src/main.rs scripted duration handling.

## Fix

<unfixed — raised only>

## Notes
