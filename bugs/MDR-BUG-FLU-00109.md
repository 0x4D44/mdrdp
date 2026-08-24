# MDR-BUG-FLU-00109 — Native input writes have no absolute wall-clock deadline

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** native/input-latency
- **Raised:** 2026-08-24T12:24:25Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T124602Z-f1c450aa
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00109-run-fix-20260824T124602Z-f1c450aa
- **Owner base:** 88a6e95a556f328a962d3ca4bec7f33af86a3557
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T12:46:02Z
- **Owner until:** 2026-08-24T14:46:02Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T12:24:25Z, raised via `deltic bugs new`)

## Observation

Native frameless input uses write_all behind a per-syscall timeout. A peer that accepts a small amount before each timeout can keep one input record in flight indefinitely, monopolizing the input worker. Apply one absolute deadline to the logical record and prove a dribbling loopback peer cannot extend it.

## Fix

<unfixed — raised only>

## Notes
