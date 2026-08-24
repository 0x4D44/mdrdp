# MDR-BUG-FLU-00049 — Native probe connect retries can exceed and misreport their deadline

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** native/startup
- **Raised:** 2026-08-23T12:09:37Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T183819Z-f4fd4740
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00049-run-fix-20260824T183819Z-f4fd4740
- **Owner base:** a6794c1d07c6177c08d531795f7fe28953049692
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T18:38:19Z
- **Owner until:** 2026-08-24T20:38:19Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

The native SSH readiness and input-probe loops grant each attempt a fixed 250 ms connect timeout and then sleep 50 ms without bounding either by the remaining deadline. A final attempt can overshoot the advertised budget and the input path can report a generic I/O failure instead of Deadline. Derive each connect and sleep duration from the remaining budget and test the deadline boundary.

## Fix

<unfixed — raised only>

## Notes
