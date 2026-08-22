# MDR-BUG-FLU-00044 — An unterminated Rhydra agent control request can grow memory without bound

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/control
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
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

tools/latency-spike/server/src/bin/agent.rs:359-379 reads control requests with BufReader::lines and has a per-read timeout but no maximum line length. A local peer can continuously send a newline-free request slowly enough to avoid the timeout while growing the allocation until the agent exhausts memory. Enforce a small protocol line ceiling before allocation grows.

## Fix

<unfixed — raised only>

## Notes
