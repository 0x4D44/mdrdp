# MDR-BUG-FLU-00110 — Native rectangle batches expose partial frames when a later rectangle is invalid

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** native/frame-atomicity
- **Raised:** 2026-08-24T12:31:15Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T091949Z-6e0bb8b8
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00110-run-verify-20260914T091949Z-6e0bb8b8
- **Owner base:** db1c593a319e9bc384dc3acd0325d779701993f7
- **Owner fingerprint:** sha256:1b9b947fb66ad57a251f4c28cf790c1234643d8a01a6074f0ed34fe8fd430e56
- **Owner since:** 2026-09-14T09:19:49Z
- **Owner until:** 2026-09-14T11:19:49Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T12:31:15Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T13:05:40Z, deltic:auto role=fix run=fix-20260824T125604Z-8a99767b branch=task/bug-MDR-BUG-FLU-00110-run-fix-20260824T125604Z-8a99767b code=d7b0482 gate=manual)

## Observation

NativeSink::paint applies each BGRA rectangle through SurfaceStore::blit_bgra_strict before the complete RectUpdate has been validated. If an early rectangle is valid and a later rectangle is out of bounds or has a malformed payload, the function returns an error after the earlier pixels and presentation generation have already changed, leaving a rejected mixed frame observable before teardown or another wake. Preflight every rectangle and payload, then apply the BGRA batch as one store mutation with one generation advance; prove a valid-first invalid-second update changes neither pixels nor generation.

## Fix

<unfixed — raised only>

## Notes
