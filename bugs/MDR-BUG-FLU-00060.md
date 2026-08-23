# MDR-BUG-FLU-00060 — Malformed ZGFX multipart segment length panics the RDP client

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/zgfx-decoder
- **Raised:** 2026-08-23T12:35:14Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T123541Z-8d2b0ce0
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00060-run-fix-20260823T123541Z-8d2b0ce0
- **Owner base:** 965e87e9be7db8adc03fd25bf89cc03e6e7a60b4
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T12:35:41Z
- **Owner until:** 2026-08-23T14:35:41Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T12:35:14Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

A server-controlled multipart ZGFX PDU declares each segment length as u32. SegmentedDataPdu::from_buffer passes that length directly to slice::split_at, so a length larger than the remaining payload panics instead of returning the decoder's Result error. Reject a declared segment that exceeds the remaining bytes and add a malformed-PDU regression.

## Fix

<unfixed — raised only>

## Notes
