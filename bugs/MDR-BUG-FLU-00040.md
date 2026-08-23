# MDR-BUG-FLU-00040 — Rhydra orphan cleanup can kill unrelated same-named processes

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/process-lifecycle
- **Raised:** 2026-08-22T19:40:40Z
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
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T20:08:11Z, deltic:auto role=fix run=fix-20260823T200141Z-40b1bfac branch=task/bug-MDR-BUG-FLU-00040-run-fix-20260823T200141Z-40b1bfac code=2b8a54a gate=manual)

## Observation

tools/latency-spike/server/src/win/agent_ops.rs:321-329 and src/bin/agent.rs:638-655 invoke taskkill by image name without PID, executable path, session, or ownership filtering. Another user or test stack running a same-named server or creator can be forcibly terminated. Cleanup must target only processes owned by this installation.

## Fix

<unfixed — raised only>

## Notes
