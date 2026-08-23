# MDR-BUG-FLU-00060 — Malformed ZGFX multipart segment length panics the RDP client

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/zgfx-decoder
- **Raised:** 2026-08-23T12:35:14Z
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
- **State history:** Open (2026-08-23T12:35:14Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T12:39:47Z, deltic:auto role=fix run=fix-20260823T123541Z-8d2b0ce0 branch=task/bug-MDR-BUG-FLU-00060-run-fix-20260823T123541Z-8d2b0ce0 code=0c8f57f gate=manual)

## Observation

A server-controlled multipart ZGFX PDU declares each segment length as u32. SegmentedDataPdu::from_buffer passes that length directly to slice::split_at, so a length larger than the remaining payload panics instead of returning the decoder's Result error. Reject a declared segment that exceeds the remaining bytes and add a malformed-PDU regression.

## Fix

<unfixed — raised only>

## Notes
