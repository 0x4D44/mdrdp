# MDR-BUG-FLU-00080 — Sparse ClearCodec updates falsely complete replacement-surface coverage

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/clearcodec-rendering
- **Raised:** 2026-08-23T22:00:53Z
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
- **State history:** Open (2026-08-23T22:00:53Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T22:43:45Z, deltic:auto role=fix run=fix-20260823T220110Z-6bacf6a1 branch=task/bug-MDR-BUG-FLU-00080-run-fix-20260823T220110Z-6bacf6a1 code=a17f497 gate=manual) -> Closed (2026-09-13T13:17:51Z, 0x4D44/Codex verify run=verify-20260913T130719Z-cc2c4cd3)

## Observation

ClearCodec decodes sparse layers into a caller-seeded rectangle, but the EGFX path blits and marks the entire rectangle as newly covered. During a same-size surface handoff, several sparse updates can therefore retire the old-frame fallback while untouched pixels in the replacement remain transparent black. Track only pixels actually written by ClearCodec, or otherwise prevent sparse decode output from proving full replacement coverage.

## Fix

`a17f49729aaa2980b40d3ad2e4ce164095762efa` carries explicit ClearCodec write
coverage from the decoder through `GfxHandler` into `SurfaceStore`, so sparse
updates cannot retire replacement fallback coverage for untouched pixels.

## Notes

## Verification

The verification tree was commit `e4ddab86c9d0f1e8ec369ae2adff8b80c4c5ee33`,
which contains the fix commit `a17f49729aaa2980b40d3ad2e4ce164095762efa`.

The lead ran these sparse regressions, each 1/1:
`gfx::tests::sparse_clearcodec_decoder_reports_explicit_writes_not_colour_differences`,
`gfx::tests::sparse_clearcodec_decode_is_not_cached_as_a_full_glyph`, and
`gfx::tests::sparse_clearcodec_tiles_do_not_complete_a_replacement_until_pixels_are_written`.
The independent verifier ran the same three, 65 `gfx` tests, 62 `surface`
tests, and the vendored sparse regression
`sparse_clearcodec_invalidates_only_the_regions_the_handler_painted`; all passed.
The vendored ClearCodec suite passed 19 tests.

As the lead root mutant, I changed `&coverage` to `&[]` at `src/gfx.rs:527`.
The replacement regression failed at `src/gfx.rs:1906`; the independent verifier
reproduced the same red mutant. Restoring `&coverage` made the focused regression
pass 1/1, and `git diff --exit-code`, `git diff --check`, and `git status` were
clean.

The six repository gates all exited 0 on the verification tree: `cargo build
--locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy
--all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and
`./scripts/check-windows.sh --locked`. The vendored battery reported 47 EGFX,
219 graphics, and 372 PDU tests. The Windows gate emitted the existing unused
`width`/`height` warnings in `src/present.rs`. No live RDP or GUI runtime was
needed for this deterministic decoder and coverage fix.
