# MDR-BUG-FLUX-00022 — rhydra HEVC rejects quench's valid Main-tier stream because the contract requires unsupported High tier

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** native-transport
- **Raised:** 2026-08-20T10:57:47Z
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
- **State history:** Open (2026-08-20T10:57:47Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-20T11:02:02Z, deltic:auto role=fix run=fix-20260820T105810Z-p67382-n663796000-c1 branch=task/bug-MDR-BUG-FLUX-00022-run-fix-20260820T105810Z-p67382-n663796000-c1 code=2fdfdf2 gate=manual) -> Closed (2026-09-13T07:53:16Z, 0x4D44/Codex verify run=verify-20260913T073846Z-1d11b877)

## Observation

Live v4 smoke on quench connected successfully, then the server rejected its first encoded access unit: Intel Hardware H265 Encoder MFT emitted Main profile, Main tier, Level 4.1, while annexb::validate_config required High tier. The probe that informed the HLD used the same 20 Mbit/s rate but its parser skipped the tier bit; Media Foundation exposes no separate HEVC tier setting. Expected: the stated 20 Mbit/s HEVC contract accepts and validates Main tier, while still refusing wrong profile, level, chroma, depth, or dimensions.

## Fix

<unfixed — raised only>

## Notes

## Verification

Independent verification confirmed fix commit `2fdfdf2ce4b9d2557ad0f7484ae7ad2f926abe06` and the Main-tier contract value at `tools/latency-spike/server/src/annexb.rs:515`, which requires `high_tier = 0`. The focused regression `annexb::tests::stream_contract_accepts_main_tier_and_rejects_every_wrong_field` passed after restoration. As a red root mutant, changing the expected tier back to `1` made it fail at `tools/latency-spike/server/src/annexb.rs:869` with `expected: 1, actual: 0`. The value was restored and the focused regression passed again.

The repository gates then passed: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`.

No live Quench protocol validation was attempted because it requires a live video connection and occupies the host's single viewer slot. No new live claim is made. The original Quench Main-tier observation remains the end-to-end product observation.
