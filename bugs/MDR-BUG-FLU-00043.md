# MDR-BUG-FLU-00043 — Rhydra NV12 converter pool can grow GPU memory without bound

- **State:** Closed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/video-memory
- **Raised:** 2026-08-22T19:40:40Z
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
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T13:52:50Z, deltic:auto role=fix run=fix-20260823T133831Z-735aea9e branch=task/bug-MDR-BUG-FLU-00043-run-fix-20260823T133831Z-735aea9e code=745d5b78ff7f1c112746b681d4e9a397babb2719 gate=manual) -> Closed (2026-09-13T09:34:16Z, 0x4D44/Codex verify run=verify-20260913T092539Z-5180b4aa)

## Observation

tools/latency-spike/server/src/win/convert.rs:231-255 allocates another GPU surface whenever all current surfaces remain leased. The capture loop at pipeline.rs:1056-1099 keeps converting tiles, so an MFT that accepts input without retiring output can grow the pool until GPU allocation fails. Bound the pool and apply backpressure or fail explicitly.

## Fix

The converter now owns a fixed four-slot lease budget per tile and never allocates
another NV12 texture after startup. Before each captured frame, the pipeline pumps
every tile encoder and releases its retired surfaces. If any tile still has no free
slot, it discards the complete desktop frame before conversion, rect emission, or
pixel-diff baseline advancement. This preserves tile and decoder coherence while
bounding the normal 5K two-tile NV12 pool at about 88 MiB. Persistent saturation
for 500 ms exits explicitly so the session supervisor rebuilds the encoder rather
than leaving a live process with permanently frozen video.

Portable lease tests prove exhaustion refuses a fifth lease, released slots are
reused round-robin, and invalid releases fail. The recovery test proves a frame
discarded before encoding increments loss telemetry without needlessly entering
keyframe recovery.

## Notes

## Verification

Independent verification confirmed fix commit 745d5b78ff7f1c112746b681d4e9a397babb2719. The current converter allocates four surfaces per tile in tools/latency-spike/server/src/win/convert.rs:243-276, tracks leases through tools/latency-spike/server/src/surface_pool.rs:28-69, pumps encoder retirement before capture, discards a complete frame when any tile has no capacity, and exits after the 500 ms saturation watchdog in tools/latency-spike/server/src/win/pipeline.rs:1550-1660. Loss telemetry increments without entering keyframe recovery through tools/latency-spike/server/src/logical_frame.rs:34-38.

The lead focused checks passed 4/4 surface-pool lease/watchdog tests and 1/1 recovery test. The independent verifier additionally ran 10 logical-frame tests, 17 matching stats tests, and a temporary four-slot harness (5/5), including fifth-lease refusal and reuse. The independent verifier also confirmed the Windows converter and pipeline wiring with the cross-target check.

As a red root mutant, changing the lease selector at tools/latency-spike/server/src/surface_pool.rs:49 from `!self.in_use[slot]` to `self.in_use[slot]` made `refuses_a_lease_when_the_fixed_budget_is_full` fail: `left: None`, `right: Some(0)`. The source was restored and the focused lease/watchdog and recovery tests passed again. The independent verifier separately changed the selector to always match and observed the expected lease assertion fail.

The six repository gates passed: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows check exited 0 and emitted only the existing unused `width`/`height` warnings in src/present.rs. No live Windows GPU or Media Foundation run was available on this macOS host, so runtime surface allocation and encoder retirement remain unverified.
