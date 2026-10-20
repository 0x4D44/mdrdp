# MDR-BUG-FLU-00093 — Deleting the final mapped surface leaves stale pixels presented indefinitely

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/presentation-lifecycle
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
- **State history:** Open (2026-08-24T09:37:41Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T09:50:26Z, deltic:auto role=fix run=fix-20260824T094417Z-dbd08b66 branch=task/bug-MDR-BUG-FLU-00093-run-fix-20260824T094417Z-dbd08b66 code=2603b58 gate=manual) -> Closed (2026-09-13T15:21:18Z, 0x4D44/Codex verify run=verify-20260913T145238Z-e2e013ef)

## Observation

Observation: SurfaceStore::delete removes the mapped output but retains presentation_fallback, and copy_presentation continues returning that fallback as Copied without a generation change. Painting and mapping a red surface, deleting it, then copying presentation still returns red forever when no replacement surface is mapped. Expected: deleting the final mapped surface invalidates the terminal presentation. Actual: the deleted surface remains visible indefinitely.

## Fix

`src/surface.rs:908-932` now clears terminal presentation state when the only mapped surface is deleted while idle, and `src/surface.rs:798-810` clears it when an active terminal deletion commits (`53d0e14`, integrated by `2603b58`). Active frames retain the last-good fallback until commit.

## Verification

Independent verifier ran `deleting_the_only_mapped_surface_clears_the_presentation` and `terminal_frame_delete_clears_only_at_commit`; each selected test ran 1 test and passed, and the SurfaceStore family passed 62 tests. Independent mutants of the idle deletion path and frame-commit path both failed their meaningful generation assertions; restoration passed with a clean diff.

Lead verifier on tree `2922d2735be8dfdc964bb37fdd1d15c011386f50` reran both deletion regressions; each passed. Mutating the idle `touch_presentation` condition at `src/surface.rs:928` failed at line 2696 with generation `1` instead of `2`. Mutating the terminal frame commit `touch_presentation` call at `src/surface.rs:805` failed at line 2727 with generation `1` instead of `2`. Restoring both paths made all six batch-focused tests pass.

All repository gates passed: `cargo build --locked`, `cargo test --locked`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/test-vendored.sh`, and `./scripts/check-windows.sh --locked`. Windows checking emitted only the existing unused `width` and `height` warnings at `src/present.rs:95`. No live RDP interoperability session was run; deterministic SurfaceStore tests exercise both terminal deletion paths directly.
