# MDR-BUG-FLU-00058 — 5K cadence gate copies frames that it immediately discards

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/presentation-latency
- **Raised:** 2026-08-23T12:21:07Z
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
- **State history:** Open (2026-08-23T12:21:07Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-23T12:27:52Z, deltic:auto role=fix run=fix-20260823T122126Z-3e6dc0d9 branch=task/bug-MDR-BUG-FLU-00058-run-fix-20260823T122126Z-3e6dc0d9 code=66d74b5 gate=manual) -> Closed (2026-09-13T11:04:16Z, 0x4D44/Codex verify run=verify-20260913T105651Z-c2cb6014)

## Observation

SessionApp::redraw acquires the surface-store lock and copies the full presentation snapshot before it checks the 33 ms large-canvas cadence. A platform redraw inside the cadence window therefore copies roughly 56 MiB at 5120x2880, blocks decoding behind the store lock, and then returns without presenting those bytes. Check the generation and cadence before allocating a platform frame or copying the presentation snapshot, while retaining the post-copy guard needed for races and geometry changes.

## Fix

The redraw path checks the current generation, dimensions, and cadence while holding the store lock, then defers before copying a large presentation snapshot when the cadence has not expired. It keeps the post-copy race and geometry guards for redraws that are allowed to proceed.

## Notes

## Verification

The verification build contains fix commit 66d74b5186875cd5ff19244ff921c8b947534607. The direct window tests passed 4/4, and the full window::tests filter passed 69/69.

As a root behavioral mutant, the pre-copy cadence gate was disabled. The selected test window::tests::an_early_large_redraw_does_not_copy_the_presentation_snapshot failed with exit 101 because it observed Ok(Copied) instead of the expected Err deadline. An independent verifier reversed the gate by copying before the cadence return; the same test failed because the snapshot changed from (7, 9, 11) to (2561, 1, 1). Restoring the exact source made the test pass 1/1 and left an empty source diff.

Deltic warned that this record shares sessionapp::redraw with MDR-BUG-FLU-00094. The root causes are distinct: 00058 gates the large-canvas cadence before snapshot copying, while 00094 preflights an all-busy IOSurface pool before expensive rendering. The 00094 record was not changed.

The six repository gates passed on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate exited 0 with the existing unused width/height warnings.

The threshold test verifies copy avoidance using a 2561x1 surface. No live 5120x2880 copy timing, compositor, GPU, or production latency measurement was collected or claimed.
