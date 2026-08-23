# MDR-BUG-FLU-00080 — Sparse ClearCodec updates falsely complete replacement-surface coverage

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/clearcodec-rendering
- **Raised:** 2026-08-23T22:00:53Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T220110Z-6bacf6a1
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00080-run-fix-20260823T220110Z-6bacf6a1
- **Owner base:** 2ad5d0ccaa7e3bc62f210ac24bb884506bbba151
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T22:01:10Z
- **Owner until:** 2026-08-24T00:01:10Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T22:00:53Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

ClearCodec decodes sparse layers into a caller-seeded rectangle, but the EGFX path blits and marks the entire rectangle as newly covered. During a same-size surface handoff, several sparse updates can therefore retire the old-frame fallback while untouched pixels in the replacement remain transparent black. Track only pixels actually written by ClearCodec, or otherwise prevent sparse decode output from proving full replacement coverage.

## Fix

<unfixed — raised only>

## Notes
