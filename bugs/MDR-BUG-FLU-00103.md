# MDR-BUG-FLU-00103 — Odd-sized AVC444 edge chroma is discarded by later luma

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/avc444
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
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T12:03:48Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-24T12:13:16Z, deltic:auto role=fix run=fix-20260824T120414Z-824ae691 branch=task/bug-MDR-BUG-FLU-00103-run-fix-20260824T120414Z-824ae691 code=404b723 gate=manual) -> Open (2026-09-14T08:22:56Z, 0x4D44/Codex verify run=verify-20260914T080750Z-2601c231, original later-luma symptom persists after a subsequent rendering-policy change, model=codex@max)

## Observation

Yuv444Buffer::record_partial_chroma requires all three auxiliary samples for every 2x2 block. On odd-width or odd-height surface edges some sample positions do not exist, so valid edge detail never becomes chroma_seen and the next LC1 luma overwrites it with 4:2:0 averages. Promote against the mask of auxiliary positions that actually exist inside the surface.

## Fix

The recorded fix commit `a8a6968` changed `record_partial_chroma` to promote a block when every auxiliary position that exists inside the surface has arrived; its associated version bump is `404b723`. A temporary direct edge-promotion oracle passed, and changing the condition from `required != 0 && samples & required == required` to the old `samples == 0b111` rule made its `chroma_seen_at(2, 1)` assertion fail. That proves the mask change itself is covered.

The original survivor regressions were removed by later commit `dea775e`, which changed `apply_luma` to clear AVC444 chroma because luma rectangles must use the main view. I reintroduced those historical v1 and v2 regressions temporarily on the current tree; each selected one test and failed its own assertion: the edge values became `(100, 100)` instead of the previously delivered detail. The current replacement tests and the 20-test AVC444 family pass, but they assert the opposite outcome. The recorded observation therefore still occurs on current HEAD, and this bug is reopened for a requirement or design decision.

## Notes
