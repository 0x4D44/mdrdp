# MDR-BUG-FLU-00098 — Same-ID surface recreation presents pixels with stale mapping geometry

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/surface-lifecycle
- **Raised:** 2026-08-24T11:19:50Z
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
- **State history:** Open (2026-08-24T11:19:50Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T11:28:40Z, deltic:auto role=fix run=fix-20260824T112015Z-908a96a3 branch=task/bug-MDR-BUG-FLU-00098-run-fix-20260824T112015Z-908a96a3 code=ca8b618 gate=manual) -> Closed (2026-09-14T07:42:20Z, 0x4D44/Codex verify run=verify-20260914T071743Z-64d78951)

## Observation

Recreating the mapped surface with the same numeric ID retains the old fallback but also leaves the prior OutputMapping active. If the new surface becomes fully painted before its MapSurface PDU, presentation pairs new dimensions and pixels with stale source/target geometry, causing cropping, stretching, or blank regions. The old fallback and its geometry must remain visible atomically until a valid new mapping and complete replacement are both available.

## Fix

`src/surface.rs` marks same-ID recreation geometry stale, retains the last painted snapshot, and clears the barrier only after a valid fresh mapping (`da5a234`, integrated by `ca8b618`).

## Verification

Independent verifier ran `same_id_recreation_waits_for_fresh_mapping_geometry`, `same_id_replacement_during_a_frame_retains_the_snapshot_until_complete`, and `end_frame_cannot_publish_a_recreated_surface_before_remap`; all three passed. The surface test family passed 62 tests. Removing `output_mapping_stale = true` from `SurfaceStore::create` failed the focused test at `src/surface.rs:2817`; restoration passed the focused tests with a clean worktree.

Lead verifier ran the three same-ID surface regressions; each passed. Removing the stale-geometry mark made `same_id_recreation_waits_for_fresh_mapping_geometry` fail at `src/surface.rs:2818` because generation advanced from `1` to `2` before remapping; restoration passed all six batch-focused tests.

All repository gates passed on tree `77bddd164ea101cd32f65a06ac95f97ddd57bc20`: `cargo build --locked`, `cargo test --locked`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/test-vendored.sh`, and `./scripts/check-windows.sh --locked`. Windows checking emitted only the existing unused `width` and `height` warnings at `src/present.rs:95`. No live RDP interoperability session was run; deterministic SurfaceStore tests exercise the atomic replacement behavior directly.
