# MDR-BUG-FLU-00098 — Same-ID surface recreation presents pixels with stale mapping geometry

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/surface-lifecycle
- **Raised:** 2026-08-24T11:19:50Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T112015Z-908a96a3
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00098-run-fix-20260824T112015Z-908a96a3
- **Owner base:** a942cc0c0f477b04fd2c07f0e1aaaf072dba6de1
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T11:20:15Z
- **Owner until:** 2026-08-24T13:20:15Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T11:19:50Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

Recreating the mapped surface with the same numeric ID retains the old fallback but also leaves the prior OutputMapping active. If the new surface becomes fully painted before its MapSurface PDU, presentation pairs new dimensions and pixels with stale source/target geometry, causing cropping, stretching, or blank regions. The old fallback and its geometry must remain visible atomically until a valid new mapping and complete replacement are both available.

## Fix

<unfixed — raised only>

## Notes
