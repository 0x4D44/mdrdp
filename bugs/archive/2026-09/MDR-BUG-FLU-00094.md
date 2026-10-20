# MDR-BUG-FLU-00094 — IOSurface backpressure repeats full 5K redraw work on the input event thread

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** window/presentation-latency
- **Raised:** 2026-08-24T09:51:00Z
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
- **State history:** Open (2026-08-24T09:51:00Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T09:58:35Z, deltic:auto role=fix run=fix-20260824T095120Z-a12ed09b branch=task/bug-MDR-BUG-FLU-00094-run-fix-20260824T095120Z-a12ed09b code=ab364fc gate=manual) -> Closed (2026-09-14T06:45:25Z, 0x4D44/Codex verify run=verify-20260913T152407Z-01671e16)

## Observation

Observation: SessionApp::redraw fills or converts the full staging frame and draws overlays before FrameBuf::present checks whether any IOSurface is writable. When all three surfaces remain compositor-owned, every 2 ms Busy retry repeats the full 5K CPU work on the winit event thread, delaying keyboard and mouse dispatch. Expected: an all-busy pool defers before expensive rendering while present retains the authoritative recheck. Actual: availability is first checked only after the work is complete.

## Fix

`src/present.rs` now checks whether an IOSurface can be selected before the expensive staging and render path, while `src/window.rs` keeps the authoritative preflight and retry (`7967921`, integrated by `ab364fc`).

## Verification

Independent verifier ran four focused IOSurface availability and render-deferral tests; all passed. Mutating the `!dimensions_match` polarity in `src/present.rs:215` made the resized-pool regression fail at its assertion in `src/present.rs:499`; restoring it returned all four tests to green with a clean diff.

Lead verifier on tree `8a131b0fc56f16d1cd5dbdca65776dcefde70664` reran the four IOSurface regressions and the FLU-00095 input-clock regression; each selected test passed. Mutating the same dimensions-polarity branch made `an_empty_or_resized_pool_allows_lazy_rebuild` fail at `src/present.rs:510`; restoration passed all five batch-focused tests.

All repository gates passed: `cargo build --locked`, `cargo test --locked`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/test-vendored.sh`, and `./scripts/check-windows.sh --locked`. Windows checking emitted only the existing unused `width` and `height` warnings at `src/present.rs:95`. No live RDP compositor session was run; deterministic presentation helper tests exercise the recorded backpressure behavior directly.
