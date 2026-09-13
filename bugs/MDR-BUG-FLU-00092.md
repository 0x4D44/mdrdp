# MDR-BUG-FLU-00092 — Scaled EGFX surface mappings discard origin and target geometry

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/surface-mapping
- **Raised:** 2026-08-24T09:37:41Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T145220Z-99998350
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00092-run-verify-20260913T145220Z-99998350
- **Owner base:** 7de029847048f9c796f8ff0457ad22a2a1a7142a
- **Owner fingerprint:** sha256:8a3f918b073b4d3fe41f9a667fd7e918ad4a7a6d047fcc337d66ebcdb08a2706
- **Owner since:** 2026-09-13T14:52:20Z
- **Owner until:** 2026-09-13T16:52:20Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T09:37:41Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T10:36:22Z, deltic:auto role=fix run=fix-20260824T095906Z-db220a0e branch=task/bug-MDR-BUG-FLU-00092-run-fix-20260824T095906Z-db220a0e code=4e797b3 gate=manual)

## Observation

Observation: GfxHandler routes MapSurfaceToScaledOutput, MapSurfaceToWindow, and scaled-window mappings to SurfaceStore::map_to_output without passing their origin or target dimensions. A distinct-colour 2x2 surface mapped to a 4x2 target therefore remains 2x2 at origin zero; real temper captures contain MapSurfaceToScaledOutput. Expected: presentation applies each mapping PDU origin and scale. Actual: the mapping parameters never reach presentation.

## Fix

<unfixed — raised only>

## Notes
