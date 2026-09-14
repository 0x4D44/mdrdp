# MDR-BUG-FLU-00109 — Native input writes have no absolute wall-clock deadline

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** native/input-latency
- **Raised:** 2026-08-24T12:24:25Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T090527Z-910bbb1e
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00109-run-verify-20260914T090527Z-910bbb1e
- **Owner base:** adfc080922d468e384b422b49d22dbf7aeeac8ca
- **Owner fingerprint:** sha256:ea1c4f276dc6da43ad4d0bf4f270980fe62ef2e362505561a78a3167dcea9893
- **Owner since:** 2026-09-14T09:05:27Z
- **Owner until:** 2026-09-14T11:05:27Z
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
