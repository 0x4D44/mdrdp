# MDR-BUG-FLU-00049 — Native probe connect retries can exceed and misreport their deadline

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** native/startup
- **Raised:** 2026-08-23T12:09:37Z
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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T18:46:12Z, deltic:auto role=fix run=fix-20260824T183819Z-f4fd4740 branch=task/bug-MDR-BUG-FLU-00049-run-fix-20260824T183819Z-f4fd4740 code=61070bb gate=manual)

## Observation

The native SSH readiness and input-probe loops grant each attempt a fixed 250 ms connect timeout and then sleep 50 ms without bounding either by the remaining deadline. A final attempt can overshoot the advertised budget and the input path can report a generic I/O failure instead of Deadline. Derive each connect and sleep duration from the remaining budget and test the deadline boundary.

## Fix

<unfixed — raised only>

## Notes
