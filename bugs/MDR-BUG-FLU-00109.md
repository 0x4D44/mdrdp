# MDR-BUG-FLU-00109 — Native input writes have no absolute wall-clock deadline

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** native/input-latency
- **Raised:** 2026-08-24T12:24:25Z
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
- **State history:** Open (2026-08-24T12:24:25Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T12:55:46Z, deltic:auto role=fix run=fix-20260824T124602Z-f1c450aa branch=task/bug-MDR-BUG-FLU-00109-run-fix-20260824T124602Z-f1c450aa code=e4e2ba5 gate=manual)

## Observation

Native frameless input uses write_all behind a per-syscall timeout. A peer that accepts a small amount before each timeout can keep one input record in flight indefinitely, monopolizing the input worker. Apply one absolute deadline to the logical record and prove a dribbling loopback peer cannot extend it.

## Fix

<unfixed — raised only>

## Notes
