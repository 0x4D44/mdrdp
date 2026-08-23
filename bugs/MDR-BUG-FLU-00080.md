# MDR-BUG-FLU-00080 — Sparse ClearCodec updates falsely complete replacement-surface coverage

- **State:** Open
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
- **State history:** Open (2026-08-23T22:00:53Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

ClearCodec decodes sparse layers into a caller-seeded rectangle, but the EGFX path blits and marks the entire rectangle as newly covered. During a same-size surface handoff, several sparse updates can therefore retire the old-frame fallback while untouched pixels in the replacement remain transparent black. Track only pixels actually written by ClearCodec, or otherwise prevent sparse decode output from proving full replacement coverage.

## Fix

<unfixed — raised only>

## Notes
