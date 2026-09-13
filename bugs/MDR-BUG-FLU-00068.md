# MDR-BUG-FLU-00068 — AVC444 luma-only updates repaint stale pixels beyond a cropped decoded frame

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/avc444-rendering
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
- **State history:** Open (2026-08-23T20:34:24Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T20:52:59Z, deltic:auto role=fix run=fix-20260823T204552Z-335ef56a branch=task/bug-MDR-BUG-FLU-00068-run-fix-20260823T204552Z-335ef56a code=0c69a57 gate=manual) -> Closed (2026-09-13T11:50:28Z, 0x4D44/Codex verify run=verify-20260913T113856Z-788a1171)

## Observation

The LC=1 path accepts any internally well-formed decoded YUV frame, even when its dimensions do not cover the advertised update rectangles. apply_luma silently updates only the intersection, but decode_avc444 emits every full wire rectangle from the persistent YUV444 buffer. A cropped or malformed main frame therefore repaints old or black pixels as though they were current, producing partial redraws and outlines. Reject the update or emit only the proven covered region.

## Fix

The AVC444 client checks every advertised luma rectangle against the decoded frame dimensions before applying persistent YUV444 state. An uncovered LC=1 or LC=0 frame is skipped, while a smaller decoded frame is accepted when it fully covers the requested ROI.

## Notes

## Verification

The verification build is commit 0ef236becc141d2bce892e29bb85bc9104bcaa4b and contains the full fix commit 0c69a572a67faf5086e9b26d00b0d1ea11323871. The coverage guard is in vendor/ironrdp-egfx/src/client.rs:1516 for the LC=0 path, with the equivalent LC=1 guard at line 1436 and the shared luma_rects_covered_by_frame helper at line 1845.

The lead focused commands client::tests::avc444_lc1_skips_rects_not_covered_by_decoded_luma_frame, client::tests::avc444_lc0_skips_rects_not_covered_by_decoded_luma_frame, and client::tests::avc444_lc1_accepts_a_smaller_frame_for_a_covered_roi each passed: 1 passed, 0 failed, 46 filtered out. An independent verifier reran the LC=1 and LC=0 uncovered-frame regressions: 1 passed each.

As a root behavioral mutant, I changed only the LC=1 coverage guard in vendor/ironrdp-egfx/src/client.rs:1516 to if false. The selected test failed with exit 101 because it observed an Update for the uncovered (0, 0, 64, 48) rectangle from the decoded 32x24 frame, instead of the expected skipped repaint. The independent verifier reproduced the same red mutant. Restoring the source made the selected test pass 1/1 and git diff --exit-code for the source returned 0; the LC=0 and covered-ROI regressions also remained green.

The six repository gates all exited 0 on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate emitted the existing unused width/height warnings in src/present.rs.

No live AVC444/RDP session, Windows runtime, network input, or fuzz run was exercised, so this closure relies on the offline decoder regressions and source-level mutant evidence.
