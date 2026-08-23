# MDR-BUG-FLU-00067 — AVC444 transient luma changes can pin valid chroma detail in permanent 4:2:0 fallback

- **State:** Fixed
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
- **State history:** Open (2026-08-23T20:34:24Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T20:45:22Z, deltic:auto role=fix run=fix-20260823T203614Z-16b2f89b branch=task/bug-MDR-BUG-FLU-00067-run-fix-20260823T203614Z-16b2f89b code=49805c0bc37268473237bf80e756e2cb7a286d6e gate=manual)

## Observation

After valid auxiliary chroma state A, an LC=1 luma update to average B sets the per-block chroma_stale bit. If a later LC=1 returns to A before an auxiliary catch-up, the client compares A with the immediately prior B and leaves the bit set; only an auxiliary pass can clear it. When the encoder omits auxiliary data because A is again its last-sent chroma state, the block stays flat 4:2:0 indefinitely, matching persistent fuzzy text or faint colour outlines after transient window movement. Preserve the last auxiliary-confirmed block average and clear stale state when luma returns to it.

## Fix

<unfixed — raised only>

## Notes
