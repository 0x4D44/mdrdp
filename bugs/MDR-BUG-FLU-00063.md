# MDR-BUG-FLU-00063 — Malformed compressed ZGFX bitstreams panic the RDP client

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/zgfx-decoder
- **Raised:** 2026-08-23T19:39:08Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T112913Z-3fd0421b
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00063-run-verify-20260913T112913Z-3fd0421b
- **Owner base:** 7160de6dbbeda006958a12b533b5d637174bae85
- **Owner fingerprint:** sha256:8f27f1700e058899a162e423a8bd4aefe4abbb906a65df8fea72bbfcb3188e35
- **Owner since:** 2026-09-13T11:29:13Z
- **Owner until:** 2026-09-13T13:29:13Z
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
