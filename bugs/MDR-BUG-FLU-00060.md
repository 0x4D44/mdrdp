# MDR-BUG-FLU-00060 — Malformed ZGFX multipart segment length panics the RDP client

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/zgfx-decoder
- **Raised:** 2026-08-23T12:35:14Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T110621Z-3ac98203
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00060-run-verify-20260913T110621Z-3ac98203
- **Owner base:** 78ef0fe0276a5736b297915b87ebf8b6d18faa03
- **Owner fingerprint:** sha256:6c310de0f1b649ec83ebf6555f47bc1ac17e651fb92a75a2ad1e7050909348f3
- **Owner since:** 2026-09-13T11:06:21Z
- **Owner until:** 2026-09-13T13:06:21Z
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
