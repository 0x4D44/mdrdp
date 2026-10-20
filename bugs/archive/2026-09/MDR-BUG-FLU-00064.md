# MDR-BUG-FLU-00064 — Rhydra raw rects can paint against a dropped video baseline during recovery

- **State:** Closed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/rendering
- **Raised:** 2026-08-23T19:42:47Z
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
- **State history:** Open (2026-08-23T19:42:47Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T19:46:30Z, deltic:auto role=fix run=fix-20260823T194320Z-e1a72827 branch=task/bug-MDR-BUG-FLU-00064-run-fix-20260823T194320Z-e1a72827 code=76ebf9f gate=manual) -> Closed (2026-09-13T11:36:54Z, 0x4D44/Codex verify run=verify-20260913T112922Z-06f88a5f)

## Observation

When a complete encoded frame is dropped by the bounded outbound queue, logical-frame recovery suppresses later inter-frame video until an all-tile keyframe arrives, but pipeline.rs still emits raw metadata/diff rects before consulting that recovery state. PixelDiff also advances its retained baseline before outbound admission, so the next raw rect can describe a delta from pixels the viewer never received and visibly corrupt the desktop. Gate rect emission and baseline trust on the same recovery invariant, while preserving the raw fast path after a confirmed recovery frame.

## Fix

Rhydra exposes whether recovery has an admitted all-tile keyframe baseline and gates metadata, raw, move, and pixel-diff overlays on that predicate. Overlays resume only after the recovery frame enters the outbound queue and are disabled again when recovery resets.

## Notes

## Verification

The verification build is commit 87a88a06276548a0d3051dee184011bd3c1ee74e and contains the full fix commit 76ebf9fc94c0983fa817b30a2a626dda0159f669. Recovery::allows_overlays at tools/latency-spike/server/src/logical_frame.rs:79 is false while a keyframe is pending. The Windows capture path gates sparse metadata, raw or move routing, and pixel diff through that predicate at tools/latency-spike/server/src/win/pipeline.rs:1679, 1874, and 1950; recovery is admitted only after outbound admission at line 1042.

The lead focused command cargo test --manifest-path tools/latency-spike/server/Cargo.toml --locked logical_frame::tests::recovery_suppresses_deltas_and_survives_a_full_queue -- --exact passed: 1 passed, 0 failed, 408 filtered out. It checks that overlays begin disabled, become allowed after an all-keyframe admission, and become disabled after reset. An independent verifier ran the full logical_frame module: 10 passed, 0 failed, 399 filtered out.

As a root behavioral mutant, I changed only Recovery::allows_overlays to return true. The selected regression failed with exit 101 at src/logical_frame.rs:454: assertion failed: !recovery.allows_overlays(). Restoring the source made the selected test pass 1/1 and left an empty source diff. The independent verifier reproduced the same failure and reran all 10 module tests green after restoration.

The six repository gates all exited 0 on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate emitted the existing unused width/height warnings in src/present.rs.

No Windows DXGI/IDD capture run, GPU pixel-diff path, or live Rhydra viewer session was available on this Mac, so this closure relies on the portable recovery oracle and source-level mutant evidence.
