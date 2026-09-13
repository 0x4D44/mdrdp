# MDR-BUG-FLU-00067 — AVC444 transient luma changes can pin valid chroma detail in permanent 4:2:0 fallback

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/avc444-rendering
- **Raised:** 2026-08-23T20:34:24Z
- **Discovery source:** Human
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
- **State history:** Open (2026-08-23T20:34:24Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T20:45:22Z, deltic:auto role=fix run=fix-20260823T203614Z-16b2f89b branch=task/bug-MDR-BUG-FLU-00067-run-fix-20260823T203614Z-16b2f89b code=49805c0bc37268473237bf80e756e2cb7a286d6e gate=manual) -> Open (2026-09-13T05:45:26Z, independent verifier: the original return-to-A regression passed only on superseded commit 49805c0; current HEAD replaces that implementation with dea775e and the original symptom still fails, model=codex@max)

## Observation

After valid auxiliary chroma state A, an LC=1 luma update to average B sets the per-block chroma_stale bit. If a later LC=1 returns to A before an auxiliary catch-up, the client compares A with the immediately prior B and leaves the bit set; only an auxiliary pass can clear it. When the encoder omits auxiliary data because A is again its last-sent chroma state, the block stays flat 4:2:0 indefinitely, matching persistent fuzzy text or faint colour outlines after transient window movement. Preserve the last auxiliary-confirmed block average and clear stale state when luma returns to it.

## Fix

The recorded fix commit `49805c0bc37268473237bf80e756e2cb7a286d6e` contains and passes
`a_luma_return_to_last_aux_average_clears_stale_without_aux` when run from that
commit's tree. The current integrated tree later replaced that stale-bit design in
`dea775e713bbb52be6d324608f37d292becf3d48`.

To re-run the original scenario against current code, a temporary verifier test sent
auxiliary state A, luma state B, then luma state A without another auxiliary frame. It
was selected and failed its own `aux detail must return without another aux frame`
assertion: current output `[0, 94, 0, 255]`, expected `[192, 0, 213, 255]`. The temporary
test and all source mutations were removed; the source diff is empty. The symptom
therefore persists on current HEAD and this record is reopened for a new fix.

## Notes
