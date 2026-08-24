# MDR-BUG-FLU-00110 — Native rectangle batches expose partial frames when a later rectangle is invalid

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** native/frame-atomicity
- **Raised:** 2026-08-24T12:31:15Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T125604Z-8a99767b
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00110-run-fix-20260824T125604Z-8a99767b
- **Owner base:** f77e2321f6c73a2af5857ffeafb213a15f0c2820
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T12:56:04Z
- **Owner until:** 2026-08-24T14:56:04Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T12:31:15Z, raised via `deltic bugs new`)

## Observation

NativeSink::paint applies each BGRA rectangle through SurfaceStore::blit_bgra_strict before the complete RectUpdate has been validated. If an early rectangle is valid and a later rectangle is out of bounds or has a malformed payload, the function returns an error after the earlier pixels and presentation generation have already changed, leaving a rejected mixed frame observable before teardown or another wake. Preflight every rectangle and payload, then apply the BGRA batch as one store mutation with one generation advance; prove a valid-first invalid-second update changes neither pixels nor generation.

## Fix

<unfixed — raised only>

## Notes
