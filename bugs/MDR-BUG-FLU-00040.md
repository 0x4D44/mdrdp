# MDR-BUG-FLU-00040 — Rhydra orphan cleanup can kill unrelated same-named processes

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/process-lifecycle
- **Raised:** 2026-08-22T19:40:40Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T085841Z-c6293da0
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00040-run-verify-20260913T085841Z-c6293da0
- **Owner base:** c185da23bac4767851e8db93e8c17af9e7a556b0
- **Owner fingerprint:** sha256:9e36e15374f544f94f859c43711d938bf3d4628720ae75115ee0659251c8ef10
- **Owner since:** 2026-09-13T08:58:41Z
- **Owner until:** 2026-09-13T10:58:41Z
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
