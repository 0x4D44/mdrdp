# MDR-BUG-FLU-00049 — Native probe connect retries can exceed and misreport their deadline

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** native/startup
- **Raised:** 2026-08-23T12:09:37Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T100235Z-9eca0c7f
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00049-run-verify-20260913T100235Z-9eca0c7f
- **Owner base:** 8c3bf36eb6995374dfb9351d640eb6e34712e09e
- **Owner fingerprint:** sha256:27aae8e7cdff9f93263575573ca74099e4ac44e4598d6873f6741604d7568458
- **Owner since:** 2026-09-13T10:02:35Z
- **Owner until:** 2026-09-13T12:02:35Z
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
