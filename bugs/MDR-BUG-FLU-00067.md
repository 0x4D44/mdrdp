# MDR-BUG-FLU-00067 — AVC444 transient luma changes can pin valid chroma detail in permanent 4:2:0 fallback

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/avc444-rendering
- **Raised:** 2026-08-23T20:34:24Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T203614Z-16b2f89b
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00067-run-fix-20260823T203614Z-16b2f89b
- **Owner base:** 81fef86c078d398588f4f383dbb41c805b7ffccb
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T20:36:14Z
- **Owner until:** 2026-08-23T22:36:14Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T20:34:24Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

After valid auxiliary chroma state A, an LC=1 luma update to average B sets the per-block chroma_stale bit. If a later LC=1 returns to A before an auxiliary catch-up, the client compares A with the immediately prior B and leaves the bit set; only an auxiliary pass can clear it. When the encoder omits auxiliary data because A is again its last-sent chroma state, the block stays flat 4:2:0 indefinitely, matching persistent fuzzy text or faint colour outlines after transient window movement. Preserve the last auxiliary-confirmed block average and clear stale state when luma returns to it.

## Fix

<unfixed — raised only>

## Notes
