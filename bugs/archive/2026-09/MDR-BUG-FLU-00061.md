# MDR-BUG-FLU-00061 — EGFX discards and regrows its decompression buffer for every large graphics PDU

- **State:** Closed
- **Priority:** Must
- **Severity:** Medium
- **Area:** rdp/egfx-latency
- **Raised:** 2026-08-23T12:41:02Z
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
- **State history:** Open (2026-08-23T12:41:02Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T12:43:46Z, deltic:auto role=fix run=fix-20260823T124127Z-14da5e71 branch=task/bug-MDR-BUG-FLU-00061-run-fix-20260823T124127Z-14da5e71 code=dde8c34 gate=manual) -> Closed (2026-09-13T11:27:36Z, 0x4D44/Codex verify run=verify-20260913T111913Z-91c668cd)

## Observation

GraphicsPipelineClient clears then shrinks its reusable decompression Vec to 16 KiB before every ZGFX payload. Normal large bitmap PDUs exceed that size, so the decode hot path repeatedly discards capacity and allocates it again. Keep clear semantics and the persistent decompressor history, but retain the output buffer capacity across PDUs.

## Fix

The EGFX client clears its reusable decompression buffer without shrinking it, so large graphics payloads retain allocated capacity across decodes while the persistent ZGFX history remains unchanged.

## Notes

## Verification

The verification build is commit 9daf83c5b6eae62bda818c2801815ece394d8044 and contains the full fix commit dde8c3486635b5eadae029b05ab1134a1996d645. Its parent 425377a81bcb66d8f70229fb1ea012f6f42d7d04 still shrank the buffer to 16 KiB; the fix removes that shrink. The current decode path is vendor/ironrdp-egfx/src/client.rs:1740, and the regression is client::tests::process_retains_decompression_capacity_after_decode_error at line 2061.

The focused command cargo test --manifest-path vendor/ironrdp-egfx/Cargo.toml --locked client::tests::process_retains_decompression_capacity_after_decode_error -- --exact passed: 1 passed, 0 failed, 46 filtered out. It reserves 128 KiB, processes invalid input, and checks that the reusable buffer capacity is unchanged.

As a root behavioral mutant, I reintroduced shrink_to(16 * 1024) after clear. The same test failed with exit 101: assertion left == right failed, left 16384 and right 131072. Restoring the source made the focused test pass 1/1 and git diff --exit-code for vendor/ironrdp-egfx/src/client.rs returned 0. An independent verifier reproduced the same capacity assertion and restoration.

The six repository gates all exited 0 on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate emitted the existing unused width/height warnings in src/present.rs.

No live EGFX/RDP runtime or allocation benchmark was exercised, so this closure relies on the focused capacity oracle and source-level mutant evidence.
