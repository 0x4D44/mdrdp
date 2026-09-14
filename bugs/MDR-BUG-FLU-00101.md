# MDR-BUG-FLU-00101 — Presentation latency includes the entire window occlusion interval

- **State:** Closed
- **Priority:** Should
- **Severity:** Medium
- **Area:** metrics/presentation-latency
- **Raised:** 2026-08-24T11:33:45Z
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
- **State history:** Open (2026-08-24T11:33:45Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T11:54:28Z, deltic:auto role=fix run=fix-20260824T114233Z-66ad510a branch=task/bug-MDR-BUG-FLU-00101-run-fix-20260824T114233Z-66ad510a code=a56efa7 gate=manual) -> Closed (2026-09-14T08:03:25Z, 0x4D44/Codex verify run=verify-20260914T074512Z-e679caa5)

## Observation

SessionStats retains a paint-to-present sample while the window is occluded. A late hidden paint can remain pending until reveal, so the first redraw records seconds or minutes of hidden time as renderer presentation latency. Occlusion transitions must cancel pending presentation samples while allowing later visible paint-to-present measurements to resume.

## Fix

`src/stats.rs` and `src/window.rs` now discard pending paint-to-present handoffs when occlusion changes, so hidden time cannot enter the renderer latency sample (`3855479`, integrated by `a56efa7`).

## Verification

Independent verifier ran `occlusion_drops_hidden_present_handoffs_but_visible_measurements_resume`; it passed. The stats family passed 36 tests and the window family passed 69 tests. Disabling `cancel_pending_present` made the focused test fail at `src/stats.rs:801` with one hidden sample instead of zero; restoration passed the focused and family runs with a clean worktree.

Lead verifier ran the occlusion regression, which passed. Disabling `cancel_pending_present` made it fail at `src/stats.rs:802` with one hidden sample instead of zero; restoration passed the cadence and occlusion regressions.

All repository gates passed on tree `837d6e13c6bc162709d3bd18507eb24ed6175e58`: `cargo build --locked`, `cargo test --locked`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/test-vendored.sh`, and `./scripts/check-windows.sh --locked`. Windows checking emitted only the existing unused `width` and `height` warnings at `src/present.rs:95`. No live RDP session was run; deterministic statistics and window tests exercise the occlusion behavior directly.
