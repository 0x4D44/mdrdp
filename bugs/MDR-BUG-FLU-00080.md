# MDR-BUG-FLU-00080 — Sparse ClearCodec updates falsely complete replacement-surface coverage

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/clearcodec-rendering
- **Raised:** 2026-08-23T22:00:53Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T130719Z-cc2c4cd3
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00080-run-verify-20260913T130719Z-cc2c4cd3
- **Owner base:** 87342ae2a70fbf3a36e96468e40a549d140e9b77
- **Owner fingerprint:** sha256:86ff42f973bd7b5d4084b6cc34aff170c348b75adcb69ed866a34f47b4892f1c
- **Owner since:** 2026-09-13T13:07:19Z
- **Owner until:** 2026-09-13T15:07:19Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T22:00:53Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T22:43:45Z, deltic:auto role=fix run=fix-20260823T220110Z-6bacf6a1 branch=task/bug-MDR-BUG-FLU-00080-run-fix-20260823T220110Z-6bacf6a1 code=a17f497 gate=manual)

## Observation

ClearCodec decodes sparse layers into a caller-seeded rectangle, but the EGFX path blits and marks the entire rectangle as newly covered. During a same-size surface handoff, several sparse updates can therefore retire the old-frame fallback while untouched pixels in the replacement remain transparent black. Track only pixels actually written by ClearCodec, or otherwise prevent sparse decode output from proving full replacement coverage.

## Fix

<unfixed — raised only>

## Notes
