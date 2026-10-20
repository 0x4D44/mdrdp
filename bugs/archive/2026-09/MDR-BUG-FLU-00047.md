# MDR-BUG-FLU-00047 — AVC444 v1 drops chroma on unaligned surface widths

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** gfx/avc444
- **Raised:** 2026-08-23T12:08:41Z
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
- **State history:** Open (2026-08-23T12:08:41Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-23T12:14:10Z, deltic:auto role=fix run=fix-20260823T120917Z-08d8da0e branch=task/bug-MDR-BUG-FLU-00047-run-fix-20260823T120917Z-08d8da0e code=e9fd732 gate=manual) -> Closed (2026-09-13T09:59:51Z, 0x4D44/Codex verify run=verify-20260913T095003Z-f482c1cb)

## Observation

The AVC444 client applies the AVC444v2 align32 width requirement to AVC444 v1 before either combined or chroma-only passes. A v1 frame decoded at an ordinary unaligned surface width, such as 1366, is therefore rejected on every chroma pass even though v1 packing uses row blocks and does not require the v2 horizontal split. The client should require surface width for v1 and align32(surface width) only for v2, while keeping the existing height and well-formedness checks.

## Fix

The AVC444 geometry gate now requires the surface width for AVC444 v1, whose chroma rows are interleaved at full width. It continues to require `align32(surface width)` for AVC444 v2, whose split chroma offsets depend on that padding.

## Notes

## Verification

Independent verification confirmed fix commit e9fd7321fa4acac1db8c8424f1f1833738c07cb. The shared geometry gate at vendor/ironrdp-egfx/src/client.rs:1386 selects the surface width for AVC444 v1 and the aligned width for v2; the combined and chroma-only paths use it at :1465 and :1559. The regression test at :3384 exercises an unaligned 60-pixel v1 surface and checks that chroma is retained.

The lead v1 regression and v2 width-guard tests each passed 1/1. The independent verifier reproduced the v1 test at 1/1 and ran the vendored suites: EGFX 47/47, graphics 219/219, and PDU 372/372. No live Windows or GPU run was available on this macOS host.

As a red root mutant, changing the AVC444 v1 arm at vendor/ironrdp-egfx/src/client.rs:1386 back to `align32` made `avc444_v1_accepts_a_frame_at_the_unaligned_surface_width` fail at :3417 with `left: 11`, `right: 25`. The independent verifier reproduced the same failure. The source was restored and the focused tests passed again.

The claim warning naming MDR-BUG-FLU-00050 was resolved as non-overlap: that record changes Rhydra sender/stats behavior, while this one changes vendored AVC444 geometry; `scripts/check-windows.sh` is shared validation infrastructure only.

The six repository gates passed: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows gate exited 0 and emitted only the existing missing icon asset and unused `width`/`height` warnings in src/present.rs.
