# MDR-BUG-FLU-00044 — An unterminated Rhydra agent control request can grow memory without bound

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/control
- **Raised:** 2026-08-22T19:40:40Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T195542Z-8f67a99b
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00044-run-fix-20260823T195542Z-8f67a99b
- **Owner base:** 08fe46e31ad9c4255ab09d5de2ec00ae8da4cb83
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T19:55:42Z
- **Owner until:** 2026-08-23T21:55:42Z
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
