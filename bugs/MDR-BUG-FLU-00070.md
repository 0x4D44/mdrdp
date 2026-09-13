# MDR-BUG-FLU-00070 — Native tiled frames can be presented after only one tile has updated

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** native/rendering-atomicity
- **Raised:** 2026-08-23T20:34:24Z
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
- **State history:** Open (2026-08-23T20:34:24Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T21:14:47Z, deltic:auto role=fix run=fix-20260823T204716Z-15a4fc0c branch=task/bug-MDR-BUG-FLU-00070-run-fix-20260823T204716Z-15a4fc0c code=42b82b1d63e7cd8b0b36ae6887f61891d23a3171 gate=manual) -> Closed (2026-09-13T12:01:35Z, 0x4D44/Codex verify run=verify-20260913T115128Z-76a9783b)

## Observation

NativeSink writes each decoded tile directly into the shared SurfaceStore and increments its visible generation before the logical frame is complete. Wakeups wait for all tiles, but an already queued platform redraw can snapshot between tile writes and present half of frame N+1 beside half of frame N. Commit a tiled sequence to the presentation-visible surface atomically only after every advertised tile for that sequence has arrived.

## Fix

Native tiled frames stage each decoded tile and validate the complete set before one SurfaceStore batch commit. A partial or malformed tile set therefore leaves the last presentation and generation unchanged.

## Notes

## Verification

The verification build is commit 5a59d3ce99883f5f82aef6537296094a81f7974c and contains the full fix commit 42b82b1d63e7cd8b0b36ae6887f61891d23a3171. NativeSink stages tiles in src/native/session.rs:2967-3018 and publishes them through SurfaceStore::blit_rgba_strict_batch at line 3038; the batch path validates all rectangles before advancing the visible generation.

The lead focused commands native::session::tests::two_tiles_with_one_capture_sequence_both_paint, native::session::tests::the_first_tile_keeps_the_last_presentation_until_commit, native::session::tests::a_malformed_later_tile_cannot_partially_paint_a_staged_batch, and surface::tests::a_tiled_batch_retires_the_presentation_fallback_at_commit each passed: 1 passed, 0 failed, 907 filtered out. An independent verifier reran the first-tile regression: 1 passed, and then ran the native session module with 64 passing tests and the surface module with 62 passing tests.

As a root behavioral mutant, I changed only the incomplete-tile guard in src/native/session.rs:3016 from the pending-set check to if false. The selected test failed with exit 101 because the first tile advanced the store generation from 1 to 2 before the complete set arrived. The independent verifier reproduced the same red mutant. Restoring the source made the selected test pass 1/1 and git diff --exit-code for the source returned 0; the malformed-batch and complete-set regressions remained green.

The six repository gates all exited 0 on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate emitted the existing unused width/height warnings in src/present.rs.

No live RDP server, compositor presentation, Windows runtime, or network stream was exercised, so this closure relies on fake-decoder and SurfaceStore regressions plus source-level mutant evidence.
