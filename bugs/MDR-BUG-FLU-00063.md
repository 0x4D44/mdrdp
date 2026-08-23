# MDR-BUG-FLU-00063 — Malformed compressed ZGFX bitstreams panic the RDP client

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/zgfx-decoder
- **Raised:** 2026-08-23T19:39:08Z
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
- **State history:** Open (2026-08-23T19:39:08Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T22:00:30Z, deltic:auto role=fix run=fix-20260823T201502Z-29cd23f9 branch=task/bug-MDR-BUG-FLU-00063-run-fix-20260823T201502Z-29cd23f9 code=2b50693 gate=manual)

## Observation

The compressed ZGFX decoder uses unchecked BitSlice ranges and split_to calls for wire-controlled token fields. A short single-segment payload such as E0 24 00 07 selects a literal token then panics while taking eight absent bits, bypassing the normal protocol-error path. Make bit consumption checked throughout the compressed decoder and regress malformed truncations without unwind.

## Fix

<unfixed — raised only>

## Notes
