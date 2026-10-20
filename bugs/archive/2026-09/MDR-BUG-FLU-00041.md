# MDR-BUG-FLU-00041 — Rhydra 5K HEVC fallback requires Level 4.1, which cannot describe a 5K stream

- **State:** Closed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/codec
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
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T19:35:07Z, deltic:auto role=fix run=fix-20260823T193446Z-e0329036 branch=task/bug-MDR-BUG-FLU-00041-run-fix-20260823T193446Z-e0329036 code=86c6731 gate=manual) -> Closed (2026-09-13T09:22:49Z, 0x4D44/Codex verify run=verify-20260913T091149Z-eaab1187)

## Observation

tools/latency-spike/server/src/win/pipeline.rs:304-307 creates one 5120x2880 HEVC stream, while annexb.rs:414-435 requires level_idc 123 and encode.rs:997-1001 requests Level 4.1. Level 4.1 cannot describe that 5K picture, so the fallback must either be rejected or advertise an invalid contract. Select and validate a level appropriate to the encoded geometry and rate.

## Fix

Rhydra now selects the smallest HEVC Main-tier level whose H.265 picture-size,
dimension, luma-sample-rate, and bitrate limits cover the requested stream. The
default 2560×1440@60 fallback requests and validates Level 5; 5120×2880@60 requests
and validates Level 6. Contracts beyond Level 6.2 fail before an encoder is opened.

The selected level is applied to the Media Foundation output type, retained through
mid-stream renegotiation checks, and independently enforced against the emitted SPS
before any HEVC access unit reaches the viewer.

## Notes

## Verification

Independent verification confirmed fix commit 86c6731d85c02d8efbbe9b6fac250415b9378ab9. The current `LEVEL_LIMITS` selector in tools/latency-spike/server/src/annexb.rs:425-502 chooses the smallest legal Main-tier level from picture size, dimensions, luma rate, and bitrate; tools/latency-spike/server/src/win/encode.rs:928 and tools/latency-spike/server/src/win/pipeline.rs:828 consume the selected level for encoder and SPS checks.

The focused `annexb::tests::hevc_level_is_selected_from_picture_rate_and_main_tier_bitrate` test passed 1/1, covering 2560×1440@60 → Level 5 (IDC 150) and 5120×2880@60 → Level 6 (IDC 180). `hevc_level_refuses_a_contract_beyond_level_6_2` passed 1/1. The independent verifier also ran the full annexb test module (17/17) and the `five_k_` tests (3/3), including both supported geometries.

As a red root mutant, changing the Level 6 `max_luma_picture` limit at tools/latency-spike/server/src/annexb.rs:452 from 35_651_584 to 8_912_896 made the focused selection test fail: it returned `Ok(183)` instead of `Ok(180)` at the 5120×2880 assertion. The source was restored; the two focused tests then passed again. The independent verifier separately changed Level 6 IDC 180 to 150 and observed the same test fail with `left: Ok(150)`, `right: Ok(180)`.

The six repository gates passed: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows check exited 0 and emitted only the existing unused `width`/`height` warnings in src/present.rs. No live Windows Media Foundation or encoder/SPS run was possible on this macOS host, so that runtime coverage remains unverified.
