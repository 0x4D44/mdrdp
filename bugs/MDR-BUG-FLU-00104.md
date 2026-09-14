# MDR-BUG-FLU-00104 — Non-AVC surface updates leave stale AVC444 detail attached

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/codec-switch
- **Raised:** 2026-08-24T12:03:48Z
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
- **Attempts:** fix=0, doubt=0, indeterminate=1
- **State history:** Open (2026-08-24T12:03:48Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-24T12:22:44Z, deltic:auto role=fix run=fix-20260824T121141Z-3afa245c branch=task/bug-MDR-BUG-FLU-00104-run-fix-20260824T121141Z-3afa245c code=0db0fa9 gate=manual) -> Open (2026-09-14T08:44:38Z, 0x4D44/Codex verify run=verify-20260914T083004Z-d8306d1e, the root invalidation mutant remained green because a later luma policy independently clears chroma, model=codex@max)

## Observation

GraphicsPipelineClient retains each surface AVC444 buffer across SolidFill, SurfaceToSurface, CacheToSurface, Uncompressed, AVC420, ClearCodec, and Progressive updates. A later LC1 luma preserves old chroma_seen samples and can repaint stale color over newer text or fills. Invalidate the destination surface AVC444 state on every non-AVC mutation and prevent chroma-only repaint until a fresh luma baseline exists.

## Fix

`edb6526` invalidates AVC444 state for non-AVC surface updates; `ccff9a0`
subsequently narrowed that invalidation to affected rectangles. The current
client calls `invalidate_avc444_rects` for regional non-AVC mutations.

## Verification

The current focused regression
`client::tests::a_non_avc_update_drops_stale_chroma_before_the_next_luma_pass`
passed for the lead and independent verifiers, and the GraphicsPipelineClient
family passed 37 tests for both.

The independent verifier removed the `invalidate_avc444_rects` call at
`vendor/ironrdp-egfx/src/client.rs:1219`. The focused test still passed, so the
root-cause mutant did not kill the oracle. The later `apply_luma` path at
`vendor/ironrdp-graphics/src/avc444.rs:483` independently clears the same
chroma state before the assertion. Restoration passed the focused and family
runs, with a clean worktree. The recorded fix therefore remains unverified and
this record is reopened until a regression specifically exercises regional
AVC444 invalidation. No live RDP session was run.

## Notes
