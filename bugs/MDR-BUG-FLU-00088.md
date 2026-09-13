# MDR-BUG-FLU-00088 — ClearCodec counts unwritten coverage as painted telemetry

- **State:** Closed
- **Priority:** Should
- **Severity:** Medium
- **Area:** rdp/clearcodec-telemetry
- **Raised:** 2026-08-23T23:16:08Z
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
- **State history:** Open (2026-08-23T23:16:08Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-23T23:20:40Z, deltic:auto role=fix run=fix-20260823T231638Z-d27cebf9 branch=task/bug-MDR-BUG-FLU-00088-run-fix-20260823T231638Z-d27cebf9 code=5210024fa35983ad03b5d0d57ca661c59b86f9a6 gate=manual) -> Closed (2026-09-13T14:34:33Z, 0x4D44/Codex verify run=verify-20260913T140805Z-dc1a5d4a)

## Observation

GfxHandler records the full ClearCodec destination as codec_bytes_painted even when exact coverage is empty or the surface write fails. Pixels and presentation generation stay unchanged, but diagnostics report bytes that never landed and can mislead codec and latency analysis. Account only successful in-bounds coverage returned by SurfaceStore::blit_rgba_with_coverage.

## Fix

`5210024fa35983ad03b5d0d57ca661c59b86f9a6` counts only the bytes returned by
`SurfaceStore::blit_rgba_with_coverage`. Empty explicit coverage and refused writes
therefore leave `codec_bytes_painted` unchanged, while successful clipped coverage
reports only the bytes that landed.

## Notes

## Verification

The verification tree was commit `2746987e4b664bec1d7d316ee209e02027d03b54`,
which contains fix commit `5210024fa35983ad03b5d0d57ca661c59b86f9a6`.

The lead and independent verifier each ran
`gfx::tests::empty_clearcodec_coverage_does_not_report_painted_bytes`; it passed
1/1 in both runs. The independent verifier also ran the 65-test graphics family,
the root suite (908 library, 17 binary, 5 integration, and 3 ZGFX tests), and all
vendored suites (638 tests): all passed.

For the lead root mutant, I restored full-destination accounting in the `Ok(0)`
branch. The regression failed at `src/gfx.rs:2004` with `Some(16)` instead of
`None` for `codec_bytes_painted`. Restoring the fix made the test pass 1/1. The
independent verifier reproduced the same failure. Source diff, `git diff --check`,
and final status were clean after restoration.

The six repository gates all exited 0 on the verification tree: `cargo build
--locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy
--all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and
`./scripts/check-windows.sh --locked`. The Windows gate emitted the existing unused
`width`/`height` warnings in `src/present.rs`. This deterministic telemetry fix
needed no live RDP or GUI runtime.
