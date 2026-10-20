# MDR-BUG-FLU-00086 — Overhanging sparse ClearCodec tiles replace untouched edge pixels with black

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/clearcodec-rendering
- **Raised:** 2026-08-23T22:33:58Z
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
- **State history:** Open (2026-08-23T22:33:58Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-23T23:07:06Z, deltic:auto role=fix run=fix-20260823T225616Z-2e30450d branch=task/bug-MDR-BUG-FLU-00086-run-fix-20260823T225616Z-2e30450d code=590720469adb8f852fbaa04c1ff16e6d3077185c gate=manual) -> Closed (2026-09-13T14:03:45Z, 0x4D44/Codex verify run=verify-20260913T140256Z-747847c6)

## Observation

GfxHandler seeds ClearCodec with Surface::extract(destination), which clips an overhanging destination and returns a shorter buffer. ClearCodecDecoder rejects that seed length and starts from zeros, so sparse layers leave visible in-bounds edge pixels black when the full decoded rectangle is blitted back. Preserve a full destination-sized seed with clipped surface pixels at their correct stride, and regress a sparse right/bottom overhang.

## Fix

`590720469adb8f852fbaa04c1ff16e6d3077185c` seeds ClearCodec with
`Surface::extract_with_zero_padding`, preserving the requested full stride while
copying clipped surface pixels. The helper rejects oversized decode extents before
allocation, and the vendored ClearCodec decoder can therefore retain untouched edge
pixels when sparse overhanging tiles are applied.

## Notes

## Verification

The verification tree was commit `25e3fc005dc5b7cd2bca4f4c012f1b340934bfdc`,
which contains fix commit `590720469adb8f852fbaa04c1ff16e6d3077185c`.

The lead and independent verifier each ran
`gfx::tests::clearcodec_right_bottom_overhang_seeds_the_full_requested_extent`;
it passed 1/1 in both runs. The independent verifier also ran the 14-test ClearCodec
family and the vendored suites, which passed 47, 219, and 372 tests.

For the lead root mutant, I changed only the production seed call from
`extract_with_zero_padding(dest)` to `extract(dest)`. The regression failed at
`src/gfx.rs:1813` with `[0, 0, 0, 0]` instead of `[18, 33, 48, 255]` for the untouched
edge pixel. Restoring the fix made the test pass 1/1. The independent verifier
reproduced the same red mutant; source diff and `git diff --check` were clean after
restoration.

The six repository gates all exited 0 on the verification tree: `cargo build
--locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy
--all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and
`./scripts/check-windows.sh --locked`. The Windows gate emitted the existing unused
`width`/`height` warnings in `src/present.rs`. This deterministic render regression
needed no live RDP or GUI runtime.
