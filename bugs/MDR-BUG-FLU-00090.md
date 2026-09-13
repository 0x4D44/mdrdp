# MDR-BUG-FLU-00090 — AVC stream region masks omit the final row and column

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/AVC
- **Raised:** 2026-08-24T09:17:01Z
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
- **State history:** Open (2026-08-24T09:17:01Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T09:24:50Z, deltic:auto role=fix run=fix-20260824T091738Z-b26417bf branch=task/bug-MDR-BUG-FLU-00090-run-fix-20260824T091738Z-b26417bf code=70a5a78 gate=manual) -> Closed (2026-09-13T14:49:44Z, 0x4D44/Codex verify run=verify-20260913T144030Z-5dfa9b95)

## Observation

Avc420Region stores inclusive right/bottom bounds but copies them unchanged into wire RDPGFX_RECT16 stream rectangles, whose right/bottom bounds are exclusive. A 4x4 full-frame region therefore emits (0,0,3,3), so AVC420, AVC444, and mixed-tile clients leave the last row and column stale. Convert producer stream-mask bounds to exclusive coordinates and cover full-frame plus subregion encoding with focused regressions.

## Fix

`vendor/ironrdp-egfx/src/pdu/avc.rs:351` now adds one to the inclusive right and bottom bounds when constructing the exclusive wire rectangle (`92c2cf5`, integrated by `70a5a78`).

## Verification

Independent verifier: `deltic timeout 300 cargo test --manifest-path vendor/ironrdp-egfx/Cargo.toml --locked --lib encoded_stream_rectangles_convert_inclusive_regions_to_exclusive_wire_bounds </dev/null` passed 1 test. Replacing both `saturating_add(1)` calls at `vendor/ironrdp-egfx/src/pdu/avc.rs:355-356` with direct bounds failed that test at line 614 with `(3,3)` and `(5,7)` instead of `(4,4)` and `(6,8)`; restoration passed. The verifier also ran the 47-test EGFX family and the vendored 47/219/372 test groups successfully, with a clean diff.

Lead verifier on tree `1772165963b2a06aeb36a034f149dbaae0330e00` reran the AVC regression and the three surface-budget regressions; each selected test ran 1 test and passed. The same direct-bounds mutant failed at `vendor/ironrdp-egfx/src/pdu/avc.rs:614`; restoring the fix made the AVC test pass again.

All repository gates passed: `cargo build --locked`, `cargo test --locked`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/test-vendored.sh`, and `./scripts/check-windows.sh --locked`. Windows checking emitted only the existing unused `width` and `height` warnings at `src/present.rs:95`. No live RDP session was needed because the deterministic wire serialization regression exercises the recorded failure and root fix.
