# MDR-BUG-FLU-00063 — Malformed compressed ZGFX bitstreams panic the RDP client

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/zgfx-decoder
- **Raised:** 2026-08-23T19:39:08Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T194353Z-c50fa622
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00063-run-fix-20260823T194353Z-c50fa622
- **Owner base:** 078bb7abe1fadef80767a5dc723427bb2ad75e2c
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T19:43:53Z
- **Owner until:** 2026-08-23T21:43:53Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T19:39:08Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

The compressed ZGFX decoder uses unchecked BitSlice ranges and split_to calls for wire-controlled token fields. A short single-segment payload such as E0 24 00 07 selects a literal token then panics while taking eight absent bits, bypassing the normal protocol-error path. Make bit consumption checked throughout the compressed decoder and regress malformed truncations without unwind.

## Fix

<unfixed — raised only>

## Notes
