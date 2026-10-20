# MDR-BUG-FLU-00092 — Scaled EGFX surface mappings discard origin and target geometry

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/surface-mapping
- **Raised:** 2026-08-24T09:37:41Z
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
- **State history:** Open (2026-08-24T09:37:41Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T10:36:22Z, deltic:auto role=fix run=fix-20260824T095906Z-db220a0e branch=task/bug-MDR-BUG-FLU-00092-run-fix-20260824T095906Z-db220a0e code=4e797b3 gate=manual) -> Closed (2026-09-13T15:21:18Z, 0x4D44/Codex verify run=verify-20260913T145220Z-99998350)

## Observation

Observation: GfxHandler routes MapSurfaceToScaledOutput, MapSurfaceToWindow, and scaled-window mappings to SurfaceStore::map_to_output without passing their origin or target dimensions. A distinct-colour 2x2 surface mapped to a 4x2 target therefore remains 2x2 at origin zero; real temper captures contain MapSurfaceToScaledOutput. Expected: presentation applies each mapping PDU origin and scale. Actual: the mapping parameters never reach presentation.

## Fix

`src/gfx.rs:806-825` now passes the EGFX output origin and target dimensions to `SurfaceStore::map_to_output_geometry`; the store retains that mapping in the presentation snapshot (`74ab8ba`, integrated by `4e797b3`). Unsupported RAIL window mappings remain separate from desktop output.

## Verification

Independent verifier ran five focused callback, scaled-geometry, and SurfaceStore tests; all five passed. Mutating only `pdu.target_width` and `pdu.target_height` at `src/gfx.rs:823` to the source dimensions made `scaled_output_mapping_reaches_the_presentation_geometry` fail at `src/gfx.rs:2517` with `2` instead of `4`; restoring the hunk made the focused tests pass again, with a clean diff.

Lead verifier on tree `2922d2735be8dfdc964bb37fdd1d15c011386f50` ran `output_maps_display_and_rail_maps_do_not_steal_the_desktop`, `scaled_output_mapping_reaches_the_presentation_geometry`, `scaled_mapping_geometry_is_copied_atomically_with_pixels`, and `scaled_surface_mapping_honours_canvas_origin_and_target_size`; each selected test ran 1 test and passed. The same target-size mutant failed at `src/gfx.rs:2517`; restoration passed all six batch-focused tests, including both FLU-00093 deletion regressions.

All repository gates passed: `cargo build --locked`, `cargo test --locked`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/test-vendored.sh`, and `./scripts/check-windows.sh --locked`. Windows checking emitted only the existing unused `width` and `height` warnings at `src/present.rs:95`. No live RDP interoperability session was run; the deterministic handler and presentation tests exercise the recorded geometry loss and its fix.
