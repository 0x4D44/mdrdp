# MDR-BUG-FLU-00100 — Damage cadence uses stale snapshot dimensions after output shrinks

- **State:** Closed
- **Priority:** Must
- **Severity:** Medium
- **Area:** window/presentation-latency
- **Raised:** 2026-08-24T11:32:42Z
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
- **State history:** Open (2026-08-24T11:32:42Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T11:42:11Z, deltic:auto role=fix run=fix-20260824T113301Z-709a42a8 branch=task/bug-MDR-BUG-FLU-00100-run-fix-20260824T113301Z-709a42a8 code=bb6a329 gate=manual) -> Closed (2026-09-14T08:03:25Z, 0x4D44/Codex verify run=verify-20260914T074453Z-762d25c9)

## Observation

request_damage_redraw chooses cadence from the previously copied presentation dimensions before copying the current store generation. After a large-to-small output change, the stale large snapshot triggers the 33 ms large-surface delay even though the current visible output is small and eligible immediately. The generation and current presentation dimensions must be read coherently from the store before the cadence decision.

## Fix

`src/window.rs` now reads the current presentation stamp and dimensions from `SurfaceStore` under one lock before choosing the damage cadence (`950cb1a`, integrated by `bb6a329`).

## Verification

Independent verifier ran `damage_cadence_uses_current_dimensions_after_a_large_snapshot`; it passed, and the window test family passed 69 tests. Replacing current store dimensions with stale snapshot dimensions made the regression fail at `src/window.rs:3390` by returning a deadline instead of `None`; restoration passed both runs with a clean worktree.

Lead verifier ran the cadence regression, which passed. The same stale-dimension mutation failed at `src/window.rs:3390` with a non-`None` deadline; restoration passed the cadence and occlusion regressions.

All repository gates passed on tree `837d6e13c6bc162709d3bd18507eb24ed6175e58`: `cargo build --locked`, `cargo test --locked`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/test-vendored.sh`, and `./scripts/check-windows.sh --locked`. Windows checking emitted only the existing unused `width` and `height` warnings at `src/present.rs:95`. No live RDP session was run; deterministic window cadence tests exercise the stale-dimension behavior directly.
