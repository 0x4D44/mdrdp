# MDR-BUG-FLU-00071 — EGFX updates can be presented before EndFrame completes the logical frame

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/rendering-atomicity
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
- **State history:** Open (2026-08-23T20:34:24Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T22:55:51Z, deltic:auto role=fix run=fix-20260823T214405Z-857a448c branch=task/bug-MDR-BUG-FLU-00071-run-fix-20260823T214405Z-857a448c code=a25b787 gate=manual) -> Closed (2026-09-13T12:01:35Z, 0x4D44/Codex verify run=verify-20260913T115138Z-4e0524c9)

## Observation

The RDP session treats any SurfaceStore generation change as paint and can wake the window after processing a DVC payload that contains StartFrame plus only part of the updates. Nothing gates presentation on EndFrame; the callback currently updates statistics only. Legal channel segmentation can therefore expose a mixed old/new desktop. Publish or wake presentation only at the EndFrame commit boundary while preserving unframed-control behavior.

## Fix

EGFX mutations now remain behind a SurfaceStore logical-frame transaction and publish only at the matching EndFrame. Aborted or mismatched frames remain terminal so a later partial frame cannot expose mixed pixels.

## Notes

## Verification

The verification build is commit 5a59d3ce99883f5f82aef6537296094a81f7974c and contains the full fix commit a25b787766b125ecfb06020fe004d01ed27d91e2. GfxHandler starts and completes the SurfaceStore transaction in src/gfx.rs:736-742 and :910-915; SurfaceStore retains the last presentation while the frame is active or aborted and commits only at the matching EndFrame.

The lead focused commands gfx::tests::split_bitmap_payloads_stay_hidden_until_end_frame, surface::tests::a_logical_frame_publishes_split_writes_once_at_commit, surface::tests::an_aborted_frame_blocks_a_later_partial_commit, and surface::tests::a_tiled_batch_retires_the_presentation_fallback_at_commit each passed: 1 passed, 0 failed, 907 filtered out. An independent verifier reran the EndFrame regression: 1 passed, and then ran the gfx module with 65 passing tests, the surface module with 62 passing tests, the window expose test, the tiled presentation test, and all vendored suites.

As a root behavioral mutant, I removed only SurfaceStore::begin_frame(frame_id) from GfxHandler::on_frame_start. The selected regression failed with exit 101 because the store generation advanced to 3 while the split payload was still in flight, instead of remaining at 1 until EndFrame. The independent verifier reproduced the same red mutant. Restoring the source made the selected test pass 1/1 and git diff --exit-code for the source returned 0; the logical-frame and aborted-frame regressions remained green.

The six repository gates all exited 0 on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate emitted the existing unused width/height warnings in src/present.rs.

No live RDP server, GUI compositor, Windows runtime, or network protocol stream was exercised, so this closure relies on the portable logical-frame tests and source-level mutant evidence.
